//! Where two recordings of the same flow diverged (`undra drift`): the calls each host made and
//! their order, what each platform's port adapters answered, the host-pushed events, what was
//! observed, and the state the stores ended in.
//!
//! The core is deterministic: with the same inputs every platform gets the same change-sets, so
//! the drift that matters is in the inputs, and that is what is compared. A [`Recording`] is first
//! put in a normal form, a [`Session`], in which everything that is per session (handles, call
//! ids, timer ids, `t`) is replaced or dropped: a handle becomes an [`Alias`], the constructor
//! type and the ordinal of that constructor's reply within the session, so the first `Todos` of an
//! iOS session is the first `Todos` of an Android session. Then [`compare`] reports every
//! [`Divergence`] of one session from the reference, under [`Rules`]: the built-in ignores (the
//! replies of `Clock.*` and `Rng.*`, the arguments of `Timer.*`, the `Idempotency-Key` header of
//! `Http.request`, `t`) and the user's dotted paths (`Http.request.req.headers`, `Todos.add.title`).
//! With a schema ([`SchemaIndex`]) names and decoded values are reported; without one the
//! comparison runs on ids and bytes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use serde_json::Value;
use undra_wire::payload::{ChangeOp, PortStatus, ReplyStatus};

use crate::decode::{SchemaIndex, undecoded};
use crate::names::standard_name;
use crate::recording::{EventKind, Recording, Target};

// ---------------------------------------------------------------------------------------------
// The normal form
// ---------------------------------------------------------------------------------------------

/// A handle as it compares across sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Alias {
    /// The `ordinal`th object a constructor of `type_id` returned in the session (from 0).
    Object {
        /// The constructed type.
        type_id: u32,
        /// Which of that type's constructions.
        ordinal: u32,
    },
    /// A handle that never came from a constructor reply, numbered by first appearance (from 1).
    Unknown(u32),
}

/// A call's target with its handle aliased.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CallKey {
    /// A free function.
    Function {
        /// `fnv1a32("fn.<name>")`.
        method: u32,
    },
    /// A method of an object.
    Method {
        /// The receiver.
        handle: Alias,
        /// `fnv1a32("<Type>.<method>")`.
        method: u32,
    },
    /// A constructor.
    Constructor {
        /// The object type.
        type_id: u32,
        /// Which constructor.
        method: u32,
    },
    /// A page of a lazy list.
    LazyPage {
        /// The list.
        handle: Alias,
        /// The first item.
        offset: u32,
        /// How many items.
        limit: u32,
    },
}

/// One host call and its reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// What was called.
    pub key: CallKey,
    /// The encoded arguments.
    pub args: Vec<u8>,
    /// The reply, when the recording has one.
    pub reply: Option<(ReplyStatus, Vec<u8>)>,
}

/// One port call and its reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortCall {
    /// The encoded arguments.
    pub args: Vec<u8>,
    /// The reply, when the recording has one.
    pub reply: Option<(PortStatus, Vec<u8>)>,
}

/// One host-pushed event of an event port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortEvent {
    /// The port.
    pub port: u32,
    /// The method.
    pub method: u32,
    /// The encoded payload.
    pub payload: Vec<u8>,
}

/// What a signal ended as: the last change-set entry that touched it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalState {
    /// How the last entry read.
    pub op: ChangeOp,
    /// The last entry's bytes (the value for `full`, the last patch otherwise).
    pub value: Vec<u8>,
    /// How many entries touched the signal.
    pub ops: usize,
}

/// A recording in the normal form that compares across sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// What the session is called in a report: the recording's `platform`, or what the caller
    /// set ([`Session::with_label`]).
    pub label: String,
    /// The schema the bytes belong to.
    pub schema_hash: u64,
    /// The host calls in order, each with its reply.
    pub calls: Vec<Call>,
    /// The port calls per `(port, method)`, in order, each with its reply.
    pub port_calls: BTreeMap<(u32, u32), Vec<PortCall>>,
    /// The host-pushed events in order.
    pub events: Vec<PortEvent>,
    /// What was observed (`on`), by first observe.
    pub observes: Vec<(Alias, u32)>,
    /// What was released, in order.
    pub releases: Vec<Alias>,
    /// The final state of every signal a change-set touched, by `(store, signal)`.
    pub state: BTreeMap<(Alias, u32), SignalState>,
    /// How many change-sets the core committed.
    pub commits: usize,
}

/// Handles of one recording to their aliases.
#[derive(Default)]
struct Aliases {
    by_handle: HashMap<u64, Alias>,
    ordinals: HashMap<u32, u32>,
    unknown: u32,
}

impl Aliases {
    fn of(&mut self, handle: u64) -> Alias {
        if let Some(alias) = self.by_handle.get(&handle) {
            return *alias;
        }
        self.unknown += 1;
        let alias = Alias::Unknown(self.unknown);
        self.by_handle.insert(handle, alias);
        alias
    }

    fn constructed(&mut self, handle: u64, type_id: u32) {
        if self.by_handle.contains_key(&handle) {
            return;
        }
        let ordinal = self.ordinals.entry(type_id).or_insert(0);
        self.by_handle.insert(
            handle,
            Alias::Object {
                type_id,
                ordinal: *ordinal,
            },
        );
        *ordinal += 1;
    }
}

