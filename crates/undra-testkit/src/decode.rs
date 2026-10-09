//! Naming and decoding what a recording holds, from a schema: the call `Todos.add`, the port call
//! `Http.request`, the signal `Todos.todos`, and their bytes as JSON a person can read.
//!
//! A recording carries ids and bytes (SPEC 1.1 to 1.3): type ids, method ids, port ids and signal
//! ids, which the schema names, and payloads in the wire encoding of SPEC 3, which the schema's
//! types decode. [`SchemaIndex`] is that lookup, built once from a [`Schema`]. Decoding never
//! fails a report: a value the schema does not describe, or bytes that do not decode, come back as
//! `{"$bytes": "<hex>", "$type": "<name>"}`.
//!
//! The JSON conventions: records are objects; an enum value is `{"$": "Variant", ...fields}` (a
//! tuple variant's fields are `"0"`, `"1"`, ..); a map is `{"$map": [[key, value], ...]}`; bytes
//! are lower-case hex strings; a 64-bit integer is a number when it fits 53 bits and a string
//! otherwise; `Duration` is `{"$dur_ns": n}`, `Timestamp` `{"$ts_ms": n}`, `Uuid` its hyphenated
//! text, `Decimal` its decimal text; an object handle is `{"$handle": "0x.."}`; `Option::None` is
//! `null`.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};
use undra_meta::{ParamDef, Schema, TypeClosure, TypeRef};
use undra_runtime::persist::{DynRecord, DynValue, decode_dyn, decode_params};
use undra_wire::payload::{ChangeOp, PortStatus, ReplyStatus};

use crate::hex;
use crate::names::standard_name;
use crate::recording::Target;

/// Where a method id points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callable {
    /// `schema.functions[i]`.
    Function(usize),
    /// `schema.objects[object].constructors[index]`.
    Constructor { object: usize, index: usize },
    /// `schema.objects[object].methods[index]`.
    Method { object: usize, index: usize },
}

/// The ids of a schema, resolved: what a recorded id names and how its bytes decode.
#[derive(Clone, Debug)]
pub struct SchemaIndex {
    schema: Schema,
    callables: HashMap<u32, Callable>,
    objects: HashMap<u32, usize>,
    object_names: HashSet<String>,
    ports: HashMap<u32, usize>,
}

/// `{"$bytes": "<hex>", "$type": "<name>"}`: bytes the schema does not decode.
#[must_use]
pub fn undecoded(bytes: &[u8], type_name: Option<&str>) -> Value {
    let mut out = Map::new();
    out.insert("$bytes".into(), Value::String(hex::encode(bytes)));
    if let Some(name) = type_name {
        out.insert("$type".into(), Value::String(name.to_owned()));
    }
    Value::Object(out)
}

/// `{"$handle": "0x.."}`: an object handle.
#[must_use]
pub fn handle_json(handle: u64) -> Value {
    let mut out = Map::new();
    out.insert("$handle".into(), Value::String(format!("{handle:#018x}")));
    Value::Object(out)
}

impl SchemaIndex {
    /// Indexes `schema`.
    #[must_use]
    pub fn new(schema: &Schema) -> SchemaIndex {
        let mut callables = HashMap::new();
        let mut objects = HashMap::new();
        let mut object_names = HashSet::new();
        for (i, f) in schema.functions.iter().enumerate() {
            callables.insert(f.method_id, Callable::Function(i));
        }
        for (object, o) in schema.objects.iter().enumerate() {
            objects.insert(o.type_id, object);
            object_names.insert(o.name.clone());
            for (index, c) in o.constructors.iter().enumerate() {
                callables.insert(c.method_id, Callable::Constructor { object, index });
            }
            for (index, m) in o.methods.iter().enumerate() {
                callables.insert(m.method_id, Callable::Method { object, index });
            }
        }
        let ports = schema
            .ports
            .iter()
            .enumerate()
            .map(|(i, p)| (p.port_id, i))
            .collect();
        SchemaIndex {
            schema: schema.clone(),
            callables,
            objects,
            object_names,
            ports,
        }
    }

    /// The schema.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The schema's hash.
    #[must_use]
    pub fn hash(&self) -> u64 {
        self.schema.hash()
    }

    /// The name of the object type `type_id`.
    #[must_use]
    pub fn object_name(&self, type_id: u32) -> Option<&str> {
        self.objects
            .get(&type_id)
            .map(|&i| self.schema.objects[i].name.as_str())
    }

