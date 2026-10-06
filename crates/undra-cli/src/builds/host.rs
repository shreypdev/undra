//! The host build: the shim as a cdylib for this machine, `build/host/lib<namespace>.{dylib,so}`.
//!
//! `undra bindgen` loads it to read the schema (through `<namespace>_undra_api`); the Kotlin
//! runtime's JVM tests load it through JNI (the generated `UndraCoreNative` loads `<namespace>`,
//! `-Djava.library.path=build/host` or `-Dundra.native.<namespace>.path=<file>`). It is built with the
//! `jni` feature so one library serves both, and so an `undra bindgen` followed by an `undra build`
//! builds `undra-ffi` once.
//!
//! **Symbols (ADR-046).** A release build strips the library it ships and keeps its symbols next
//! to it: on macOS a dSYM (`lib<namespace>.dylib.dSYM`, from `dsymutil`) and `strip -S -x`; elsewhere
//! `lib<namespace>.so.debug` (`objcopy --only-keep-debug`) and `strip --strip-unneeded`. The
//! manifest records the library's `LC_UUID` / GNU build id (which the Linux library is linked with
//! explicitly, see [`identity_args`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cargo::{Build, Profile};
use crate::error::{CliError, Code, Result};
use crate::fsutil::{copy_file, remove_dir_all, size_of};
use crate::session::Session;
use crate::symbols::{Entry, Format, Symbols, image, sha256, slash_relative, tools};
use crate::sys::Os;

use super::Artifact;

/// The file name of a library called `name` on this machine (`lib<name>.dylib`, `lib<name>.so`,
/// `<name>.dll`): the core's is `name` = its namespace (ADR-044), what `System.loadLibrary` and
/// `dlopen` look for.
#[must_use]
pub fn library_file_name(name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("lib{name}.dylib")
    } else if cfg!(windows) {
        format!("{name}.dll")
    } else {
        format!("lib{name}.so")
    }
}

/// The rustc arguments that give the host library a location-independent identity.
///
/// A macOS dylib records its *install name*, and rustc defaults it to the absolute path of the
/// file it just wrote (`/Users/ana/app/target/debug/deps/libundra_core_1234abcd.dylib`). Anything linked
/// against, or embedding, a copy of the library would then look for it on the machine that built
/// it, and the path leaks into whatever embeds it. `@rpath/<name>` lets the consumer decide where
/// the library lives. ELF records no path unless asked, but its identity, the GNU build id a panic
/// report carries and the symbols manifest keys the `.so.debug` by (ADR-046), is written only when
/// the linker is asked for it: `lld` (rustc's default linker on x86_64 Linux) writes none of its own,
/// and whether `cc` asks depends on how the distribution built it. So Linux gets
/// `--build-id=sha1`, as an Android library does ([`super::android::BUILD_ID_LINK_ARG`]). Windows
/// gets nothing.
#[must_use]
pub fn identity_args(os: Os, file_name: &str) -> Vec<String> {
    match os {
        Os::Macos => vec![format!("-Clink-arg=-Wl,-install_name,@rpath/{file_name}")],
        Os::Linux => vec![format!("-C{}", super::android::BUILD_ID_LINK_ARG)],
        Os::Windows => Vec::new(),
    }
}

/// Builds the shim for the host and returns the library inside Cargo's target directory.
///
/// # Errors
///
/// See [`Session::core`] and [`crate::cargo::Cargo::build_library`].
pub fn cdylib(session: &Session<'_>, release: bool) -> Result<PathBuf> {
    cdylib_with(session, release, Vec::new())
}

