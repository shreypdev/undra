//! The debugger path into Rust (ADR-046 decision 2): a debug build of the core keeps its full
//! DWARF, so LLDB stops at a breakpoint set by Rust file and line, with the toolchain's Rust
//! formatters loaded from the `.lldbinit` `undra init` writes.
//!
//! The playground core is built in debug (`undra build`, no `--release`) and a small C host
//! (`tests/symbols/harness.c`) calls its `explode` function through the C ABI. LLDB runs in batch
//! mode (`lldb -b`), sets `breakpoint set -f lab.rs -l <the line of the panic in explode>`, runs,
//! and the stop must be in that file at that line, in `playground_core::lab::explode`, called from
//! the generated dispatcher; with the formatters the `reason` argument reads `"kaboom"`. Then a `bt`
//! of the full depth, through the core's guard frames (`&Runtime`, closures, `catch_unwind`) down to
//! the host's `main`, must end with LLDB alive: on Xcode 26.6 with the Rust 1.99 formatters it died
//! twice (SIGILL on a type LLDB laid out as containing itself, then an abort when the struct summary
//! followed a pointer cycle), see `.10x/decisions/sde/lldb-formatters-bt.md`.
//!
//! Two processes are debugged: the host one (the macOS dylib, LLDB attached to the process) and, through the
//! prelinked iOS object that the app links (ADR-044), a process in a booted iOS simulator that LLDB
//! attaches to. Tests skip with a message when a toolchain is absent and fail when
//! `UNDRA_REQUIRE_TOOLCHAINS=1`.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use common::{
    StableProject, has_rust_target, has_tool, playground_at, repo_root, run_ok, serial, skip_unless,
};

fn project() -> &'static StableProject {
    static PROJECT: OnceLock<StableProject> = OnceLock::new();
    PROJECT.get_or_init(|| playground_at("debugging"))
}

/// The line of the `panic!` in `explode`.
fn explode_line() -> u32 {
    let text = std::fs::read_to_string(project().root.join("core/src/lab.rs")).unwrap();
    let mut lines = text.lines().enumerate();
    lines
        .find(|(_, l)| l.contains("pub fn explode(reason: String)"))
        .expect("explode");
    let (index, _) = lines
        .find(|(_, l)| l.contains("panic!("))
        .expect("its panic!");
    index as u32 + 1
}

fn harness_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/symbols/harness.c")
}

fn header_dir() -> PathBuf {
    repo_root().join("runtimes/swift/UndraRuntime/Sources/UndraFFI/include")
}

/// The `.lldbinit` `undra init` writes.
fn lldbinit() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/project/lldbinit")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join("target/undra-cli-projects/scratch-debugging")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_text(cmd: &mut Command) -> String {
    let out = cmd.output().expect("the command starts");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "{cmd:?} failed ({}):\n{text}",
        out.status
    );
    text
}

/// Runs a debugger session to its end, at most `seconds`: a debugger that waits for an authorisation
/// nobody can give (macOS asks before one process controls another, and cannot ask without a login
/// session) must fail the test with what it printed, not hang it.
fn run_debugger(cmd: &mut Command, seconds: u64, work: &Path) -> String {
    let log = work.join("debugger.out");
    let file = std::fs::File::create(&log).unwrap();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .expect("the debugger starts");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    match status {
        Some(status) => assert!(status.success(), "{cmd:?} failed ({status}):\n{text}"),
        None => panic!(
            "{cmd:?} did not finish in {seconds} s (a debugger needs Developer Tools access to the process: allow it for this terminal, \
and run the test from a logged-in session, not a detached one):\n{text}"
        ),
    }
    text
}

/// What a batch LLDB session printed when it stopped at the breakpoint in `explode`.
fn assert_stopped_in_explode(output: &str, line: u32) {
    let stop = output
        .find("stop reason = breakpoint 1.1")
        .unwrap_or_else(|| panic!("no stop at the breakpoint:\n{output}"));
    // From the stop on (an attach stops first, in dyld, before the breakpoint is reached).
    let after = &output[stop..];
    let frame0 = after
        .lines()
        .find(|l| l.contains("frame #0:"))
        .unwrap_or_else(|| panic!("no frame #0 after the stop in\n{output}"));
    assert!(
        frame0.contains("playground_core::lab::explode")
            && frame0.contains(&format!("lab.rs:{line}")),
        "the stop is not in explode at lab.rs:{line}: {frame0}"
    );
    // The caller is the generated dispatcher of the core, a few lines above `explode` in the same file.
    assert!(
        after
            .lines()
            .any(|l| l.contains("frame #1:") && l.contains("__undra_dispatch_fn_explode")),
        "{output}"
    );
    // The source line is shown: LLDB read the file the DWARF names.
    assert!(after.contains("panic!(\"{reason}\")"), "{output}");
}