impl Session {
    /// The normal form of `recording`.
    #[must_use]
    pub fn from_recording(recording: &Recording) -> Session {
        let mut session = Session {
            label: recording
                .platform
                .clone()
                .unwrap_or_else(|| "unnamed".to_owned()),
            schema_hash: recording.schema_hash,
            calls: Vec::new(),
            port_calls: BTreeMap::new(),
            events: Vec::new(),
            observes: Vec::new(),
            releases: Vec::new(),
            state: BTreeMap::new(),
            commits: 0,
        };
        let mut aliases = Aliases::default();
        // Host call id to its index in `calls`, and the type a pending constructor call makes.
        let mut call_index: HashMap<u32, usize> = HashMap::new();
        let mut constructing: HashMap<u32, u32> = HashMap::new();
        // Port call id to its place.
        let mut port_index: HashMap<u32, (u32, u32, usize)> = HashMap::new();
        for event in &recording.events {
            match &event.kind {
                EventKind::Call { target, call, args } => {
                    let key = match *target {
                        Target::Function { method } => CallKey::Function { method },
                        Target::Method { handle, method } => CallKey::Method {
                            handle: aliases.of(handle),
                            method,
                        },
                        Target::Constructor { type_id, method } => {
                            constructing.insert(*call, type_id);
                            CallKey::Constructor { type_id, method }
                        }
                        Target::LazyPage {
                            handle,
                            offset,
                            limit,
                        } => CallKey::LazyPage {
                            handle: aliases.of(handle),
                            offset,
                            limit,
                        },
                    };
                    call_index.insert(*call, session.calls.len());
                    session.calls.push(Call {
                        key,
                        args: args.clone(),
                        reply: None,
                    });
                }
                EventKind::Reply { call, status, body } => {
                    if let Some(&at) = call_index.get(call)
                        && session.calls[at].reply.is_none()
                    {
                        session.calls[at].reply = Some((*status, body.clone()));
                    }
                    if let Some(type_id) = constructing.remove(call)
                        && *status == ReplyStatus::Ok
                        && let Some(bytes) = body.get(..8)
                    {
                        let handle = u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]));
                        aliases.constructed(handle, type_id);
                    }
                }
                EventKind::ChangeSet { entries, .. } => {
                    session.commits += 1;
                    for entry in entries {
                        let key = (aliases.of(entry.handle), entry.signal);
                        let state = session.state.entry(key).or_insert(SignalState {
                            op: entry.op,
                            value: Vec::new(),
                            ops: 0,
                        });
                        state.op = entry.op;
                        state.value.clone_from(&entry.value);
                        state.ops += 1;
                    }
                }
                EventKind::PortCall {
                    port,
                    method,
                    call,
                    args,
                } => {
                    let list = session.port_calls.entry((*port, *method)).or_default();
                    port_index.insert(*call, (*port, *method, list.len()));
                    list.push(PortCall {
                        args: args.clone(),
                        reply: None,
                    });
                }
                EventKind::PortReply { call, status, body } => {
                    if let Some(&(port, method, at)) = port_index.get(call)
                        && let Some(list) = session.port_calls.get_mut(&(port, method))
                        && list[at].reply.is_none()
                    {
                        list[at].reply = Some((*status, body.clone()));
                    }
                }
                EventKind::PortEvent {
                    port,
                    method,
                    payload,
                } => session.events.push(PortEvent {
                    port: *port,
                    method: *method,
                    payload: payload.clone(),
                }),
                EventKind::Observe { handle, signal, on } => {
                    if *on {
                        let key = (aliases.of(*handle), *signal);
                        if !session.observes.contains(&key) {
                            session.observes.push(key);
                        }
                    }
                }
                EventKind::Release { handle } => session.releases.push(aliases.of(*handle)),
                EventKind::StreamItem { .. }
                | EventKind::TimerFired { .. }
                | EventKind::Cancel { .. } => {}
            }
        }
        session
    }

    /// `self` called `label` in reports (a file name when the recording has no `platform`).
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Session {
        self.label = label.into();
        self
    }
}

// ---------------------------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------------------------

/// One user ignore: `Http.request.req.headers` is the name `Http.request` and the path
/// `req.headers` inside its decoded arguments; `Todos.add` alone ignores the arguments (or, for a
/// signal, the final value) of what it names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ignore {
    name: String,
    path: Vec<String>,
}

/// What a comparison leaves out.
///
/// Built in, always: the replies of `Clock.*` and `Rng.*` (a reading differs by nature), the
/// arguments of `Timer.*` (deadlines are absolute times), the value of an `Idempotency-Key` header
/// inside `Http.request` arguments (a fresh key per run), and `t`. The user's ignores are dotted
/// paths matched after decoding, so they need the schema to reach inside a value.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    schema: Option<SchemaIndex>,
    ignores: Vec<Ignore>,
}

/// The line a report prints for the built-in ignores.
pub const BUILT_IN_IGNORES: &str = "the replies of Clock.* and Rng.*, the arguments of Timer.*, \
                                    the Idempotency-Key header of Http.request, and t";

impl Rules {
    /// The built-in rules, naming and decoding with `schema` when there is one.
    #[must_use]
    pub fn new(schema: Option<SchemaIndex>) -> Rules {
        Rules {
            schema,
            ignores: Vec::new(),
        }
    }

    /// Adds the user's dotted paths (`Http.request.req.headers`, `Todos.add.title`,
    /// `Todos.todos`). The name is the first two segments (`Type.method`, `Port.method`,
    /// `Store.signal`), or the first one when the schema says it is a free function.
    #[must_use]
    pub fn with_ignores<I, S>(mut self, paths: I) -> Rules
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for path in paths {
            let segments: Vec<String> = path
                .as_ref()
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            if segments.is_empty() {
                continue;
            }
            let is_function = self
                .schema
                .as_ref()
                .is_some_and(|s| s.schema().functions.iter().any(|f| f.name == segments[0]));
            let name_len = if is_function || segments.len() == 1 {
                1
            } else {
                2
            };
            self.ignores.push(Ignore {
                name: segments[..name_len].join("."),
                path: segments[name_len..].to_vec(),
            });
        }
        self
    }

    /// The schema, when there is one.
    #[must_use]
    pub fn schema(&self) -> Option<&SchemaIndex> {
        self.schema.as_ref()
    }

    /// The user's ignores as given, for the report's header.
    #[must_use]
    pub fn user_ignores(&self) -> Vec<String> {
        self.ignores
            .iter()
            .map(|i| {
                let mut text = i.name.clone();
                for segment in &i.path {
                    text.push('.');
                    text.push_str(segment);
                }
                text
            })
            .collect()
    }

    /// Whether everything of `name` (arguments, or a signal's value) is ignored.
    fn ignores_whole(&self, name: &str) -> bool {
        self.ignores
            .iter()
            .any(|i| i.name == name && i.path.is_empty())
    }

    /// Removes the user's ignored paths of `name` from `value`, and the built-in header.
    fn strip(&self, name: &str, value: &mut Value) {
        if name == "Http.request" {
            strip_idempotency_key(value);
        }
        for ignore in self.ignores.iter().filter(|i| i.name == name) {
            if !ignore.path.is_empty() {
                strip_path(value, &ignore.path);
            }
        }
    }

    /// The built-in rule: a `Clock.*` or `Rng.*` reply is never compared.
    fn ignores_reply(name: &str) -> bool {
        name.starts_with("Clock.") || name.starts_with("Rng.")
    }

    /// The built-in rule: `Timer.*` arguments are never compared.
    fn ignores_args(name: &str) -> bool {
        name.starts_with("Timer.")
    }
}

/// Removes `path` from `value`: a key of an object, applied to every item of an array.
fn strip_path(value: &mut Value, path: &[String]) {
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    match value {
        Value::Object(map) => {
            if rest.is_empty() {
                map.remove(first);
            } else if let Some(inner) = map.get_mut(first) {
                strip_path(inner, rest);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_path(item, path);
            }
        }
        _ => {}
    }
}

