//! Behaviour of the generators that the golden files do not pin: configuration
//! options, determinism, file layout and structural sanity of every output.

mod common;

use undra_bindgen::{GeneratedFile, Generator, SwiftObservation};
use undra_meta::{Schema, TypeRef};

fn all_files(case: &str, configure: impl Fn(&mut Generator)) -> Vec<(String, Vec<GeneratedFile>)> {
    let schema = common::case(case);
    let mut generator = common::generator_for(case, &schema);
    configure(&mut generator);
    vec![
        ("swift".to_owned(), generator.swift(&schema).unwrap()),
        ("kotlin".to_owned(), generator.kotlin(&schema).unwrap()),
        ("ts".to_owned(), generator.typescript(&schema).unwrap()),
    ]
}

fn file<'a>(files: &'a [GeneratedFile], suffix: &str) -> &'a str {
    &files
        .iter()
        .find(|f| f.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("no file ends with {suffix}"))
        .contents
}

// ----- the TypeScript entry and the runtime's stream feature (ADR-057) ---------------------

/// A schema with a stream method or function passes `features: [streams]` to the runtime, so the stream support is up front with
/// the entry; a schema without one generates the entry it always did, byte for byte (the goldens of those cases pin the rest). The
/// check is independent of the generator's own predicate: it reads the generated code for a `core.stream(` call.
#[test]
fn the_typescript_entry_passes_the_stream_feature_exactly_when_the_schema_has_a_stream() {
    let mut with = 0;
    let mut without = 0;
    for &name in common::CASES {
        let schema = common::case(name);
        let files = common::generator_for(name, &schema)
            .typescript(&schema)
            .unwrap();
        let calls_stream = files
            .iter()
            .filter(|f| f.path.ends_with(".ts") && !f.path.ends_with("src/core.ts"))
            .any(|f| f.contents.contains("core.stream("));
        let entry = file(&files, "src/core.ts");
        let import = entry
            .lines()
            .find(|l| l.starts_with("import { UndraCore"))
            .unwrap();
        if calls_stream {
            with += 1;
            assert_eq!(
                import,
                "import { UndraCore, UndraError, streams, type AttachOptions, type LoadOptions, type Transport } from \"@undra/runtime\";",
                "{name}"
            );
            assert_eq!(
                entry.matches("features: [streams]").count(),
                2,
                "{name}: load and attach"
            );
            assert_eq!(
                entry
                    .matches("\"expectedSchemaHash\" | \"namespace\" | \"features\"")
                    .count(),
                2,
                "{name}: the option a generated entry fills in is not one the app passes"
            );
        } else {
            without += 1;
            assert_eq!(
                import,
                "import { UndraCore, UndraError, type AttachOptions, type LoadOptions, type Transport } from \"@undra/runtime\";",
                "{name}"
            );
            assert!(
                !entry.contains("features"),
                "{name}: no feature without a stream"
            );
            assert!(
                !entry.contains("streams"),
                "{name}: no stream support without a stream"
            );
            assert_eq!(
                entry
                    .matches("\"expectedSchemaHash\" | \"namespace\">")
                    .count(),
                2,
                "{name}"
            );
        }
    }
    assert!(
        with >= 4 && without >= 4,
        "the cases cover both ({with} with a stream, {without} without)"
    );
}

// ----- layout ---------------------------------------------------------------------------

#[test]
fn every_language_writes_the_documented_files() {
    let schema = common::case("full");
    let generator = Generator::for_crate(&schema.crate_name);
    let paths =
        |files: Vec<GeneratedFile>| -> Vec<String> { files.into_iter().map(|f| f.path).collect() };

    let mut swift: Vec<String> = [
        "Types", "Errors", "Objects", "Stores", "Ports", "Queries", "Ids", "Core",
    ]
    .map(|n| format!("Sources/PlaygroundCore/Generated/{n}.swift"))
    .to_vec();
    // ADR-044: the C module that declares the core's entry, `playground_core_undra_api`.
    swift.extend(
        [
            "include/playground_core_undra.h",
            "include/module.modulemap",
            "playground_core_undra.c",
        ]
        .map(|f| format!("Sources/PlaygroundCoreFFI/{f}")),
    );
    assert_eq!(paths(generator.swift(&schema).unwrap()), swift);
    let mut kotlin: Vec<String> = [
        "Types", "Errors", "Objects", "Stores", "Ports", "Queries", "Ids", "Core",
    ]
    .map(|n| format!("src/main/kotlin/dev/undra/generated/playground_core/{n}.kt"))
    .to_vec();
    // ADR-044: the R8 rules that keep the natives `JNI_OnLoad` registers by name.
    kotlin.push("src/main/resources/META-INF/proguard/undra-playground_core.pro".to_owned());
    assert_eq!(paths(generator.kotlin(&schema).unwrap()), kotlin);
    let mut expected: Vec<String> = [
        "types", "errors", "objects", "stores", "ports", "queries", "ids", "core", "index",
    ]
    .iter()
    .map(|n| format!("src/{n}.ts"))
    .collect();
    expected.extend(["package.json".to_owned(), "tsconfig.json".to_owned()]);
    assert_eq!(paths(generator.typescript(&schema).unwrap()), expected);
}

#[test]
fn an_empty_schema_still_writes_valid_files() {
    let schema = Schema::new("empty-core");
    let generator = Generator::for_crate("empty-core");
    let ts = generator.typescript(&schema).unwrap();
    // A TypeScript file must be a module for `export *` to accept it.
    for f in ts.iter().filter(|f| f.path.ends_with(".ts")) {
        assert!(
            f.contents.contains("export "),
            "{} has no export:\n{}",
            f.path,
            f.contents
        );
    }
    assert!(file(&ts, "src/types.ts").contains("export {};"));
    let swift = generator.swift(&schema).unwrap();
    assert!(file(&swift, "Ids.swift").contains("public enum UndraIds"));
    let kotlin = generator.kotlin(&schema).unwrap();
    assert!(file(&kotlin, "Ids.kt").contains("object UndraIds"));
}