/// [`cdylib`] with extra `--config` arguments for Cargo (see [`Symbols::cargo_config`]).
///
/// # Errors
///
/// See [`cdylib`].
pub fn cdylib_with(
    session: &Session<'_>,
    release: bool,
    cargo_config: Vec<String>,
) -> Result<PathBuf> {
    let manifest = session.shim_manifest()?;
    let namespace = session.namespace()?;
    let files = session.cargo().build_library(&Build {
        manifest,
        // A target directory of its own, not the project's: see `host_lib_target_dir` (ADR-029).
        target_dir: crate::shim::host_lib_target_dir(&session.target_dir()?, &session.project.root),
        triple: None,
        profile: if release {
            Profile::Release
        } else {
            Profile::Dev
        },
        crate_type: "cdylib",
        features: vec!["jni".to_owned()],
        // Belt to the profile's `incremental = false`: overrides an inherited `CARGO_INCREMENTAL=1`.
        env: vec![("CARGO_INCREMENTAL".to_owned(), "0".to_owned())],
        lib_name: crate::shim::shim_lib_name(&session.project.root),
        rustc_args: identity_args(session.sys.os(), &library_file_name(&namespace)),
        cargo_config,
        remap: session.remap_roots(),
    })?;
    let wanted = library_file_name(&crate::shim::shim_lib_name(&session.project.root));
    Ok(files
        .iter()
        .find(|f| f.file_name().is_some_and(|n| n == wanted.as_str()))
        .or_else(|| files.first())
        .cloned()
        .expect("build_library returns at least one file"))
}

/// Builds the host library and copies it to `build/host/`; a release build strips the copy and keeps
/// its symbols next to it (see the module docs).
///
/// # Errors
///
/// See [`cdylib`]; `C0003` when `dsymutil` / `strip` is missing for a release build, `C0004` when
/// one fails.
pub fn package(
    session: &Session<'_>,
    release: bool,
    symbols: &Symbols<'_, '_>,
) -> Result<Vec<Artifact>> {
    let profile = if release {
        Profile::Release
    } else {
        Profile::Dev
    };
    let apple = session.sys.os() == Os::Macos;
    let built = cdylib_with(session, release, symbols.cargo_config(profile, apple))?;
    let dest = session
        .project
        .build_dir()
        .join("host")
        .join(library_file_name(&session.namespace()?));
    copy_file(&built, &dest)?;
    let mut artifacts = Vec::new();
    if release && symbols.enabled && session.sys.os() != Os::Windows {
        artifacts.extend(keep_symbols(session, symbols, &dest)?);
    } else if release {
        // Nothing of an earlier build's symbols may sit next to a library they do not describe.
        symbols.forget("host")?;
        for stale in ["dSYM", "debug"] {
            let twin = dest.with_file_name(format!(
                "{}.{stale}",
                dest.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ));
            remove_any(&twin)?;
        }
    }
    artifacts.insert(
        0,
        Artifact {
            label: "host".to_owned(),
            size: size_of(&dest),
            path: dest,
            budget: None,
            note: None,
        },
    );
    Ok(artifacts)
}