    /// `Todos.add` for a method or constructor id, `configure_remote` for a function id.
    #[must_use]
    pub fn method_name(&self, method: u32) -> Option<String> {
        Some(match *self.callables.get(&method)? {
            Callable::Function(i) => self.schema.functions[i].name.clone(),
            Callable::Constructor { object, index } => {
                let o = &self.schema.objects[object];
                format!("{}.{}", o.name, o.constructors[index].name)
            }
            Callable::Method { object, index } => {
                let o = &self.schema.objects[object];
                format!("{}.{}", o.name, o.methods[index].name)
            }
        })
    }

    /// The name of a recorded call's target (`None` for a lazy page, which has no method).
    #[must_use]
    pub fn call_name(&self, target: &Target) -> Option<String> {
        match target {
            Target::Function { method }
            | Target::Method { method, .. }
            | Target::Constructor { method, .. } => self.method_name(*method),
            Target::LazyPage { .. } => None,
        }
    }

    fn params_and_returns(&self, c: Callable) -> (&[ParamDef], &TypeRef) {
        match c {
            Callable::Function(i) => {
                let f = &self.schema.functions[i];
                (&f.params, &f.returns)
            }
            Callable::Constructor { object, index } => {
                let m = &self.schema.objects[object].constructors[index];
                (&m.params, &m.returns)
            }
            Callable::Method { object, index } => {
                let m = &self.schema.objects[object].methods[index];
                (&m.params, &m.returns)
            }
        }
    }

    /// The arguments of a call to `method`, as an object of its parameters by name.
    #[must_use]
    pub fn decode_args(&self, method: u32, args: &[u8]) -> Value {
        let Some(&c) = self.callables.get(&method) else {
            return undecoded(args, None);
        };
        let (params, _) = self.params_and_returns(c);
        let name = self.method_name(method).unwrap_or_default();
        self.decode_param_list(&name, params, args)
    }

    /// The reply body of a call to `method`: the return type's value on `ok`, the error type's on
    /// `error`, `null` for an empty body.
    #[must_use]
    pub fn decode_reply(&self, method: u32, status: ReplyStatus, body: &[u8]) -> Value {
        let Some(&c) = self.callables.get(&method) else {
            return empty_or_undecoded(body, None);
        };
        let (_, returns) = self.params_and_returns(c);
        match status {
            ReplyStatus::Ok => self.decode_ok(returns, body),
            ReplyStatus::Error => self.decode_err(returns, body),
            _ => empty_or_undecoded(body, None),
        }
    }

    /// `Http.request` for a port method: from the schema, or the standard ports' names table.
    #[must_use]
    pub fn port_name(&self, port: u32, method: u32) -> Option<String> {
        if let Some(&i) = self.ports.get(&port) {
            let p = &self.schema.ports[i];
            if let Some(m) = p.methods.iter().find(|m| m.method_id == method) {
                return Some(format!("{}.{}", p.name, m.name));
            }
        }
        standard_name(port, method)
    }

    fn port_method(&self, port: u32, method: u32) -> Option<&undra_meta::MethodDef> {
        let &i = self.ports.get(&port)?;
        self.schema.ports[i]
            .methods
            .iter()
            .find(|m| m.method_id == method)
    }

    /// The arguments of a port call (or the payload of a port event), as an object of the
    /// method's parameters by name.
    #[must_use]
    pub fn decode_port_args(&self, port: u32, method: u32, args: &[u8]) -> Value {
        let Some(m) = self.port_method(port, method) else {
            return undecoded(args, None);
        };
        let name = self.port_name(port, method).unwrap_or_default();
        self.decode_param_list(&name, &m.params, args)
    }

    /// The body of a port reply: the method's return value on `ok`, its error on `error`.
    #[must_use]
    pub fn decode_port_reply(
        &self,
        port: u32,
        method: u32,
        status: PortStatus,
        body: &[u8],
    ) -> Value {
        let Some(m) = self.port_method(port, method) else {
            return empty_or_undecoded(body, None);
        };
        match status {
            PortStatus::Ok => self.decode_ok(&m.returns, body),
            PortStatus::Error => self.decode_err(&m.returns, body),
            PortStatus::Unavailable => empty_or_undecoded(body, None),
        }
    }

    /// `Todos.todos` for a store's signal (`*` for `u32::MAX`, every signal).
    #[must_use]
    pub fn signal_name(&self, type_id: u32, signal: u32) -> Option<String> {
        let &i = self.objects.get(&type_id)?;
        let o = &self.schema.objects[i];
        if signal == u32::MAX {
            return Some(format!("{}.*", o.name));
        }
        let s = o
            .store
            .as_ref()?
            .signals
            .iter()
            .find(|s| s.signal_id == signal)?;
        Some(format!("{}.{}", o.name, s.name))
    }

