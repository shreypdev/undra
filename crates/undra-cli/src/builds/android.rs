//! The Android build: the shim as one `lib<namespace>.so` per ABI, cross-built by Cargo with the NDK's
//! clang as the linker (ADR-065; no `cargo-ndk`).
//!
//! The result is the `jniLibs/` layout Gradle packages (`<abi>/lib<namespace>.so`). The name
//! matters: the generated `UndraCoreNative` loads the core by its namespace (`System.loadLibrary`),
//! so two cores (two namespaces) sit side by side in one APK (ADR-044). The library is built with
//! the `jni` feature, whose `JNI_OnLoad` registers the natives on that generated class
//! (`<kotlin package>.UndraCoreNative`), and is checked for the 16 KB page alignment that Google Play requires of
//! 64-bit libraries (the build asks the linker for 16 KB pages itself, [`ndk::PAGE_SIZE_LINK_ARG`]; the
//! check says so if a toolchain does not).
//!
//! **The NDK.** `ANDROID_NDK_HOME`, else `ANDROID_HOME/ndk/<version>` (the newest) is the NDK; for each
//! ABI the CLI runs `cargo rustc --target <triple>` with the environment Cargo reads to link with it:
//! `CARGO_TARGET_<TRIPLE>_LINKER`, `CC_<triple>`, `AR_<triple>`, and for the build scripts of C libraries
//! the sysroot for `bindgen`, the NDK's clang and a CMake toolchain ([`ndk::environment`]). The API level of
//! the clang wrapper is `[android] min_sdk` of `undra.toml`, 26 when the project says nothing (what
//! `undra init` writes).
//!
//! **Symbols (ADR-046).** The libraries carry a GNU build id (`-Wl,--build-id=sha1`), which is what
//! a panic report names its image by. A release build keeps the line tables the shim's profile asks
//! for in the library Cargo made, ships a stripped copy of it (`llvm-strip --strip-debug
//! --strip-unneeded` from the NDK) and keeps the unstripped one as the symbol file:
//!
//! * `jniLibs/<abi>/lib<namespace>.so` is what Gradle packages;
//! * `symbols/android/<abi>/lib<namespace>.so` is its unstripped twin (same code, same build id);
//! * `symbols/android/native-debug-symbols.zip` holds the twins as `<abi>/lib<namespace>.so`, the
//!   layout the Play Console takes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::binary::{elf_is_64, elf_min_load_alignment};
use crate::cargo::{Build, Profile};
use crate::error::{CliError, Code, Result};
use crate::fsutil::{
    copy_file, create_dir_all, human_size, remove_dir_all, size_of, write_if_changed,
};
use crate::session::Session;
use crate::symbols::{Entry, Format, Symbols, image, sha256, slash_relative, tools, zip};
use crate::toolchain::{Concern, ndk_major};

use super::{Artifact, gradle, ndk};

/// The Rust target of an Android ABI.
#[must_use]
pub fn triple_of(abi: &str) -> &'static str {
    match abi {
        "arm64-v8a" => "aarch64-linux-android",
        "x86_64" => "x86_64-linux-android",
        "armeabi-v7a" => "armv7-linux-androideabi",
        _ => "i686-linux-android",
    }
}

