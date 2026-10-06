//! The Android NDK as Cargo sees it: the environment that makes `cargo build --target <triple>` link
//! with the NDK's clang (ADR-065).
//!
//! Cargo needs four things to cross-build a Rust library for Android: a linker, a C compiler and
//! archiver for the build scripts of crates with C code (the `cc` crate reads `CC_<triple>` and
//! `AR_<triple>`), and the arguments the NDK's linker needs to align the library to 16 KB pages. All
//! of it is in the NDK, under `toolchains/llvm/prebuilt/<host>/bin`, where each `<triple><api>-clang`
//! is a wrapper that already knows its `--target` and sysroot. The build scripts of C libraries need
//! three more that `cargo-ndk` used to export: the sysroot for `bindgen` (`BINDGEN_EXTRA_CLANG_ARGS_<triple>`),
//! the NDK's clang for `clang-sys` (`CLANG_PATH`) and a CMake toolchain for the `cmake` crate
//! (`CMAKE_TOOLCHAIN_FILE_<triple>`, [`cmake_toolchain`]). [`environment`] computes the variables from
//! the NDK's directory, the host and the API level, without running anything; the Android build (and
//! the Bazel rule, through the CLI) sets them on a plain Cargo command. No `cargo-ndk`.

use std::path::{Path, PathBuf};

use crate::error::{CliError, Code, Result};
use crate::sys::{Os, Sys};

/// The `-C` argument that makes the linker align LOAD segments to 16 KB pages, which Google Play
/// requires of 64-bit libraries (NDK r27 does not do it by default; r28 does).
pub const PAGE_SIZE_LINK_ARG: &str = "link-arg=-Wl,-z,max-page-size=16384";

/// The name the NDK gives the directory of a host's prebuilt toolchain (`darwin-x86_64`, on Apple
/// silicon as well: the NDK ships one universal toolchain per OS).
#[must_use]
pub fn host_tag(os: Os) -> &'static str {
    match os {
        Os::Macos => "darwin-x86_64",
        Os::Linux => "linux-x86_64",
        Os::Windows => "windows-x86_64",
    }
}

/// The directory of the NDK's prebuilt LLVM tools for `os`.
#[must_use]
pub fn toolchain_bin(ndk: &Path, os: Os) -> PathBuf {
    ndk.join("toolchains/llvm/prebuilt")
        .join(host_tag(os))
        .join("bin")
}

/// The prefix of the NDK's clang wrapper for `triple`: the triple, except that 32-bit ARM names its
/// wrappers `armv7a-linux-androideabi<api>-clang`.
#[must_use]
pub fn clang_prefix(triple: &str) -> &str {
    if triple == "armv7-linux-androideabi" {
        "armv7a-linux-androideabi"
    } else {
        triple
    }
}

/// Where the NDK keeps the clang wrapper that compiles and links for `triple` at `api`.
#[must_use]
pub fn clang(ndk: &Path, os: Os, triple: &str, api: u32) -> PathBuf {
    toolchain_bin(ndk, os).join(format!(
        "{}{api}-clang{}",
        clang_prefix(triple),
        wrapper_suffix(os)
    ))
}

/// The NDK's `clang++` wrapper for `triple` at `api`.
#[must_use]
pub fn clangxx(ndk: &Path, os: Os, triple: &str, api: u32) -> PathBuf {
    toolchain_bin(ndk, os).join(format!(
        "{}{api}-clang++{}",
        clang_prefix(triple),
        wrapper_suffix(os)
    ))
}

/// What Cargo links with: the clang wrapper of [`clang`], except on Windows, where it is `clang.exe` itself and the API level
/// travels as `--target=` ([`rustc_args`]). A `.cmd` wrapper as the linker is the old way Windows tools break: rustc passes the
/// linker a long, quoted argument list, and cmd.exe (which runs a `.cmd`) reads quotes and `%` its own way, so a build that
/// links through it can fail on a path or an argument the same build links on macOS and Linux. `clang.exe` takes the arguments
/// as they are; it is what `cargo-ndk` linked with on Windows too.
#[must_use]
pub fn linker(ndk: &Path, os: Os, triple: &str, api: u32) -> PathBuf {
    if os == Os::Windows {
        toolchain_bin(ndk, os).join("clang.exe")
    } else {
        clang(ndk, os, triple, api)
    }
}