#[test]
fn the_configuration_names_the_output() {
    let schema = common::case("full");
    let mut generator = Generator::for_crate(&schema.crate_name);
    generator.swift_module = "Acme".into();
    generator.kotlin_package = "com.acme.core".into();
    generator.ts_scope = "acme".into();
    generator.ts_package = "core".into();
    generator.package_version = "2.3.4".into();

    let swift = generator.swift(&schema).unwrap();
    assert!(swift[0].path.starts_with("Sources/Acme/Generated/"));
    let kotlin = generator.kotlin(&schema).unwrap();
    assert!(kotlin[0].path.starts_with("src/main/kotlin/com/acme/core/"));
    assert!(kotlin[0].contents.contains("\npackage com.acme.core\n"));
    let ts = generator.typescript(&schema).unwrap();
    let package: serde_json::Value = serde_json::from_str(file(&ts, "package.json")).unwrap();
    assert_eq!(package["name"], "@acme/core");
    assert_eq!(package["version"], "2.3.4");
    // The runtime it asks for: this generator's release by default (the number moves with every release, so it is not
    // written here: scripts/bump-version.sh does not edit Rust), the project's release when `undra bindgen` says so.
    assert_eq!(
        package["peerDependencies"]["@undra/runtime"],
        undra_bindgen::RUNTIME_RANGE
    );
    assert_eq!(
        undra_bindgen::RUNTIME_RANGE,
        concat!("^", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(package["type"], "module");
    generator.ts_runtime_range = "^9.8.7".into();
    let ts = generator.typescript(&schema).unwrap();
    let package: serde_json::Value = serde_json::from_str(file(&ts, "package.json")).unwrap();
    assert_eq!(package["peerDependencies"]["@undra/runtime"], "^9.8.7");
    assert_eq!(package["devDependencies"]["@undra/runtime"], "^9.8.7");

    generator.ts_scope = String::new();
    let ts = generator.typescript(&schema).unwrap();
    let package: serde_json::Value = serde_json::from_str(file(&ts, "package.json")).unwrap();
    assert_eq!(package["name"], "core");
}

// ----- options -------------------------------------------------------------------------

#[test]
fn swift_typed_throws_only_changes_port_requirements() {
    // ADR-032: calls never use typed throws; only the requirements of a port, which the host
    // implements, do.
    let typed = all_files("ports", |_| {});
    let untyped = all_files("ports", |g| g.swift_typed_throws = false);
    let typed_ports = file(&typed[0].1, "Ports.swift");
    let untyped_ports = file(&untyped[0].1, "Ports.swift");
    assert!(typed_ports.contains("throws(HttpError)"));
    assert!(!untyped_ports.contains("throws("));
    assert!(untyped_ports.contains("async throws -> HttpResponse"));

    let typed = all_files("objects", |_| {});
    let untyped = all_files("objects", |g| g.swift_typed_throws = false);
    for name in ["Objects.swift", "Stores.swift", "Queries.swift"] {
        let (a, b) = (file(&typed[0].1, name), file(&untyped[0].1, name));
        assert_eq!(a, b, "{name} must not depend on swift_typed_throws");
        assert!(
            !a.contains("throws("),
            "{name}: a call must not use typed throws"
        );
    }
}

#[test]
fn swift_records_derive_codable_only_from_codable_fields() {
    let files = all_files("records", |_| {});
    let types = file(&files[0].1, "Types.swift");
    // `Swift.Duration` is `Codable` only from the Swift 6.0 standard library, above the
    // runtime's deployment floor, so a record holding one must not derive it.
    assert!(types.contains("public struct Numbers: UndraRecord, Sendable, Hashable {"));
    assert!(types.contains("public struct Containers: UndraRecord, Sendable, Hashable, Codable {"));
}

#[test]
fn swift_streams_are_decoded_by_the_runtime_so_credit_follows_the_consumer() {
    // A stream method must hand the core's pull-based stream to `UndraCore.stream(..decode:)`
    // (SPEC 3.7). Copying it into an `AsyncThrowingStream` with `yield` buffers without bound and
    // the core runs ahead of the consumer (playground finding 3).
    for case in ["objects", "stores", "full", "queries", "stdlib"] {
        let out = all_files(case, |_| {});
        for f in &out[0].1 {
            assert!(
                !f.contents.contains("continuation.yield")
                    && !f.contents.contains("undraDecodeStream"),
                "{case}/{}: a generated stream must not copy items into its own buffer",
                f.path
            );
        }
    }
    let out = all_files("objects", |_| {});
    let swift = file(&out[0].1, "Objects.swift");
    assert!(swift.contains("return self.core.stream("));
    assert!(swift.contains("decode: { try UInt32.undraDecoded(from: $0) }"));
    // Every stream maps its failure in the runtime (ADR-032); a typed error is the domain.
    assert!(swift.contains(
        "mapError: { UndraCallError.mapped(streamFailure: $0, domain: CalcError.self) }"
    ));
    assert!(swift.contains("mapError: { UndraCallError.mapped(streamFailure: $0) }"));
    assert!(!swift.contains("mapError: { $0 }"));
}

// ----- the iOS 15 / 16 mode (ADR-045) ----------------------------------------------------

fn swift_in(case: &str, observation: SwiftObservation, min_ios: u32) -> Vec<GeneratedFile> {
    let schema = common::case(case);
    let mut generator = common::generator_for(case, &schema);
    generator.swift_observation = observation;
    generator.swift_min_ios = min_ios;
    generator.swift(&schema).unwrap()
}

#[test]
fn the_observable_object_mode_changes_only_how_a_store_is_observed() {
    let default = swift_in("stores", SwiftObservation::Observation, 17);
    let floor = swift_in("stores", SwiftObservation::ObservableObject, 15);
    let stores = file(&floor, "Stores.swift");
    assert!(stores.contains("import Combine\n") && !stores.contains("import Observation"));
    assert!(!stores.contains("@Observable"));
    assert!(stores.contains(
        "@MainActor\npublic final class Todos: UndraStore, ObservableObject, @unchecked Sendable {"
    ));
    // Every property of a store is `@Published`, and each is still read-only outside it.
    let props: Vec<&str> = stores
        .lines()
        .filter(|l| l.contains("public private(set) var "))
        .collect();
    assert!(props.len() > 10);
    assert!(
        props.iter().all(|l| l
            .trim_start()
            .starts_with("@Published public private(set) var ")),
        "{props:?}"
    );
    // The default is `@Observable` and says nothing of a mode.
    let observation = file(&default, "Stores.swift");
    assert!(observation.contains(
        "@MainActor @Observable\npublic final class Todos: UndraStore, @unchecked Sendable {"
    ));
    assert!(!observation.contains("@Published") && !observation.contains("Swift mode:"));
    // Apart from those lines, the two modes generate the same store: the constructors, the commands
    // and the apply function are shared (what the mode changes, written back to the other's spelling).
    let to_observation = |text: &str| -> String {
        text.lines()
            .filter(|l| !l.starts_with("// Swift mode:") && !l.starts_with("import "))
            .map(|l| {
                l.replace("@Published ", "")
                    .replace("@MainActor @Observable", "@MainActor")
                    .replace(", ObservableObject", "")
                    .replace("UndraDuration", "Duration")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        to_observation(observation) == to_observation(stores),
        "the stores of the two modes differ in more than the observation lines"
    );
}

#[test]
fn query_handles_follow_the_observation_mode_too() {
    let floor = swift_in("queries", SwiftObservation::ObservableObject, 15);
    let queries = file(&floor, "Queries.swift");
    assert!(queries.contains("import Combine\n") && !queries.contains("@Observable"));
    assert!(queries.contains("ObservableObject, @unchecked Sendable"));
    assert!(queries.contains("@Published public private(set) var "));
}

#[test]
fn the_mode_is_recorded_in_every_swift_file_header_and_the_default_has_no_such_line() {
    for file in swift_in("stores", SwiftObservation::ObservableObject, 15)
        .iter()
        .filter(|f| f.path.ends_with(".swift") && f.path.contains("/Generated/"))
    {
        let second = file.contents.lines().nth(1).unwrap_or_default();
        assert!(
            second.starts_with("// Swift mode: observable-object") && second.contains("iOS 15"),
            "{}: {second}",
            file.path
        );
    }
    for file in swift_in("stores", SwiftObservation::Observation, 17)
        .iter()
        .filter(|f| f.path.contains("/Generated/"))
    {
        assert!(!file.contents.contains("Swift mode:"), "{}", file.path);
    }
    // A switch of mode changes the files, so `undra bindgen --check` sees stale bindings.
    assert_ne!(
        swift_in("stores", SwiftObservation::Observation, 17),
        swift_in("stores", SwiftObservation::ObservableObject, 17)
    );
}

#[test]
fn the_wire_duration_is_swift_duration_from_ios_16_and_undra_duration_below() {
    let types = |min_ios: u32| {
        let files = swift_in("full", SwiftObservation::ObservableObject, min_ios);
        file(&files, "Types.swift").to_owned()
    };
    let at_16 = types(16);
    assert!(
        at_16.contains("public var elapsed: Duration\n")
            && at_16.contains("elapsed: Duration.undraDecode(&r)")
    );
    let at_15 = types(15);
    assert!(
        at_15.contains("public var elapsed: UndraDuration\n")
            && at_15.contains("elapsed: UndraDuration.undraDecode(&r)")
    );
    // Not tied to the observation mode: an app on iOS 17 and later keeps `Swift.Duration`.
    let files = swift_in("full", SwiftObservation::Observation, 17);
    assert!(file(&files, "Types.swift").contains("public var elapsed: Duration\n"));
}

#[test]
fn the_observation_mode_follows_the_deployment_floor_unless_told_otherwise() {
    assert_eq!(
        SwiftObservation::for_ios(15),
        SwiftObservation::ObservableObject
    );
    assert_eq!(
        SwiftObservation::for_ios(16),
        SwiftObservation::ObservableObject
    );
    assert_eq!(SwiftObservation::for_ios(17), SwiftObservation::Observation);
    assert_eq!(SwiftObservation::for_ios(26), SwiftObservation::Observation);
    for mode in [
        SwiftObservation::Observation,
        SwiftObservation::ObservableObject,
    ] {
        assert_eq!(mode.to_string().parse(), Ok(mode));
    }
    assert!("combine".parse::<SwiftObservation>().is_err());
    assert_eq!(
        Generator::for_crate("x").swift_observation,
        SwiftObservation::Observation
    );
    assert_eq!(Generator::for_crate("x").swift_min_ios, 17);
}

#[test]
fn a_store_member_that_observable_object_declares_is_a_diagnostic_only_in_that_mode() {
    let mut schema = common::case("stores");
    let todos = schema
        .objects
        .iter_mut()
        .find(|o| o.name == "Todos")
        .unwrap();
    todos.store.as_mut().unwrap().signals[0].name = "object_will_change".into();
    let mut generator = Generator::for_crate(&schema.crate_name);
    assert!(generator.swift(&schema).is_ok());
    generator.swift_observation = SwiftObservation::ObservableObject;
    let errors = generator.swift(&schema).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let text = errors[0].to_string();
    assert!(
        errors[0].code() == "E0051"
            && text.contains("objectWillChange")
            && text.contains("ObservableObject"),
        "{text}"
    );
    // The other languages do not mind.
    assert!(generator.kotlin(&schema).is_ok() && generator.typescript(&schema).is_ok());
}

#[test]
fn swift_stores_and_objects_restate_unchecked_sendable() {
    // `UndraObject` and `UndraStore` are `@unchecked Sendable`; Swift 6 warns when a subclass does
    // not say so again (playground finding 6).
    for case in ["objects", "stores", "full", "queries", "stdlib"] {
        let out = all_files(case, |_| {});
        for f in &out[0].1 {
            for line in f
                .contents
                .lines()
                .filter(|l| l.starts_with("public final class "))
            {
                assert!(
                    line.contains("@unchecked Sendable"),
                    "{case}/{}: {line}",
                    f.path
                );
            }
        }
    }
}

#[test]
fn asynchronous_methods_without_a_result_can_throw_in_both_modes() {
    for typed in [true, false] {
        let out = all_files("objects", |g| g.swift_typed_throws = typed);
        let swift = file(&out[0].1, "Objects.swift");
        assert!(swift.contains("public func compute(input: Double?) async throws -> Double"));
        assert!(swift.contains("public func warmUp() async throws {"));
        // A synchronous method that returns a value throws too (ADR-032); only a command, which
        // returns nothing, stays non-throwing.
        assert!(swift.contains("public func add(a: Int32, b: Int32) throws -> Int32 {"));
    }
}

#[test]
fn swift_generated_code_never_stops_the_process() {
    // ADR-032, decision 1: no outcome of a call reaches a trap, in any case or configuration.
    const TRAPS: [&str; 8] = [
        "fatalError",
        "undraUnexpected",
        "preconditionFailure",
        "precondition(",
        "assertionFailure",
        "assert(",
        "try!",
        "as!",
    ];
    for case in common::CASES {
        for typed in [true, false] {
            let out = all_files(case, |g| g.swift_typed_throws = typed);
            for f in &out[0].1 {
                for trap in TRAPS {
                    assert!(
                        !f.contents.contains(trap),
                        "{case}/{} (typed throws {typed}) contains `{trap}`",
                        f.path
                    );
                }
            }
        }
    }
}

/// The text of the method that starts at `head` in `swift`, up to the next method.
fn swift_method<'a>(swift: &'a str, head: &str) -> &'a str {
    let start = swift
        .find(head)
        .unwrap_or_else(|| panic!("no method starts with `{head}`"));
    let rest = &swift[start + head.len()..];
    let end = rest.find("\n    public ").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn swift_call_shapes_follow_adr_032() {
    let out = all_files("objects", |_| {});
    let swift = file(&out[0].1, "Objects.swift");
    // A call throws plain `throws`, whatever its Rust signature.
    for shape in [
        "public func add(a: Int32, b: Int32) throws -> Int32",
        "public func divide(a: Int64, b: Int64) throws -> Int64",
        "public func check() throws {",
        "public func lookup(id: UUID) async throws -> Todo",
        "public func compute(input: Double?) async throws -> Double",
        ") throws -> Calculator {",
        ") async throws -> Calculator {",
    ] {
        assert!(swift.contains(shape), "missing `{shape}`");
    }
    assert!(
        !swift.contains("throws("),
        "calls must not use typed throws"
    );
    // The mapping is the runtime's, once: the typed error is its `domain`.
    assert!(swift.contains("throw UndraCallError.mapped(error, domain: CalcError.self)"));
    assert!(swift.contains("throw UndraCallError.mapped(error)\n"));
    // A command (a synchronous method that returns nothing and has no error type) does not throw;
    // it reports and returns.
    let reset = swift_method(swift, "public func reset() {");
    assert!(reset.contains("self.core.report(error, operation: \"Calculator.reset\")"));
    assert!(!reset.contains("throw "));
    // The error the call can throw is documented on the method.
    let divide = swift_method(
        swift,
        "public func divide(a: Int64, b: Int64) throws -> Int64",
    );
    assert!(divide.contains("UndraCallError.mapped(error, domain: CalcError.self)"));
    assert!(swift.contains(
        "/// - Throws: ``CalcError``, or ``UndraCallError`` if the call fails in the core or cannot reach it."
    ));
    assert!(swift.contains(
        "/// - Throws: ``CalcError``, `CancellationError` if the task is cancelled, or ``UndraCallError``."
    ));
    assert!(swift.contains(
        "/// - Throws: `CancellationError` if the task is cancelled, or ``UndraCallError``."
    ));
    assert!(swift.contains(
        "/// - Throws: ``UndraCallError`` if the call fails in the core or cannot reach it."
    ));
    // Streams map their failures; they are not `throws`.
    assert!(swift.contains(
        "mapError: { UndraCallError.mapped(streamFailure: $0, domain: CalcError.self) }"
    ));
    assert!(swift.contains("mapError: { UndraCallError.mapped(streamFailure: $0) }"));
    // A stream and a command say in their docs where a failure goes.
    assert!(swift.contains(
        "/// - Note: Iterating throws ``CalcError``, or ``UndraCallError`` if the call fails in the core or cannot reach it; cancelling the iterating task ends the loop quietly."
    ));
    assert!(swift.contains(
        "/// - Note: Iterating throws ``UndraCallError`` if the call fails in the core or cannot reach it; cancelling the iterating task ends the loop quietly."
    ));
    assert!(swift.contains(
        "/// - Note: A failure is logged and passed to `LoadOptions.onError`; the method does not throw.\n    public func reset() {"
    ));
    // An async constructor checks the handle the way `UndraCore.construct` does.
    assert!(swift.contains(
        "if handle.isNull {\n                throw UndraProtocolError.nullHandle\n            }"
    ));
    // The per-error helpers of the old policy are gone.
    let errors = file(&out[0].1, "Errors.swift");
    assert!(!errors.contains("undraFromReply"));
    assert!(!errors.contains("undraUnexpected"));
}

#[test]
fn swift_free_function_commands_report_through_their_context() {
    // A command on a free function reports through its own `ctx`, by its Swift name without the
    // backticks that escape a keyword; it is not `throws`.
    let mut schema = common::case("objects");
    schema.functions.push(common::function(
        "fire_and_forget",
        "",
        vec![],
        undra_meta::TypeRef::Unit,
        false,
    ));
    schema.functions.push(common::function(
        "default",
        "",
        vec![],
        undra_meta::TypeRef::Unit,
        false,
    ));
    let generator = common::generator_for("objects", &schema);
    let files = generator.swift(&schema).unwrap();
    let swift = file(&files, "Objects.swift");
    let command = swift_method_at_top_level(
        swift,
        "public func fireAndForget(ctx: UndraCore = UndraGoldenObjects.core) {",
    );
    assert!(command.contains("ctx.report(error, operation: \"fireAndForget\")"));
    assert!(!command.contains("throw "));
    let keyword = swift_method_at_top_level(
        swift,
        "public func `default`(ctx: UndraCore = UndraGoldenObjects.core) {",
    );
    assert!(keyword.contains("ctx.report(error, operation: \"default\")"));
}

/// The text of the top-level function that starts at `head`, up to the next top-level item.
fn swift_method_at_top_level<'a>(swift: &'a str, head: &str) -> &'a str {
    let start = swift
        .find(head)
        .unwrap_or_else(|| panic!("no function starts with `{head}`"));
    let rest = &swift[start + head.len()..];
    let end = rest.find("\n}\n").map_or(rest.len(), |i| i + 3);
    &rest[..end]
}

#[test]
fn swift_store_apply_reports_undecodable_changes() {
    let out = all_files("stores", |_| {});
    let stores = file(&out[0].1, "Stores.swift");
    assert!(stores.contains("self.core.report(error, operation: \""));
    assert!(stores.contains(".apply(signal: \\(signal))\")"));
    assert!(!stores.contains("assertionFailure"));
    // A `PatchError` still re-observes the signal.
    assert!(stores.contains("} catch is PatchError {"));
    // A full value is stored only once it decoded whole, so a change with trailing bytes is
    // skipped, not half applied.
    assert!(stores.contains("let value = try "));
    assert!(stores.contains("try reader.finish()\n") && stores.contains(" = value\n"));
    let finish = stores.find("try reader.finish()").unwrap();
    let store = stores.find(" = value\n").unwrap();
    assert!(
        finish < store,
        "a full value is stored before `finish()` checks it"
    );
}

#[test]
fn js_number_maps_wide_integers_to_number() {
    let big = all_files("records", |_| {});
    let small = all_files("records", |g| g.ts_js_number = true);
    let big = file(&big[2].1, "src/types.ts");
    let small = file(&small[2].1, "src/types.ts");
    assert!(
        big.contains("d: bigint;")
            && big.contains("w.writeI64(v.d)")
            && big.contains("r.readU64()")
    );
    assert!(small.contains("d: number;") && small.contains("w.writeI64Number(v.d)"));
    assert!(small.contains("r.readU64Number()") && small.contains("codecs.u64Number"));
    assert!(!small.contains("bigint"));
}

// ----- determinism -----------------------------------------------------------------------

fn shuffled(schema: &Schema) -> Schema {
    let mut s = schema.clone();
    s.records.reverse();
    s.enums.reverse();
    s.objects.reverse();
    s.functions.reverse();
    s.ports.reverse();
    s.queries.reverse();
    for en in &mut s.enums {
        en.variants.reverse();
    }
    s
}

#[test]
fn the_output_does_not_depend_on_the_order_of_the_schema() {
    for case in common::CASES {
        let schema = common::case(case);
        let generator = common::generator_for(case, &schema);
        let other = shuffled(&schema);
        assert_eq!(
            generator.swift(&schema).unwrap(),
            generator.swift(&other).unwrap(),
            "{case}/swift"
        );
        assert_eq!(
            generator.kotlin(&schema).unwrap(),
            generator.kotlin(&other).unwrap(),
            "{case}/kotlin"
        );
        assert_eq!(
            generator.typescript(&schema).unwrap(),
            generator.typescript(&other).unwrap(),
            "{case}/ts"
        );
    }
}

#[test]
fn docs_do_not_change_the_ids_but_do_change_the_output() {
    // The schema hash embedded in every header excludes docs.
    let schema = common::case("full");
    let mut documented = schema.clone();
    documented.records[0].docs = "A different doc.".into();
    let generator = Generator::for_crate("playground-core");
    let a = generator.typescript(&schema).unwrap();
    let b = generator.typescript(&documented).unwrap();
    assert_eq!(file(&a, "ids.ts"), file(&b, "ids.ts"));
    assert_ne!(file(&a, "types.ts"), file(&b, "types.ts"));
}

// ----- structural sanity ------------------------------------------------------------------

/// Checks that brackets balance outside strings and comments.
fn balanced(path: &str, text: &str) -> Result<(), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => line += 1,
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
                continue;
            }
            '"' | '`' if path.ends_with(".swift") && c == '`' => {
                // A Swift backtick identifier: skip to the closing backtick.
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
            }
            '"' | '`' => {
                let quote = c;
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    if chars.get(i) == Some(&'\n') {
                        line += 1;
                    }
                    i += 1;
                }
            }
            '(' | '[' | '{' => stack.push((c, line)),
            ')' | ']' | '}' => {
                let want = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match stack.pop() {
                    Some((open, _)) if open == want => {}
                    other => return Err(format!("{path}:{line}: `{c}` closes {other:?}")),
                }
            }
            _ => {}
        }
        i += 1;
    }
    match stack.pop() {
        None => Ok(()),
        Some((open, at)) => Err(format!(
            "{path}: `{open}` opened on line {at} is never closed"
        )),
    }
}

