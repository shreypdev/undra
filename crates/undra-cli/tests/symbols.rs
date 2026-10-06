//! Symbol files and `undra symbolicate`, proved on the real playground core (ADR-046, R9).
//!
//! The playground core is built in release for each platform; a small C host (`tests/symbols/harness.c`,
//! or `web.mjs` for the wasm module) loads it through the C ABI (or the wasm exports), registers a
//! `Diagnostics` port, makes the core panic (`explode`, a panic in a call; `explode_detached`, a panic
//! in a task of its own: two different crash sites) and prints the `PanicReport` the core hands the port.
//! The report's addresses are then resolved by `undra symbolicate` with the symbol files the build
//! wrote, and the line it names must be the line the core itself reports as the panic location
//! (and the line of the `panic!` in `core/src/lab.rs`):
//!
//! | Platform | The image | Run | Resolved with |
//! |---|---|---|---|
//! | iOS | the harness linked against the prelinked `lib<ns>.a` (simulator slice) | `simctl spawn` in a booted simulator | `atos`, with the dSYM `dsymutil` makes (as Xcode does) |
//! | Android | the harness against `jniLibs/<abi>/lib<ns>.so` | `adb shell` on the emulator | `llvm-symbolizer`, on the unstripped twin of `build/symbols` |
//! | Web | the shipped wasm module | node | the function map (the debug module has the shipped code) and the DWARF of `<ns>.dwarf.wasm` |
//! | macOS host | the harness against `build/host/lib<ns>.dylib` | natively | `atos`, with the dSYM next to the library |
//!
//! A size test builds the same core with `--no-symbols` (the profile of before ADR-046) and checks the
//! shipped artefacts did not grow: on Android, section by section (`not_the_plain_library`). Tests skip with a message when a toolchain is absent, and fail
//! when `UNDRA_REQUIRE_TOOLCHAINS=1`.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};

use common::{
    StableProject, has_android_ndk, has_rust_target, has_tool, playground_at, repo_root, run_ok,
    serial, skip_unless, toolchains_required,
};

// ---- the projects ------------------------------------------------------------------------------

/// The playground copy every symbol test builds with the default (symbols on).
fn project() -> &'static StableProject {
    static PROJECT: OnceLock<StableProject> = OnceLock::new();
    PROJECT.get_or_init(|| playground_at("symbols"))
}

/// A second copy, built with `--no-symbols`: what shipped before ADR-046, for the size test.
fn plain_project() -> &'static StableProject {
    static PROJECT: OnceLock<StableProject> = OnceLock::new();
    PROJECT.get_or_init(|| playground_at("symbols-plain"))
}

/// Builds `platform` in release once per process (`plain`: with `--no-symbols`).
fn build(platform: &str, plain: bool) {
    static DONE: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let key = format!("{platform}/{plain}");
    let mut done = DONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if done.contains(&key) {
        return;
    }
    let project = if plain { plain_project() } else { project() };
    let mut cmd = project.undra();
    cmd.args(["build", "--platform", platform, "--release"]);
    if plain {
        cmd.arg("--no-symbols");
    }
    run_ok(&mut cmd);
    done.insert(key);
}

fn manifest(project: &StableProject) -> serde_json::Value {
    let path = project.root.join("build/symbols/manifest.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("manifest.json is JSON")
}

/// The manifest entries of one platform.
fn entries(project: &StableProject, platform: &str) -> Vec<serde_json::Value> {
    manifest(project)["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .filter(|e| e["platform"] == platform)
        .cloned()
        .collect()
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .len()
}

/// The size of `path` gzipped at level 9 (the system `gzip`, as the build reports it).
fn gzip_size(path: &Path) -> u64 {
    let out = Command::new("gzip")
        .args(["-9", "-c"])
        .arg(path)
        .output()
        .expect("gzip runs");
    out.stdout.len() as u64
}

/// The SHA-256 of a file, from the system tool.
fn sha256(path: &Path) -> String {
    for (tool, args) in [("shasum", vec!["-a", "256"]), ("sha256sum", vec![])] {
        if let Ok(out) = Command::new(tool).args(args).arg(path).output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
            }
        }
    }
    panic!("neither shasum nor sha256sum runs");
}

fn on_path(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ---- the crash sites ---------------------------------------------------------------------------

/// The 1-based line of `core/src/lab.rs` that holds `needle`, looking after the line `after` when given.
fn lab_line(after: Option<&str>, needle: &str) -> u32 {
    let text = std::fs::read_to_string(project().root.join("core/src/lab.rs")).unwrap();
    let mut lines = text.lines().enumerate();
    if let Some(after) = after {
        lines
            .find(|(_, l)| l.contains(after))
            .expect("the anchor line");
    }
    let (index, _) = lines
        .find(|(_, l)| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle}"));
    index as u32 + 1
}

/// The line of the `panic!` in `explode` (a panic inside a call).
fn explode_line() -> u32 {
    lab_line(Some("pub fn explode(reason: String)"), "panic!(")
}

/// The line of `pub fn explode`: where the function is defined, which is what a wasm frame resolves to.
fn explode_definition_line() -> u32 {
    lab_line(None, "pub fn explode(reason: String)")
}

/// The line of the `panic!` in the task `explode_detached` spawns (a second crash site).
fn detached_line() -> u32 {
    lab_line(
        Some("pub fn explode_detached("),
        "ctx.spawn(async move { panic!",
    )
}

/// What a harness printed.
#[derive(Debug, Default)]
struct Run {
    operation: String,
    namespace: String,
    version: String,
    schema: String,
    image_id: String,
    location: String,
    message: String,
    frames: Vec<String>,
    frame_count: usize,
    status: Option<i32>,
    reports: Option<usize>,
    raw: String,
}

fn parse_run(output: &str) -> Run {
    let mut run = Run {
        raw: output.to_owned(),
        ..Run::default()
    };
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("REPORT ") {
            for pair in rest.split(' ') {
                match pair.split_once('=') {
                    Some(("operation", v)) => run.operation = v.to_owned(),
                    Some(("namespace", v)) => run.namespace = v.to_owned(),
                    Some(("version", v)) => run.version = v.to_owned(),
                    Some(("schema", v)) => run.schema = v.to_owned(),
                    Some(("image", v)) => run.image_id = v.to_owned(),
                    _ => {}
                }
            }
        } else if let Some(rest) = line.strip_prefix("LOCATION ") {
            run.location = rest.to_owned();
        } else if let Some(rest) = line.strip_prefix("MESSAGE ") {
            run.message = rest.to_owned();
        } else if let Some(rest) = line.strip_prefix("FRAME ") {
            run.frames.push(rest.to_owned());
            run.frame_count += 1;
        } else if let Some(rest) = line.strip_prefix("REPLY status=") {
            run.status = rest.split(' ').next().and_then(|s| s.parse().ok());
        } else if let Some(rest) = line.strip_prefix("DONE reports=") {
            run.reports = rest.parse().ok();
        }
    }
    run
}