/// The NDK's `llvm-ar`.
#[must_use]
pub fn ar(ndk: &Path, os: Os) -> PathBuf {
    toolchain_bin(ndk, os).join(format!("llvm-ar{}", exe_suffix(os)))
}

/// The NDK's `llvm-ranlib`.
#[must_use]
pub fn ranlib(ndk: &Path, os: Os) -> PathBuf {
    toolchain_bin(ndk, os).join(format!("llvm-ranlib{}", exe_suffix(os)))
}

/// The NDK's clang itself (not a wrapper of an API level): what `clang-sys` runs, through `CLANG_PATH`,
/// to find the include directories `bindgen` passes to libclang.
#[must_use]
pub fn plain_clang(ndk: &Path, os: Os) -> PathBuf {
    toolchain_bin(ndk, os).join(format!("clang{}", exe_suffix(os)))
}

/// The NDK's sysroot: the C library's headers and libraries for every ABI and API level.
#[must_use]
pub fn sysroot(ndk: &Path, os: Os) -> PathBuf {
    ndk.join("toolchains/llvm/prebuilt")
        .join(host_tag(os))
        .join("sysroot")
}

/// The directory the sysroot keeps the ABI-specific headers of `triple` in (`usr/include/<this>`):
/// the triple, except that 32-bit ARM's are under `arm-linux-androideabi`.
#[must_use]
pub fn sysroot_triple(triple: &str) -> &str {
    if triple == "armv7-linux-androideabi" {
        "arm-linux-androideabi"
    } else {
        triple
    }
}

/// The Android ABI name of a Rust target (`aarch64-linux-android` is `arm64-v8a`), what CMake's
/// `ANDROID_ABI` takes.
#[must_use]
pub fn abi_of(triple: &str) -> &'static str {
    match triple {
        "aarch64-linux-android" => "arm64-v8a",
        "x86_64-linux-android" => "x86_64",
        "armv7-linux-androideabi" => "armeabi-v7a",
        _ => "x86",
    }
}

/// The CMake toolchain file the NDK ships, `build/cmake/android.toolchain.cmake`.
#[must_use]
pub fn ndk_cmake_toolchain(ndk: &Path) -> PathBuf {
    ndk.join("build/cmake/android.toolchain.cmake")
}

/// The text of the CMake toolchain file a build of `triple` at `api` hands the `cmake` crate: the
/// NDK's own toolchain file ([`ndk_cmake_toolchain`]) with the two variables it needs set first,
/// `ANDROID_ABI` and `ANDROID_PLATFORM`.
///
/// The NDK's file reads both as CMake variables (`-D`), never from the environment, and a build
/// script that uses the `cmake` crate passes none of its own; without them the NDK's file builds for
/// `armeabi-v7a` whatever the target is. A build script that does define either keeps its value.
#[must_use]
pub fn cmake_toolchain(ndk: &Path, triple: &str, api: u32) -> String {
    let include = ndk_cmake_toolchain(ndk)
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    let abi = abi_of(triple);
    format!(
        "# Written by `undra build`: the Android NDK's CMake toolchain for {abi} ({triple}) at API {api}.\n\
         # The `cmake` crate reads it from CMAKE_TOOLCHAIN_FILE_{cc}; a build script's own -D wins.\n\
         if(NOT ANDROID_ABI)\n  set(ANDROID_ABI \"{abi}\")\nendif()\n\
         if(NOT ANDROID_PLATFORM)\n  set(ANDROID_PLATFORM \"android-{api}\")\nendif()\n\
         include(\"{include}\")\n",
        cc = cc_env_triple(triple),
    )
}