/// The symbols of the host library `shipped` (still unstripped): a dSYM or a `.debug` file next to
/// it, then the library is stripped, and the manifest says which image it is.
fn keep_symbols(
    session: &Session<'_>,
    symbols: &Symbols<'_, '_>,
    shipped: &Path,
) -> Result<Vec<Artifact>> {
    let identity = symbols.identity()?.clone();
    let mac = session.sys.os() == Os::Macos;
    let name = shipped
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let twin = shipped.with_file_name(if mac {
        format!("{name}.dSYM")
    } else {
        format!("{name}.debug")
    });
    let (tool_names, args): (&[&str], Vec<String>) = if mac {
        (
            &["dsymutil"],
            vec![
                shipped.display().to_string(),
                "-o".to_owned(),
                twin.display().to_string(),
            ],
        )
    } else {
        (
            &["llvm-objcopy", "objcopy"],
            vec![
                "--only-keep-debug".to_owned(),
                shipped.display().to_string(),
                twin.display().to_string(),
            ],
        )
    };
    remove_any(&twin)?;
    run_tool(
        session,
        tool_names,
        &args,
        "keeping the symbols of the host library",
    )?;
    let (strip_names, strip_args): (&[&str], Vec<String>) = if mac {
        (
            &["strip"],
            vec![
                "-S".to_owned(),
                "-x".to_owned(),
                shipped.display().to_string(),
            ],
        )
    } else {
        (
            &["llvm-strip", "strip"],
            vec!["--strip-unneeded".to_owned(), shipped.display().to_string()],
        )
    };
    run_tool(
        session,
        strip_names,
        &strip_args,
        "stripping the host library that ships",
    )?;

    let bytes = std::fs::read(shipped).map_err(|e| CliError::io("read", shipped, &e))?;
    let (format, arch, image_id) = if mac {
        let slice = image::macho_slices(&bytes).ok().and_then(|mut s| {
            if s.is_empty() {
                None
            } else {
                Some(s.remove(0))
            }
        });
        (
            Format::Macho,
            slice
                .as_ref()
                .map_or_else(|| "arm64".to_owned(), |s| s.arch.clone()),
            slice.and_then(|s| s.uuid),
        )
    } else {
        (
            Format::Elf,
            std::env::consts::ARCH.to_owned(),
            image::elf_build_id(&bytes),
        )
    };
    symbols.record(Entry {
        platform: "host".to_owned(),
        namespace: identity.namespace,
        core_version: identity.core_version,
        schema_hash: identity.schema_hash,
        arch,
        format,
        image_id,
        sha256: sha256::hex(&bytes),
        shipped: slash_relative(&session.project.build_dir(), shipped),
        shipped_bytes: bytes.len() as u64,
        symbols: Some(slash_relative(&symbols.dir(), &twin)),
        function_map: None,
        dwarf: None,
    })?;
    Ok(vec![Artifact {
        label: "symbols host".to_owned(),
        size: size_of(&twin),
        path: twin,
        budget: None,
        note: Some("not shipped".to_owned()),
    }])
}

/// Removes a file or a directory (a dSYM is one, a `.debug` file the other); nothing there is fine.
fn remove_any(path: &Path) -> Result<()> {
    if path.is_dir() {
        remove_dir_all(path)
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(CliError::io("remove", path, &e))
            }
            _ => Ok(()),
        }
    }
}

/// Runs the first of `names` that exists with `args`.
fn run_tool(session: &Session<'_>, names: &[&str], args: &[String], what: &str) -> Result<()> {
    let tool = tools::find(session.sys, &session.toolchain, names).ok_or_else(|| {
        CliError::missing_tool(
            names[0],
            what,
            "install Xcode's command line tools (`xcode-select --install`) or binutils, or build with `undra build --release --no-symbols`",
        )
    })?;
    let mut cmd = Command::new(&tool);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null());
    session.toolchain.apply(&mut cmd);
    let output = cmd.output().map_err(|e| CliError::io("run", &tool, &e))?;
    if output.status.success() {
        return Ok(());
    }
    Err(CliError::new(
        Code::ToolFailed,
        format!("`{}` failed while {what} ({})", names[0], output.status),
        "a release build of the host library keeps its symbols and ships a stripped copy",
        "run the tool by hand to see why, or build with `undra build --release --no-symbols`",
    )
    .with_detail(String::from_utf8_lossy(&output.stderr).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_macos_library_is_named_relative_to_rpath() {
        assert_eq!(
            identity_args(Os::Macos, "libacme_pay.dylib"),
            vec!["-Clink-arg=-Wl,-install_name,@rpath/libacme_pay.dylib".to_owned()]
        );
    }

    #[test]
    fn a_linux_library_is_linked_with_a_build_id_whatever_the_linker_defaults_to() {
        // The id a panic report carries and the manifest keys the `.so.debug` by (ADR-046): without it
        // a report of the shipped library cannot name its symbols.
        assert_eq!(
            identity_args(Os::Linux, "libacme_pay.so"),
            vec!["-Clink-arg=-Wl,--build-id=sha1".to_owned()]
        );
    }

    #[test]
    fn windows_records_no_path_so_nothing_is_passed() {
        assert!(identity_args(Os::Windows, "acme_pay.dll").is_empty());
    }

    #[test]
    fn the_library_is_named_after_the_namespace() {
        let name = library_file_name("acme_pay");
        assert!(
            ["libacme_pay.dylib", "libacme_pay.so", "acme_pay.dll"].contains(&name.as_str()),
            "{name}"
        );
    }
}