#[test]
fn every_generated_file_is_well_formed() {
    for case in common::CASES {
        for (language, files) in all_files(case, |_| {}) {
            for f in files {
                let path = format!("{case}/{language}/{}", f.path);
                assert!(
                    f.contents.ends_with('\n'),
                    "{path} does not end in a newline"
                );
                assert!(!f.contents.ends_with("\n\n"), "{path} ends in a blank line");
                assert!(!f.contents.contains('\t'), "{path} contains a tab");
                // `TODO` is legitimate inside identifiers such as `TODOS` (a query called `todos`).
                for marker in ["TODO(", "// TODO", "unimplemented"] {
                    assert!(!f.contents.contains(marker), "{path} contains {marker}");
                }
                for (n, line) in f.contents.lines().enumerate() {
                    assert!(
                        line == line.trim_end(),
                        "{path}:{}: trailing whitespace",
                        n + 1
                    );
                }
                if f.path.ends_with(".swift") || f.path.ends_with(".kt") || f.path.ends_with(".ts")
                {
                    if let Err(e) = balanced(&f.path, &f.contents) {
                        panic!("{case}/{language}: {e}");
                    }
                }
                // C headers, module maps and R8 rules say it in their own comment syntax.
                assert!(
                    f.contents.starts_with("// Generated by undra-bindgen")
                        || f.contents.lines().next().is_some_and(|line| {
                            line.contains("Generated by undra")
                                && ["/*", "//", "#"].iter().any(|c| line.starts_with(c))
                        })
                        || f.path.ends_with(".json"),
                    "{path} has no generated header"
                );
            }
        }
    }
}