/// `--sysroot=<sysroot> --target=<clang target><api> -I<sysroot>/usr/include/<triple>`: what
/// `bindgen` (`BINDGEN_EXTRA_CLANG_ARGS_<triple>`) needs to parse the C library's headers the way the
/// NDK's clang compiles them. The host's libclang knows no Android sysroot (`stdio.h` is not found
/// without it), and the API level makes the headers declare what `api` has, as the compiler does.
/// Forward slashes, and a path with a space quoted: `bindgen` splits the value like a shell.
#[must_use]
pub fn bindgen_clang_args(ndk: &Path, os: Os, triple: &str, api: u32) -> String {
    let root = sysroot(ndk, os).to_string_lossy().replace('\\', "/");
    let include = format!("{root}/usr/include/{}", sysroot_triple(triple));
    [
        format!("--sysroot={root}"),
        format!("--target={}{api}", clang_prefix(triple)),
        format!("-I{include}"),
    ]
    .iter()
    .map(|arg| shell_word(arg))
    .collect::<Vec<_>>()
    .join(" ")
}

/// `arg` as one word for `bindgen`'s shell-like split: as it is when it has no space or quote, else
/// in single quotes (a `'` in it closed, escaped and reopened).
fn shell_word(arg: &str) -> String {
    if arg
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
    {
        format!("'{}'", arg.replace('\'', "'\\''"))
    } else {
        arg.to_owned()
    }
}

/// Windows ships the wrappers as `.cmd` scripts.
fn wrapper_suffix(os: Os) -> &'static str {
    if os == Os::Windows { ".cmd" } else { "" }
}

fn exe_suffix(os: Os) -> &'static str {
    if os == Os::Windows { ".exe" } else { "" }
}

/// `aarch64-linux-android` as Cargo spells it in `CARGO_TARGET_<TRIPLE>_LINKER`: upper case, `_` for `-`.
#[must_use]
pub fn cargo_env_triple(triple: &str) -> String {
    triple.to_ascii_uppercase().replace('-', "_")
}

/// `aarch64-linux-android` as the `cc` crate spells it in `CC_<triple>`: `_` for `-`.
#[must_use]
pub fn cc_env_triple(triple: &str) -> String {
    triple.replace('-', "_")
}

/// Whether the library of `triple` is 64-bit (and so needs the 16 KB alignment Google Play checks).
#[must_use]
pub fn is_64_bit(triple: &str) -> bool {
    triple.starts_with("aarch64") || triple.starts_with("x86_64")
}