/// What a `bt` of the full depth printed with the formatters of `.lldbinit` loaded: every frame down
/// to the host's `main`, with the summaries of the arguments on the way. The frames between hold a
/// `&Runtime` (the whole object graph, cyclic through `Weak<Runtime>`), closures and `catch_unwind`'s
/// `Data` union: what killed LLDB before the variant rename in `undra-runtime` and the pointer-skipping
/// line of the template. `run_debugger` has already asserted that LLDB exited normally.
fn assert_full_backtrace_with_formatters(output: &str) {
    let stop = output
        .find("stop reason = breakpoint 1.1")
        .unwrap_or_else(|| panic!("no stop at the breakpoint:\n{output}"));
    let after = &output[stop..];
    assert!(
        after.lines().any(|l| l.contains("frame #")
            && l.contains("`main(argc=2, argv=")
            && l.contains("harness.c")),
        "the backtrace did not reach the host's main:\n{output}"
    );
    assert!(
        after
            .lines()
            .any(|l| l.contains("frame #0:") && l.contains("explode(reason=\"kaboom\")")),
        "the formatters did not summarize the arguments:\n{output}"
    );
    // The core's entry is on the way, with a `&Runtime` printed as its address (pointers are skipped), and
    // the `Data` union of `catch_unwind` (frame 4 of the crash) is passed.
    assert!(
        after.lines().any(|l| l.contains("run_dispatcher(self=0x")),
        "{output}"
    );
    assert!(
        after
            .lines()
            .any(|l| l.contains("std::panicking::catch_unwind::do_call") && l.contains("(data=0x")),
        "{output}"
    );
}

#[test]
fn lldb_stops_at_a_rust_line_in_a_debug_core_on_the_host() {
    let _serial = serial();
    if skip_unless(
        cfg!(target_os = "macos"),
        "this test drives the macOS dylib with Xcode's LLDB",
    ) {
        return;
    }
    if skip_unless(
        has_tool("xcrun", "--version"),
        "Xcode's tools (xcrun) are not installed",
    ) {
        return;
    }
    let p = project();
    let line = explode_line();
    run_ok(p.undra().args(["build", "--platform", "host"]));
    let library_dir = p.root.join("build/host");
    assert!(library_dir.join("libplayground_core.dylib").is_file());

    let work = scratch("host");
    let harness = work.join("harness");
    run_text(
        Command::new("xcrun")
            .args(["clang", "-g"])
            .arg(format!("-I{}", header_dir().display()))
            .arg("-DUNDRA_NS=playground_core")
            .arg(harness_source())
            .arg(format!("-L{}", library_dir.display()))
            .args(["-lplayground_core", "-o"])
            .arg(&harness)
            .arg(format!("-Wl,-rpath,{}", library_dir.display())),
    );
    // A method that does not panic runs through the same library: the harness itself works.
    let sum = run_text(Command::new(&harness).arg("add"));
    assert!(sum.contains("sum=42"), "{sum}");

    // The host process stops itself at its start (`UNDRA_HARNESS_WAIT`) and LLDB attaches, as to the
    // simulator process below: `lldb -- harness` (a launch) waits forever on some machines under load
    // for debugserver's own permission checks, which has nothing to do with the core.
    let mut app = Command::new(&harness)
        .arg("explode")
        .env("UNDRA_HARNESS_WAIT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .expect("the harness starts");
    wait_until_stopped(app.id());
    let attach = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_debugger(
            Command::new("xcrun")
                .args(["lldb", "-b"])
                .args(["-o", &format!("command source {}", lldbinit().display())])
                .args(["-o", &format!("process attach -p {}", app.id())])
                .args(["-o", &format!("breakpoint set -f lab.rs -l {line}")])
                .args(["-o", "continue"])
                .args(["-o", "bt"])
                .args(["-o", "frame variable reason"])
                .args(["-o", "detach"]),
            120,
            &work,
        )
    }));
    let _ = app.kill();
    let _ = app.wait();
    let output = attach.unwrap_or_else(|e| std::panic::resume_unwind(e));
    eprintln!("{output}");
    assert!(
        output.contains("Breakpoint 1: ") && !output.contains("no locations (pending)"),
        "the breakpoint set by file and line resolved:\n{output}"
    );
    assert_stopped_in_explode(&output, line);
    assert_full_backtrace_with_formatters(&output);
    // The Rust formatters of `.lldbinit` are loaded: a Rust `String` reads as its text.
    assert!(
        output.contains("reason = \"kaboom\""),
        "the formatters are not loaded:\n{output}"
    );
}