#[test]
fn the_balance_check_catches_a_missing_brace() {
    assert!(balanced("x.kt", "class A { fun f() { } }").is_ok());
    assert!(balanced("x.kt", "class A { fun f() { }").is_err());
    assert!(balanced("x.kt", "val s = \"}\" // )\nclass A { }").is_ok());
    assert!(balanced("x.swift", "let `default` = 1\nstruct A { }").is_ok());
}

#[test]
fn generated_code_never_mentions_a_type_the_schema_does_not_define() {
    // `Todo` is defined by `full`; a Kotlin/TS/Swift reference to it needs no import beyond the
    // generated files themselves, and the runtime is imported explicitly.
    let out = all_files("full", |_| {});
    let kotlin = file(&out[1].1, "Objects.kt");
    assert!(kotlin.contains("import dev.undra.runtime.UndraCore"));
    assert!(kotlin.contains("import dev.undra.runtime.wire.Payloads.CallTarget"));
    let ts = file(&out[2].1, "src/objects.ts");
    assert!(ts.contains("from \"@undra/runtime\""));
    assert!(ts.contains("from \"./types.js\""));
    assert!(ts.contains("from \"./errors.js\""));
}

#[test]
fn documentation_is_carried_into_every_language() {
    let schema = common::case("records");
    let generator = Generator::for_crate(&schema.crate_name);
    let swift = generator.swift(&schema).unwrap();
    let swift = file(&swift, "Types.swift");
    assert!(swift.contains(
        "/// A todo item.\n/// Shown in the list and synced to the server.\npublic struct Todo"
    ));
    assert!(swift.contains("    /// Stable identifier.\n    public var id: UUID"));
    let kotlin = generator.kotlin(&schema).unwrap();
    let kotlin = file(&kotlin, "Types.kt");
    assert!(kotlin.contains("/**\n * A todo item.\n * Shown in the list and synced to the server.\n */\ndata class Todo("));
    assert!(kotlin.contains("    /** Stable identifier. */\n    val id: UUID,"));
    let ts = generator.typescript(&schema).unwrap();
    let ts = file(&ts, "src/types.ts");
    assert!(ts.contains("/**\n * A todo item.\n * Shown in the list and synced to the server.\n */\nexport interface Todo"));
    assert!(ts.contains("  /** Stable identifier. */\n  id: string;"));
}

#[test]
fn comment_terminators_in_docs_cannot_break_out() {
    let mut schema = Schema::new("t");
    schema.records.push(common::record(
        "R",
        "closes */ early and opens /* nested",
        vec![],
    ));
    let generator = Generator::for_crate("t");
    for text in [
        file(&generator.kotlin(&schema).unwrap(), "Types.kt").to_owned(),
        file(&generator.typescript(&schema).unwrap(), "src/types.ts").to_owned(),
    ] {
        assert!(balanced("x.kt", &text).is_ok(), "{text}");
        assert!(!text.contains("closes */ early"), "{text}");
    }
}

// ----- recursive types (Swift) ---------------------------------------------------------------

/// `Types.swift` of `schema`.
fn swift_types(schema: &Schema) -> String {
    let files = Generator::for_crate("t").swift(schema).unwrap();
    file(&files, "Types.swift").to_owned()
}

/// The text of the Swift declaration that starts with `head`, up to its closing line.
fn swift_decl<'a>(swift: &'a str, head: &str) -> &'a str {
    let start = swift
        .find(head)
        .unwrap_or_else(|| panic!("no declaration starts with `{head}` in:\n{swift}"));
    let rest = &swift[start..];
    let end = rest.find("\n}\n").map_or(rest.len(), |i| i + 3);
    &rest[..end]
}

/// A record whose fields are `(name, type)`.
fn record_of(name: &str, fields: Vec<(&str, TypeRef)>) -> undra_meta::RecordDef {
    common::record(
        name,
        "",
        fields
            .into_iter()
            .map(|(n, t)| common::field(n, t))
            .collect(),
    )
}

fn optional(name: &str) -> TypeRef {
    TypeRef::option(common::named(name))
}

#[test]
fn swift_keeps_the_public_shape_of_a_recursive_record_and_boxes_only_its_storage() {
    let types = swift_types(&common::case("recursive"));
    let node = swift_decl(&types, "public struct ListNode");
    // A Swift engineer reads this as an ordinary optional property.
    assert!(
        node.contains("    public var next: ListNode? {\n"),
        "{node}"
    );
    assert!(node.contains("get { _next?.value }"), "{node}");
    assert!(node.contains("set { _next = newValue.map(UndraIndirect.init) }"));
    assert!(node.contains("    private var _next: UndraIndirect<ListNode>?\n"));
    // The public initializer takes the plain optional; the key on the wire and in `Codable`
    // stays `next`, so a missing key decodes as nil and a nil child is omitted.
    assert!(node.contains("public init(value: Int32, next: ListNode?)"));
    assert!(node.contains("self._next = next.map(UndraIndirect.init)"));
    assert!(node.contains("case _next = \"next\""));
    assert!(node.contains("UndraRecord, Sendable, Hashable, Codable"));
    // The wire code reads and writes the public property.
    assert!(node.contains("self.next.undraEncode(&w)"));
    assert!(node.contains("next: Optional<ListNode>.undraDecode(&r)"));
    // An array keeps its elements on the heap: no box, no storage, no coding keys.
    let tree = swift_decl(&types, "public struct Tree");
    assert!(tree.contains("    public var children: [Tree]\n"));
    assert!(
        !tree.contains("UndraIndirect") && !tree.contains("CodingKeys"),
        "{tree}"
    );
}

#[test]
fn swift_emits_the_indirect_box_once_and_only_when_a_record_needs_it() {
    for case in common::CASES {
        let out = all_files(case, |_| {});
        let types = file(&out[0].1, "Types.swift");
        let declarations = types.matches("final class UndraIndirect<").count();
        let uses = out[0]
            .1
            .iter()
            .filter(|f| !f.path.ends_with("Types.swift"))
            .any(|f| f.contents.contains("UndraIndirect"));
        assert!(!uses, "{case}: only Types.swift mentions the box");
        assert_eq!(
            declarations,
            usize::from(*case == "recursive"),
            "{case}: the box is declared exactly when a record holds itself"
        );
    }
    // `records` already has a record with a `Vec<Self>` field and an optional that is not a
    // cycle: neither needs the box.
    let types = swift_types(&common::case("records"));
    assert!(!types.contains("UndraIndirect"));
}

#[test]
fn swift_boxes_each_field_of_a_cycle_through_records_and_only_those() {
    let mut s = Schema::new("t");
    // A holds B, B holds C, C holds B again: B and C are on a cycle, A only leads into it.
    s.records.push(record_of(
        "A",
        vec![("b", optional("B")), ("n", TypeRef::I32)],
    ));
    s.records.push(record_of("B", vec![("c", optional("C"))]));
    s.records.push(record_of(
        "C",
        vec![("b", optional("B")), ("a", optional("A"))],
    ));
    // D and E hold each other through arrays, which never need a box.
    s.records.push(record_of(
        "D",
        vec![("e", TypeRef::vec(common::named("E")))],
    ));
    s.records.push(record_of(
        "E",
        vec![
            ("d", optional("D")),
            ("m", TypeRef::map(TypeRef::String, common::named("D"))),
        ],
    ));
    let types = swift_types(&s);
    // C -> A -> B -> C closes a cycle too, so every edge among A, B and C is on one: A.b, B.c,
    // C.b and C.a.
    assert!(swift_decl(&types, "public struct A").contains("private var _b: UndraIndirect<B>?"));
    assert!(swift_decl(&types, "public struct B").contains("private var _c: UndraIndirect<C>?"));
    let c = swift_decl(&types, "public struct C");
    assert!(c.contains("private var _b: UndraIndirect<B>?"), "{c}");
    assert!(c.contains("private var _a: UndraIndirect<A>?"), "{c}");
    // `n` is not a field that holds anything.
    assert!(swift_decl(&types, "public struct A").contains("    public var n: Int32\n"));
    // D -> E through an array, E -> D through an optional: the array breaks the cycle.
    let d = swift_decl(&types, "public struct D");
    assert!(
        d.contains("    public var e: [E]\n") && !d.contains("UndraIndirect"),
        "{d}"
    );
    let e = swift_decl(&types, "public struct E");
    assert!(
        e.contains("    public var d: D?\n") && !e.contains("UndraIndirect"),
        "{e}"
    );
    assert_eq!(types.matches("final class UndraIndirect<").count(), 1);

    // Break C -> A: A is no longer on a cycle, so its field goes back to a plain property.
    s.records[2].fields.retain(|f| f.name != "a");
    let types = swift_types(&s);
    assert!(swift_decl(&types, "public struct A").contains("    public var b: B?\n"));
    assert!(swift_decl(&types, "public struct B").contains("private var _c: UndraIndirect<C>?"));
    assert!(swift_decl(&types, "public struct C").contains("private var _b: UndraIndirect<B>?"));
}