/// Builds `build/android/jniLibs/<abi>/lib<namespace>.so` for every ABI of `[android] abis`.
///
/// # Errors
///
/// `C0003` without the NDK (or with one that lacks the clang of the project's API level), `C0011`
/// without the Rust targets, `C0004` when the build fails.
pub fn build(
    session: &Session<'_>,
    release: bool,
    symbols: &Symbols<'_, '_>,
) -> Result<Vec<Artifact>> {
    let cfg = &session.project.config.android;
    let Some(ndk) = session.toolchain.android_ndk.clone() else {
        return Err(CliError::new(
            Code::MissingTool,
            "the Android NDK was not found",
            "the core is compiled for Android with the NDK's clang and linker; neither ANDROID_NDK_HOME nor an `ndk/<version>` directory of the Android SDK exists",
            "install it with `sdkmanager \"ndk;27.2.12479018\"` (r27 or newer, for 16 KB pages) and set ANDROID_NDK_HOME, or set ANDROID_HOME so `ndk/` is found",
        ));
    };
    for note in session.toolchain.notes_for(Concern::Android) {
        session.ui.detail(note);
    }
    if ndk_major(&ndk).is_some_and(|major| major < 27) {
        session.ui.warn(&format!(
            "the NDK at {} is older than r27: its libraries are not 16 KB page aligned, which Google Play requires for 64-bit apps",
            ndk.display()
        ));
    }
    let os = session.sys.os();
    for abi in &cfg.abis {
        let triple = triple_of(abi);
        let missing = ndk::missing_tools(session.sys, &ndk, os, triple, cfg.min_sdk);
        if !missing.is_empty() {
            return Err(incomplete_ndk(&ndk, &missing, cfg.min_sdk));
        }
    }
    for abi in &cfg.abis {
        session.cargo().require_target(triple_of(abi))?;
    }

    let manifest = session.shim_manifest()?;
    let namespace = session.namespace()?;
    let library = format!("lib{namespace}.so");
    let target_dir = session.target_dir()?;
    // A release build is the profile `[android] opt_level` names, the size-tuned `release-mobile` by
    // default (ADR-052, "native size gates"); a debug build is cargo's dev profile.
    let profile = Profile::mobile(release, session.project.config.android.opt_level);

    // One Cargo build per ABI, linked with the NDK's clang. The library lands where Cargo puts any
    // cross-built cdylib, `<target>/<triple>/<profile>/lib<shim>.so`; the jniLibs layout is made below.
    let mut built_libs = Vec::new();
    for abi in &cfg.abis {
        let triple = triple_of(abi);
        session
            .ui
            .detail(&format!("rust target {triple} (NDK API {})", cfg.min_sdk));
        // The CMake toolchain of this ABI, for build scripts that use the `cmake` crate: the NDK's,
        // with the ABI and the API level it cannot read from the environment ([`ndk::cmake_toolchain`]).
        let cmake_toolchain = target_dir
            .join("undra-ndk")
            .join(format!("{triple}-{}", cfg.min_sdk))
            .join("android.toolchain.cmake");
        write_if_changed(
            &cmake_toolchain,
            &ndk::cmake_toolchain(&ndk, triple, cfg.min_sdk),
        )?;
        let files = session.cargo().build_library(&Build {
            manifest: manifest.clone(),
            target_dir: target_dir.clone(),
            triple: Some(triple.to_owned()),
            profile,
            crate_type: "cdylib",
            features: vec!["jni".to_owned()],
            env: ndk::environment(session.sys, &ndk, os, triple, cfg.min_sdk, &cmake_toolchain)?,
            lib_name: crate::shim::shim_lib_name(&session.project.root),
            // The image's identity (what a panic report names it by) and the 16 KB pages, in the
            // unstripped library and in the stripped copy alike.
            rustc_args: ndk::rustc_args(os, triple, cfg.min_sdk),
            cargo_config: symbols.cargo_config(profile, false),
            remap: session.remap_roots(),
        })?;
        let built = files
            .into_iter()
            .find(|f| f.extension().is_some_and(|e| e == "so"))
            .ok_or_else(|| {
                CliError::new(
                    Code::ToolFailed,
                    format!("cargo built for {triple} but produced no shared library"),
                    "the Android build packages the shim as a .so for every ABI it was asked for",
                    "run `cargo clean` and try again; if it persists this is a bug in undra-cli",
                )
            })?;
        built_libs.push((abi.clone(), built));
    }

    let jni_libs: PathBuf = session.project.build_dir().join("android/jniLibs");
    remove_dir_all(&jni_libs)?;
    let mut artifacts = Vec::new();
    for (abi, built) in &built_libs {
        let dest = jni_libs.join(abi).join(&library);
        copy_file(built, &dest)?;
        if release && symbols.enabled {
            // The profile no longer strips (it keeps the line tables for the twin below): the copy
            // that ships is stripped here instead.
            let strip = strip_tool(session)?;
            strip_shipped(&strip, &dest, session)?;
        }
        let mut note = None;
        if let Ok(bytes) = std::fs::read(&dest) {
            if elf_is_64(&bytes) {
                match elf_min_load_alignment(&bytes) {
                    Some(align) if align >= 16 * 1024 => note = Some("16 KB aligned".to_owned()),
                    Some(align) => session.ui.warn(&format!(
                        "{abi}: LOAD segments are aligned to {align} bytes, not 16 KB; Google Play rejects 64-bit libraries like this. Use NDK r27 or newer"
                    )),
                    None => {}
                }
            }
        }
        if release && symbols.enabled {
            artifacts.extend(keep_symbols(session, symbols, abi, built, &dest, &library)?);
        }
        artifacts.push(Artifact {
            label: format!("android {abi}"),
            size: size_of(&dest),
            path: dest,
            budget: Some("1.2 MB per ABI (hello world, release)".to_owned()),
            note,
        });
    }
    if release && symbols.enabled {
        artifacts.extend(play_archive(session, symbols, &library)?);
    } else if release {
        symbols.forget("android")?;
    }
    let root = &session.project.root;
    if let Some(message) =
        gradle::reconcile(root, &jni_libs, &cfg.abis, &library)?.message(root, &jni_libs)
    {
        session.ui.warn(&message);
    }
    Ok(artifacts)
}