#[test]
fn lldb_stops_at_a_rust_line_in_a_simulator_process_linked_with_the_prelinked_core() {
    let _serial = serial();
    if skip_unless(cfg!(target_os = "macos"), "iOS needs macOS") {
        return;
    }
    if skip_unless(
        has_rust_target("aarch64-apple-ios") && has_rust_target("aarch64-apple-ios-sim"),
        "rustup target add aarch64-apple-ios aarch64-apple-ios-sim",
    ) {
        return;
    }
    if skip_unless(
        has_tool("xcrun", "--version"),
        "Xcode's tools (xcrun) are not installed",
    ) {
        return;
    }
    let Some(udid) = booted_simulator() else {
        skip_unless(
            false,
            "no booted iOS simulator (xcrun simctl boot <device>)",
        );
        return;
    };
    let p = project();
    let line = explode_line();
    // A debug build prelinks with `-all_load` and keeps full DWARF (the debug map names it).
    run_ok(p.undra().args(["build", "--platform", "ios"]));
    let library = p
        .root
        .join("build/ios/PlaygroundCore.xcframework/ios-arm64-simulator/libplayground_core.a");
    let work = scratch("ios");
    let harness = work.join("harness");
    run_text(
        Command::new("xcrun")
            .args([
                "--sdk",
                "iphonesimulator",
                "clang",
                "-arch",
                "arm64",
                "-mios-simulator-version-min=17.0",
                "-g",
            ])
            .arg(format!("-I{}", header_dir().display()))
            .arg("-DUNDRA_NS=playground_core")
            .arg(harness_source())
            .arg(&library)
            .arg("-o")
            .arg(&harness),
    );

    // The app starts in the simulator and waits for a debugger (`simctl spawn -w`); LLDB attaches.
    let log = std::fs::File::create(work.join("app.out")).unwrap();
    let mut app = Command::new("xcrun")
        .args(["simctl", "spawn", "-w", &udid])
        .arg(&harness)
        .arg("explode")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .expect("simctl spawn starts");
    let pattern = format!("{} explode", harness.display());
    let mut pid = None;
    for _ in 0..100 {
        let found = Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
            .unwrap();
        // The process of the app, not `simctl` (whose command line has the same words).
        pid = String::from_utf8_lossy(&found.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .find(|candidate| {
                let comm = Command::new("ps")
                    .args(["-o", "comm=", "-p", &candidate.to_string()])
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&comm.stdout)
                    .trim()
                    .ends_with("harness")
            });
        if pid.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let Some(pid) = pid else {
        let _ = app.kill();
        let _ = app.wait();
        panic!("the app did not start in the simulator");
    };
    let attach = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_debugger(
            Command::new("xcrun")
                .args(["lldb", "-b"])
                .args(["-o", &format!("command source {}", lldbinit().display())])
                .args(["-o", &format!("process attach -p {pid}")])
                .args(["-o", &format!("breakpoint set -f lab.rs -l {line}")])
                .args(["-o", "continue"])
                .args(["-o", "bt"])
                .args(["-o", "detach"]),
            120,
            &work,
        )
    }));
    // The attached process runs on to its end once detached; make sure nothing is left behind.
    let _ = Command::new("kill").arg(pid.to_string()).output();
    let _ = app.kill();
    let _ = app.wait();
    let output = attach.unwrap_or_else(|e| std::panic::resume_unwind(e));
    eprintln!("{output}");
    assert_stopped_in_explode(&output, line);
    assert_full_backtrace_with_formatters(&output);
}