#[test]
fn swift_names_the_storage_after_the_field_even_when_the_field_is_a_keyword() {
    let mut s = Schema::new("t");
    // A field called `self` is a keyword and is backticked; its storage, `_self`, is not.
    s.records.push(record_of(
        "Node",
        vec![("self", optional("Node")), ("class_name", TypeRef::String)],
    ));
    let types = swift_types(&s);
    let node = swift_decl(&types, "public struct Node");
    assert!(node.contains("    public var `self`: Node? {\n"), "{node}");
    assert!(
        node.contains("private var _self: UndraIndirect<Node>?"),
        "{node}"
    );
    assert!(node.contains("case _self = \"self\""), "{node}");
    assert!(node.contains("case className\n"), "{node}");
}

#[test]
fn swift_makes_an_enum_indirect_when_a_payload_lies_on_a_cycle() {
    let mut s = Schema::new("t");
    // Direct, optional and mutual self reference: all `indirect`.
    s.enums.push(common::enum_def(
        "Direct",
        "",
        vec![
            common::unit_variant("End", 0),
            common::tuple_variant("Next", 1, vec![common::named("Direct")]),
        ],
    ));
    s.enums.push(common::enum_def(
        "Maybe",
        "",
        vec![common::tuple_variant("Next", 0, vec![optional("Maybe")])],
    ));
    s.enums.push(common::enum_def(
        "Ping",
        "",
        vec![common::tuple_variant(
            "Pong",
            0,
            vec![common::named("Pong")],
        )],
    ));
    s.enums.push(common::enum_def(
        "Pong",
        "",
        vec![
            common::tuple_variant("Ping", 0, vec![optional("Ping")]),
            common::unit_variant("Stop", 1),
        ],
    ));
    // An enum that leads into a cycle without being on it, an array, and a map: none of them.
    s.enums.push(common::enum_def(
        "Into",
        "",
        vec![common::tuple_variant(
            "Ping",
            0,
            vec![common::named("Ping")],
        )],
    ));
    s.enums.push(common::enum_def(
        "Listy",
        "",
        vec![
            common::tuple_variant("Items", 0, vec![TypeRef::vec(common::named("Listy"))]),
            common::tuple_variant(
                "Named",
                1,
                vec![TypeRef::map(TypeRef::String, common::named("Listy"))],
            ),
        ],
    ));
    // A record on a cycle with an enum: the enum is `indirect`, the record boxes its field.
    s.enums.push(common::enum_def(
        "Expr",
        "",
        vec![
            common::tuple_variant("Num", 0, vec![TypeRef::F64]),
            common::tuple_variant("Block", 1, vec![common::named("Block")]),
        ],
    ));
    s.records
        .push(record_of("Block", vec![("last", optional("Expr"))]));
    // An error that wraps itself.
    s.enums.push(common::error_def(
        "Failure",
        "",
        vec![
            common::with_message(common::unit_variant("Leaf", 0), "leaf"),
            common::with_message(
                common::tuple_variant("Wrapped", 1, vec![TypeRef::U8, common::named("Failure")]),
                "wrapped",
            ),
        ],
    ));
    let files = Generator::for_crate("t").swift(&s).unwrap();
    let types = file(&files, "Types.swift");
    for name in ["Direct", "Maybe", "Ping", "Pong", "Expr"] {
        assert!(
            types.contains(&format!("public indirect enum {name}: UndraEnum")),
            "{name} must be indirect:\n{types}"
        );
    }
    for name in ["Into", "Listy"] {
        assert!(
            types.contains(&format!("public enum {name}: UndraEnum")),
            "{name} must not be indirect:\n{types}"
        );
    }
    assert!(
        swift_decl(types, "public struct Block")
            .contains("private var _last: UndraIndirect<Expr>?")
    );
    assert!(file(&files, "Errors.swift").contains("public indirect enum Failure: UndraError"));
}

/// Store signals whose types recurse: an enum whose base case is its last variant, a record that
/// holds it, and two enums that need each other unless one takes its other variant. A signal is
/// initialised with a placeholder until the core's first change-set arrives; the placeholder of a
/// recursive type is built from the first variant that does not need the type itself, wherever the
/// schema lists it, and never from an optional, array or map.
fn recursive_placeholders() -> Schema {
    let mut s = Schema::new("t");
    s.enums.push(common::enum_def(
        "Sum",
        "",
        vec![
            common::tuple_variant("Add", 0, vec![common::named("Sum"), common::named("Sum")]),
            common::tuple_variant("Neg", 1, vec![common::named("Sum")]),
            common::unit_variant("Zero", 2),
        ],
    ));
    s.records.push(record_of(
        "Frame",
        vec![
            ("sum", common::named("Sum")),
            ("next", optional("Frame")),
            ("kids", TypeRef::vec(common::named("Frame"))),
        ],
    ));
    // Two types that need each other unless one of them takes its other variant.
    s.enums.push(common::enum_def(
        "Left",
        "",
        vec![
            common::tuple_variant("Over", 0, vec![common::named("Right")]),
            common::unit_variant("Done", 1),
        ],
    ));
    s.enums.push(common::enum_def(
        "Right",
        "",
        vec![common::tuple_variant(
            "Over",
            0,
            vec![common::named("Left")],
        )],
    ));
    s.objects.push(common::store(
        common::object(
            "Box",
            "",
            vec![common::ctor("Box", "new", vec![], false)],
            vec![],
        ),
        vec![
            ("sum", common::named("Sum"), false, None),
            ("frame", common::named("Frame"), false, None),
            ("left", common::named("Left"), false, None),
            ("right", common::named("Right"), false, None),
        ],
    ));
    s
}

#[test]
fn swift_placeholders_of_a_recursive_type_use_its_base_case() {
    // A signal is initialised with a placeholder until the core's first change-set arrives. The
    // placeholder of a recursive type is built from its first variant that does not need the
    // type itself, wherever the schema lists it, and never from an optional, array or map.
    let s = recursive_placeholders();
    let files = Generator::for_crate("t").swift(&s).unwrap();
    let stores = file(&files, "Stores.swift");
    assert!(
        stores.contains("public private(set) var sum: Sum = Sum.zero\n"),
        "{stores}"
    );
    assert!(
        stores.contains(
            "public private(set) var frame: Frame = Frame(sum: Sum.zero, next: nil, kids: [])\n"
        ),
        "{stores}"
    );
    // `Left` cannot take `Over` (it needs a `Right`, which needs a `Left` again): it takes `Done`.
    assert!(
        stores.contains("public private(set) var left: Left = Left.done\n"),
        "{stores}"
    );
    assert!(
        stores.contains("public private(set) var right: Right = Right.over(Left.done)\n"),
        "{stores}"
    );
    for f in &files {
        for trap in ["fatalError", "preconditionFailure", "try!"] {
            assert!(!f.contents.contains(trap), "{}: `{trap}`", f.path);
        }
    }
}

#[test]
fn kotlin_and_typescript_placeholders_of_a_recursive_type_use_its_base_case() {
    // The same search as Swift's (`crate::zero`). Before it, Kotlin expanded the first variant of
    // `Sum` exponentially until a depth guard wrote `error("recursive default")`, which throws
    // when the store is created, and TypeScript overflowed the generator's stack.
    let s = recursive_placeholders();
    let kotlin = Generator::for_crate("t").kotlin(&s).unwrap();
    let stores = file(&kotlin, "Stores.kt");
    for expected in [
        "private val _sum: MutableStateFlow<Sum> = signal(Sum.Zero)\n",
        "private val _frame: MutableStateFlow<Frame> = signal(Frame(sum = Sum.Zero, next = null, kids = emptyList()))\n",
        "private val _left: MutableStateFlow<Left> = signal(Left.Done)\n",
        "private val _right: MutableStateFlow<Right> = signal(Right.Over(value = Left.Done))\n",
    ] {
        assert!(stores.contains(expected), "{expected}\n{stores}");
    }
    let ts = Generator::for_crate("t").typescript(&s).unwrap();
    let stores = file(&ts, "stores.ts");
    for expected in [
        "readonly sum: Signal<Sum> = new Signal<Sum>({ kind: \"zero\" });\n",
        "readonly frame: Signal<Frame> = new Signal<Frame>({ sum: { kind: \"zero\" }, next: null, kids: [] });\n",
        "readonly left: Signal<Left> = new Signal<Left>({ kind: \"done\" });\n",
        "readonly right: Signal<Right> = new Signal<Right>({ kind: \"over\", value: { kind: \"done\" } });\n",
    ] {
        assert!(stores.contains(expected), "{expected}\n{stores}");
    }
    for f in kotlin.iter().chain(&ts) {
        assert!(!f.contents.contains("recursive default"), "{}", f.path);
        assert!(!f.contents.contains("as never"), "{}", f.path);
    }
}

#[test]
fn kotlin_placeholders_of_deeply_nested_records_are_built_whole() {
    // The old depth guard stopped at nine levels and wrote a trap for a type that is not recursive
    // at all; the path-based search has no depth limit.
    let mut s = Schema::new("t");
    s.records.push(record_of("L0", vec![("v", TypeRef::I32)]));
    for level in 1..12 {
        let inner = format!("L{}", level - 1);
        s.records.push(record_of(
            &format!("L{level}"),
            vec![("inner", common::named(&inner))],
        ));
    }
    s.objects.push(common::store(
        common::object(
            "Deep",
            "",
            vec![common::ctor("Deep", "new", vec![], false)],
            vec![],
        ),
        vec![("top", common::named("L11"), false, None)],
    ));
    let kotlin = Generator::for_crate("t").kotlin(&s).unwrap();
    let stores = file(&kotlin, "Stores.kt");
    assert!(!stores.contains("recursive default"), "{stores}");
    assert!(
        stores.contains("signal(L11(inner = L10(inner = L9("),
        "{stores}"
    );
    assert!(stores.contains("L0(v = 0)"), "{stores}");
}