/// The error for an NDK directory that lacks the tools of the build: an incomplete download or an
/// NDK older than `api`.
fn incomplete_ndk(ndk: &Path, missing: &[PathBuf], api: u32) -> CliError {
    let list = missing
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    CliError::new(
        Code::MissingTool,
        format!("the Android NDK at {} lacks {list}", ndk.display()),
        format!(
            "the core is linked with the NDK's clang for API level {api} (`[android] min_sdk` of undra.toml); this directory is not a whole NDK for this machine, or is older than that API level"
        ),
        "point ANDROID_NDK_HOME at an NDK r27 or newer (`sdkmanager \"ndk;27.2.12479018\"`), or lower `min_sdk`",
    )
}

/// The linker argument that gives the library a GNU build id: the identity a panic report names
/// the image by (`PanicReport.image_id`), and what Play Console and Crashlytics match symbol files
/// with. SHA-1 (20 bytes) rather than lld's default 8-byte hash, so two builds do not collide.
pub const BUILD_ID_LINK_ARG: &str = "link-arg=-Wl,--build-id=sha1";

/// The arguments of the NDK's `llvm-strip` that make the shipped copy: debug info and every symbol
/// the dynamic loader does not need.
pub const STRIP_ARGS: [&str; 2] = ["--strip-debug", "--strip-unneeded"];

/// The NDK's `llvm-strip`.
fn strip_tool(session: &Session<'_>) -> Result<PathBuf> {
    tools::find(session.sys, &session.toolchain, &["llvm-strip"]).ok_or_else(|| {
        CliError::missing_tool(
            "llvm-strip",
            "stripping the library that ships (the unstripped one is kept as the symbol file)",
            "install the NDK (`sdkmanager \"ndk;27.2.12479018\"`), which has it; or `undra build --release --no-symbols` to skip the symbol files",
        )
    })
}

/// Strips `library` in place with [`STRIP_ARGS`].
fn strip_shipped(strip: &Path, library: &Path, session: &Session<'_>) -> Result<()> {
    let mut cmd = Command::new(strip);
    cmd.args(STRIP_ARGS)
        .arg(library)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    session.toolchain.apply(&mut cmd);
    let output = cmd.output().map_err(|e| CliError::io("run", strip, &e))?;
    if output.status.success() {
        return Ok(());
    }
    Err(CliError::tool_failed(
        "llvm-strip",
        "stripping the library that ships",
        &output.status.to_string(),
    )
    .with_detail(String::from_utf8_lossy(&output.stderr).into_owned()))
}