/// Waits until process `pid` is stopped (`SIGSTOP`), which is how the harness waits for a debugger.
fn wait_until_stopped(pid: u32) {
    for _ in 0..100 {
        let state = Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        if state.starts_with('T') {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the harness did not stop itself for the debugger");
}

/// A booted simulator's UDID.
fn booted_simulator() -> Option<String> {
    let out = Command::new("xcrun")
        .args(["simctl", "list", "devices", "booted", "-j"])
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    json["devices"]
        .as_object()?
        .values()
        .flat_map(|runtime| runtime.as_array().cloned().unwrap_or_default())
        .find(|d| d["state"] == "Booted")
        .and_then(|d| d["udid"].as_str().map(ToOwned::to_owned))
}

#[test]
fn the_lldbinit_undra_init_writes_loads_the_formatters_and_says_nothing_without_them() {
    // The file is two `script` lines: the first asks the toolchain on this machine for its formatters (a
    // machine without `rustc` on PATH gets an empty stdout, not an error, and no formatters); the second
    // re-registers the struct and enum summaries of the Rust 1.99 formatters with pointers skipped, and
    // does nothing when they are not loaded or are older than that.
    let text = std::fs::read_to_string(lldbinit()).unwrap();
    let script: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    assert_eq!(script.len(), 2, "{text}");
    for line in &script {
        assert!(line.starts_with("script "), "{line}");
    }
    for needle in [
        "rustc",
        "--print",
        "sysroot",
        "lldb_lookup.py",
        "lldb_commands",
        "os.path.exists",
    ] {
        assert!(script[0].contains(needle), "{needle}: {}", script[0]);
    }
    for needle in [
        "sys.modules.get(\"lldb_lookup\")",
        "register_summary",
        "StructSummaryProvider",
        "\"is_udt\"",
        "ClangEncodedEnumSummaryProvider",
        "\"is_gnu_enum\"",
        "eFormatterMatchCallback",
        "eTypeOptionSkipPointers",
        "hasattr(L, \"register_summary\")",
    ] {
        assert!(script[1].contains(needle), "{needle}: {}", script[1]);
    }
}

/// The crates linked into a core: `undra-ffi` and every workspace crate its `[dependencies]` reach.
fn core_crates() -> Vec<(String, PathBuf)> {
    let crates = repo_root().join("crates");
    let mut todo = vec!["undra-ffi".to_owned()];
    let mut seen: Vec<String> = Vec::new();
    while let Some(name) = todo.pop() {
        if seen.contains(&name) {
            continue;
        }
        let manifest = std::fs::read_to_string(crates.join(&name).join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("{name}/Cargo.toml: {e}"));
        let mut in_deps = false;
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                // `[dependencies]` and `[target.'cfg(..)'.dependencies]`, not dev or build ones.
                in_deps = line.ends_with("dependencies]")
                    && !line.contains("dev-dependencies")
                    && !line.contains("build-dependencies");
                continue;
            }
            // `undra-wire = { workspace = true }` or `undra-wire.workspace = true`.
            if let Some((key, _)) = line.split_once('=')
                && in_deps
                && let Some(name) = key.trim().split(['.', ' ']).next()
                && name.starts_with("undra-")
            {
                todo.push(name.to_owned());
            }
        }
        seen.push(name);
    }
    seen.sort();
    seen.into_iter()
        .map(|name| (name.clone(), crates.join(name)))
        .collect()
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
}

/// The first identifier of `text`, when it starts with one.
fn identifier(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    (end > 0 && !text.starts_with(|c: char| c.is_ascii_digit())).then(|| &text[..end])
}

/// `pub`, `pub(crate)`, `pub(super)`: the visibility before an item, stripped.
fn without_visibility(line: &str) -> &str {
    let rest = line.trim_start();
    match rest.strip_prefix("pub") {
        Some(after) if after.starts_with('(') => after[after.find(')').unwrap() + 1..].trim_start(),
        Some(after) if after.starts_with(char::is_whitespace) => after.trim_start(),
        _ => rest,
    }
}

/// The names of the types a source file defines (`struct`, `enum`, `union`, `type`).
fn defined_types(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let item = without_visibility(line);
            ["struct ", "enum ", "union ", "type "]
                .iter()
                .find_map(|kw| item.strip_prefix(kw))
                .and_then(|rest| identifier(rest.trim_start()))
                .map(ToOwned::to_owned)
        })
        .collect()
}