    /// The value of a change-set entry: decoded for `full`; a patch stays bytes, named after the
    /// signal's type. A `Lazy<T>` signal's value (`handle u64, len u32, version u64`, SPEC 3.1) is
    /// `{"$lazy": "Lazy<T>", "len": n, "version": v}` and a lazy invalidation (`len u32,
    /// version u64`) `{"$invalidated": "Lazy<T>", "len": n, "version": v}`: the page server's
    /// handle is left out, being per session like every handle.
    #[must_use]
    pub fn decode_signal(&self, type_id: u32, signal: u32, op: ChangeOp, value: &[u8]) -> Value {
        let ty = self.objects.get(&type_id).and_then(|&i| {
            self.schema.objects[i]
                .store
                .as_ref()?
                .signals
                .iter()
                .find(|s| s.signal_id == signal)
                .map(|s| &s.ty)
        });
        match (op, ty) {
            (ChangeOp::Full, Some(ty @ TypeRef::Lazy(_))) => {
                let name = type_name(ty);
                lazy_json("$lazy", &name, value.get(8..))
                    .unwrap_or_else(|| undecoded(value, Some(&name)))
            }
            (ChangeOp::Full, Some(ty)) => self.decode_value(ty, value),
            (ChangeOp::Full, None) => undecoded(value, None),
            (ChangeOp::KeyedPatch, ty) => {
                undecoded(value, Some(&format!("patch of {}", type_text(ty))))
            }
            (ChangeOp::LazyInvalidated, ty) => {
                let name = type_text(ty);
                lazy_json("$invalidated", &name, Some(value))
                    .unwrap_or_else(|| undecoded(value, Some(&format!("invalidation of {name}"))))
            }
        }
    }

    /// `bytes` as one value of `ty`.
    #[must_use]
    pub fn decode_value(&self, ty: &TypeRef, bytes: &[u8]) -> Value {
        let Some(wire) = self.wire_type(ty) else {
            return undecoded(bytes, Some(&type_name(ty)));
        };
        let closure = self.schema.closure(&wire);
        match decode_dyn(bytes, &wire, &closure) {
            Ok(v) => self.to_json(&v, Some(ty), &closure),
            Err(_) => undecoded(bytes, Some(&type_name(ty))),
        }
    }

    fn decode_ok(&self, returns: &TypeRef, body: &[u8]) -> Value {
        let ok = match returns {
            TypeRef::Result(ok, _) => ok.as_ref(),
            other => other,
        };
        match ok {
            TypeRef::Unit if body.is_empty() => Value::Null,
            TypeRef::Stream(_) => empty_or_undecoded(body, Some(&type_name(ok))),
            other => self.decode_value(other, body),
        }
    }

    fn decode_err(&self, returns: &TypeRef, body: &[u8]) -> Value {
        match returns {
            TypeRef::Result(_, err) => self.decode_value(err, body),
            _ => empty_or_undecoded(body, None),
        }
    }

    /// Decodes `bytes` as `params`, named `name`, to an object by parameter name.
    fn decode_param_list(&self, name: &str, params: &[ParamDef], bytes: &[u8]) -> Value {
        if params.is_empty() && bytes.is_empty() {
            return Value::Object(Map::new());
        }
        let Some(wire_params) = params
            .iter()
            .map(|p| {
                Some(ParamDef {
                    name: p.name.clone(),
                    ty: self.wire_type(&p.ty)?,
                })
            })
            .collect::<Option<Vec<_>>>()
        else {
            return undecoded(bytes, Some(name));
        };
        let closure = self.schema.closure_of_params(&wire_params);
        match decode_params(bytes, name, &closure) {
            Ok(record) => {
                let mut out = Map::new();
                for (field, value) in &record.fields {
                    let ty = params.iter().find(|p| &p.name == field).map(|p| &p.ty);
                    out.insert(field.clone(), self.to_json(value, ty, &closure));
                }
                Value::Object(out)
            }
            Err(_) => undecoded(bytes, Some(name)),
        }
    }

    /// Whether `name` names an object type (which crosses as a handle).
    fn is_object(&self, name: &str) -> bool {
        self.object_names.contains(name)
    }

    /// `ty` with every object reference as the `u64` its handle is on the wire, or `None` for a
    /// type that is not a value (a lazy list, a stream, a `Result`, `Unit`).
    fn wire_type(&self, ty: &TypeRef) -> Option<TypeRef> {
        Some(match ty {
            TypeRef::Object(_) | TypeRef::Callback(_) => TypeRef::U64,
            TypeRef::Named(n) if self.is_object(n) => TypeRef::U64,
            TypeRef::Option(inner) => TypeRef::option(self.wire_type(inner)?),
            TypeRef::Vec(item) => TypeRef::vec(self.wire_type(item)?),
            TypeRef::Map(k, v) => TypeRef::map(self.wire_type(k)?, self.wire_type(v)?),
            TypeRef::Lazy(_) | TypeRef::Stream(_) | TypeRef::Result(..) | TypeRef::Unit => {
                return None;
            }
            other => other.clone(),
        })
    }