/// The line number a panic location `file:line:column` names.
fn location_line(location: &str) -> u32 {
    let mut parts = location.rsplitn(3, ':');
    let _column = parts.next();
    parts.next().and_then(|l| l.parse().ok()).unwrap_or(0)
}

/// What the core said about the panic, before anything is resolved: the report is the core's, whole.
fn check_report(run: &Run, operation: &str, line: u32) {
    assert_eq!(run.operation, operation, "{}", run.raw);
    assert_eq!(run.namespace, "playground_core", "{}", run.raw);
    assert_eq!(
        run.version, "0.1.0",
        "the core crate's version: {}",
        run.raw
    );
    assert!(
        run.schema.starts_with("0x") && run.schema.len() == 18,
        "{}",
        run.raw
    );
    assert!(
        run.location.contains("lab.rs:"),
        "the panic location names the file: {}",
        run.raw
    );
    assert_eq!(location_line(&run.location), line, "{}", run.raw);
    assert!(run.frame_count >= 4, "{}", run.raw);
}

/// The report as the apps' `onPanic` produces it, serialised as JSON.
fn report_json(run: &Run) -> String {
    let frames: Vec<serde_json::Value> = run
        .frames
        .iter()
        .map(|a| serde_json::json!({ "address": a, "symbol": null, "file": null, "line": null }))
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "message": run.message,
        "location": run.location,
        "operation": run.operation,
        "namespace": run.namespace,
        "coreVersion": run.version,
        "schemaHash": run.schema,
        "imageId": run.image_id,
        "frames": frames,
    }))
    .unwrap()
}

/// `undra symbolicate` on the report of `run`, with `args`; its stdout.
fn symbolicate(run: &Run, dir: &Path, args: &[&str]) -> String {
    let file = dir.join(format!(
        "report-{}-{}.json",
        run.operation.replace(' ', "-"),
        run.frames.len()
    ));
    std::fs::write(&file, report_json(run)).unwrap();
    let out = run_ok(project().undra().arg("symbolicate").arg(&file).args(args));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The frame of `function` resolved to `core/src/lab.rs:<line>`: the file, the line the core itself
/// gave as the panic location and the line of the `panic!` in the source all agree.
fn assert_resolved(resolved: &str, function: &str, line: u32) {
    let wanted = format!("lab.rs:{line})");
    let found = resolved
        .lines()
        .find(|l| l.contains(function) && l.contains(&wanted))
        .unwrap_or_else(|| {
            panic!("no frame of `{function}` at lab.rs:{line} in\n{resolved}");
        });
    assert!(found.contains("core/src/lab.rs"), "{found}");
    // Release builds name the project `/undra/app` (ADR-052): the path is the same in any checkout and
    // names no machine, whatever the project's directory is below.
    assert!(
        found.contains("(/undra/app/core/src/lab.rs")
            && !found.contains(&project().root.display().to_string()),
        "the project's path is remapped to /undra/app: {found}"
    );
    if let Some(home) = std::env::var_os("HOME").filter(|h| h.len() > 1) {
        assert!(
            !found.contains(&*home.to_string_lossy()),
            "the home directory is not in a frame: {found}"
        );
    }
}

/// A frame of one of `functions` placed in `core/src/lab.rs` within `lines` (a wasm frame is placed where its
/// function is defined: for `explode` that is the attribute line above it or the `pub fn` line, the
/// optimised module having inlined `explode` into the dispatcher the attribute generates).
fn assert_defined_in_lab(resolved: &str, functions: &[&str], lines: std::ops::RangeInclusive<u32>) {
    let found = resolved.lines().find(|l| {
        functions.iter().any(|f| l.contains(f))
            && lines
                .clone()
                .any(|line| l.contains(&format!("core/src/lab.rs:{line})")))
    });
    assert!(
        found.is_some(),
        "no frame of {functions:?} defined at lab.rs:{lines:?} in\n{resolved}"
    );
}

/// The names of the custom sections of a wasm module, in order.
fn custom_sections(bytes: &[u8]) -> Vec<String> {
    walk_sections(bytes)
        .into_iter()
        .filter_map(|(id, name, _)| (id == 0).then_some(name))
        .collect()
}

/// The module without its custom sections: every byte of the others, as they are.
fn without_custom_sections(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes[..8].to_vec();
    for (id, _, whole) in walk_sections(bytes) {
        if id != 0 {
            out.extend_from_slice(&whole);
        }
    }
    out
}

/// The sections of a wasm module: id, the name of a custom one, the bytes it spans.
fn walk_sections(bytes: &[u8]) -> Vec<(u8, String, Vec<u8>)> {
    fn leb(bytes: &[u8], mut at: usize) -> (usize, usize) {
        let (mut value, mut shift) = (0_usize, 0);
        loop {
            let byte = bytes[at];
            at += 1;
            value |= usize::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return (value, at);
            }
        }
    }
    let mut out = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let start = at;
        let id = bytes[at];
        let (size, contents) = leb(bytes, at + 1);
        let end = contents + size;
        let name = if id == 0 {
            let (len, from) = leb(bytes, contents);
            String::from_utf8_lossy(&bytes[from..from + len]).into_owned()
        } else {
            String::new()
        };
        out.push((id, name, bytes[start..end].to_vec()));
        at = end;
    }
    out
}