/// The tuple variants of the enums in `source`: (line number, variant name, the last path segment of
/// the type of its first field).
fn tuple_variants(source: &str) -> Vec<(usize, String, String)> {
    let mut found = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let indent = line.len() - line.trim_start().len();
        let item = without_visibility(line);
        if item.starts_with("enum ") && line.trim_end().ends_with('{') {
            let close = format!("{}}}", " ".repeat(indent));
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_end() != close {
                let body = without_visibility(lines[j]);
                if let Some(variant) = identifier(body)
                    && variant.starts_with(|c: char| c.is_ascii_uppercase())
                    && let Some(fields) = body[variant.len()..].strip_prefix('(')
                {
                    // The type of the first field: `&'a mut path::Type<..>` to `Type`.
                    let first = fields
                        .split([',', ')', '<'])
                        .next()
                        .unwrap()
                        .trim_start_matches(['&', '\''])
                        .split_whitespace()
                        .last()
                        .unwrap_or("");
                    let last = first.rsplit("::").next().unwrap_or(first);
                    if let Some(ty) = identifier(last)
                        && ty.starts_with(|c: char| c.is_ascii_uppercase())
                    {
                        found.push((j + 1, variant.to_owned(), ty.to_owned()));
                    }
                }
                j += 1;
            }
            i = j;
        }
        i += 1;
    }
    found
}

#[test]
fn no_enum_variant_of_the_core_is_named_after_a_type_of_its_own_crate() {
    // LLDB (Xcode 26.6, Rust 1.99 DWARF) takes a variant's struct (`Blocking::Pool`, the tuple variant
    // `Pool(native::Pool)`) for its payload type when the two share a name and a byte size in one
    // compilation unit: the variant then contains itself, the record layout recurses until the stack
    // is gone, and LLDB exits with SIGILL as soon as a value holding it is shown (a backtrace included,
    // since the formatters inspect the pointee of every `&T` argument). A type of another crate
    // (`String(String)`) is parsed first and is not confused. Keep the pattern out of the core.
    let crates = core_crates();
    assert!(
        crates.iter().any(|(name, _)| name == "undra-runtime"),
        "{crates:?}"
    );
    let mut offenders = Vec::new();
    for (name, root) in &crates {
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        let sources: Vec<(PathBuf, String)> = files
            .into_iter()
            .map(|f| {
                let text = std::fs::read_to_string(&f).unwrap();
                (f, text)
            })
            .collect();
        let types: Vec<String> = sources.iter().flat_map(|(_, s)| defined_types(s)).collect();
        for (file, source) in &sources {
            for (line, variant, ty) in tuple_variants(source) {
                if variant == ty && types.contains(&ty) {
                    offenders.push(format!(
                        "{name}/{}:{line}: `{variant}({ty})`",
                        file.strip_prefix(root).unwrap().display()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an enum variant carries a type of its own crate that has the variant's name; Xcode's LLDB takes \
one for the other and dies on a backtrace (docs/ONBOARDING.md, \"Debugging into Rust\"): rename the \
variant or the type:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_variant_scan_reads_rust_the_way_the_core_writes_it() {
    let source = "\
/// Where blocking closures run.
pub(crate) enum Blocking {
    /// Inline.
    Inline,
    /// A pool of worker threads.
    #[cfg(not(target_family = \"wasm\"))]
    Pool(native::Pool),
    Named { pool: Pool },
    Borrowed(&'a mut Pool),
    Text(String),
    Two(Pool, u32),
}
pub struct Pool {
    workers: Vec<Worker>,
}
mod native {
    pub(crate) struct Pool;
}
pub enum Expand { Plain, Instance(Instance) }
";
    assert_eq!(
        defined_types(source),
        ["Blocking", "Pool", "Pool", "Expand"]
    );
    let variants = tuple_variants(source);
    assert_eq!(
        variants,
        [
            (7, "Pool".to_owned(), "Pool".to_owned()),
            (9, "Borrowed".to_owned(), "Pool".to_owned()),
            (10, "Text".to_owned(), "String".to_owned()),
            (11, "Two".to_owned(), "Pool".to_owned()),
        ]
    );
}