/// Keeps the unstripped library `built` as `build/symbols/android/<abi>/lib<namespace>.so` and
/// records it, with the stripped copy `shipped`, in the manifest.
fn keep_symbols(
    session: &Session<'_>,
    symbols: &Symbols<'_, '_>,
    abi: &str,
    built: &Path,
    shipped: &Path,
    library: &str,
) -> Result<Vec<Artifact>> {
    let identity = symbols.identity()?.clone();
    let twin = symbols.dir().join("android").join(abi).join(library);
    copy_file(built, &twin)?;
    let bytes = std::fs::read(&twin).map_err(|e| CliError::io("read", &twin, &e))?;
    let image_id = image::elf_build_id(&bytes);
    if image_id.is_none() {
        session.ui.warn(&format!(
            "{abi}: the library has no GNU build id, so a crash report cannot name it; the linker was asked for one with {BUILD_ID_LINK_ARG}"
        ));
    }
    let shipped_bytes = std::fs::read(shipped).map_err(|e| CliError::io("read", shipped, &e))?;
    // The copy that ships is the same image: the same build id (stripping keeps the note).
    if image::elf_build_id(&shipped_bytes) != image_id {
        session.ui.warn(&format!(
            "{abi}: the stripped library has another build id than the unstripped one, so the symbol file does not match what ships"
        ));
    }
    symbols.record(Entry {
        platform: "android".to_owned(),
        namespace: identity.namespace,
        core_version: identity.core_version,
        schema_hash: identity.schema_hash,
        arch: abi.to_owned(),
        format: Format::Elf,
        image_id,
        sha256: sha256::hex(&shipped_bytes),
        shipped: slash_relative(&session.project.build_dir(), shipped),
        shipped_bytes: shipped_bytes.len() as u64,
        symbols: Some(slash_relative(&symbols.dir(), &twin)),
        function_map: None,
        dwarf: None,
    })?;
    Ok(vec![Artifact {
        label: format!("symbols android {abi}"),
        size: size_of(&twin),
        path: twin,
        budget: None,
        note: Some("unstripped, not shipped".to_owned()),
    }])
}

/// Writes `build/symbols/android/native-debug-symbols.zip`: every ABI's unstripped library as
/// `<abi>/lib<namespace>.so`, the layout the Play Console takes. With `zip` installed it is
/// compressed; without, the CLI writes the archive itself, stored.
fn play_archive(
    session: &Session<'_>,
    symbols: &Symbols<'_, '_>,
    library: &str,
) -> Result<Option<Artifact>> {
    let dir = symbols.dir().join("android");
    let abis = &session.project.config.android.abis;
    let archive = dir.join(PLAY_ARCHIVE);
    let _ = std::fs::remove_file(&archive);
    create_dir_all(&dir)?;
    let tool = session.toolchain.which(session.sys, "zip");
    let made = tool.is_some_and(|zip_tool| {
        let mut cmd = Command::new(zip_tool);
        cmd.args(["-q", "-X", "-9", PLAY_ARCHIVE])
            .args(abis.iter().map(|abi| format!("{abi}/{library}")))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        session.toolchain.apply(&mut cmd);
        cmd.status().is_ok_and(|status| status.success())
    });
    if !made {
        let mut files = Vec::new();
        for abi in abis {
            let path = dir.join(abi).join(library);
            let bytes = std::fs::read(&path).map_err(|e| CliError::io("read", &path, &e))?;
            files.push((format!("{abi}/{library}"), bytes));
        }
        let bytes = zip::archive(&files).map_err(|why| {
            CliError::new(
                Code::ToolFailed,
                format!("cannot write {PLAY_ARCHIVE}: {why}"),
                "the archive holds every ABI's unstripped library",
                "install `zip`, or build fewer ABIs",
            )
        })?;
        std::fs::write(&archive, bytes).map_err(|e| CliError::io("write", &archive, &e))?;
    }
    Ok(Some(Artifact {
        label: "symbols android play".to_owned(),
        size: size_of(&archive),
        path: archive,
        budget: None,
        note: Some("upload to the Play Console (native debug symbols)".to_owned()),
    }))
}

/// The Play Console's native debug symbols archive, below `build/symbols/android`.
pub const PLAY_ARCHIVE: &str = "native-debug-symbols.zip";

/// The size of the release library an earlier `undra build --platform android --release` left in
/// Cargo's target directory for `abi` under `profile`, if there is one (`shim` is the shim's library
/// name).
fn earlier_release_size(target_dir: &Path, profile: Profile, abi: &str, shim: &str) -> Option<u64> {
    let library = target_dir
        .join(triple_of(abi))
        .join(format!("{}/lib{shim}.so", profile.dir_name()));
    std::fs::metadata(library).ok().map(|m| m.len())
}