    /// `value` as JSON, `ty` (when known) telling which integers are handles.
    fn to_json(&self, value: &DynValue, ty: Option<&TypeRef>, closure: &TypeClosure) -> Value {
        match (value, ty) {
            (DynValue::Int(i), Some(TypeRef::Object(_) | TypeRef::Callback(_))) => {
                u64::try_from(*i).map_or_else(|_| plain(value), handle_json)
            }
            (DynValue::Int(i), Some(TypeRef::Named(n))) if self.is_object(n) => {
                u64::try_from(*i).map_or_else(|_| plain(value), handle_json)
            }
            (DynValue::Some(inner), Some(TypeRef::Option(t))) => {
                self.to_json(inner, Some(t), closure)
            }
            (DynValue::List(items), Some(TypeRef::Vec(t))) => Value::Array(
                items
                    .iter()
                    .map(|v| self.to_json(v, Some(t), closure))
                    .collect(),
            ),
            (DynValue::Map(entries), Some(TypeRef::Map(k, v))) => map_json(
                entries
                    .iter()
                    .map(|(key, value)| {
                        (
                            self.to_json(key, Some(k), closure),
                            self.to_json(value, Some(v), closure),
                        )
                    })
                    .collect(),
            ),
            (DynValue::Record(record), Some(TypeRef::Named(n))) => {
                let fields = closure.record(n).map(|r| &r.fields);
                self.record_json(record, fields.map(Vec::as_slice), closure)
            }
            (
                DynValue::Enum {
                    variant, fields, ..
                },
                Some(TypeRef::Named(n)),
            ) => {
                let types = closure
                    .enum_def(n)
                    .and_then(|e| e.variants.iter().find(|v| &v.name == variant))
                    .map(|v| v.fields.as_slice());
                enum_json(variant, self.record_json(fields, types, closure))
            }
            (other, _) => plain(other),
        }
    }

    fn record_json(
        &self,
        record: &DynRecord,
        types: Option<&[undra_meta::ClosureField]>,
        closure: &TypeClosure,
    ) -> Value {
        let mut out = Map::new();
        for (name, value) in &record.fields {
            let ty =
                types.and_then(|fields| fields.iter().find(|f| &f.name == name).map(|f| &f.ty));
            out.insert(name.clone(), self.to_json(value, ty, closure));
        }
        Value::Object(out)
    }
}

/// `{tag: name, "len": n, "version": v}` from the `len u32, version u64` that `bytes` must be
/// exactly (ADR-043), or `None` when they are not.
fn lazy_json(tag: &str, name: &str, bytes: Option<&[u8]>) -> Option<Value> {
    let bytes = bytes?;
    if bytes.len() != 12 {
        return None;
    }
    let len = u32::from_le_bytes(bytes[..4].try_into().ok()?);
    let version = u64::from_le_bytes(bytes[4..].try_into().ok()?);
    let mut out = Map::new();
    out.insert(tag.into(), Value::String(name.to_owned()));
    out.insert("len".into(), Value::from(len));
    out.insert("version".into(), int_json(i128::from(version)));
    Some(Value::Object(out))
}

fn type_text(ty: Option<&TypeRef>) -> String {
    ty.map_or_else(|| "an unknown type".to_owned(), type_name)
}

/// `ty` as a reader of the bindings knows it: `Vec<Todo>`, `Option<String>`, `Map<String, u32>`.
#[must_use]
pub fn type_name(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Bool => "bool".to_owned(),
        TypeRef::I8 => "i8".to_owned(),
        TypeRef::I16 => "i16".to_owned(),
        TypeRef::I32 => "i32".to_owned(),
        TypeRef::I64 => "i64".to_owned(),
        TypeRef::U8 => "u8".to_owned(),
        TypeRef::U16 => "u16".to_owned(),
        TypeRef::U32 => "u32".to_owned(),
        TypeRef::U64 => "u64".to_owned(),
        TypeRef::F32 => "f32".to_owned(),
        TypeRef::F64 => "f64".to_owned(),
        TypeRef::String => "String".to_owned(),
        TypeRef::Bytes => "Bytes".to_owned(),
        TypeRef::Unit => "()".to_owned(),
        TypeRef::Duration => "Duration".to_owned(),
        TypeRef::Timestamp => "Timestamp".to_owned(),
        TypeRef::Uuid => "Uuid".to_owned(),
        TypeRef::Decimal => "Decimal".to_owned(),
        TypeRef::Option(t) => format!("Option<{}>", type_name(t)),
        TypeRef::Vec(t) => format!("Vec<{}>", type_name(t)),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", type_name(k), type_name(v)),
        TypeRef::Lazy(t) => format!("Lazy<{}>", type_name(t)),
        TypeRef::Result(t, e) => format!("Result<{}, {}>", type_name(t), type_name(e)),
        TypeRef::Stream(t) => format!("Stream<{}>", type_name(t)),
        TypeRef::Named(n) | TypeRef::Object(n) | TypeRef::Callback(n) => n.clone(),
    }
}