#[test]
fn kotlin_and_typescript_survive_a_type_that_has_no_value_at_all() {
    // As for Swift: a schema is data, and a type every way to build which needs itself must
    // neither loop nor overflow the generator's stack. No Rust core can have such a store.
    let mut s = Schema::new("t");
    s.enums.push(common::enum_def(
        "Never2",
        "",
        vec![common::tuple_variant(
            "Again",
            0,
            vec![common::named("Never2")],
        )],
    ));
    s.records
        .push(record_of("Own", vec![("again", common::named("Own"))]));
    s.objects.push(common::store(
        common::object(
            "Holder",
            "",
            vec![common::ctor("Holder", "new", vec![], false)],
            vec![],
        ),
        vec![
            ("never", common::named("Never2"), false, None),
            ("own", common::named("Own"), false, None),
        ],
    ));
    let kotlin = Generator::for_crate("t").kotlin(&s).unwrap();
    assert!(file(&kotlin, "Stores.kt").contains("class Holder"));
    let ts = Generator::for_crate("t").typescript(&s).unwrap();
    let stores = file(&ts, "stores.ts");
    assert!(
        stores.contains("new Signal<Never2>(undefined as never)"),
        "{stores}"
    );
}

#[test]
fn swift_survives_a_type_that_has_no_value_at_all() {
    // Nothing in Rust can be like this (`enum E { A(Box<E>) }` has no value), but a schema is
    // data: the generator must neither loop nor overflow the stack on it.
    let mut s = Schema::new("t");
    s.enums.push(common::enum_def(
        "Never2",
        "",
        vec![common::tuple_variant(
            "Again",
            0,
            vec![common::named("Never2")],
        )],
    ));
    s.records
        .push(record_of("Own", vec![("again", common::named("Own"))]));
    // Signals of both types need a placeholder, which no value can give.
    s.objects.push(common::store(
        common::object(
            "Holder",
            "",
            vec![common::ctor("Holder", "new", vec![], false)],
            vec![],
        ),
        vec![
            ("never", common::named("Never2"), false, None),
            ("own", common::named("Own"), false, None),
        ],
    ));
    let files = Generator::for_crate("t").swift(&s).unwrap();
    let types = file(&files, "Types.swift");
    assert!(types.contains("public indirect enum Never2"));
    // A record that holds itself outright is boxed too, so the layout stays finite.
    assert!(
        types.contains("private var _again: UndraIndirect<Own>\n"),
        "{types}"
    );
    assert!(file(&files, "Stores.swift").contains("public final class Holder"));
}

// ----- derived lists (ADR-039 decision 7) -------------------------------------------------

/// The declaration lines of the store property `name` (the doc line above included), with the
/// name replaced, so two properties can be compared modulo their names.
fn declaration(text: &str, lang: &str, name: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let is_decl = |line: &str| match lang {
        "swift" => line.contains(&format!(" var {name}: ")),
        "kotlin" => {
            line.contains(&format!(" val {name}: ")) || line.contains(&format!(" val _{name}: "))
        }
        _ => line.contains(&format!("readonly {name}: ")),
    };
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if is_decl(line) {
            if i > 0
                && (lines[i - 1].trim_start().starts_with("/*")
                    || lines[i - 1].trim_start().starts_with("///"))
            {
                out.push(lines[i - 1].trim().to_owned());
            }
            out.push(line.replace(name, "NAME").trim().to_owned());
        }
    }
    assert!(!out.is_empty(), "{lang}: no declaration of {name}");
    out
}

#[test]
fn a_derived_list_is_declared_exactly_as_a_computed_list_and_also_applies_patches() {
    // A derived list is `computed: true` with a `key` (ADR-039): `visible` in the stores case.
    // `done_todos`, added here, is the same type, computed, without a key. Their generated
    // declarations are the same; only `visible`'s `apply` has the keyed-patch case. No new
    // generated surface.
    let mut schema = common::case("stores");
    let todos = schema
        .objects
        .iter_mut()
        .find(|o| o.name == "Todos")
        .and_then(|o| o.store.as_mut())
        .unwrap();
    let visible = todos.signals.iter().find(|s| s.name == "visible").unwrap();
    assert!(visible.computed && visible.key.as_deref() == Some("id"));
    let mut done = visible.clone();
    done.name = "done_todos".into();
    done.key = None;
    done.signal_id = u32::try_from(todos.signals.len()).unwrap();
    todos.signals.push(done);
    let generator = common::generator_for("stores", &schema);
    let outputs = [
        ("swift".to_owned(), generator.swift(&schema).unwrap()),
        ("kotlin".to_owned(), generator.kotlin(&schema).unwrap()),
        ("ts".to_owned(), generator.typescript(&schema).unwrap()),
    ];
    for (lang, files) in outputs {
        let (stores, derived, plain, patch_of) = match lang.as_str() {
            "swift" => (
                "Stores.swift",
                "visible",
                "doneTodos",
                "try applyPatch(ops, to: &self.NAME)",
            ),
            "kotlin" => (
                "Stores.kt",
                "visible",
                "doneTodos",
                "_NAME.value = KeyedPatch.applyPatch(_NAME.value, ops)",
            ),
            _ => (
                "stores.ts",
                "visible",
                "doneTodos",
                "this.NAME._set(applyPatch(this.NAME.peek(), ops));",
            ),
        };
        let text = file(&files, stores);
        // The declarations are the same shape; only the doc comment says which kind it is.
        let code_only = |lines: Vec<String>| -> Vec<String> {
            lines
                .into_iter()
                .filter(|line| {
                    let l = line.trim_start();
                    !(l.starts_with("///") || l.starts_with("/**") || l.starts_with('*'))
                })
                .collect()
        };
        assert_eq!(
            code_only(declaration(text, &lang, derived)),
            code_only(declaration(text, &lang, plain)),
            "{lang}: a derived list's declaration is a computed list's"
        );
        assert!(
            declaration(text, &lang, derived)
                .iter()
                .any(|line| line.contains("Derived by the core from another list; read-only.")),
            "{lang}: documented as derived and read-only"
        );
        assert!(
            declaration(text, &lang, plain)
                .iter()
                .any(|line| line.contains("Computed by the core; read-only.")),
            "{lang}: a plain computed list stays documented as computed"
        );
        assert!(
            text.contains(&patch_of.replace("NAME", derived)),
            "{lang}: the derived list applies keyed patches"
        );
        assert!(
            !text.contains(&patch_of.replace("NAME", plain)),
            "{lang}: a computed list without a key is only ever replaced"
        );
    }
}

// ----- newtypes, polling, infinite queries and lazy lists (ADR-042, ADR-043) --------------------

/// The newtypes of the `newtypes` case whose inner type has an order that means something.
const ORDERED: &[&str] = &["Created", "Meters", "OrderNo", "Span", "Timeout", "TodoId"];

/// The others: `Uuid`, `bool`, bytes, an option, a list, a map, a record, an enum, and newtypes of those.
const UNORDERED: &[&str] = &[
    "Blob", "Boss", "Flag", "Level", "Maybe", "Nickname", "Owner", "Ratings", "Tags", "UserId",
    "Wrapped",
];

/// The line that declares `head` (`public struct Meters`, `value class Meters(`, ...).
fn declaring<'a>(text: &'a str, head: &str) -> &'a str {
    text.lines()
        .find(|l| l.contains(head))
        .unwrap_or_else(|| panic!("no line declares `{head}`"))
}

#[test]
fn newtypes_are_comparable_only_where_the_order_means_something() {
    let out = all_files("newtypes", |_| {});
    let swift = file(&out[0].1, "Types.swift");
    let kotlin = file(&out[1].1, "Types.kt");
    for name in ORDERED {
        assert!(
            declaring(swift, &format!("public struct {name}:")).ends_with(", Comparable {"),
            "{name}"
        );
        assert!(
            declaring(kotlin, &format!("value class {name}("))
                .contains(&format!("Comparable<{name}>")),
            "{name}"
        );
    }
    for name in UNORDERED {
        assert!(
            !declaring(swift, &format!("public struct {name}:")).contains("Comparable"),
            "{name}"
        );
        assert!(
            !declaring(kotlin, &format!("class {name}(")).contains("Comparable"),
            "{name}"
        );
    }
    // No literal conformance: that would let any literal become a `UserId`.
    assert!(!swift.contains("ExpressibleBy"));
}

#[test]
fn swift_newtypes_wrap_their_value_and_cross_as_it() {
    let out = all_files("newtypes", |_| {});
    let swift = file(&out[0].1, "Types.swift");
    let user = swift_decl(swift, "public struct UserId:");
    assert!(
        user.starts_with(
            "public struct UserId: RawRepresentable, UndraRecord, Sendable, Hashable, Codable {"
        ),
        "{user}"
    );
    for part in [
        "public var rawValue: UUID\n",
        "public init(rawValue: UUID) {",
        "public init(_ rawValue: UUID) {",
        "rawValue = try decoder.singleValueContainer().decode(UUID.self)",
        "try container.encode(rawValue)",
        "return try UserId(UUID.undraDecode(&r))",
        "self.rawValue.undraEncode(&w)",
    ] {
        assert!(user.contains(part), "`{part}` is missing from:\n{user}");
    }
    // The wire says nothing of the wrapper: no tag, no length.
    assert!(!user.contains("writeU"));
    // Bytes and options go through the same paths as everywhere else.
    let blob = swift_decl(swift, "public struct Blob:");
    assert!(
        blob.contains("return try Blob(r.readBytes())")
            && blob.contains("w.writeBytes(self.rawValue)")
    );
    let nick = swift_decl(swift, "public struct Nickname:");
    assert!(nick.contains("decode(Optional<String>.self)"), "{nick}");
    // A `Duration` is not `Codable` before Swift 6, and so neither is a newtype of it.
    let timeout = swift_decl(swift, "public struct Timeout:");
    assert!(
        timeout
            .lines()
            .next()
            .unwrap()
            .ends_with("Hashable, Comparable {")
            && !timeout.contains("Decoder"),
        "{timeout}"
    );
}

