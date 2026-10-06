//! Helpers shared by the integration tests: they drive the real `undra` binary.
#![allow(dead_code)]

pub mod devserver;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// The root of the Undra repository this crate is part of.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

/// One target directory for every test, so the Undra crates compile once per run, not once per
/// test. It is below the repository's `target/`, so nothing is written outside it.
pub fn shared_target() -> PathBuf {
    repo_root().join("target/undra-cli-tests")
}

/// A scratch directory, removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("undra-it-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Serializes the tests of one test binary that assert on what Cargo compiles: other tests building
/// into the shared target directory at the same time make "nothing was recompiled" unknowable.
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `undra` binary under test, with a hermetic environment.
pub fn undra() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_undra"));
    cmd.env("NO_COLOR", "1")
        .env("CARGO_TARGET_DIR", shared_target())
        .env_remove("UNDRA_PATH")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .stdin(Stdio::null());
    cmd
}

/// Runs `cmd` to completion and returns its output; panics with everything it printed when it
/// fails.
pub fn run_ok(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("the command starts");
    assert!(
        out.status.success(),
        "{cmd:?} failed with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Runs `cmd` and expects it to fail; returns its stderr.
pub fn run_err(cmd: &mut Command) -> (i32, String) {
    let out = cmd.output().expect("the command starts");
    assert!(
        !out.status.success(),
        "{cmd:?} unexpectedly succeeded:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A project created with `undra init --undra-path <this repository>` in a fresh directory.
pub struct Project {
    pub dir: TempDir,
    pub root: PathBuf,
}

/// Creates a project named `name` for `platforms` ("web", "ios,android,web", ...).
pub fn init_project(name: &str, platforms: &str) -> Project {
    init_project_with(name, platforms, &[])
}

/// [`init_project`] in a scratch directory named after `tag` (the project is `name`, below it), so
/// two projects of one name can live at paths that differ.
pub fn init_project_in(tag: &str, name: &str, platforms: &str) -> Project {
    init_project_tagged(tag, name, platforms, &[])
}

/// [`init_project`] with more `undra init` arguments (`--ios-deployment-target 15.0`).
pub fn init_project_with(name: &str, platforms: &str, extra: &[&str]) -> Project {
    init_project_tagged(name, name, platforms, extra)
}

fn init_project_tagged(tag: &str, name: &str, platforms: &str, extra: &[&str]) -> Project {
    let dir = TempDir::new(tag);
    run_ok(
        undra()
            .args(["init", name, "--platforms", platforms, "--undra-path"])
            .arg(repo_root())
            .arg("--dir")
            .arg(dir.path())
            .args(extra),
    );
    let root = dir.path().join(name);
    // Pin the dependency versions the workspace itself was tested with, so the tests resolve the
    // same crates as everything else and need no index update.
    let _ = std::fs::copy(repo_root().join("Cargo.lock"), root.join("Cargo.lock"));
    Project { dir, root }
}

impl Project {
    /// `undra -C <root> <args>`.
    pub fn undra(&self) -> Command {
        let mut cmd = undra();
        cmd.arg("-C").arg(&self.root);
        cmd
    }
}

/// Writes a copy of the playground (its core and `undra.toml`) at `root`, with the core depending
/// on this repository's crates by path.
fn write_playground(root: &Path, own_workspace: bool) {
    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let repo = repo_root();
    copy_dir(
        &repo.join("examples/playground/core/src"),
        &root.join("core/src"),
    );
    std::fs::write(
        root.join("core/Cargo.toml"),
        format!(
            "[package]\nname = \"playground-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\nrust-version = \"1.85\"\npublish = false\n\n\
[lints.rust]\nunexpected_cfgs = {{ level = \"warn\", check-cfg = [\"cfg(playground_v2)\"] }}\n\n\
[dependencies]\nundra = {{ path = \"{}\", features = [\"websocket\", \"sse\", \"db\", \"uuid\", \"chrono\", \"rust_decimal\", \"bytes\"] }}\nuuid = {{ version = \"1\", default-features = false }}\nchrono = {{ version = \"0.4\", default-features = false }}\nrust_decimal = {{ version = \"1\", default-features = false }}\nbytes = {{ version = \"1\", default-features = false }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n{}",
            repo.join("crates/undra").display(),
            // Below this repository's `target/`, the copy would be taken for a member of its workspace.
            if own_workspace { "\n[workspace]\n" } else { "" }
        ),
    )
    .unwrap();
    let manifest = std::fs::read_to_string(repo.join("examples/playground/undra.toml")).unwrap();
    std::fs::write(
        root.join("undra.toml"),
        manifest.replace(
            "path = \"../..\"",
            &format!("path = \"{}\"", repo.display()),
        ),
    )
    .unwrap();
    let _ = std::fs::copy(repo.join("Cargo.lock"), root.join("Cargo.lock"));
}

/// A copy of the playground (its core and `undra.toml`) in a scratch directory, with the core
/// depending on this repository's crates by path, so a test can edit the core's sources and run
/// `undra dev` on it without touching the checkout.
pub fn playground_copy(tag: &str) -> Project {
    let dir = TempDir::new(tag);
    let root = dir.path().join("playground");
    write_playground(&root, false);
    Project { dir, root }
}

/// A project at a stable place: a copy of the playground below `target/` of this repository, made
/// fresh by [`playground_at`] and kept after the test, so the next run finds the core's
/// dependencies already compiled. Its path is below the home directory, which is what a release
/// build remaps to `~` in the paths it embeds.
pub struct StableProject {
    /// The project root.
    pub root: PathBuf,
}

impl StableProject {
    /// `undra -C <root> <args>`.
    pub fn undra(&self) -> Command {
        let mut cmd = undra();
        cmd.arg("-C").arg(&self.root);
        cmd
    }
}

/// The playground copy named `name` below `target/undra-cli-projects/`, written anew.
pub fn playground_at(name: &str) -> StableProject {
    let root = repo_root().join("target/undra-cli-projects").join(name);
    let _ = std::fs::remove_dir_all(&root);
    write_playground(&root, true);
    StableProject { root }
}

/// Whether a test that needs a toolchain must fail instead of skipping when it is missing:
/// `UNDRA_REQUIRE_TOOLCHAINS=1`, as in the other platform tests and CI.
pub fn toolchains_required() -> bool {
    flag("UNDRA_REQUIRE_TOOLCHAINS")
}

/// `true` when the test should stop because `present` is false: it says why, and fails
/// when toolchains are required (`UNDRA_REQUIRE_TOOLCHAINS=1`).
pub fn skip_unless(present: bool, what: &str) -> bool {
    if present {
        return false;
    }
    assert!(
        !toolchains_required(),
        "UNDRA_REQUIRE_TOOLCHAINS=1 and {what}"
    );
    eprintln!("skipped: {what}");
    true
}

/// Whether the Rust standard library for `triple` is installed.
pub fn has_rust_target(triple: &str) -> bool {
    let Ok(out) = Command::new("rustc").args(["--print", "sysroot"]).output() else {
        return false;
    };
    Path::new(String::from_utf8_lossy(&out.stdout).trim())
        .join("lib/rustlib")
        .join(triple)
        .is_dir()
}

/// Whether an Android NDK is installed where `undra build --platform android` looks for one: the
/// first of `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT` and `NDK_HOME` that is set (and only that one:
/// the build stops with C0003 when it names no directory, so the tests that need the NDK skip
/// rather than fail on it), else a version below `ANDROID_HOME/ndk` (ADR-065: no `cargo-ndk`).
pub fn has_android_ndk() -> bool {
    let named = |key: &str| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(chosen) = ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"]
        .iter()
        .find_map(|key| named(key))
    {
        return chosen.is_dir();
    }
    ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|key| named(key))
        .any(|sdk| {
            std::fs::read_dir(sdk.join("ndk"))
                .is_ok_and(|mut versions| versions.any(|v| v.is_ok_and(|v| v.path().is_dir())))
        })
}

/// Whether `program --version` runs.
pub fn has_tool(program: &str, version_arg: &str) -> bool {
    Command::new(program)
        .arg(version_arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `true` when environment variable `name` is `1`: how the heavy platform tests are switched on.
pub fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// `PATH` with the directory of the `undra` under test in front: what the build systems of a generated
/// project (the Gradle task, the Xcode build phase, the Vite plugin) find `undra` through.
pub fn path_with_undra() -> std::ffi::OsString {
    let dir = Path::new(env!("CARGO_BIN_EXE_undra"))
        .parent()
        .expect("the binary has a directory")
        .to_path_buf();
    let mut dirs = vec![dir];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).expect("a PATH")
}

/// A git command in `dir`, with an identity and no global configuration, so a developer's hooks and
/// signing settings cannot reach a scratch repository.
pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Undra Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The command with git kept inside the scratch directory: it never finds a repository above it.
pub fn undra_in(scratch: &Path) -> Command {
    let mut cmd = undra();
    cmd.env(
        "GIT_CEILING_DIRECTORIES",
        scratch.parent().unwrap_or(scratch),
    )
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd
}