/// The hint printed after a debug Android build: what a debug core weighs and what packaging
/// with `--release` would change.
///
/// `debug` is the largest library of the build and `abi` its ABI; `target_dir` is where Cargo put
/// an earlier release build of the shim `shim` with `profile` (the one `--release` builds), whose
/// size makes the hint exact.
#[must_use]
pub fn debug_size_hint(
    target_dir: &Path,
    profile: Profile,
    shim: &str,
    debug: u64,
    abi: &str,
) -> String {
    let release = earlier_release_size(target_dir, profile, abi, shim).filter(|size| *size > 0);
    let how = "undra build --platform android --release";
    match release {
        Some(release) => format!(
            "this Android core is a debug build ({} per ABI, fine for the dev loop); a release build is {}, {}x smaller. Package with `{how}`",
            human_size(debug),
            human_size(release),
            debug / release
        ),
        None => format!(
            "this Android core is a debug build ({} per ABI, fine for the dev loop); a release build is typically 20x or more smaller. Package with `{how}`",
            human_size(debug)
        ),
    }
}

/// The hint for a debug build of an app whose Gradle script strips the core's debug info from
/// the APK: what to add so the native debugger can stop in Rust (ADR-046).
pub(crate) fn debugger_hint(session: &Session<'_>) -> Option<String> {
    let library = format!("lib{}.so", session.namespace().ok()?);
    let (script, line) = gradle::missing_debug_symbols(&session.project.root, &library)?;
    Some(format!(
        "to step into Rust from Android Studio, {} has to keep the debug info of {library} in the debug APK; add: {line}",
        script
            .strip_prefix(&session.project.root)
            .unwrap_or(&script)
            .display()
    ))
}

/// The hint for a debug build, from what `artifacts` holds; `None` when nothing Android was built.
pub(crate) fn hint(session: &Session<'_>, artifacts: &[Artifact]) -> Option<String> {
    let largest = artifacts
        .iter()
        .filter(|a| a.label.starts_with("android "))
        .max_by_key(|a| a.size)?;
    let abi = largest.path.parent()?.file_name()?.to_str()?;
    Some(debug_size_hint(
        &session.target_dir().ok()?,
        Profile::mobile(true, session.project.config.android.opt_level),
        &crate::shim::shim_lib_name(&session.project.root),
        largest.size,
        abi,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_hint_names_the_release_command_and_the_saving() {
        // No release build to measure: a rule of thumb.
        let none = debug_size_hint(
            Path::new("/does/not/exist"),
            Profile::ReleaseMobile,
            "shim",
            42_400_000,
            "arm64-v8a",
        );
        assert!(
            none.contains("debug build")
                && none.contains("42.4 MB per ABI")
                && none.contains("20x or more smaller")
                && none.contains("`undra build --platform android --release`"),
            "{none}"
        );
        assert!(!none.contains('\n'), "one line");

        // With the release library of an earlier build at hand: the real numbers.
        let target = crate::fsutil::unique_temp_dir("android-hint");
        let release = target.join("aarch64-linux-android/release-mobile");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(release.join("libshim.so"), vec![0_u8; 1_500_000]).unwrap();
        let known = debug_size_hint(
            &target,
            Profile::ReleaseMobile,
            "shim",
            42_400_000,
            "arm64-v8a",
        );
        assert!(
            known.contains("a release build is 1.5 MB, 28x smaller"),
            "{known}"
        );
        // Another ABI has no release library of its own.
        assert!(
            debug_size_hint(
                &target,
                Profile::ReleaseMobile,
                "shim",
                42_300_000,
                "x86_64"
            )
            .contains("typically")
        );
        // Nor has the profile `[android] opt_level = "z"` would build.
        assert!(
            debug_size_hint(
                &target,
                Profile::ReleaseMobileZ,
                "shim",
                42_400_000,
                "arm64-v8a"
            )
            .contains("typically")
        );
        let _ = std::fs::remove_dir_all(target);
    }

    #[test]
    fn the_libraries_get_a_build_id_and_the_shipped_copy_loses_everything_the_loader_does_not_need()
    {
        assert_eq!(BUILD_ID_LINK_ARG, "link-arg=-Wl,--build-id=sha1");
        assert_eq!(STRIP_ARGS, ["--strip-debug", "--strip-unneeded"]);
    }

    #[test]
    fn abis_map_to_rust_targets() {
        assert_eq!(triple_of("arm64-v8a"), "aarch64-linux-android");
        assert_eq!(triple_of("x86_64"), "x86_64-linux-android");
        assert_eq!(triple_of("armeabi-v7a"), "armv7-linux-androideabi");
        assert_eq!(triple_of("x86"), "i686-linux-android");
    }
}