#[test]
fn kotlin_newtypes_are_value_classes_except_bytes() {
    let out = all_files("newtypes", |_| {});
    let kotlin = file(&out[1].1, "Types.kt");
    assert!(
        kotlin.contains("@JvmInline\nvalue class UserId(val value: UUID) : UndraRecord {"),
        "{kotlin}"
    );
    assert!(
        kotlin.contains(
            "override fun decode(r: UndraReader): UserId = UserId(Codecs.uuid.decode(r))"
        )
    );
    // A value class cannot override `equals`, and a `ByteArray` compares by identity.
    let blob = declaring(kotlin, "class Blob(");
    assert_eq!(blob, "class Blob(val value: ByteArray) : UndraRecord {");
    assert!(kotlin.contains("other is Blob && value.contentEquals(other.value)"));
    assert!(kotlin.contains("override fun hashCode(): Int = value.contentHashCode()"));
    // Nested and optional newtypes keep their own types.
    assert!(kotlin.contains("value class Boss(val value: Owner) : UndraRecord {"));
    assert!(kotlin.contains("value class Nickname(val value: String?) : UndraRecord {"));
}

#[test]
fn typescript_newtypes_are_branded_once_and_their_codecs_do_not_wait_for_each_other() {
    let out = all_files("newtypes", |_| {});
    let types = file(&out[2].1, "src/types.ts");
    assert!(types.contains("export type UserId = string & { readonly __brand: \"UserId\" };"));
    assert!(
        types.contains(
            "export function UserId(value: string): UserId {\n  return value as UserId;\n}"
        )
    );
    assert!(
        types.contains("export const UserIdCodec: Codec<UserId> = codecs.uuid as Codec<UserId>;")
    );
    // A brand on a branded type is two `__brand` literals in one intersection, which is `never`: a
    // newtype of a newtype is branded on what the inner one wraps, and so is an option of one.
    for line in types.lines().filter(|l| l.starts_with("export type ")) {
        assert!(line.matches("__brand").count() <= 1, "{line}");
    }
    assert!(types.contains("export type Boss = string & { readonly __brand: \"Boss\" };"));
    assert!(types.contains(
        "export function Boss(value: Owner): Boss {\n  return value as string as Boss;\n}"
    ));
    assert!(
        types.contains("export type Maybe = (string & { readonly __brand: \"Maybe\" }) | null;")
    );
    // An optional is branded inside (`null & brand` is `never`).
    assert!(
        types.contains(
            "export type Nickname = (string & { readonly __brand: \"Nickname\" }) | null;"
        )
    );
    // The codec of a composite or a named inner type is built when it runs: `ZedCodec` below a
    // newtype of `Zed` would still be unassigned when a constant read it at module load.
    assert!(types.contains(
        "export const TagsCodec: Codec<Tags> = {\n  encode(w, v) {\n    vecString.encode(w, v);"
    ));
    assert!(types.contains("return TodoCodec.decode(r) as Wrapped;"));
}

#[test]
fn newtypes_are_valid_map_keys_and_keyed_list_keys() {
    let out = all_files("newtypes", |_| {});
    let account = |text: &str, needle: &str| assert!(text.contains(needle), "`{needle}`");
    account(
        file(&out[0].1, "Types.swift"),
        "public var scores: [UserId: Meters]",
    );
    account(
        file(&out[1].1, "Types.kt"),
        "val scores: Map<UserId, Meters>,",
    );
    account(
        file(&out[2].1, "src/types.ts"),
        "scores: Map<UserId, Meters>;",
    );
    // `Board.todos` is keyed on a newtype field: the patches are positional, so it applies like any list.
    account(
        file(&out[0].1, "Stores.swift"),
        "try applyPatch(ops, to: &self.todos)",
    );
    account(
        file(&out[1].1, "Stores.kt"),
        "KeyedPatch.applyPatch(_todos.value, ops)",
    );
    account(
        file(&out[2].1, "src/stores.ts"),
        "this.todos._set(applyPatch(this.todos.peek(), ops));",
    );
}

#[test]
fn a_newtype_has_its_inner_types_zero_value_in_every_language() {
    let out = all_files("newtypes", |_| {});
    let stores = file(&out[0].1, "Stores.swift");
    assert!(stores.contains("public private(set) var owner: UserId = UserId(UUID(uuid: (0, 0, 0"));
    assert!(stores.contains("public private(set) var blob: Blob = Blob([])"));
    assert!(stores.contains("public private(set) var nickname: Nickname = Nickname(nil)"));
    let stores = file(&out[1].1, "Stores.kt");
    assert!(stores.contains("signal(UserId(UUID(0L, 0L)))"));
    assert!(stores.contains("signal(Blob(ByteArray(0)))"));
    assert!(stores.contains("signal(Nickname(null))"));
    let stores = file(&out[2].1, "src/stores.ts");
    assert!(
        stores.contains("new Signal<UserId>(UserId(\"00000000-0000-0000-0000-000000000000\"))")
    );
    assert!(stores.contains("new Signal<Blob>(Blob(new Uint8Array(0)))"));
    assert!(stores.contains("new Signal<Nickname>(Nickname(null))"));
    // The constructor function is a value import beside the type.
    assert!(stores.contains("UserId,") || stores.contains(" UserId"));
    assert!(!stores.contains("type UserId"), "UserId is used as a value");
}

#[test]
fn every_query_handle_sets_its_poll_interval_in_the_native_spelling() {
    let handles = |files: &[GeneratedFile], suffix: &str, class: &str| {
        let text = file(files, suffix).to_owned();
        (text.matches(class).count(), text)
    };
    for case in ["queries", "polling", "infinite"] {
        let out = all_files(case, |_| {});
        let (n, swift) = handles(&out[0].1, "Queries.swift", "QueryHandle: UndraStore");
        assert_eq!(
            swift
                .matches("public func setPollInterval(_ interval: Duration?) {")
                .count(),
            n,
            "{case}"
        );
        let (n, kotlin) = handles(&out[1].1, "Queries.kt", "QueryHandle internal constructor");
        assert_eq!(
            kotlin
                .matches("fun setPollInterval(interval: Duration?) {")
                .count(),
            n,
            "{case}"
        );
        let ts = file(&out[2].1, "src/queries.ts");
        let n = ts.matches("export class ").count();
        assert_eq!(
            ts.matches("async setPollInterval(ms: Duration | null): Promise<void> {")
                .count(),
            n,
            "{case}"
        );
        assert!(n > 0, "{case} has a query handle");
    }
    // Below iOS 16 the wire duration is the runtime's own.
    let floor = swift_in("polling", SwiftObservation::ObservableObject, 15);
    assert!(
        file(&floor, "Queries.swift")
            .contains("public func setPollInterval(_ interval: UndraDuration?) {")
    );
    // `None` clears the override: the argument is the optional duration, nothing else.
    let ids = file(
        &swift_in("polling", SwiftObservation::Observation, 17),
        "Ids.swift",
    )
    .to_owned();
    assert_eq!(
        ids.matches("public static let setPollInterval: UInt32 = 0xe327e53b")
            .count(),
        3
    );
}

#[test]
fn only_infinite_handles_page_and_their_data_is_a_keyed_list() {
    let out = all_files("infinite", |_| {});
    let swift = file(&out[0].1, "Queries.swift");
    let feed = &swift[swift.find("public final class FeedQueryHandle").unwrap()
        ..swift.find("public final class ProfileQueryHandle").unwrap()];
    for part in [
        "public private(set) var data: [Post] = []",
        "public private(set) var hasNextPage: Bool = false",
        "public private(set) var fetchingNextPage: Bool = false",
        "public func fetchNextPage() {",
        "public func loadMore(ifNeededFor item: Post, threshold: Int = 5) {",
        "data.suffix(max(threshold, 0)).contains(where: { $0.id == item.id })",
        "try applyPatch(ops, to: &self.data)",
    ] {
        assert!(
            feed.contains(part),
            "`{part}` is missing from the feed handle"
        );
    }
    // The other handles are what they were.
    let profile = &swift[swift.find("public final class ProfileQueryHandle").unwrap()
        ..swift.find("/// Observes the `search` query").unwrap()];
    assert!(profile.contains("public private(set) var data: String? = nil"));
    assert!(!profile.contains("fetchNextPage") && !profile.contains("loadMore"));
    // The key of a search result is `slug`: the row is not `Identifiable`, and its `loadMore` says so.
    let types = file(&out[0].1, "Types.swift");
    assert!(declaring(types, "public struct Post:").ends_with("Codable, Identifiable {"));
    assert!(declaring(types, "public struct Hit:").ends_with("Hashable, Codable {"));
    assert!(
        swift.contains("data.suffix(max(threshold, 0)).contains(where: { $0.slug == item.slug })")
    );

    let kotlin = file(&out[1].1, "Queries.kt");
    assert!(kotlin.contains("class FeedQueryHandle internal constructor(core: UndraCore, handle: Long) : UndraStore(core, handle), InfiniteQuery {"));
    assert!(
        kotlin
            .contains("override val hasNextPage: StateFlow<Boolean> = _hasNextPage.asStateFlow()")
    );
    assert!(kotlin.contains(
        "override val fetchingNextPage: StateFlow<Boolean> = _fetchingNextPage.asStateFlow()"
    ));
    assert!(kotlin.contains("override fun fetchNextPage() {"));
    assert!(kotlin.contains("import dev.undra.runtime.InfiniteQuery"));
    assert!(kotlin.contains("KeyedPatch.applyPatch(_data.value, ops)"));
    assert_eq!(
        kotlin.matches("override fun fetchNextPage()").count(),
        2,
        "feed and search"
    );
    assert!(kotlin.contains("class ProfileQueryHandle internal constructor(core: UndraCore, handle: Long) : UndraStore(core, handle) {"));

    let ts = file(&out[2].1, "src/queries.ts");
    assert!(ts.contains("readonly hasNextPage: Signal<boolean> = new Signal<boolean>(false);"));
    assert!(
        ts.contains("readonly fetchingNextPage: Signal<boolean> = new Signal<boolean>(false);")
    );
    assert!(ts.contains("async fetchNextPage(): Promise<void> {"));
    assert!(ts.contains("this.data._set(applyPatch(this.data.peek(), ops));"));
    // Two infinite handles page; the plain one does not.
    assert_eq!(ts.matches("async fetchNextPage()").count(), 2);
}