fn temp(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join("target/undra-cli-projects/scratch")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn harness_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/symbols/harness.c")
}

/// The directory of `undra.h`, which the harness includes.
fn header_dir() -> PathBuf {
    repo_root().join("runtimes/swift/UndraRuntime/Sources/UndraFFI/include")
}

fn run_capture(cmd: &mut Command) -> String {
    let out: Output = cmd.output().expect("the command starts");
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---- iOS ---------------------------------------------------------------------------------------

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

/// The UUID of a Mach-O file or dSYM, as the lowercase hex of an image id.
fn uuid_of(path: &Path) -> String {
    let out = run_capture(
        Command::new("xcrun")
            .args(["dwarfdump", "--uuid"])
            .arg(path),
    );
    out.lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .expect("dwarfdump prints a UUID")
        .replace('-', "")
        .to_lowercase()
}

#[test]
fn ios_frames_resolve_to_file_and_line_through_the_apps_dsym() {
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
    build("ios", false);

    // The manifest has one entry per slice; an iOS image is the app, whose identity is the app's UUID.
    let ios = entries(p, "ios");
    assert_eq!(ios.len(), 2, "{ios:?}");
    for entry in &ios {
        assert_eq!(entry["format"], "macho");
        assert!(entry["imageId"].is_null(), "{entry}");
        assert!(
            entry["symbols"].is_null(),
            "the symbols are the app's dSYM: {entry}"
        );
        assert_eq!(entry["namespace"], "playground_core");
        assert_eq!(entry["coreVersion"], "0.1.0");
        assert!(entry["schemaHash"].as_str().unwrap().starts_with("0x"));
        let shipped = p
            .root
            .join("build")
            .join(entry["shipped"].as_str().unwrap());
        assert_eq!(sha256(&shipped), entry["sha256"].as_str().unwrap());
        assert_eq!(size(&shipped), entry["shippedBytes"].as_u64().unwrap());
    }

    // The app, linked the way Xcode links it (nothing but the library), and its dSYM, made the way
    // Xcode makes it (`dsymutil` over the linked binary: the debug map names the DWARF).
    let work = temp("ios");
    let library = p
        .root
        .join("build/ios/PlaygroundCore.xcframework/ios-arm64-simulator/libplayground_core.a");
    let app = work.join("harness");
    run_capture(
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
            .arg(&app),
    );
    let dsym = work.join("harness.dSYM");
    let _ = Command::new("xcrun")
        .arg("dsymutil")
        .arg(&app)
        .arg("-o")
        .arg(&dsym)
        .output();
    assert!(dsym.is_dir(), "dsymutil made no dSYM");
    let app_uuid = uuid_of(&app);
    assert_eq!(uuid_of(&dsym), app_uuid, "the dSYM is the app's");

    // A panic in a call.
    let simulate = |scenario: &str| {
        parse_run(&run_capture(
            Command::new("xcrun")
                .args(["simctl", "spawn", &udid])
                .arg(&app)
                .arg(scenario),
        ))
    };
    let run = simulate("explode");
    check_report(&run, "explode", explode_line());
    assert_eq!(
        run.status,
        Some(2),
        "the call fails with status 2 as before: {}",
        run.raw
    );
    assert_eq!(run.reports, Some(1), "{}", run.raw);
    assert_eq!(
        run.image_id, app_uuid,
        "the image the report names is the app: its LC_UUID, 32 hex digits"
    );
    let resolved = symbolicate(&run, &work, &["--dsym", dsym.to_str().unwrap()]);
    eprintln!("ios explode:\n{resolved}");
    assert_eq!(resolved.lines().count(), run.frame_count);
    assert_resolved(&resolved, "playground_core::lab::explode", explode_line());

    // A panic in a task of its own: another crash site, another operation.
    let run = simulate("detached");
    check_report(&run, "task", detached_line());
    let resolved = symbolicate(&run, &work, &["--dsym", dsym.to_str().unwrap()]);
    eprintln!("ios detached:\n{resolved}");
    assert!(
        resolved
            .lines()
            .any(|l| l.contains(&format!("lab.rs:{})", detached_line()))),
        "the closure of explode_detached at lab.rs:{}:\n{resolved}",
        detached_line()
    );

    // A dSYM of another build is refused with the reason, not resolved to nonsense.
    let other = work.join("other.dSYM");
    std::fs::create_dir_all(&other).unwrap();
    let report = work.join("report-explode-wrong.json");
    let mut wrong = parse_run(&run.raw);
    wrong.image_id = "00".repeat(16);
    wrong.frames = run.frames.clone();
    std::fs::write(&report, report_json(&wrong)).unwrap();
    let (code, stderr) = common::run_err(
        project()
            .undra()
            .arg("symbolicate")
            .arg(&report)
            .args(["--dsym", dsym.to_str().unwrap()]),
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("another build of the app"), "{stderr}");
}

// ---- Android -----------------------------------------------------------------------------------

/// What the Android test needs: `adb`, the NDK's clang for the device's ABI, an emulator that is online.
struct Device {
    adb: PathBuf,
    serial: String,
    abi: String,
    clang: PathBuf,
    /// The NDK's `llvm-readelf`, for the sections of a library.
    readelf: PathBuf,
}

/// One section of an ELF file, as `llvm-readelf -S -W` lists it.
#[derive(Debug)]
struct ElfSection {
    name: String,
    /// `PROGBITS`, `NOBITS`, `STRTAB`, ...
    kind: String,
    size: u64,
}

/// The sections of the code and what points at it: what `debug = "line-tables-only"` lets LLVM lay out
/// differently. The relocations and the relro data (`.rela.dyn`, `.data.rel.ro`) are here because they are
/// the vtables and function pointers of the code: at a size-optimising profile (`release-mobile`, ADR-052's
/// amendment of 2026-10-02) LLVM folds identical functions (`RawVec::grow_one` of one type and another) a
/// little differently with line tables than without, so a few of them are one function in one library and two
/// in the other: measured 4 relocations of about 5,100 and 48 bytes of 98,000 of `.data.rel.ro`, beside 216 bytes
/// of 1.74 MB of `.text`. At `opt-level = 3`, the profile this test was written for, they were equal.
const CODE_SECTIONS: [&str; 6] = [
    ".text",
    ".eh_frame",
    ".eh_frame_hdr",
    ".gcc_except_table",
    ".rela.dyn",
    ".data.rel.ro",
];

/// Why the Android library `with` (built with symbols, ADR-046, then stripped by `undra build`)
/// is not the library `plain` (built with `--no-symbols`: `debug = false`, `strip = "symbols"`,
/// what shipped before) without its symbols; `None` when it is. What keeping the symbols must not
/// do to what ships, and what each comparison is:
///
/// * add a section: the same sections, in the same order, of the same kinds (a `.debug_*`,
///   `.symtab` or `.strtab` left behind would be one), compared exactly;
/// * change the data: every section that holds bytes in the file and is neither code nor what points at
///   it (`.rodata`, `.data`, the dynamic symbols, the notes) has the same size, exactly; `.shstrtab`,
///   which `llvm-strip` writes anew, is not larger;
/// * change the code: the code, its unwind tables and the relocations and relro data that point at it
///   ([`CODE_SECTIONS`]) differ by at most a thousandth. Line tables do not change what is compiled,
///   but LLVM's output with them is not byte-identical (a function's alignment here and there:
///   measured 0 or 16 bytes over 2.1 MB of `.text`, either way, on this branch and before ADR-058;
///   at `opt-level = "s"` also which of several identical functions are folded: 216 bytes of
///   `.text` over 1.74 MB, 4 relocations). A thousandth is two orders above the first and one above
///   the second, and two below what a change in what is compiled costs (a dependency, an
///   opt-level, an inlining policy).
///
/// The file sizes are not compared: they move with that noise, and an exact `<=` on them failed
/// on code that kept the symbols correctly.
fn not_the_plain_library(plain: &[ElfSection], with: &[ElfSection]) -> Option<String> {
    let layout = |sections: &[ElfSection]| -> Vec<(String, String)> {
        sections
            .iter()
            .map(|s| (s.name.clone(), s.kind.clone()))
            .collect()
    };
    if layout(plain) != layout(with) {
        return Some(format!(
            "has other sections than the --no-symbols build: {:?} against {:?}",
            layout(with),
            layout(plain)
        ));
    }
    let (mut code_plain, mut code_with) = (0_u64, 0_u64);
    for (p, w) in plain.iter().zip(with) {
        if CODE_SECTIONS.contains(&p.name.as_str()) {
            code_plain += p.size;
            code_with += w.size;
        } else if p.name == ".shstrtab" {
            if w.size > p.size {
                return Some(format!(
                    "has a larger section name table: {} bytes against {}",
                    w.size, p.size
                ));
            }
        } else if p.kind != "NOBITS" && p.size != w.size {
            return Some(format!(
                "has `{}` of {} bytes, and the --no-symbols build {}",
                p.name, w.size, p.size
            ));
        }
    }
    if code_with.abs_diff(code_plain) > code_plain / 1000 {
        return Some(format!(
            "has {code_with} bytes of code and unwind tables, and the --no-symbols build {code_plain}"
        ));
    }
    None
}

impl Device {
    /// The sections of an ELF file, in order, without the null section.
    fn section_table(&self, library: &Path) -> Vec<ElfSection> {
        let text = run_capture(Command::new(&self.readelf).args(["-S", "-W"]).arg(library));
        text.lines()
            .filter_map(|l| {
                l.trim_start()
                    .strip_prefix('[')
                    .and_then(|l| l.split_once(']'))
            })
            .filter_map(|(_, rest)| {
                // Name Type Address Off Size ES [Flg] Lk Inf Al
                let fields: Vec<&str> = rest.split_whitespace().collect();
                let (name, kind, size) = (fields.first()?, fields.get(1)?, fields.get(4)?);
                (*name != "NULL" && *name != "Name").then(|| ElfSection {
                    name: (*name).to_owned(),
                    kind: (*kind).to_owned(),
                    size: u64::from_str_radix(size, 16)
                        .unwrap_or_else(|e| panic!("{}: size {size}: {e}", library.display())),
                })
            })
            .collect()
    }

    /// The names of the sections of an ELF file.
    fn sections(&self, library: &Path) -> Vec<String> {
        let text = run_capture(Command::new(&self.readelf).arg("-S").arg(library));
        text.lines()
            .filter_map(|l| l.split_once(']').map(|(_, rest)| rest))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(ToOwned::to_owned)
            .collect()
    }
}

fn android_device() -> Option<Device> {
    let sdk = std::env::var_os("ANDROID_HOME")
        .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .or_else(|| {
            let fallback = PathBuf::from("/opt/homebrew/share/android-commandlinetools");
            fallback.is_dir().then_some(fallback)
        })?;
    let adb = {
        let in_sdk = sdk.join("platform-tools/adb");
        if in_sdk.is_file() {
            in_sdk
        } else if on_path("adb") {
            PathBuf::from("adb")
        } else {
            return None;
        }
    };
    let devices = Command::new(&adb).arg("devices").output().ok()?;
    let listing = String::from_utf8_lossy(&devices.stdout).into_owned();
    let online: Vec<&str> = listing
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, state)| state.trim() == "device")
        .map(|(serial, _)| serial)
        .collect();
    // The shared `undra` AVD is emulator-5554: used, never killed or wiped.
    let serial = online
        .iter()
        .find(|s| **s == "emulator-5554")
        .or_else(|| online.iter().find(|s| s.starts_with("emulator-")))?
        .to_string();
    let abi = String::from_utf8_lossy(
        &Command::new(&adb)
            .args(["-s", &serial, "shell", "getprop", "ro.product.cpu.abi"])
            .output()
            .ok()?
            .stdout,
    )
    .trim()
    .to_owned();
    let ndk = std::env::var_os("ANDROID_NDK_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            let mut versions: Vec<PathBuf> = std::fs::read_dir(sdk.join("ndk"))
                .ok()?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .collect();
            versions.sort();
            versions.pop()
        })?;
    let prebuilt = std::fs::read_dir(ndk.join("toolchains/llvm/prebuilt"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .next()?;
    let triple = match abi.as_str() {
        "arm64-v8a" => "aarch64-linux-android26",
        "x86_64" => "x86_64-linux-android26",
        _ => return None,
    };
    let clang = prebuilt.join("bin").join(format!("{triple}-clang"));
    let readelf = prebuilt.join("bin/llvm-readelf");
    (clang.is_file() && readelf.is_file()).then_some(Device {
        adb,
        serial,
        abi,
        clang,
        readelf,
    })
}

#[test]
fn android_frames_resolve_with_the_unstripped_twin_and_the_play_archive_has_every_abi() {
    let _serial = serial();
    if skip_unless(
        has_android_ndk(),
        "install the NDK: sdkmanager \"ndk;27.2.12479018\"",
    ) {
        return;
    }
    if skip_unless(
        has_rust_target("aarch64-linux-android") && has_rust_target("x86_64-linux-android"),
        "rustup target add aarch64-linux-android x86_64-linux-android",
    ) {
        return;
    }
    let Some(device) = android_device() else {
        skip_unless(
            false,
            "no Android emulator online with the NDK (the shared `undra` AVD, emulator-5554)",
        );
        return;
    };
    let p = project();
    build("android", false);

    // Per ABI: a stripped library ships, its unstripped twin is a symbol file, both with one build id.
    let android = entries(p, "android");
    assert_eq!(android.len(), 2, "{android:?}");
    for entry in &android {
        let abi = entry["arch"].as_str().unwrap();
        assert_eq!(entry["format"], "elf");
        let id = entry["imageId"].as_str().expect("an ELF build id");
        assert!(
            id.len() >= 16 && id.chars().all(|c| c.is_ascii_hexdigit()),
            "{id}"
        );
        let shipped = p
            .root
            .join("build")
            .join(entry["shipped"].as_str().unwrap());
        assert_eq!(
            shipped,
            p.root
                .join(format!("build/android/jniLibs/{abi}/libplayground_core.so"))
        );
        let twin = p
            .root
            .join("build/symbols")
            .join(entry["symbols"].as_str().unwrap());
        assert_eq!(
            twin,
            p.root
                .join(format!("build/symbols/android/{abi}/libplayground_core.so"))
        );
        assert!(
            size(&twin) > size(&shipped) * 3,
            "the twin carries the debug info: {abi}"
        );
        // Stripped: no symbol table and no DWARF. The twin has both.
        let shipped_sections = device.sections(&shipped);
        let twin_sections = device.sections(&twin);
        assert!(
            !shipped_sections
                .iter()
                .any(|s| s == ".symtab" || s.starts_with(".debug")),
            "the shipped library is not stripped: {shipped_sections:?}"
        );
        assert!(
            shipped_sections.iter().any(|s| s == ".note.gnu.build-id")
                && shipped_sections.iter().any(|s| s == ".dynsym"),
            "the shipped library keeps its build id and what the loader needs: {shipped_sections:?}"
        );
        assert!(
            twin_sections.iter().any(|s| s == ".debug_line")
                && twin_sections.iter().any(|s| s == ".symtab"),
            "the twin has line tables and symbols: {twin_sections:?}"
        );
        assert_eq!(sha256(&shipped), entry["sha256"].as_str().unwrap());
    }
    // The Play Console's layout: `<abi>/lib<ns>.so`, one directory per ABI.
    let archive = p
        .root
        .join("build/symbols/android/native-debug-symbols.zip");
    assert!(archive.is_file(), "no {}", archive.display());
    if on_path("unzip") {
        let listing = run_capture(Command::new("unzip").args(["-Z1"]).arg(&archive));
        let names: BTreeSet<&str> = listing.lines().collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "arm64-v8a/libplayground_core.so",
                "x86_64/libplayground_core.so"
            ])
        );
        run_capture(Command::new("unzip").args(["-tq"]).arg(&archive));
    }

    // The harness for the emulator's ABI, run on the device with the library that ships.
    let work = temp("android");
    let library_dir = p.root.join("build/android/jniLibs").join(&device.abi);
    let harness = work.join("harness");
    run_capture(
        Command::new(&device.clang)
            .arg("-g")
            .arg(format!("-I{}", header_dir().display()))
            .arg("-DUNDRA_NS=playground_core")
            .arg(harness_source())
            .arg(format!("-L{}", library_dir.display()))
            .args(["-lplayground_core", "-ldl", "-o"])
            .arg(&harness),
    );
    let remote = format!("/data/local/tmp/undra-symbols-test-{}", std::process::id());
    let adb = |args: &[&str]| {
        let mut cmd = Command::new(&device.adb);
        cmd.args(["-s", &device.serial]).args(args);
        run_capture(&mut cmd)
    };
    adb(&["shell", "mkdir", "-p", &remote]);
    adb(&[
        "push",
        harness.to_str().unwrap(),
        &format!("{remote}/harness"),
    ]);
    adb(&[
        "push",
        library_dir.join("libplayground_core.so").to_str().unwrap(),
        &format!("{remote}/libplayground_core.so"),
    ]);
    let on_device = |scenario: &str| {
        parse_run(&adb(&[
            "shell",
            &format!("cd {remote} && LD_LIBRARY_PATH=. ./harness {scenario}"),
        ]))
    };
    let explode = on_device("explode");
    let detached = on_device("detached");
    adb(&["shell", "rm", "-rf", &remote]);

    check_report(&explode, "explode", explode_line());
    assert_eq!(explode.status, Some(2), "{}", explode.raw);
    check_report(&detached, "task", detached_line());
    // The report names the library by its build id: the one the manifest has for this ABI.
    let manifest_id = android
        .iter()
        .find(|e| e["arch"] == device.abi.as_str())
        .and_then(|e| e["imageId"].as_str())
        .unwrap();
    assert_eq!(explode.image_id, manifest_id, "{}", explode.raw);
    assert_eq!(detached.image_id, manifest_id);

    // Found by the image id (the manifest has two ABIs): no `--platform`, no `--symbols`.
    let resolved = symbolicate(&explode, &work, &[]);
    eprintln!("android explode:\n{resolved}");
    assert_eq!(resolved.lines().count(), explode.frame_count);
    assert_resolved(&resolved, "playground_core::lab::explode", explode_line());
    let resolved = symbolicate(&detached, &work, &[]);
    eprintln!("android detached:\n{resolved}");
    assert!(
        resolved
            .lines()
            .any(|l| l.contains(&format!("lab.rs:{})", detached_line()))),
        "the closure of explode_detached:\n{resolved}"
    );
}