/// Replaces the value of an `Idempotency-Key` header in decoded `Http.request` arguments
/// (`req.headers[*].{name, value}`) with `"$ignored"`.
fn strip_idempotency_key(value: &mut Value) {
    let Some(headers) = value
        .get_mut("req")
        .and_then(|req| req.get_mut("headers"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for header in headers {
        let is_key = header
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n.eq_ignore_ascii_case("Idempotency-Key"));
        if is_key && let Some(slot) = header.get_mut("value") {
            *slot = Value::String("$ignored".to_owned());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Naming and decoding under the rules
// ---------------------------------------------------------------------------------------------

/// The rules applied to one session's items: names, and values with the ignores removed.
struct View<'a> {
    rules: &'a Rules,
}

impl View<'_> {
    fn schema(&self) -> Option<&SchemaIndex> {
        self.rules.schema()
    }

    fn alias(&self, alias: Alias) -> String {
        match alias {
            Alias::Object { type_id, ordinal } => {
                let name = self
                    .schema()
                    .and_then(|s| s.object_name(type_id).map(str::to_owned))
                    .unwrap_or_else(|| format!("type {type_id}"));
                format!("{name}#{ordinal}")
            }
            Alias::Unknown(n) => format!("#unknown-{n}"),
        }
    }

    /// The method name of a key (`Todos.add`), or its ids.
    fn call_rule_name(&self, key: &CallKey) -> String {
        let named = |method: u32| self.schema().and_then(|s| s.method_name(method));
        match key {
            CallKey::Function { method } => {
                named(*method).unwrap_or_else(|| format!("function {method}"))
            }
            CallKey::Method { method, .. } => {
                named(*method).unwrap_or_else(|| format!("method {method}"))
            }
            CallKey::Constructor { type_id, method } => {
                named(*method).unwrap_or_else(|| format!("constructor {method} of type {type_id}"))
            }
            CallKey::LazyPage { .. } => "page".to_owned(),
        }
    }

    /// The name a report shows for a call: the method, and the receiver for a method or a page.
    fn call_name(&self, key: &CallKey) -> String {
        match key {
            CallKey::Method { handle, .. } => {
                format!("{} on {}", self.call_rule_name(key), self.alias(*handle))
            }
            CallKey::LazyPage {
                handle,
                offset,
                limit,
            } => format!(
                "page {offset}..{} of {}",
                offset.saturating_add(*limit),
                self.alias(*handle)
            ),
            _ => self.call_rule_name(key),
        }
    }

    /// The compared arguments of a call: decoded when the schema can, the ignores removed;
    /// `None` when the rules ignore them whole.
    fn call_args(&self, call: &Call) -> Option<Value> {
        let name = self.call_rule_name(&call.key);
        if self.rules.ignores_whole(&name) {
            return None;
        }
        let mut value = match (&call.key, self.schema()) {
            (CallKey::LazyPage { .. }, _) => Value::Null,
            (
                CallKey::Function { method }
                | CallKey::Method { method, .. }
                | CallKey::Constructor { method, .. },
                Some(schema),
            ) => schema.decode_args(*method, &call.args),
            (_, None) => undecoded(&call.args, None),
        };
        self.rules.strip(&name, &mut value);
        Some(value)
    }

    fn port_name(&self, port: u32, method: u32) -> String {
        self.schema()
            .and_then(|s| s.port_name(port, method))
            .or_else(|| standard_name(port, method))
            .unwrap_or_else(|| format!("port {port}.{method}"))
    }

    /// The compared arguments of a port call or the payload of an event; `None` when ignored.
    fn port_args(&self, port: u32, method: u32, args: &[u8]) -> Option<Value> {
        let name = self.port_name(port, method);
        if self.rules.ignores_whole(&name) || Rules::ignores_args(&name) {
            return None;
        }
        let mut value = match self.schema() {
            Some(schema) => schema.decode_port_args(port, method, args),
            None => undecoded(args, None),
        };
        self.rules.strip(&name, &mut value);
        Some(value)
    }

    /// The compared reply of a port call; `None` when ignored.
    fn port_reply(
        &self,
        port: u32,
        method: u32,
        reply: Option<&(PortStatus, Vec<u8>)>,
    ) -> Option<Value> {
        let name = self.port_name(port, method);
        if self.rules.ignores_whole(&name) || Rules::ignores_reply(&name) {
            return None;
        }
        let Some((status, body)) = reply else {
            return Some(Value::String("unanswered".to_owned()));
        };
        let body = match self.schema() {
            Some(schema) => schema.decode_port_reply(port, method, *status, body),
            None => undecoded(body, None),
        };
        Some(serde_json::json!({ "status": port_status_name(*status), "body": body }))
    }

    /// `Todos.todos`: the rule name of a signal (no ordinal).
    fn signal_rule_name(&self, alias: Alias, signal: u32) -> String {
        match (alias, self.schema()) {
            (Alias::Object { type_id, .. }, Some(schema)) => schema
                .signal_name(type_id, signal)
                .unwrap_or_else(|| format!("type {type_id}.signal {signal}")),
            (Alias::Object { type_id, .. }, None) => format!("type {type_id}.signal {signal}"),
            (Alias::Unknown(_), _) => format!("signal {signal}"),
        }
    }

    /// `Todos#0.todos`: the name a report shows for a signal of one store.
    fn signal_name(&self, alias: Alias, signal: u32) -> String {
        let rule = self.signal_rule_name(alias, signal);
        let field = rule.rsplit_once('.').map_or(rule.as_str(), |(_, f)| f);
        if signal == u32::MAX {
            format!("{}.*", self.alias(alias))
        } else {
            format!("{}.{field}", self.alias(alias))
        }
    }

    /// The compared final state of a signal; `None` when ignored.
    fn signal_state(&self, alias: Alias, signal: u32, state: &SignalState) -> Option<Value> {
        let name = self.signal_rule_name(alias, signal);
        if self.rules.ignores_whole(&name) {
            return None;
        }
        let mut value = match (alias, self.schema()) {
            (Alias::Object { type_id, .. }, Some(schema)) => {
                schema.decode_signal(type_id, signal, state.op, &state.value)
            }
            _ => undecoded(&state.value, None),
        };
        self.rules.strip(&name, &mut value);
        Some(match state.op {
            ChangeOp::Full => value,
            ChangeOp::KeyedPatch | ChangeOp::LazyInvalidated => {
                serde_json::json!({ "ops": state.ops, "last": value })
            }
        })
    }
}

fn port_status_name(status: PortStatus) -> &'static str {
    match status {
        PortStatus::Ok => "ok",
        PortStatus::Error => "error",
        PortStatus::Unavailable => "unavailable",
    }
}

// ---------------------------------------------------------------------------------------------
// Divergences
// ---------------------------------------------------------------------------------------------

/// What kind of thing diverged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// The host calls: one missing, one extra, or one made with other arguments.
    Calls,
    /// The port calls: a method called another number of times, or answered differently.
    Ports,
    /// The host-pushed events.
    Events,
    /// What was observed.
    Observes,
    /// The state a signal ended in.
    State,
}