fn empty_or_undecoded(body: &[u8], type_name: Option<&str>) -> Value {
    if body.is_empty() {
        Value::Null
    } else {
        undecoded(body, type_name)
    }
}

fn map_json(entries: Vec<(Value, Value)>) -> Value {
    let mut out = Map::new();
    out.insert(
        "$map".into(),
        Value::Array(
            entries
                .into_iter()
                .map(|(k, v)| Value::Array(vec![k, v]))
                .collect(),
        ),
    );
    Value::Object(out)
}

/// `{"$": variant, ...fields}`.
fn enum_json(variant: &str, fields: Value) -> Value {
    let mut out = Map::new();
    out.insert("$".into(), Value::String(variant.to_owned()));
    if let Value::Object(fields) = fields {
        out.extend(fields);
    }
    Value::Object(out)
}

/// An integer as a JSON number when it fits 53 bits (every JavaScript reader keeps it exact), as
/// a string otherwise.
fn int_json(i: i128) -> Value {
    const LIMIT: i128 = 1 << 53;
    if (-LIMIT..=LIMIT).contains(&i) {
        // Fits an i64, and serde_json keeps i64 exact.
        Value::from(i64::try_from(i).unwrap_or_default())
    } else {
        Value::String(i.to_string())
    }
}

/// A float as a JSON number, or its text when JSON has no number for it (NaN, infinities).
fn float_json(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or_else(|| Value::String(f.to_string()), Value::Number)
}

/// `mantissa x 10^-scale` as decimal text.
fn decimal_text(mantissa: i128, scale: u8) -> String {
    let negative = mantissa < 0;
    let digits = mantissa.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    if scale == 0 {
        text.push_str(&digits);
    } else if digits.len() > scale {
        let (whole, frac) = digits.split_at(digits.len() - scale);
        text.push_str(whole);
        text.push('.');
        text.push_str(frac);
    } else {
        text.push_str("0.");
        text.extend(std::iter::repeat_n('0', scale - digits.len()));
        text.push_str(&digits);
    }
    text
}