// ---- web ---------------------------------------------------------------------------------------

#[test]
fn web_frames_resolve_through_the_function_map_and_the_dwarf_module() {
    let _serial = serial();
    if skip_unless(
        has_rust_target("wasm32-unknown-unknown"),
        "rustup target add wasm32-unknown-unknown",
    ) {
        return;
    }
    if skip_unless(has_tool("node", "--version"), "node is not installed") {
        return;
    }
    if skip_unless(
        has_tool("wasm-opt", "--version"),
        "wasm-opt (binaryen) is not installed: the debug module and the function map come from it",
    ) {
        return;
    }
    let p = project();
    build("web", false);
    let shipped = p.root.join("build/web/playground_core.wasm");
    let debug = p.root.join("build/symbols/web/playground_core.debug.wasm");
    let dwarf = p.root.join("build/symbols/web/playground_core.dwarf.wasm");
    let map = p
        .root
        .join("build/symbols/web/playground_core.wasm.functions.txt");
    for file in [&shipped, &debug, &dwarf, &map] {
        assert!(file.is_file(), "no {}", file.display());
    }

    let web = entries(p, "web");
    assert_eq!(web.len(), 1);
    let entry = &web[0];
    assert_eq!(entry["format"], "wasm");
    assert_eq!(entry["arch"], "wasm32");
    let hash = sha256(&shipped);
    assert_eq!(
        entry["imageId"],
        hash.as_str(),
        "a wasm image is named by its SHA-256"
    );
    assert_eq!(entry["sha256"], hash.as_str());
    assert_eq!(entry["symbols"], "web/playground_core.debug.wasm");
    assert_eq!(
        entry["functionMap"],
        "web/playground_core.wasm.functions.txt"
    );
    assert_eq!(entry["dwarf"], "web/playground_core.dwarf.wasm");
    assert_eq!(entry["coreVersion"], "0.1.0");

    // The shipped module carries no names and no DWARF. The debug module is the shipped module with
    // its name section, and nothing else (the same code, which the frames below prove by agreeing);
    // the DWARF module has the line tables.
    let sections = |path: &Path| custom_sections(&std::fs::read(path).unwrap());
    let shipped_sections = sections(&shipped);
    assert!(
        !shipped_sections
            .iter()
            .any(|s| s == "name" || s == "producers" || s.starts_with(".debug")),
        "the shipped module keeps debug sections: {shipped_sections:?}"
    );
    let debug_sections = sections(&debug);
    assert!(
        debug_sections.iter().any(|s| s == "name"),
        "{debug_sections:?}"
    );
    assert!(
        !debug_sections.iter().any(|s| s.starts_with(".debug")),
        "the debug module has the shipped code and no DWARF: {debug_sections:?}"
    );
    let dwarf_sections = sections(&dwarf);
    assert!(
        dwarf_sections.iter().any(|s| s == ".debug_line")
            && dwarf_sections.iter().any(|s| s == "name"),
        "{dwarf_sections:?}"
    );
    // Everything but the custom sections is the same bytes in the shipped and the debug module: the
    // same code at the same offsets (what a stack trace's offsets are), the only difference being
    // the names.
    assert_eq!(
        without_custom_sections(&std::fs::read(&debug).unwrap()),
        without_custom_sections(&std::fs::read(&shipped).unwrap()),
        "the shipped module is the debug module without its names"
    );
    assert!(size(&debug) > size(&shipped));
    // The function map: `index:name` per function.
    let map_text = std::fs::read_to_string(&map).unwrap();
    assert!(
        map_text
            .lines()
            .any(|l| l.contains("playground_core") && l.contains("explode")),
        "{}",
        &map_text[..200.min(map_text.len())]
    );

    // The module under node, trapping inside the core: V8's `wasm-function[i]:0x…` positions.
    let work = temp("web");
    let node_script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/symbols/web.mjs");
    let trap = |scenario: &str| {
        let out = run_capture(
            Command::new("node")
                .arg(&node_script)
                .arg(&shipped)
                .arg(scenario),
        );
        let mut run = parse_run(&out);
        // The core logs the panic (level 5) before it traps: the message, then `at file:line:col`
        // and `in operation` on lines of their own (ADR-046).
        let fatal = out
            .lines()
            .find_map(|l| l.strip_prefix("FATAL "))
            .expect("a FATAL record");
        let parts: Vec<&str> = fatal.split("\\n").collect();
        run.message = parts[0].to_owned();
        run.location = parts[1].trim().trim_start_matches("at ").to_owned();
        run.operation = parts
            .get(2)
            .map_or("task", |s| s.trim().trim_start_matches("in "))
            .to_owned();
        run.namespace = "playground_core".to_owned();
        run.version = "0.1.0".to_owned();
        run.schema = entry["schemaHash"].as_str().unwrap().to_owned();
        run.image_id = hash.clone();
        run.frame_count = run.frames.len();
        run
    };

    let explode = trap("explode");
    assert!(explode.frames.len() >= 4, "{}", explode.raw);
    assert_eq!(explode.operation, "explode");
    assert_eq!(location_line(&explode.location), explode_line());
    let resolved = symbolicate(&explode, &work, &[]);
    eprintln!("web explode:\n{resolved}");
    assert_eq!(resolved.lines().count(), explode.frames.len());
    // The frame is the function that panicked, named exactly (the map has the shipped module's
    // functions) and placed where it is defined: `lab.rs`, at the attribute or the `pub fn` line of
    // `explode` (the shipped code has `explode` inlined into the dispatcher the attribute generates).
    // The line of the `panic!` inside it, 222, is the report's `location`, which the core logged.
    let defined = explode_definition_line();
    assert_defined_in_lab(
        &resolved,
        &[
            "playground_core::lab::explode",
            "playground_core::lab::__undra_dispatch_fn_explode",
        ],
        defined - 1..=defined,
    );

    let detached = trap("detached");
    assert_eq!(location_line(&detached.location), detached_line());
    let resolved = symbolicate(&detached, &work, &[]);
    eprintln!("web detached:\n{resolved}");
    assert!(
        resolved
            .lines()
            .any(|l| l.contains("playground_core::lab::explode_detached") && l.contains("lab.rs:")),
        "the closure of explode_detached:\n{resolved}"
    );

    // Bare V8 positions work too, and a frame is found by its module offset when only that is given.
    let first = &explode.frames[0];
    let out = run_ok(
        p.undra()
            .args(["symbolicate", "--platform", "web", "--image-id", &hash])
            .arg(first),
    );
    assert!(String::from_utf8_lossy(&out.stdout).lines().count() == 1);
    let offset = first.split(':').nth(1).unwrap();
    let out = run_ok(p.undra().args(["symbolicate", "--platform", "web", offset]));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("panic_with_hook"),
        "the function at that offset, found from the module's code ranges: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---- the macOS host ----------------------------------------------------------------------------

#[test]
fn host_frames_resolve_with_the_dsym_next_to_the_library() {
    let _serial = serial();
    if skip_unless(
        cfg!(target_os = "macos"),
        "the dSYM of a host library is made on macOS",
    ) {
        return;
    }
    let p = project();
    build("host", false);
    let library = p.root.join("build/host/libplayground_core.dylib");
    let dsym = p.root.join("build/host/libplayground_core.dylib.dSYM");
    assert!(
        library.is_file() && dsym.is_dir(),
        "the library and its dSYM"
    );
    let host = entries(p, "host");
    assert_eq!(host.len(), 1);
    let entry = &host[0];
    assert_eq!(entry["format"], "macho");
    assert_eq!(entry["symbols"], "../host/libplayground_core.dylib.dSYM");
    let uuid = uuid_of(&library);
    assert_eq!(
        entry["imageId"],
        uuid.as_str(),
        "the manifest names the library by its LC_UUID"
    );
    assert_eq!(
        uuid_of(&dsym),
        uuid,
        "the dSYM is the library's, which was stripped after it was made"
    );

    // The stripped library still works: a harness loads it through its C ABI and makes it panic.
    let work = temp("host");
    let app = work.join("harness");
    run_capture(
        Command::new("xcrun")
            .args(["clang", "-g"])
            .arg(format!("-I{}", header_dir().display()))
            .arg("-DUNDRA_NS=playground_core")
            .arg(harness_source())
            .arg(format!("-L{}", library.parent().unwrap().display()))
            .args(["-lplayground_core", "-o"])
            .arg(&app)
            .arg(format!(
                "-Wl,-rpath,{}",
                library.parent().unwrap().display()
            )),
    );
    let run = parse_run(&run_capture(Command::new(&app).arg("explode")));
    check_report(&run, "explode", explode_line());
    assert_eq!(
        run.image_id, uuid,
        "the image holding the core is the dylib: {}",
        run.raw
    );
    let resolved = symbolicate(&run, &work, &[]);
    eprintln!("host explode:\n{resolved}");
    assert_resolved(&resolved, "playground_core::lab::explode", explode_line());
}

// ---- sizes -------------------------------------------------------------------------------------

#[test]
fn shipped_artefacts_do_not_grow_and_no_symbols_writes_none() {
    let _serial = serial();
    let mut compared = 0;
    let plain = plain_project();
    let with = project();
    let before_after = |what: &str, before: u64, after: u64| {
        eprintln!("SIZE {what}: before={before} after={after}");
        assert!(
            after <= before,
            "{what} grew: {before} bytes without symbols, {after} with"
        );
    };

    if has_rust_target("wasm32-unknown-unknown") && has_tool("wasm-opt", "--version") {
        build("web", false);
        build("web", true);
        let a = with.root.join("build/web/playground_core.wasm");
        let b = plain.root.join("build/web/playground_core.wasm");
        // `wasm-opt` breaks ties between functions by name, so the run that keeps the names (it makes the
        // function map the shipped module's frames resolve with) orders them differently from the nameless
        // one, and its module has about twenty functions more (3,098 against 3,079 on main at `a309e9f`,
        // 3,072 against 3,052 after wt/cold-restore-regression; rustc 1.99.0, binaryen 133; why the run with
        // names is left with more is not known). That is 0.22% of the playground's module on main and 0.26%
        // after that piece, which changed no build step and no symbol code: one function more or less is
        // 0.04%. Half a percent is the margin (it was a quarter, set when the playground measured smaller
        // with names); the names left in the shipped module would be 38%, the line tables several times it.
        let margin = |bytes: u64| bytes / 200;
        before_after("web wasm", size(&b) + margin(size(&b)), size(&a));
        before_after(
            "web wasm gzip",
            gzip_size(&b) + margin(gzip_size(&b)),
            gzip_size(&a),
        );
        assert!(
            !plain.root.join("build/symbols").exists(),
            "--no-symbols writes no symbol files"
        );
        compared += 1;
    }
    if has_android_ndk()
        && has_rust_target("aarch64-linux-android")
        && has_rust_target("x86_64-linux-android")
        && android_device().is_some()
    {
        build("android", false);
        build("android", true);
        let device = android_device().expect("checked above");
        for abi in ["arm64-v8a", "x86_64"] {
            let rel = format!("build/android/jniLibs/{abi}/libplayground_core.so");
            let (before, after) = (plain.root.join(&rel), with.root.join(&rel));
            // Informational: the files differ by the code generation of line tables (below).
            eprintln!(
                "SIZE android {abi}: before={} after={}",
                size(&before),
                size(&after)
            );
            let plain_sections = device.section_table(&before);
            if let Some(why) = not_the_plain_library(&plain_sections, &device.section_table(&after))
            {
                panic!("android {abi}: the library that ships {why}");
            }
            // The check can tell: the unstripped twin, which keeps the symbols, is not it.
            let twin = with
                .root
                .join(format!("build/symbols/android/{abi}/libplayground_core.so"));
            assert!(
                not_the_plain_library(&plain_sections, &device.section_table(&twin)).is_some(),
                "android {abi}: the unstripped twin passes for the stripped library"
            );
        }
        assert!(!plain.root.join("build/symbols/android").exists());
        compared += 1;
    }
    if cfg!(target_os = "macos")
        && has_rust_target("aarch64-apple-ios")
        && has_rust_target("aarch64-apple-ios-sim")
        && has_tool("xcrun", "--version")
    {
        build("ios", false);
        build("ios", true);
        for slice in ["ios-arm64", "ios-arm64-simulator"] {
            let rel = format!("build/ios/PlaygroundCore.xcframework/{slice}/libplayground_core.a");
            // Informational: the archive carries the debug map and its object paths, which differ
            // by a few hundred bytes either way; what ships is the linked app.
            eprintln!(
                "SIZE ios {slice} archive: without symbols={} with={}",
                size(&plain.root.join(&rel)),
                size(&with.root.join(&rel))
            );
        }
        // The app linked against each library and stripped the way a Release build strips it
        // (`strip -S -x`): the same code, so the same size.
        let work = temp("ios-size");
        let linked = |project: &StableProject, tag: &str| -> u64 {
            let library = project.root.join(
                "build/ios/PlaygroundCore.xcframework/ios-arm64-simulator/libplayground_core.a",
            );
            let app = work.join(format!("app-{tag}"));
            run_capture(
                Command::new("xcrun")
                    .args([
                        "--sdk",
                        "iphonesimulator",
                        "clang",
                        "-arch",
                        "arm64",
                        "-mios-simulator-version-min=17.0",
                    ])
                    .arg(format!("-I{}", header_dir().display()))
                    .arg("-DUNDRA_NS=playground_core")
                    .arg(harness_source())
                    .arg(&library)
                    .arg("-o")
                    .arg(&app),
            );
            run_capture(Command::new("xcrun").args(["strip", "-S", "-x"]).arg(&app));
            size(&app)
        };
        // The same code, so the same size, up to what LLVM's output with line tables costs at a
        // size-optimising profile (`release-mobile`, ADR-052's amendment of 2026-10-02): a few bytes
        // of function alignment either way, measured +16 (2,340,344 without symbols, 2,340,360 with)
        // and -8 (2,340,336 and 2,340,328, the review's run) over 2.3 MB. Strict `<=` held at
        // opt-level 3 and fails here on the sign: a thousandth, two orders above what was measured and
        // far below what a symbol table or debug info left in the app would add.
        let without = linked(plain, "plain");
        before_after(
            "ios app (linked, stripped)",
            without + without / 1000,
            linked(with, "symbols"),
        );
        compared += 1;
    }
    if cfg!(target_os = "macos") {
        build("host", false);
        build("host", true);
        let rel = "build/host/libplayground_core.dylib";
        // Apple's `strip` cannot remove the one `N_OPT` stab (`radr://5614542`) the linker writes
        // into any image whose objects have debug info: 40 bytes with the re-signed code directory.
        // The linker's own `-S` (what `strip = "symbols"` asks for) would not write it, but then
        // the dSYM could not be made from the unstripped link.
        let (before, after) = (size(&plain.root.join(rel)), size(&with.root.join(rel)));
        before_after("host dylib", before, after.saturating_sub(64));
        assert!(
            !plain
                .root
                .join("build/host/libplayground_core.dylib.dSYM")
                .exists()
        );
        compared += 1;
    }
    if compared == 0 {
        skip_unless(false, "no platform toolchain is installed to compare sizes");
    } else {
        assert!(
            !toolchains_required() || compared >= 2,
            "compared {compared} platforms"
        );
    }
}