impl Kind {
    /// The word a line starts with, at most eight letters (`observes`), so lines align.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Kind::Calls => "calls",
            Kind::Ports => "ports",
            Kind::Events => "events",
            Kind::Observes => "observes",
            Kind::State => "state",
        }
    }
}

/// One way a session differs from the reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// What kind of thing.
    pub kind: Kind,
    /// Which thing: `Todos.add #3`, `Http.request #1`, `Todos#0.todos`.
    pub path: String,
    /// What differs, naming both sessions.
    pub text: String,
}

/// One step of a diff script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    /// `a[i]` and `b[j]` are the same.
    Keep(usize, usize),
    /// `a[i]` is not in `b`.
    Delete(usize),
    /// `b[j]` is not in `a`.
    Insert(usize),
}

/// The edit script from `a` to `b` by a longest common subsequence (quadratic: sessions are
/// hundreds of events, not millions).
fn lcs_diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Edit> {
    let (n, m) = (a.len(), b.len());
    // table[i][j]: the LCS length of a[i..] and b[j..].
    let mut table = vec![vec![0_usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            edits.push(Edit::Keep(i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            edits.push(Edit::Delete(i));
            i += 1;
        } else {
            edits.push(Edit::Insert(j));
            j += 1;
        }
    }
    edits.extend((i..n).map(Edit::Delete));
    edits.extend((j..m).map(Edit::Insert));
    edits
}

/// Every divergence of `other` from `reference` under `rules`, in a fixed order: calls, ports,
/// events, observes, state.
#[must_use]
pub fn compare(reference: &Session, other: &Session, rules: &Rules) -> Vec<Divergence> {
    let view = View { rules };
    let mut out = Vec::new();
    compare_calls(&view, reference, other, &mut out);
    compare_ports(&view, reference, other, &mut out);
    compare_events(&view, reference, other, &mut out);
    compare_observes(&view, reference, other, &mut out);
    compare_state(&view, reference, other, &mut out);
    out
}

fn compare_calls(view: &View<'_>, reference: &Session, other: &Session, out: &mut Vec<Divergence>) {
    let keys = |s: &Session| -> Vec<(CallKey, Option<Value>)> {
        s.calls
            .iter()
            .map(|c| (c.key.clone(), view.call_args(c)))
            .collect()
    };
    let (a, b) = (keys(reference), keys(other));
    let edits = lcs_diff(&a, &b);
    // Within one run of deletions and insertions, a deleted and an inserted call of the same
    // target are one call made with other arguments.
    let mut deleted: Vec<usize> = Vec::new();
    let mut inserted: Vec<usize> = Vec::new();
    let mut flush = |deleted: &mut Vec<usize>, inserted: &mut Vec<usize>| {
        let mut unpaired_inserts: Vec<usize> = Vec::new();
        for j in inserted.drain(..) {
            match deleted.iter().position(|&i| a[i].0 == b[j].0) {
                Some(at) => {
                    let i = deleted.remove(at);
                    out.push(Divergence {
                        kind: Kind::Calls,
                        path: format!("{} #{}", view.call_name(&a[i].0), i + 1),
                        text: format!(
                            "arguments differ: {} {}, {} {}",
                            reference.label,
                            shown(a[i].1.as_ref()),
                            other.label,
                            shown(b[j].1.as_ref())
                        ),
                    });
                }
                None => unpaired_inserts.push(j),
            }
        }
        for &i in deleted.iter() {
            out.push(Divergence {
                kind: Kind::Calls,
                path: format!("{} #{}", view.call_name(&a[i].0), i + 1),
                text: format!("missing on {} ({})", other.label, shown(a[i].1.as_ref())),
            });
        }
        deleted.clear();
        for j in unpaired_inserts {
            out.push(Divergence {
                kind: Kind::Calls,
                path: format!("{} #{}", view.call_name(&b[j].0), j + 1),
                text: format!("extra on {} ({})", other.label, shown(b[j].1.as_ref())),
            });
        }
    };
    for edit in edits {
        match edit {
            Edit::Keep(..) => flush(&mut deleted, &mut inserted),
            Edit::Delete(i) => deleted.push(i),
            Edit::Insert(j) => inserted.push(j),
        }
    }
    flush(&mut deleted, &mut inserted);
}

/// A compared value as a report shows it (`ignored` when the rules left it out).
fn shown(value: Option<&Value>) -> String {
    value.map_or_else(|| "ignored".to_owned(), ToString::to_string)
}

fn compare_ports(view: &View<'_>, reference: &Session, other: &Session, out: &mut Vec<Divergence>) {
    let keys: BTreeSet<(u32, u32)> = reference
        .port_calls
        .keys()
        .chain(other.port_calls.keys())
        .copied()
        .collect();
    let mut named: Vec<(String, (u32, u32))> = keys
        .into_iter()
        .map(|k| (view.port_name(k.0, k.1), k))
        .collect();
    named.sort();
    let empty = Vec::new();
    for (name, (port, method)) in named {
        if view.rules.ignores_whole(&name) {
            continue;
        }
        let a = reference.port_calls.get(&(port, method)).unwrap_or(&empty);
        let b = other.port_calls.get(&(port, method)).unwrap_or(&empty);
        if a.len() != b.len() {
            out.push(Divergence {
                kind: Kind::Ports,
                path: name.clone(),
                text: format!(
                    "{} {} on {}, {} on {}",
                    a.len(),
                    if a.len() == 1 { "call" } else { "calls" },
                    reference.label,
                    b.len(),
                    other.label
                ),
            });
        }
        for (n, (x, y)) in a.iter().zip(b).enumerate() {
            let (xa, ya) = (
                view.port_args(port, method, &x.args),
                view.port_args(port, method, &y.args),
            );
            if xa != ya {
                out.push(Divergence {
                    kind: Kind::Ports,
                    path: format!("{name} #{}", n + 1),
                    text: format!(
                        "arguments differ: {} {}, {} {}",
                        reference.label,
                        shown(xa.as_ref()),
                        other.label,
                        shown(ya.as_ref())
                    ),
                });
            }
            let (xr, yr) = (
                view.port_reply(port, method, x.reply.as_ref()),
                view.port_reply(port, method, y.reply.as_ref()),
            );
            if xr != yr {
                out.push(Divergence {
                    kind: Kind::Ports,
                    path: format!("{name} #{}", n + 1),
                    text: format!(
                        "reply differs: {} {}, {} {}",
                        reference.label,
                        shown(xr.as_ref()),
                        other.label,
                        shown(yr.as_ref())
                    ),
                });
            }
        }
    }
}

fn compare_events(
    view: &View<'_>,
    reference: &Session,
    other: &Session,
    out: &mut Vec<Divergence>,
) {
    let keys = |s: &Session| -> Vec<(u32, u32, Option<Value>)> {
        s.events
            .iter()
            .map(|e| {
                (
                    e.port,
                    e.method,
                    view.port_args(e.port, e.method, &e.payload),
                )
            })
            .collect()
    };
    let (a, b) = (keys(reference), keys(other));
    for edit in lcs_diff(&a, &b) {
        match edit {
            Edit::Keep(..) => {}
            Edit::Delete(i) => out.push(Divergence {
                kind: Kind::Events,
                path: format!("{} #{}", view.port_name(a[i].0, a[i].1), i + 1),
                text: format!("missing on {} ({})", other.label, shown(a[i].2.as_ref())),
            }),
            Edit::Insert(j) => out.push(Divergence {
                kind: Kind::Events,
                path: format!("{} #{}", view.port_name(b[j].0, b[j].1), j + 1),
                text: format!("extra on {} ({})", other.label, shown(b[j].2.as_ref())),
            }),
        }
    }
}

fn compare_observes(
    view: &View<'_>,
    reference: &Session,
    other: &Session,
    out: &mut Vec<Divergence>,
) {
    for &(alias, signal) in &reference.observes {
        if !other.observes.contains(&(alias, signal)) {
            out.push(Divergence {
                kind: Kind::Observes,
                path: view.signal_name(alias, signal),
                text: format!("observed on {} only", reference.label),
            });
        }
    }
    for &(alias, signal) in &other.observes {
        if !reference.observes.contains(&(alias, signal)) {
            out.push(Divergence {
                kind: Kind::Observes,
                path: view.signal_name(alias, signal),
                text: format!("observed on {} only", other.label),
            });
        }
    }
}

fn compare_state(view: &View<'_>, reference: &Session, other: &Session, out: &mut Vec<Divergence>) {
    let keys: BTreeSet<(Alias, u32)> = reference
        .state
        .keys()
        .chain(other.state.keys())
        .copied()
        .collect();
    for (alias, signal) in keys {
        let path = view.signal_name(alias, signal);
        match (
            reference.state.get(&(alias, signal)),
            other.state.get(&(alias, signal)),
        ) {
            (Some(x), Some(y)) => {
                let (xv, yv) = (
                    view.signal_state(alias, signal, x),
                    view.signal_state(alias, signal, y),
                );
                if xv != yv {
                    out.push(Divergence {
                        kind: Kind::State,
                        path,
                        text: format!(
                            "final value differs: {} {}, {} {}",
                            reference.label,
                            shown(xv.as_ref()),
                            other.label,
                            shown(yv.as_ref())
                        ),
                    });
                }
            }
            (Some(x), None) => {
                if view.signal_state(alias, signal, x).is_some() {
                    out.push(Divergence {
                        kind: Kind::State,
                        path,
                        text: format!("never changed on {}", other.label),
                    });
                }
            }
            (None, Some(y)) => {
                if view.signal_state(alias, signal, y).is_some() {
                    out.push(Divergence {
                        kind: Kind::State,
                        path,
                        text: format!("never changed on {}", reference.label),
                    });
                }
            }
            (None, None) => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------------------------

/// One compared recording of a [`Report`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compared {
    /// The file, as the user named it.
    pub file: String,
    /// The session's label (its platform).
    pub label: String,
    /// Its divergences from the reference.
    pub divergences: Vec<Divergence>,
}

/// What `undra drift` prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The reference file, as the user named it.
    pub reference: String,
    /// The schema hash, when a schema was given (names and decoded values in the report).
    pub schema: Option<u64>,
    /// The user's ignores, as given.
    pub user_ignores: Vec<String>,
    /// Every other recording, in the order given.
    pub compared: Vec<Compared>,
}

impl Report {
    /// How many divergences there are in all.
    #[must_use]
    pub fn total(&self) -> usize {
        self.compared.iter().map(|c| c.divergences.len()).sum()
    }

    /// The report's text: a header, the ignores, one line per divergence
    /// (`<platform>  <kind>  <path>: <text>`) and a summary.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let others: Vec<&str> = self.compared.iter().map(|c| c.file.as_str()).collect();
        let _ = write!(
            out,
            "Drift: {} (reference) against {}",
            self.reference,
            others.join(", ")
        );
        match self.schema {
            Some(hash) => {
                let _ = writeln!(out, " (schema {hash:#018x})");
            }
            None => out.push('\n'),
        }
        let _ = writeln!(out, "Ignored: {BUILT_IN_IGNORES}");
        if !self.user_ignores.is_empty() {
            let _ = writeln!(out, "Also ignored: {}", self.user_ignores.join(", "));
        }
        if self.schema.is_none() {
            out.push_str(
                "No schema: ids and bytes are compared, and only the standard ports are named \
                 (give --schema FILE, or run inside a project that has a schema.json)\n",
            );
        }
        let width = self
            .compared
            .iter()
            .map(|c| c.label.len())
            .max()
            .unwrap_or(0);
        if self.total() > 0 {
            out.push('\n');
        }
        for compared in &self.compared {
            for d in &compared.divergences {
                let _ = writeln!(
                    out,
                    "{:<width$}  {:<8}  {}: {}",
                    compared.label,
                    d.kind.label(),
                    d.path,
                    d.text
                );
            }
        }
        out.push('\n');
        let _ = writeln!(out, "{}", self.summary());
        out
    }

    /// `N divergences on M of K recordings`, or `no drift: K recordings agree`.
    #[must_use]
    pub fn summary(&self) -> String {
        let total = self.total();
        let recordings = self.compared.len() + 1;
        if total == 0 {
            return format!("no drift: {recordings} recordings agree");
        }
        let drifted = self
            .compared
            .iter()
            .filter(|c| !c.divergences.is_empty())
            .count();
        format!(
            "{total} {} on {drifted} of {recordings} recordings",
            if total == 1 {
                "divergence"
            } else {
                "divergences"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use undra_meta::{TypeRef, ids};
    use undra_wire::Writer;

    use super::*;
    use crate::decode::tests::schema;
    use crate::recording::{Entry, Event};

    const HASH: u64 = 0xdead_beef;

    fn bytes(f: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut w = Writer::new();
        f(&mut w);
        w.into_vec()
    }

    fn recording(platform: &str, kinds: Vec<EventKind>) -> Recording {
        let mut r = Recording::new(HASH, "test");
        r.platform = Some(platform.to_owned());
        r.events = kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Event {
                t: i as u64 * 7,
                kind,
            })
            .collect();
        r
    }

    fn todos() -> u32 {
        ids::type_id("Todos")
    }

    fn construct(call: u32, handle: u64) -> Vec<EventKind> {
        vec![
            EventKind::Call {
                target: Target::Constructor {
                    type_id: todos(),
                    method: ids::method_id("Todos", "new"),
                },
                call,
                args: vec![],
            },
            EventKind::Reply {
                call,
                status: ReplyStatus::Ok,
                body: handle.to_le_bytes().to_vec(),
            },
        ]
    }

    fn add(call: u32, handle: u64, title: &str) -> EventKind {
        EventKind::Call {
            target: Target::Method {
                handle,
                method: ids::method_id("Todos", "add"),
            },
            call,
            args: bytes(|w| w.write_str(title)),
        }
    }

    fn change(handle: u64, signal: u32, op: ChangeOp, value: Vec<u8>) -> EventKind {
        EventKind::ChangeSet {
            txn: 1,
            entries: vec![Entry {
                handle,
                signal,
                op,
                value,
            }],
        }
    }

    fn port_call(port: &str, method: &str, call: u32, args: Vec<u8>) -> EventKind {
        EventKind::PortCall {
            port: ids::port_id(port),
            method: ids::port_method_id(port, method),
            call,
            args,
        }
    }

    fn port_reply(call: u32, body: Vec<u8>) -> EventKind {
        EventKind::PortReply {
            call,
            status: PortStatus::Ok,
            body,
        }
    }

    fn rules() -> Rules {
        Rules::new(Some(SchemaIndex::new(&schema())))
    }

    fn session(platform: &str, kinds: Vec<EventKind>) -> Session {
        Session::from_recording(&recording(platform, kinds))
    }

    fn kinds(d: &[Divergence]) -> Vec<(Kind, &str)> {
        d.iter().map(|d| (d.kind, d.path.as_str())).collect()
    }

    #[test]
    fn handles_are_aliased_by_constructor_and_ordinal_and_otherwise_by_appearance() {
        // Two sessions with different handle values and ids make the same normal form.
        let mut a = construct(1, 0x0000_0001_0000_0000);
        a.extend(construct(2, 0x0000_0001_0000_0001));
        a.push(add(3, 0x0000_0001_0000_0001, "milk"));
        a.push(EventKind::Observe {
            handle: 0x0000_0001_0000_0001,
            signal: u32::MAX,
            on: true,
        });
        a.push(EventKind::Release {
            handle: 0x0000_0001_0000_0000,
        });
        a.push(add(4, 0x7777, "stray"));
        let mut b = construct(10, 0x0000_0009_0000_0005);
        b.extend(construct(11, 0x0000_0009_0000_0006));
        b.push(add(12, 0x0000_0009_0000_0006, "milk"));
        b.push(EventKind::Observe {
            handle: 0x0000_0009_0000_0006,
            signal: u32::MAX,
            on: true,
        });
        b.push(EventKind::Release {
            handle: 0x0000_0009_0000_0005,
        });
        b.push(add(13, 0x8888, "stray"));
        let (sa, sb) = (session("ios", a), session("ios", b));
        let keys = |s: &Session| s.calls.iter().map(|c| c.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&sa), keys(&sb));
        assert_eq!((&sa.observes, &sa.releases), (&sb.observes, &sb.releases));
        let second = Alias::Object {
            type_id: todos(),
            ordinal: 1,
        };
        assert_eq!(
            sa.calls[2].key,
            CallKey::Method {
                handle: second,
                method: ids::method_id("Todos", "add")
            }
        );
        assert_eq!(sa.observes, [(second, u32::MAX)]);
        assert_eq!(
            sa.releases,
            [Alias::Object {
                type_id: todos(),
                ordinal: 0
            }]
        );
        assert!(matches!(
            sa.calls[3].key,
            CallKey::Method {
                handle: Alias::Unknown(1),
                ..
            }
        ));
        assert_eq!(
            sa.calls[0].reply.as_ref().map(|r| r.0),
            Some(ReplyStatus::Ok)
        );
        assert!(compare(&sa, &sb, &rules()).is_empty());
    }

    #[test]
    fn change_sets_fold_to_a_final_value_per_signal() {
        let h = 0x0000_0001_0000_0000;
        let mut events = construct(1, h);
        events.push(change(h, 0, ChangeOp::Full, vec![0, 0, 0, 0]));
        events.push(change(h, 0, ChangeOp::KeyedPatch, vec![1]));
        events.push(change(h, 0, ChangeOp::KeyedPatch, vec![2]));
        events.push(change(h, 1, ChangeOp::Full, vec![0, 0]));
        let s = session("ios", events);
        assert_eq!(s.commits, 4);
        let alias = Alias::Object {
            type_id: todos(),
            ordinal: 0,
        };
        assert_eq!(
            s.state[&(alias, 0)],
            SignalState {
                op: ChangeOp::KeyedPatch,
                value: vec![2],
                ops: 3
            }
        );
        assert_eq!(
            s.state[&(alias, 1)],
            SignalState {
                op: ChangeOp::Full,
                value: vec![0, 0],
                ops: 1
            }
        );
    }

    #[test]
    fn the_lcs_keeps_the_common_calls_and_names_the_rest() {
        let edits = lcs_diff(&[1, 2, 3, 4], &[1, 3, 5, 4]);
        assert_eq!(
            edits,
            [
                Edit::Keep(0, 0),
                Edit::Delete(1),
                Edit::Keep(2, 1),
                Edit::Insert(2),
                Edit::Keep(3, 3)
            ]
        );
        assert_eq!(lcs_diff::<u8>(&[], &[]), []);
        assert_eq!(lcs_diff(&[1], &[]), [Edit::Delete(0)]);
        assert_eq!(lcs_diff(&[], &[1, 2]), [Edit::Insert(0), Edit::Insert(1)]);
    }

    #[test]
    fn calls_missing_extra_and_with_other_arguments() {
        let h = 0x0000_0001_0000_0000;
        let mut a = construct(1, h);
        a.push(add(2, h, "milk"));
        a.push(add(3, h, "eggs"));
        a.push(add(4, h, "bread"));
        let mut b = construct(1, h);
        b.push(add(2, h, "milk"));
        b.push(add(3, h, "tea"));
        b.push(add(4, h, "bread"));
        b.push(add(5, h, "jam"));
        let (sa, sb) = (session("ios", a), session("android", b));
        let d = compare(&sa, &sb, &rules());
        assert_eq!(
            kinds(&d),
            [
                (Kind::Calls, "Todos.add on Todos#0 #3"),
                (Kind::Calls, "Todos.add on Todos#0 #5")
            ]
        );
        assert_eq!(
            d[0].text,
            "arguments differ: ios {\"title\":\"eggs\"}, android {\"title\":\"tea\"}"
        );
        assert_eq!(d[1].text, "extra on android ({\"title\":\"jam\"})");
        // The other way round, the extra call is missing.
        let d = compare(&sb, &sa, &rules());
        assert_eq!(d[1].text, "missing on ios ({\"title\":\"jam\"})");
        // Ignoring the argument makes the two adds the same call.
        let d = compare(&sa, &sb, &rules().with_ignores(["Todos.add.title"]));
        assert_eq!(kinds(&d), [(Kind::Calls, "Todos.add on Todos#0 #5")]);
        assert_eq!(d[0].text, "extra on android ({})");
        // Without a schema the arguments are bytes, and the receiver is a type id.
        let d = compare(&sa, &sb, &Rules::new(None));
        assert_eq!(d.len(), 2);
        assert!(
            d[0].path.starts_with(&format!(
                "method {} on type {}#0",
                ids::method_id("Todos", "add"),
                todos()
            )),
            "{}",
            d[0].path
        );
        assert!(
            d[0].text.contains("{\"$bytes\":\"04000000656767"),
            "{}",
            d[0].text
        );
    }

    #[test]
    fn port_calls_compare_by_count_arguments_and_reply_with_the_built_in_ignores() {
        let notify = |call: u32, text: &str| {
            port_call("Notifier", "notify", call, bytes(|w| w.write_str(text)))
        };
        let a = vec![
            port_call("Clock", "now_ms", 1, vec![]),
            port_reply(1, 1_i64.to_le_bytes().to_vec()),
            port_call("Rng", "fill", 2, vec![4, 0, 0, 0]),
            port_reply(2, vec![4, 0, 0, 0, 1, 2, 3, 4]),
            port_call("Timer", "set", 3, vec![1, 0, 0, 0, 0, 0, 0, 0]),
            notify(4, "hi"),
            port_reply(4, bytes(|w| w.write_u32(1))),
            notify(5, "again"),
            port_reply(5, bytes(|w| w.write_u32(2))),
        ];
        let b = vec![
            port_call("Clock", "now_ms", 1, vec![]),
            port_reply(1, 99_i64.to_le_bytes().to_vec()),
            port_call("Rng", "fill", 2, vec![4, 0, 0, 0]),
            port_reply(2, vec![4, 0, 0, 0, 9, 9, 9, 9]),
            port_call("Timer", "set", 3, vec![2, 0, 0, 0, 0, 0, 0, 0]),
            notify(4, "hi"),
            port_reply(4, bytes(|w| w.write_u32(7))),
        ];
        let (sa, sb) = (session("ios", a), session("web", b));
        let d = compare(&sa, &sb, &rules());
        assert_eq!(
            kinds(&d),
            [
                (Kind::Ports, "Notifier.notify"),
                (Kind::Ports, "Notifier.notify #1")
            ],
            "{d:?}"
        );
        assert_eq!(d[0].text, "2 calls on ios, 1 on web");
        assert_eq!(
            d[1].text,
            "reply differs: ios {\"body\":1,\"status\":\"ok\"}, web {\"body\":7,\"status\":\"ok\"}"
        );
        // An ignored port method makes no line at all.
        let d = compare(&sa, &sb, &rules().with_ignores(["Notifier.notify"]));
        assert!(d.is_empty(), "{d:?}");
        // Timer arguments: the rule is on the name, so it holds without a schema too; a Timer
        // reply does compare.
        let c = vec![
            port_call("Timer", "set", 3, vec![2, 0, 0, 0, 0, 0, 0, 0]),
            port_reply(3, vec![1]),
        ];
        let e = vec![
            port_call("Timer", "set", 3, vec![3, 0, 0, 0, 0, 0, 0, 0]),
            port_reply(3, vec![2]),
        ];
        let d = compare(&session("ios", c), &session("web", e), &Rules::new(None));
        assert_eq!(kinds(&d), [(Kind::Ports, "Timer.set #1")]);
        assert!(d[0].text.starts_with("reply differs"), "{}", d[0].text);
        // An unanswered call differs from an answered one.
        let d = compare(
            &session(
                "ios",
                vec![notify(1, "x"), port_reply(1, bytes(|w| w.write_u32(1)))],
            ),
            &session("web", vec![notify(1, "x")]),
            &rules(),
        );
        assert_eq!(d.len(), 1);
        assert!(d[0].text.ends_with("web \"unanswered\""), "{}", d[0].text);
    }

    #[test]
    fn the_idempotency_key_header_of_an_http_request_is_ignored() {
        use undra_meta::{FieldDef, MethodDef, ParamDef, PortDef, PortKind, RecordDef};
        // A schema with the standard `Http` port as the real one describes it.
        let mut s = schema();
        let field = |name: &str, ty: TypeRef| FieldDef {
            name: name.into(),
            ty,
            default: false,
            docs: String::new(),
        };
        s.records.push(RecordDef {
            name: "Header".into(),
            type_id: ids::type_id("Header"),
            fields: vec![
                field("name", TypeRef::String),
                field("value", TypeRef::String),
            ],
            transparent: false,
            docs: String::new(),
        });
        s.records.push(RecordDef {
            name: "HttpRequest".into(),
            type_id: ids::type_id("HttpRequest"),
            fields: vec![
                field("url", TypeRef::String),
                field("headers", TypeRef::vec(TypeRef::named("Header"))),
            ],
            transparent: false,
            docs: String::new(),
        });
        s.ports.push(PortDef {
            name: "Http".into(),
            port_id: ids::port_id("Http"),
            kind: PortKind::Async,
            background: false,
            methods: vec![MethodDef {
                name: "request".into(),
                method_id: ids::port_method_id("Http", "request"),
                params: vec![ParamDef {
                    name: "req".into(),
                    ty: TypeRef::named("HttpRequest"),
                }],
                returns: TypeRef::U16,
                is_async: true,
                takes_ctx: false,
                coalesce: false,
                generic: None,
                docs: String::new(),
            }],
            docs: String::new(),
        });
        let request = |key: &str, agent: &str| {
            port_call(
                "Http",
                "request",
                1,
                bytes(|w| {
                    w.write_str("https://api.test/x");
                    w.write_len(2);
                    w.write_str("Idempotency-Key");
                    w.write_str(key);
                    w.write_str("User-Agent");
                    w.write_str(agent);
                }),
            )
        };
        let rules = Rules::new(Some(SchemaIndex::new(&s)));
        let same_but_key = compare(
            &session("ios", vec![request("k1", "ios/1")]),
            &session("android", vec![request("k2", "ios/1")]),
            &rules,
        );
        assert!(same_but_key.is_empty(), "{same_but_key:?}");
        let other_agent = compare(
            &session("ios", vec![request("k1", "ios/1")]),
            &session("android", vec![request("k1", "android/1")]),
            &rules,
        );
        assert_eq!(kinds(&other_agent), [(Kind::Ports, "Http.request #1")]);
        assert!(
            other_agent[0].text.contains("\"$ignored\""),
            "{}",
            other_agent[0].text
        );
        // A user path reaches inside the decoded arguments.
        let headers_ignored = compare(
            &session("ios", vec![request("k1", "ios/1")]),
            &session("android", vec![request("k1", "android/1")]),
            &rules.clone().with_ignores(["Http.request.req.headers"]),
        );
        assert!(headers_ignored.is_empty(), "{headers_ignored:?}");
        // Without a schema the header cannot be found, so the keys differ as bytes.
        let raw = compare(
            &session("ios", vec![request("k1", "ios/1")]),
            &session("android", vec![request("k2", "ios/1")]),
            &Rules::new(None),
        );
        assert_eq!(kinds(&raw), [(Kind::Ports, "Http.request #1")]);
    }

    #[test]
    fn events_observes_and_state_each_have_a_kind() {
        let h = 0x0000_0001_0000_0000;
        let connectivity = |online: bool| EventKind::PortEvent {
            port: ids::port_id("Connectivity"),
            method: ids::port_method_id("Connectivity", "changed"),
            payload: vec![u8::from(online)],
        };
        let mut a = construct(1, h);
        a.push(EventKind::Observe {
            handle: h,
            signal: u32::MAX,
            on: true,
        });
        a.push(connectivity(true));
        a.push(change(h, 1, ChangeOp::Full, bytes(|w| w.write_u16(0))));
        a.push(change(h, 0, ChangeOp::KeyedPatch, vec![1]));
        let mut b = construct(1, h);
        b.push(EventKind::Observe {
            handle: h,
            signal: 1,
            on: true,
        });
        b.push(connectivity(true));
        b.push(connectivity(false));
        b.push(change(
            h,
            1,
            ChangeOp::Full,
            bytes(|w| {
                w.write_u16(1);
                w.write_str("home");
            }),
        ));
        let (sa, sb) = (session("ios", a), session("android", b));
        let d = compare(&sa, &sb, &rules());
        assert_eq!(
            kinds(&d),
            [
                (Kind::Events, "Connectivity.changed #2"),
                (Kind::Observes, "Todos#0.*"),
                (Kind::Observes, "Todos#0.filter"),
                (Kind::State, "Todos#0.items"),
                (Kind::State, "Todos#0.filter"),
            ],
            "{d:?}"
        );
        assert_eq!(d[0].text, "extra on android ({\"$bytes\":\"00\"})");
        assert_eq!(d[1].text, "observed on ios only");
        assert_eq!(d[2].text, "observed on android only");
        assert_eq!(d[3].text, "never changed on android");
        assert_eq!(
            d[4].text,
            "final value differs: ios {\"$\":\"All\"}, android {\"$\":\"Tagged\",\"0\":\"home\"}"
        );
        // A patched signal compares by its op count and last patch.
        let mut c = construct(1, h);
        c.push(EventKind::Observe {
            handle: h,
            signal: u32::MAX,
            on: true,
        });
        c.push(connectivity(true));
        c.push(change(h, 1, ChangeOp::Full, bytes(|w| w.write_u16(0))));
        c.push(change(h, 0, ChangeOp::KeyedPatch, vec![1]));
        c.push(change(h, 0, ChangeOp::KeyedPatch, vec![2]));
        let sc = session("web", c);
        let d = compare(&sa, &sc, &rules());
        assert_eq!(kinds(&d), [(Kind::State, "Todos#0.items")]);
        let patch_of = format!("patch of {}", TypeRef::vec(TypeRef::named("Todo")));
        assert_eq!(
            d[0].text,
            format!(
                "final value differs: ios {{\"last\":{{\"$bytes\":\"01\",\"$type\":\"{patch_of}\"}},\"ops\":1}}, \
                 web {{\"last\":{{\"$bytes\":\"02\",\"$type\":\"{patch_of}\"}},\"ops\":2}}"
            )
        );
        // Ignoring the signal by name drops the line.
        let d = compare(&sa, &sc, &rules().with_ignores(["Todos.items"]));
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn user_ignores_parse_to_a_name_and_a_path() {
        let r = rules().with_ignores(["Todos.add.title", "link.store", "Todos.items", "", "a"]);
        assert_eq!(
            r.user_ignores(),
            ["Todos.add.title", "link.store", "Todos.items", "a"]
        );
        assert_eq!(
            r.ignores,
            [
                Ignore {
                    name: "Todos.add".into(),
                    path: vec!["title".into()]
                },
                Ignore {
                    name: "link".into(),
                    path: vec!["store".into()]
                },
                Ignore {
                    name: "Todos.items".into(),
                    path: vec![]
                },
                Ignore {
                    name: "a".into(),
                    path: vec![]
                },
            ]
        );
        assert!(r.ignores_whole("Todos.items") && !r.ignores_whole("Todos.add"));
        let mut value = json!({"title": "x", "tags": [{"a": 1, "b": 2}]});
        strip_path(&mut value, &["tags".into(), "a".into()]);
        strip_path(&mut value, &["none".into(), "x".into()]);
        assert_eq!(value, json!({"title": "x", "tags": [{"b": 2}]}));
    }

    #[test]
    fn the_report_has_a_header_the_ignores_the_lines_and_a_summary() {
        let report = Report {
            reference: "ios.json".into(),
            schema: Some(HASH),
            user_ignores: vec!["Todos.add.title".into()],
            compared: vec![
                Compared {
                    file: "android.json".into(),
                    label: "android".into(),
                    divergences: vec![Divergence {
                        kind: Kind::Calls,
                        path: "Todos.add #2".into(),
                        text: "missing on android ({})".into(),
                    }],
                },
                Compared {
                    file: "web.json".into(),
                    label: "web".into(),
                    divergences: vec![],
                },
            ],
        };
        assert_eq!(
            report.render(),
            "Drift: ios.json (reference) against android.json, web.json (schema 0x00000000deadbeef)\n\
             Ignored: the replies of Clock.* and Rng.*, the arguments of Timer.*, the Idempotency-Key header of Http.request, and t\n\
             Also ignored: Todos.add.title\n\
             \n\
             android  calls     Todos.add #2: missing on android ({})\n\
             \n\
             1 divergence on 1 of 3 recordings\n"
        );
        let agree = Report {
            reference: "a.json".into(),
            schema: None,
            user_ignores: vec![],
            compared: vec![Compared {
                file: "b.json".into(),
                label: "b".into(),
                divergences: vec![],
            }],
        };
        let text = agree.render();
        assert!(
            text.starts_with("Drift: a.json (reference) against b.json\n"),
            "{text}"
        );
        assert!(
            text.contains("No schema: ids and bytes are compared"),
            "{text}"
        );
        assert!(text.ends_with("\nno drift: 2 recordings agree\n"), "{text}");
    }
}