/// The environment of a `cargo` build for `triple` with the NDK at `ndk`, for an app whose minimum
/// API level is `api`, on `os`:
///
/// | Variable | Value |
/// |---|---|
/// | `CARGO_TARGET_<TRIPLE>_LINKER` | the NDK's `<triple><api>-clang` ([`linker`]: `clang.exe` on Windows) |
/// | `CC_<triple>`, `CXX_<triple>` | the `<triple><api>-clang` and `-clang++` wrappers (for build scripts that compile C or C++) |
/// | `AR_<triple>`, `RANLIB_<triple>` | `llvm-ar`, `llvm-ranlib` |
/// | `ANDROID_NDK_HOME` | `ndk` |
/// | `BINDGEN_EXTRA_CLANG_ARGS_<triple>` | the sysroot, target and ABI include of [`bindgen_clang_args`] |
/// | `CLANG_PATH` | the NDK's own clang ([`plain_clang`]), which `clang-sys` runs for include paths |
/// | `CMAKE_TOOLCHAIN_FILE_<triple>` | `cmake_toolchain_file`, the file [`cmake_toolchain`] writes |
/// | `ANDROID_ABI`, `ANDROID_PLATFORM` | the ABI (`arm64-v8a`) and `api`, as `cargo-ndk` exported them |
///
/// The last five are for the build scripts of crates that wrap C libraries, and are the user's to
/// override: one is left out when the user has set it, or a name its reader takes before it or
/// instead of it (`BINDGEN_EXTRA_CLANG_ARGS` without a triple, `TARGET_CMAKE_TOOLCHAIN_FILE`,
/// `CMAKE_TOOLCHAIN_FILE`, the triple spelled with `-`), read through `sys`. The linker, compiler and
/// archiver variables are always set: they are what the build links with.
///
/// Nothing is run and no file is read: the paths are computed (a missing file is [`missing_tools`]'s
/// to report).
///
/// # Errors
///
/// `C0003` when `ndk` is not valid UTF-8: the variables hold text, and a path that cannot be
/// spelled as text would reach Cargo mangled (the usual replacement character), naming a linker
/// that does not exist.
pub fn environment(
    sys: &dyn Sys,
    ndk: &Path,
    os: Os,
    triple: &str,
    api: u32,
    cmake_toolchain_file: &Path,
) -> Result<Vec<(String, String)>> {
    if ndk.to_str().is_none() {
        return Err(CliError::new(
            Code::MissingTool,
            format!(
                "the Android NDK's path, {}, is not valid UTF-8",
                ndk.display()
            ),
            "Cargo is told where the NDK's linker and compilers are through environment variables (CARGO_TARGET_<triple>_LINKER, CC_<triple>, AR_<triple>), which hold text; a path with bytes that are not UTF-8 cannot be passed as it is",
            "move the NDK to a path of plain characters, or point ANDROID_NDK_HOME at one",
        ));
    }
    // Every path below is `ndk` plus ASCII components, so this is exact.
    let path = |p: PathBuf| p.to_string_lossy().into_owned();
    let cc_triple = cc_env_triple(triple);
    let mut env = vec![
        (
            format!("CARGO_TARGET_{}_LINKER", cargo_env_triple(triple)),
            path(linker(ndk, os, triple, api)),
        ),
        (format!("CC_{cc_triple}"), path(clang(ndk, os, triple, api))),
        (
            format!("CXX_{cc_triple}"),
            path(clangxx(ndk, os, triple, api)),
        ),
        (format!("AR_{cc_triple}"), path(ar(ndk, os))),
        (format!("RANLIB_{cc_triple}"), path(ranlib(ndk, os))),
        ("ANDROID_NDK_HOME".to_owned(), path(ndk.to_path_buf())),
    ];
    // For build scripts, each with the names its reader takes it from (the first is the one set).
    let for_build_scripts: [(Vec<String>, String); 5] = [
        (
            vec![
                format!("BINDGEN_EXTRA_CLANG_ARGS_{cc_triple}"),
                format!("BINDGEN_EXTRA_CLANG_ARGS_{triple}"),
                "BINDGEN_EXTRA_CLANG_ARGS".to_owned(),
            ],
            bindgen_clang_args(ndk, os, triple, api),
        ),
        (vec!["CLANG_PATH".to_owned()], path(plain_clang(ndk, os))),
        (
            vec![
                format!("CMAKE_TOOLCHAIN_FILE_{cc_triple}"),
                format!("CMAKE_TOOLCHAIN_FILE_{triple}"),
                "TARGET_CMAKE_TOOLCHAIN_FILE".to_owned(),
                "CMAKE_TOOLCHAIN_FILE".to_owned(),
            ],
            cmake_toolchain_file.to_string_lossy().into_owned(),
        ),
        (vec!["ANDROID_ABI".to_owned()], abi_of(triple).to_owned()),
        (vec!["ANDROID_PLATFORM".to_owned()], api.to_string()),
    ];
    for (names, value) in for_build_scripts {
        if names.iter().all(|name| sys.env(name).is_none()) {
            env.push((names[0].clone(), value));
        }
    }
    Ok(env)
}

/// The arguments for rustc itself (after `--`) of the Android library for `triple` at `api` on `os`: the GNU build
/// id that names the image in a crash report (ADR-046), for a 64-bit library the 16 KB page size ([`PAGE_SIZE_LINK_ARG`]),
/// and on Windows the `--target=<triple><api>` the `.cmd` wrapper would have given `clang.exe` ([`linker`]).
#[must_use]
pub fn rustc_args(os: Os, triple: &str, api: u32) -> Vec<String> {
    let mut args = vec![
        "-C".to_owned(),
        super::android::BUILD_ID_LINK_ARG.to_owned(),
    ];
    if is_64_bit(triple) {
        args.extend(["-C".to_owned(), PAGE_SIZE_LINK_ARG.to_owned()]);
    }
    if os == Os::Windows {
        args.extend([
            "-C".to_owned(),
            format!("link-arg=--target={}{api}", clang_prefix(triple)),
        ]);
    }
    args
}