fn uuid_text(bytes: &[u8; 16]) -> String {
    let h = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// `value` as JSON without a type (every integer is a number or a string, never a handle).
fn plain(value: &DynValue) -> Value {
    match value {
        DynValue::Bool(b) => Value::Bool(*b),
        DynValue::Int(i) => int_json(*i),
        DynValue::Float(f) => float_json(*f),
        DynValue::Float32(f) => float_json(f64::from(*f)),
        DynValue::String(s) => Value::String(s.clone()),
        DynValue::Bytes(b) => Value::String(hex::encode(b)),
        DynValue::Duration(n) => {
            let mut out = Map::new();
            out.insert("$dur_ns".into(), int_json(i128::from(*n)));
            Value::Object(out)
        }
        DynValue::Timestamp(n) => {
            let mut out = Map::new();
            out.insert("$ts_ms".into(), int_json(i128::from(*n)));
            Value::Object(out)
        }
        DynValue::Uuid(bytes) => Value::String(uuid_text(bytes)),
        DynValue::Decimal { mantissa, scale } => Value::String(decimal_text(*mantissa, *scale)),
        DynValue::None => Value::Null,
        DynValue::Some(inner) => plain(inner),
        DynValue::List(items) => Value::Array(items.iter().map(plain).collect()),
        DynValue::Map(entries) => {
            map_json(entries.iter().map(|(k, v)| (plain(k), plain(v))).collect())
        }
        DynValue::Record(record) => {
            let mut out = Map::new();
            for (name, value) in &record.fields {
                out.insert(name.clone(), plain(value));
            }
            Value::Object(out)
        }
        DynValue::Enum {
            variant, fields, ..
        } => {
            let mut out = Map::new();
            for (name, value) in &fields.fields {
                out.insert(name.clone(), plain(value));
            }
            enum_json(variant, Value::Object(out))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;
    use undra_meta::{
        EnumDef, FieldDef, FunctionDef, MethodDef, ObjectDef, ParamDef, PortDef, PortKind,
        RecordDef, SignalDef, StoreDef, VariantDef, ids,
    };
    use undra_wire::Writer;

    use super::*;

    fn field(name: &str, ty: TypeRef) -> FieldDef {
        FieldDef {
            name: name.into(),
            ty,
            default: false,
            docs: String::new(),
        }
    }

    fn param(name: &str, ty: TypeRef) -> ParamDef {
        ParamDef {
            name: name.into(),
            ty,
        }
    }

    fn method(owner: &str, name: &str, params: Vec<ParamDef>, returns: TypeRef) -> MethodDef {
        MethodDef {
            name: name.into(),
            method_id: ids::method_id(owner, name),
            params,
            returns,
            is_async: false,
            takes_ctx: false,
            coalesce: false,
            generic: None,
            docs: String::new(),
        }
    }

    /// A to-do core: a record, an enum with a tuple variant, an error, a store, a function that
    /// takes an object, and a port.
    pub(crate) fn schema() -> Schema {
        let mut s = Schema::new("todo-core");
        s.records.push(RecordDef {
            name: "Todo".into(),
            type_id: ids::type_id("Todo"),
            fields: vec![
                field("id", TypeRef::U64),
                field("title", TypeRef::String),
                field("due", TypeRef::option(TypeRef::Timestamp)),
            ],
            transparent: false,
            docs: String::new(),
        });
        s.enums.push(EnumDef {
            name: "Filter".into(),
            type_id: ids::type_id("Filter"),
            is_error: false,
            variants: vec![
                VariantDef {
                    name: "All".into(),
                    index: 0,
                    fields: vec![],
                    tuple: false,
                    message: None,
                    docs: String::new(),
                },
                VariantDef {
                    name: "Tagged".into(),
                    index: 1,
                    fields: vec![field("0", TypeRef::String)],
                    tuple: true,
                    message: None,
                    docs: String::new(),
                },
            ],
            docs: String::new(),
        });
        s.enums.push(EnumDef {
            name: "TodoError".into(),
            type_id: ids::type_id("TodoError"),
            is_error: true,
            variants: vec![VariantDef {
                name: "EmptyTitle".into(),
                index: 0,
                fields: vec![],
                tuple: false,
                message: Some("empty".into()),
                docs: String::new(),
            }],
            docs: String::new(),
        });
        s.objects.push(ObjectDef {
            name: "Todos".into(),
            type_id: ids::type_id("Todos"),
            constructors: vec![method("Todos", "new", vec![], TypeRef::named("Todos"))],
            methods: vec![
                method(
                    "Todos",
                    "add",
                    vec![param("title", TypeRef::String)],
                    TypeRef::result(TypeRef::named("Todo"), TypeRef::named("TodoError")),
                ),
                method(
                    "Todos",
                    "tags",
                    vec![],
                    TypeRef::map(TypeRef::String, TypeRef::vec(TypeRef::U8)),
                ),
            ],
            store: Some(StoreDef {
                signals: vec![
                    SignalDef {
                        name: "items".into(),
                        signal_id: 0,
                        ty: TypeRef::vec(TypeRef::named("Todo")),
                        computed: false,
                        key: Some("id".into()),
                        no_coalesce: false,
                        default: false,
                    },
                    SignalDef {
                        name: "filter".into(),
                        signal_id: 1,
                        ty: TypeRef::named("Filter"),
                        computed: false,
                        key: None,
                        no_coalesce: false,
                        default: false,
                    },
                    SignalDef {
                        name: "archive".into(),
                        signal_id: 2,
                        ty: TypeRef::Lazy(Box::new(TypeRef::named("Todo"))),
                        computed: false,
                        key: Some("id".into()),
                        no_coalesce: false,
                        default: false,
                    },
                ],
            }),
            docs: String::new(),
        });
        s.functions.push(FunctionDef {
            name: "link".into(),
            method_id: ids::function_id("link"),
            params: vec![
                param("store", TypeRef::object("Todos")),
                param("wait", TypeRef::Duration),
                param("raw", TypeRef::Bytes),
            ],
            returns: TypeRef::Unit,
            is_async: false,
            takes_ctx: false,
            generic: None,
            docs: String::new(),
        });
        s.ports.push(PortDef {
            name: "Notifier".into(),
            port_id: ids::port_id("Notifier"),
            kind: PortKind::Async,
            background: false,
            methods: vec![method(
                "Notifier",
                "notify",
                vec![param("text", TypeRef::String)],
                TypeRef::result(TypeRef::U32, TypeRef::named("TodoError")),
            )],
            docs: String::new(),
        });
        s
    }

    fn bytes(f: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut w = Writer::new();
        f(&mut w);
        w.into_vec()
    }

    #[test]
    fn names_calls_ports_and_signals() {
        let index = SchemaIndex::new(&schema());
        assert_eq!(
            index.method_name(ids::method_id("Todos", "add")).as_deref(),
            Some("Todos.add")
        );
        assert_eq!(
            index.method_name(ids::method_id("Todos", "new")).as_deref(),
            Some("Todos.new")
        );
        assert_eq!(
            index.method_name(ids::function_id("link")).as_deref(),
            Some("link")
        );
        assert_eq!(index.method_name(7), None);
        assert_eq!(
            index
                .call_name(&Target::Method {
                    handle: 1,
                    method: ids::method_id("Todos", "add")
                })
                .as_deref(),
            Some("Todos.add")
        );
        assert_eq!(
            index.call_name(&Target::LazyPage {
                handle: 1,
                offset: 0,
                limit: 1
            }),
            None
        );
        assert_eq!(index.object_name(ids::type_id("Todos")), Some("Todos"));
        assert_eq!(
            index
                .port_name(
                    ids::port_id("Notifier"),
                    ids::port_method_id("Notifier", "notify")
                )
                .as_deref(),
            Some("Notifier.notify")
        );
        // A standard port is named even when the schema does not list it.
        assert_eq!(
            index
                .port_name(ids::port_id("Http"), ids::port_method_id("Http", "request"))
                .as_deref(),
            Some("Http.request")
        );
        assert_eq!(index.port_name(1, 2), None);
        assert_eq!(
            index.signal_name(ids::type_id("Todos"), 1).as_deref(),
            Some("Todos.filter")
        );
        assert_eq!(
            index
                .signal_name(ids::type_id("Todos"), u32::MAX)
                .as_deref(),
            Some("Todos.*")
        );
        assert_eq!(index.signal_name(ids::type_id("Todos"), 9), None);
    }

    #[test]
    fn arguments_decode_to_an_object_by_parameter_name() {
        let index = SchemaIndex::new(&schema());
        let args = bytes(|w| w.write_str("milk"));
        assert_eq!(
            index.decode_args(ids::method_id("Todos", "add"), &args),
            json!({"title": "milk"})
        );
        // No parameters: an empty object.
        assert_eq!(
            index.decode_args(ids::method_id("Todos", "new"), &[]),
            json!({})
        );
        // An object parameter is a handle; a duration and bytes keep their conventions.
        let args = bytes(|w| {
            w.write_u64(0x0000_0001_0000_0002);
            w.write_i64(1_500_000_000);
            w.write_bytes(&[0xab, 0xcd]);
        });
        assert_eq!(
            index.decode_args(ids::function_id("link"), &args),
            json!({"store": {"$handle": "0x0000000100000002"}, "wait": {"$dur_ns": 1_500_000_000}, "raw": "abcd"})
        );
        // Bytes that do not decode, and an unknown method, are hex with what was expected.
        assert_eq!(
            index.decode_args(ids::method_id("Todos", "add"), &[1, 2]),
            json!({"$bytes": "0102", "$type": "Todos.add"})
        );
        assert_eq!(index.decode_args(99, &[1]), json!({"$bytes": "01"}));
    }

    #[test]
    fn replies_decode_by_status_and_return_type() {
        let index = SchemaIndex::new(&schema());
        let add = ids::method_id("Todos", "add");
        let ok = bytes(|w| {
            w.write_u64(1);
            w.write_str("milk");
            w.write_u8(1);
            w.write_i64(1_700_000_000_000);
        });
        assert_eq!(
            index.decode_reply(add, ReplyStatus::Ok, &ok),
            json!({"id": 1, "title": "milk", "due": {"$ts_ms": 1_700_000_000_000_i64}})
        );
        let err = bytes(|w| w.write_u16(0));
        assert_eq!(
            index.decode_reply(add, ReplyStatus::Error, &err),
            json!({"$": "EmptyTitle"})
        );
        // A constructor's reply is a handle; a unit reply is null; a cancelled call has no body.
        let handle = bytes(|w| w.write_u64(0x0000_0001_0000_0000));
        assert_eq!(
            index.decode_reply(ids::method_id("Todos", "new"), ReplyStatus::Ok, &handle),
            json!({"$handle": "0x0000000100000000"})
        );
        assert_eq!(
            index.decode_reply(ids::function_id("link"), ReplyStatus::Ok, &[]),
            Value::Null
        );
        assert_eq!(
            index.decode_reply(add, ReplyStatus::Cancelled, &[]),
            Value::Null
        );
        // A map reply.
        let map = bytes(|w| {
            w.write_len(1);
            w.write_str("k");
            w.write_len(2);
            w.write_u8(7);
            w.write_u8(8);
        });
        assert_eq!(
            index.decode_reply(ids::method_id("Todos", "tags"), ReplyStatus::Ok, &map),
            json!({"$map": [["k", [7, 8]]]})
        );
    }

    #[test]
    fn port_traffic_decodes_like_calls() {
        let index = SchemaIndex::new(&schema());
        let (port, method) = (
            ids::port_id("Notifier"),
            ids::port_method_id("Notifier", "notify"),
        );
        assert_eq!(
            index.decode_port_args(port, method, &bytes(|w| w.write_str("hi"))),
            json!({"text": "hi"})
        );
        assert_eq!(
            index.decode_port_reply(port, method, PortStatus::Ok, &bytes(|w| w.write_u32(3))),
            json!(3)
        );
        assert_eq!(
            index.decode_port_reply(port, method, PortStatus::Error, &bytes(|w| w.write_u16(0))),
            json!({"$": "EmptyTitle"})
        );
        assert_eq!(
            index.decode_port_reply(port, method, PortStatus::Unavailable, &[]),
            Value::Null
        );
        // A port the schema does not describe: bytes.
        assert_eq!(index.decode_port_args(1, 2, &[9]), json!({"$bytes": "09"}));
    }

    #[test]
    fn signals_decode_full_values_and_keep_patches_as_bytes() {
        let index = SchemaIndex::new(&schema());
        let todos = ids::type_id("Todos");
        let filter = bytes(|w| {
            w.write_u16(1);
            w.write_str("home");
        });
        assert_eq!(
            index.decode_signal(todos, 1, ChangeOp::Full, &filter),
            json!({"$": "Tagged", "0": "home"})
        );
        assert_eq!(
            index.decode_signal(todos, 0, ChangeOp::Full, &bytes(|w| w.write_len(0))),
            json!([])
        );
        assert_eq!(
            index.decode_signal(todos, 0, ChangeOp::KeyedPatch, &[1, 0]),
            json!({"$bytes": "0100", "$type": "patch of Vec<Todo>"})
        );
        assert_eq!(
            index.decode_signal(todos, 0, ChangeOp::LazyInvalidated, &[]),
            json!({"$bytes": "", "$type": "invalidation of Vec<Todo>"})
        );
        assert_eq!(
            type_name(&TypeRef::map(
                TypeRef::String,
                TypeRef::option(TypeRef::object("Todos"))
            )),
            "Map<String, Option<Todos>>"
        );
        assert_eq!(
            index.decode_signal(5, 0, ChangeOp::Full, &[1]),
            json!({"$bytes": "01"})
        );
    }

    #[test]
    fn a_lazy_signal_is_its_length_and_version_without_the_page_servers_handle() {
        let index = SchemaIndex::new(&schema());
        let todos = ids::type_id("Todos");
        let full = bytes(|w| {
            w.write_u64(0x0000_0001_0000_0003);
            w.write_u32(3);
            w.write_u64(1);
        });
        assert_eq!(
            index.decode_signal(todos, 2, ChangeOp::Full, &full),
            json!({"$lazy": "Lazy<Todo>", "len": 3, "version": 1})
        );
        let invalidated = bytes(|w| {
            w.write_u32(4);
            w.write_u64(2);
        });
        assert_eq!(
            index.decode_signal(todos, 2, ChangeOp::LazyInvalidated, &invalidated),
            json!({"$invalidated": "Lazy<Todo>", "len": 4, "version": 2})
        );
        // Cut short, the bytes stay bytes.
        assert_eq!(
            index.decode_signal(todos, 2, ChangeOp::Full, &full[..19]),
            json!({"$bytes": hex::encode(&full[..19]), "$type": "Lazy<Todo>"})
        );
        assert_eq!(
            index.decode_signal(todos, 2, ChangeOp::LazyInvalidated, &[1]),
            json!({"$bytes": "01", "$type": "invalidation of Lazy<Todo>"})
        );
    }

    #[test]
    fn integers_past_53_bits_are_strings_and_small_ones_numbers() {
        assert_eq!(int_json(7), json!(7));
        assert_eq!(int_json(-(1 << 53)), json!(-(1_i64 << 53)));
        assert_eq!(int_json((1 << 53) + 1), json!("9007199254740993"));
        assert_eq!(
            int_json(i128::from(u64::MAX)),
            json!("18446744073709551615")
        );
        assert_eq!(
            plain(&DynValue::Decimal {
                mantissa: -12_345,
                scale: 2
            }),
            json!("-123.45")
        );
        assert_eq!(
            plain(&DynValue::Decimal {
                mantissa: 5,
                scale: 3
            }),
            json!("0.005")
        );
        assert_eq!(
            plain(&DynValue::Uuid([0x12; 16])),
            json!("12121212-1212-1212-1212-121212121212")
        );
        assert_eq!(plain(&DynValue::Float(f64::NAN)), json!("NaN"));
    }
}