#[test]
fn a_lazy_signal_is_the_runtimes_lazy_list_made_with_its_store() {
    let out = all_files("lazy", |_| {});
    let swift = file(&out[0].1, "Stores.swift");
    for part in [
        "public let books: UndraLazyList<Book>\n",
        "public let recent: UndraLazyList<Book>\n",
        "self.books = UndraLazyList(core: core)\n        self.recent = UndraLazyList(core: core)\n        super.init(core: core, handle: handle)",
        "try self.books.applyFull(&reader)",
        "try self.books.applyInvalidated(&reader)",
    ] {
        assert!(swift.contains(part), "`{part}` is missing:\n{swift}");
    }
    // It is not a placeholder-valued property.
    assert!(!swift.contains("var books") && !swift.contains("var recent"));
    // The ordinary signals around it are what they were.
    assert!(swift.contains("public private(set) var selected: Book? = nil"));

    let floor = swift_in("lazy", SwiftObservation::ObservableObject, 15);
    let swift = file(&floor, "Stores.swift");
    assert!(swift.contains("public let books: UndraLazyListObject<Book>\n"));
    assert!(swift.contains("self.books = UndraLazyListObject(core: core)"));
    assert!(!swift.contains("@Published public let"));

    let kotlin = file(&out[1].1, "Stores.kt");
    assert!(kotlin.contains("val books: UndraLazyList<Book> = UndraLazyList(core, Book)"));
    assert!(
        kotlin.contains(
            "if (op == ChangeOp.FULL) {\n                        books.applyFull(reader)"
        )
    );
    assert!(kotlin.contains("} else if (op == ChangeOp.INVALIDATED) {\n                        books.applyInvalidated(reader)"));
    assert!(kotlin.contains("override fun close() {\n        books.close()\n        recent.close()\n        super.close()"));

    let ts = file(&out[2].1, "src/stores.ts");
    assert!(ts.contains("readonly books: LazyList<Book> = new LazyList(this.core, BookCodec);"));
    assert!(ts.contains("books.applyFull(new UndraReader(value));"));
    assert!(ts.contains("books.applyInvalidated(new UndraReader(value));"));
    // Only the signals are in the list of signals.
    assert!(
        ts.contains("this._signals = [this.shelves, this.selected, this.total];"),
        "{ts}"
    );
    // A store without a lazy list has no `close` of its own.
    let stores = file(&all_files("stores", |_| {})[1].1, "Stores.kt").to_owned();
    assert!(!stores.contains("override fun close"));
}

#[test]
fn generic_instantiations_are_the_plain_records_and_enums_they_stand_for() {
    let out = all_files("generics", |_| {});
    let swift = file(&out[0].1, "Types.swift");
    assert!(swift.contains("public struct TodoPage: UndraRecord, Sendable, Hashable, Codable {"));
    assert!(
        swift.contains("public enum LoadableUser: UndraEnum, Sendable, Hashable {"),
        "{swift}"
    );
    let ts = file(&out[2].1, "src/types.ts");
    assert!(
        ts.contains("export interface UserPage {\n  items: User[];\n  next: string | null;\n}")
    );
}

#[test]
fn decimal_maps_to_each_languages_exact_type() {
    let out = all_files("decimal", |_| {});
    let swift = file(&out[0].1, "Types.swift");
    assert!(
        swift.contains("public var total: Decimal\n")
            && swift.contains("public var lines: [Decimal]\n")
    );
    assert!(swift.contains("public var discount: Decimal?\n"));
    // A decimal has an order, and so does a newtype of one.
    assert!(declaring(swift, "public struct Price:").ends_with("Codable, Comparable {"));
    let kotlin = file(&out[1].1, "Types.kt");
    assert!(
        kotlin.contains("val total: BigDecimal,")
            && kotlin.contains("val lines: List<BigDecimal>,")
    );
    assert!(
        kotlin.contains(
            "value class Price(val value: BigDecimal) : UndraRecord, Comparable<Price> {"
        )
    );
    assert!(kotlin.contains("val tax: BigDecimal = BigDecimal.ZERO,"));
    let ts = file(&out[2].1, "src/types.ts");
    assert!(
        ts.contains("total: Decimal;")
            && ts.contains("lines: Decimal[];")
            && ts.contains("discount: Decimal | null;")
    );
    assert!(ts.contains("export type Price = Decimal & { readonly __brand: \"Price\" };"));
    assert!(ts.contains("export const PriceCodec: Codec<Price> = decimalCodec as Codec<Price>;"));
    assert!(!ts.contains("Map<Decimal"), "a decimal is never a map key");
}

// ----- the Swift port builder from closures (ADR-066) ----------------------------------------

/// A port whose methods are named like the locals of a method-table entry (`args`, `r`, `result`,
/// `error`) and like the adapter's parameter (`impl`): the builder names its closures after the
/// methods, so the entry's own locals step aside and every call reaches the closure.
fn awkward_port() -> Schema {
    use undra_meta::PortKind;
    let mut s = Schema::new("awkward");
    s.enums.push(common::error_def(
        "AwkwardError",
        "",
        vec![common::with_message(common::unit_variant("Bad", 0), "bad")],
    ));
    s.ports.push(common::port(
        "Awkward",
        "",
        PortKind::Sync,
        vec![
            common::port_method(
                "Awkward",
                "args",
                "",
                vec![common::param("args", TypeRef::U8)],
                TypeRef::U8,
                false,
            ),
            common::port_method(
                "Awkward",
                "r",
                "",
                vec![common::param("r", TypeRef::String)],
                TypeRef::Unit,
                false,
            ),
            common::port_method("Awkward", "result", "", vec![], TypeRef::I64, false),
            common::port_method(
                "Awkward",
                "error",
                "",
                vec![common::param("error", TypeRef::Bool)],
                TypeRef::result(TypeRef::Unit, TypeRef::named("AwkwardError")),
                false,
            ),
            common::port_method("Awkward", "impl", "", vec![], TypeRef::Unit, false),
        ],
    ));
    s
}

#[test]
fn the_swift_port_builder_never_shadows_a_closure_named_like_an_entry_local() {
    let schema = awkward_port();
    let generator = common::generator_for("awkward", &schema);
    let swift = generator.swift(&schema).unwrap();
    let ports = file(&swift, "Ports.swift");
    assert!(ports.contains(
        "public func awkwardPortImpl(\n    args: @escaping @Sendable (_ args: UInt8) -> UInt8,\n    r: @escaping @Sendable (_ r: String) -> Void,\n    result: @escaping @Sendable () -> Int64,\n    error: @escaping @Sendable (_ error: Bool) throws(AwkwardError) -> Void,\n    impl: @escaping @Sendable () -> Void\n) -> PortImpl"
    ), "the builder's signature:\n{ports}");
    let builder = ports.split("impl: @escaping").nth(1).unwrap();
    for expected in [
        // `args`: the entry's argument bytes and the method's own parameter both step aside.
        "{ args_ in\n            var r = UndraReader(args_)\n            let args__ = try UInt8.undraDecode(&r)\n            try r.finish()\n            let result = args(args__)\n            return result.undraEncoded()",
        // `r`: the reader steps aside.
        "var r_ = UndraReader(args)\n            let r__ = try String.undraDecode(&r_)\n            try r_.finish()\n            r(r__)\n            return []",
        // `result`: the local holding what the closure returned steps aside.
        "let result_ = result()\n            return result_.undraEncoded()",
        // `error`: the caught error steps aside.
        "try error(error__)\n                return []\n            } catch let error_ as AwkwardError {\n                throw UndraPortError(body: error_.undraEncoded())",
        "impl()\n            return []",
    ] {
        assert!(
            builder.contains(expected),
            "missing in the builder:\n{expected}\n\n{builder}"
        );
    }
    // The protocol adapter is what it was: its locals keep their names and only `impl` is reserved.
    let adapter = ports
        .split("public func awkwardPortImpl(_ impl: any Awkward)")
        .nth(1)
        .unwrap();
    let adapter = adapter
        .split("public func awkwardPortImpl(")
        .next()
        .unwrap();
    assert!(
        adapter.contains("let result = impl.args(args: args_)"),
        "{adapter}"
    );
    assert!(adapter.contains("let result = impl.result()"), "{adapter}");
    assert!(
        adapter.contains("} catch let error as AwkwardError {"),
        "{adapter}"
    );
}