/// The NDK tools the build of `triple` needs that are not files of the NDK at `ndk`: the linker and
/// the archiver. Empty when the NDK is whole.
#[must_use]
pub fn missing_tools(sys: &dyn Sys, ndk: &Path, os: Os, triple: &str, api: u32) -> Vec<PathBuf> {
    // The wrapper of the API level is checked on every host: an NDK older than the level has none, and the C compiler of build
    // scripts is that wrapper even where the linker is `clang.exe`.
    let mut tools = vec![
        clang(ndk, os, triple, api),
        linker(ndk, os, triple, api),
        ar(ndk, os),
    ];
    tools.dedup();
    tools.retain(|tool| !sys.is_file(tool));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeSys;

    fn get<'a>(env: &'a [(String, String)], key: &str) -> &'a str {
        env.iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("{key} is not set: {env:?}"))
            .1
            .as_str()
    }

    const TOOLCHAIN_FILE: &str =
        "/app/target/undra-ndk/aarch64-linux-android-26/android.toolchain.cmake";

    /// [`environment`] on a machine where the user has exported nothing.
    fn env_of(ndk: &Path, os: Os, triple: &str, api: u32) -> Result<Vec<(String, String)>> {
        environment(
            &FakeSys::default(),
            ndk,
            os,
            triple,
            api,
            Path::new(TOOLCHAIN_FILE),
        )
    }

    #[test]
    fn build_scripts_get_the_sysroot_for_bindgen_the_ndk_clang_and_a_cmake_toolchain() {
        let ndk = Path::new("/sdk/ndk/27.2.12479018");
        let env = env_of(ndk, Os::Macos, "aarch64-linux-android", 26).unwrap();
        let sysroot = "/sdk/ndk/27.2.12479018/toolchains/llvm/prebuilt/darwin-x86_64/sysroot";
        assert_eq!(
            get(&env, "BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android"),
            format!(
                "--sysroot={sysroot} --target=aarch64-linux-android26 -I{sysroot}/usr/include/aarch64-linux-android"
            )
        );
        assert_eq!(
            get(&env, "CLANG_PATH"),
            "/sdk/ndk/27.2.12479018/toolchains/llvm/prebuilt/darwin-x86_64/bin/clang"
        );
        assert_eq!(
            get(&env, "CMAKE_TOOLCHAIN_FILE_aarch64_linux_android"),
            TOOLCHAIN_FILE
        );
        assert_eq!(get(&env, "ANDROID_ABI"), "arm64-v8a");
        assert_eq!(get(&env, "ANDROID_PLATFORM"), "26");
    }

    #[test]
    fn x86_64_and_32_bit_arm_get_their_own_include_and_target() {
        let env = env_of(Path::new("/ndk"), Os::Linux, "x86_64-linux-android", 31).unwrap();
        let sysroot = "/ndk/toolchains/llvm/prebuilt/linux-x86_64/sysroot";
        assert_eq!(
            get(&env, "BINDGEN_EXTRA_CLANG_ARGS_x86_64_linux_android"),
            format!(
                "--sysroot={sysroot} --target=x86_64-linux-android31 -I{sysroot}/usr/include/x86_64-linux-android"
            )
        );
        assert_eq!(get(&env, "ANDROID_ABI"), "x86_64");
        assert_eq!(get(&env, "ANDROID_PLATFORM"), "31");
        // 32-bit ARM: clang's armv7a target, the sysroot's arm-linux-androideabi headers.
        let arm = env_of(Path::new("/ndk"), Os::Linux, "armv7-linux-androideabi", 26).unwrap();
        let args = get(&arm, "BINDGEN_EXTRA_CLANG_ARGS_armv7_linux_androideabi");
        assert!(
            args.contains("--target=armv7a-linux-androideabi26")
                && args.ends_with("/usr/include/arm-linux-androideabi"),
            "{args}"
        );
        assert_eq!(get(&arm, "ANDROID_ABI"), "armeabi-v7a");
        let windows = env_of(
            Path::new("C:/ndk"),
            Os::Windows,
            "aarch64-linux-android",
            26,
        )
        .unwrap();
        assert!(get(&windows, "CLANG_PATH").ends_with("windows-x86_64/bin/clang.exe"));
    }

    #[test]
    fn a_sysroot_with_a_space_is_quoted_for_bindgens_split() {
        let args = bindgen_clang_args(
            Path::new("/Users/a b/ndk"),
            Os::Macos,
            "aarch64-linux-android",
            26,
        );
        assert_eq!(
            args,
            "'--sysroot=/Users/a b/ndk/toolchains/llvm/prebuilt/darwin-x86_64/sysroot' --target=aarch64-linux-android26 '-I/Users/a b/ndk/toolchains/llvm/prebuilt/darwin-x86_64/sysroot/usr/include/aarch64-linux-android'"
        );
    }

    #[test]
    fn what_the_user_exported_for_a_build_script_is_kept() {
        let ndk = Path::new("/ndk");
        let file = Path::new(TOOLCHAIN_FILE);
        let triple = "aarch64-linux-android";
        let set = |sys: &FakeSys| environment(sys, ndk, Os::Linux, triple, 26, file).unwrap();
        let keys =
            |env: &[(String, String)]| env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
        let user = FakeSys::linux()
            .with_env("BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android", "--mine")
            .with_env("CLANG_PATH", "/usr/bin/clang")
            .with_env("CMAKE_TOOLCHAIN_FILE_aarch64_linux_android", "/mine.cmake")
            .with_env("ANDROID_ABI", "arm64-v8a")
            .with_env("ANDROID_PLATFORM", "30");
        let env = set(&user);
        for key in [
            "BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android",
            "CLANG_PATH",
            "CMAKE_TOOLCHAIN_FILE_aarch64_linux_android",
            "ANDROID_ABI",
            "ANDROID_PLATFORM",
        ] {
            assert!(!keys(&env).contains(&key.to_owned()), "{key}: {env:?}");
        }
        // The linker and the compilers are what the build links with: always set.
        assert!(keys(&env).contains(&"CC_aarch64_linux_android".to_owned()));
        // A less specific name the reader also takes counts as the user's choice too.
        for general in [
            "BINDGEN_EXTRA_CLANG_ARGS",
            "BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android",
        ] {
            let env = set(&FakeSys::linux().with_env(general, "--mine"));
            assert!(
                !keys(&env).contains(&"BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android".to_owned()),
                "{general}"
            );
        }
        for general in [
            "CMAKE_TOOLCHAIN_FILE",
            "TARGET_CMAKE_TOOLCHAIN_FILE",
            "CMAKE_TOOLCHAIN_FILE_aarch64-linux-android",
        ] {
            let env = set(&FakeSys::linux().with_env(general, "/mine.cmake"));
            assert!(
                !keys(&env).contains(&"CMAKE_TOOLCHAIN_FILE_aarch64_linux_android".to_owned()),
                "{general}"
            );
        }
        // Another triple's variable is not this build's.
        let other =
            FakeSys::linux().with_env("BINDGEN_EXTRA_CLANG_ARGS_x86_64_linux_android", "--x");
        assert!(
            keys(&set(&other))
                .contains(&"BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android".to_owned())
        );
    }

    #[test]
    fn the_cmake_toolchain_names_the_abi_and_level_then_includes_the_ndks_file() {
        let text = cmake_toolchain(Path::new("/sdk/ndk/27"), "aarch64-linux-android", 26);
        assert!(
            text.contains("if(NOT ANDROID_ABI)\n  set(ANDROID_ABI \"arm64-v8a\")\nendif()"),
            "{text}"
        );
        assert!(
            text.contains("set(ANDROID_PLATFORM \"android-26\")"),
            "{text}"
        );
        assert!(
            text.ends_with("include(\"/sdk/ndk/27/build/cmake/android.toolchain.cmake\")\n"),
            "{text}"
        );
        let x86 = cmake_toolchain(Path::new("/ndk"), "x86_64-linux-android", 31);
        assert!(
            x86.contains("\"x86_64\"") && x86.contains("android-31"),
            "{x86}"
        );
        // CMake reads `\` as an escape: a Windows path is written with `/`.
        let windows = cmake_toolchain(Path::new("C:\\sdk\\ndk"), "armv7-linux-androideabi", 26);
        assert!(
            windows.contains("include(\"C:/sdk/ndk/build/cmake/android.toolchain.cmake\")")
                && windows.contains("\"armeabi-v7a\""),
            "{windows}"
        );
    }

    #[test]
    fn the_hosts_name_the_ndk_toolchain_directory() {
        assert_eq!(host_tag(Os::Macos), "darwin-x86_64");
        assert_eq!(host_tag(Os::Linux), "linux-x86_64");
        assert_eq!(host_tag(Os::Windows), "windows-x86_64");
    }

    #[test]
    fn arm64_on_a_mac_links_with_the_ndk_clang_of_the_projects_api_level() {
        let env = env_of(
            Path::new("/sdk/ndk/27.2.12479018"),
            Os::Macos,
            "aarch64-linux-android",
            26,
        )
        .unwrap();
        let bin = "/sdk/ndk/27.2.12479018/toolchains/llvm/prebuilt/darwin-x86_64/bin";
        let clang = format!("{bin}/aarch64-linux-android26-clang");
        assert_eq!(
            get(&env, "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"),
            clang
        );
        assert_eq!(get(&env, "CC_aarch64_linux_android"), clang);
        assert_eq!(
            get(&env, "CXX_aarch64_linux_android"),
            format!("{bin}/aarch64-linux-android26-clang++")
        );
        assert_eq!(
            get(&env, "AR_aarch64_linux_android"),
            format!("{bin}/llvm-ar")
        );
        assert_eq!(
            get(&env, "RANLIB_aarch64_linux_android"),
            format!("{bin}/llvm-ranlib")
        );
        assert_eq!(get(&env, "ANDROID_NDK_HOME"), "/sdk/ndk/27.2.12479018");
    }

    #[test]
    fn x86_64_on_linux_follows_the_api_level_and_the_host() {
        let env = env_of(Path::new("/ndk"), Os::Linux, "x86_64-linux-android", 31).unwrap();
        assert_eq!(
            get(&env, "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"),
            "/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android31-clang"
        );
        assert_eq!(
            get(&env, "CC_x86_64_linux_android"),
            "/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android31-clang"
        );
        assert_eq!(
            get(&env, "AR_x86_64_linux_android"),
            "/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar"
        );
    }

    #[test]
    fn the_32_bit_abis_get_the_wrapper_names_the_ndk_gives_them() {
        let arm = env_of(Path::new("/ndk"), Os::Linux, "armv7-linux-androideabi", 26).unwrap();
        assert_eq!(
            get(&arm, "CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER"),
            "/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/armv7a-linux-androideabi26-clang"
        );
        // The cc crate's variable keeps the Rust triple, which says armv7.
        assert!(
            get(&arm, "CC_armv7_linux_androideabi").ends_with("armv7a-linux-androideabi26-clang")
        );
        let x86 = env_of(Path::new("/ndk"), Os::Linux, "i686-linux-android", 26).unwrap();
        assert!(
            get(&x86, "CARGO_TARGET_I686_LINUX_ANDROID_LINKER")
                .ends_with("i686-linux-android26-clang")
        );
    }

    #[test]
    fn windows_links_with_clang_exe_and_compiles_with_the_cmd_wrappers() {
        let env = env_of(
            Path::new("C:/ndk"),
            Os::Windows,
            "aarch64-linux-android",
            26,
        )
        .unwrap();
        // The linker is not a `.cmd` (cmd.exe would re-read the quoting of rustc's arguments), so the target comes as an argument.
        assert!(
            get(&env, "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER")
                .ends_with("windows-x86_64/bin/clang.exe")
        );
        assert!(
            get(&env, "CC_aarch64_linux_android")
                .ends_with("windows-x86_64/bin/aarch64-linux-android26-clang.cmd")
        );
        assert!(get(&env, "AR_aarch64_linux_android").ends_with("windows-x86_64/bin/llvm-ar.exe"));
        assert!(
            rustc_args(Os::Windows, "aarch64-linux-android", 26)
                .contains(&"link-arg=--target=aarch64-linux-android26".to_owned())
        );
        assert!(
            rustc_args(Os::Windows, "armv7-linux-androideabi", 21)
                .contains(&"link-arg=--target=armv7a-linux-androideabi21".to_owned()),
            "the 32-bit ABI keeps the NDK's armv7a name"
        );
    }

    #[test]
    fn macos_and_linux_pass_no_target_the_wrapper_has_it() {
        for os in [Os::Macos, Os::Linux] {
            assert!(
                !rustc_args(os, "aarch64-linux-android", 26)
                    .iter()
                    .any(|a| a.contains("--target")),
                "{os:?}"
            );
        }
    }

    #[test]
    fn on_windows_both_the_exe_and_the_wrapper_of_the_level_are_required() {
        let ndk = Path::new("C:/ndk");
        let bin = "C:/ndk/toolchains/llvm/prebuilt/windows-x86_64/bin";
        let has_exe = FakeSys {
            os: Some(Os::Windows),
            ..FakeSys::default()
        }
        .with_file(&format!("{bin}/clang.exe"))
        .with_file(&format!("{bin}/llvm-ar.exe"));
        // No wrapper for the level: the NDK is older than min_sdk.
        assert_eq!(
            missing_tools(&has_exe, ndk, Os::Windows, "aarch64-linux-android", 99),
            [PathBuf::from(format!(
                "{bin}/aarch64-linux-android99-clang.cmd"
            ))]
        );
    }

    #[test]
    fn a_64_bit_library_is_linked_for_16_kb_pages_and_a_build_id_and_a_32_bit_one_for_the_id() {
        assert_eq!(
            rustc_args(Os::Linux, "aarch64-linux-android", 26),
            [
                "-C",
                "link-arg=-Wl,--build-id=sha1",
                "-C",
                "link-arg=-Wl,-z,max-page-size=16384"
            ]
        );
        assert_eq!(
            rustc_args(Os::Linux, "x86_64-linux-android", 26),
            rustc_args(Os::Linux, "aarch64-linux-android", 26)
        );
        assert_eq!(
            rustc_args(Os::Linux, "armv7-linux-androideabi", 26),
            ["-C", "link-arg=-Wl,--build-id=sha1"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused_not_mangled() {
        use std::os::unix::ffi::OsStrExt;
        let ndk = Path::new(std::ffi::OsStr::from_bytes(b"/sdk/ndk/27\xff"));
        let error = env_of(ndk, Os::Linux, "aarch64-linux-android", 26).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("C0003") && text.contains("not valid UTF-8"),
            "{text}"
        );
        assert!(text.contains("ANDROID_NDK_HOME"), "{text}");
    }

    #[test]
    fn the_cargo_variable_is_the_triple_in_capitals() {
        assert_eq!(
            cargo_env_triple("aarch64-linux-android"),
            "AARCH64_LINUX_ANDROID"
        );
        assert_eq!(
            cc_env_triple("x86_64-linux-android"),
            "x86_64_linux_android"
        );
    }

    #[test]
    fn missing_tools_names_what_an_incomplete_ndk_lacks() {
        let ndk = Path::new("/ndk");
        let bin = "/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin";
        let whole = FakeSys::linux()
            .with_file(&format!("{bin}/aarch64-linux-android26-clang"))
            .with_file(&format!("{bin}/llvm-ar"));
        assert!(missing_tools(&whole, ndk, Os::Linux, "aarch64-linux-android", 26).is_empty());
        let no_ar = FakeSys::linux().with_file(&format!("{bin}/aarch64-linux-android26-clang"));
        assert_eq!(
            missing_tools(&no_ar, ndk, Os::Linux, "aarch64-linux-android", 26),
            [PathBuf::from(format!("{bin}/llvm-ar"))]
        );
        // An NDK older than the API level has no wrapper for it.
        assert_eq!(
            missing_tools(&whole, ndk, Os::Linux, "aarch64-linux-android", 99),
            [PathBuf::from(format!(
                "{bin}/aarch64-linux-android99-clang"
            ))]
        );
    }
}
