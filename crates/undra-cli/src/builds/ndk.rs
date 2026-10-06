//! The Android NDK as Cargo sees it: the environment that makes `cargo build --target <triple>` link
//! with the NDK's clang (ADR-065).
//!
//! Cargo needs four things to cross-build a Rust library for Android and nothing else: a linker, a C
//! compiler and archiver for the build scripts of crates with C code (the `cc` crate reads `CC_<triple>`
//! and `AR_<triple>`), and the arguments the NDK's linker needs to align the library to 16 KB pages.
//! All of it is in the NDK, under `toolchains/llvm/prebuilt/<host>/bin`, where each `<triple><api>-clang`
//! is a wrapper that already knows its `--target` and sysroot. [`environment`] computes the variables
//! from the NDK's directory, the host and the API level, without running anything; the Android build
//! (and the Bazel rule, through the CLI) sets them on a plain Cargo command. No `cargo-ndk`.

use std::path::{Path, PathBuf};

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
///
/// Nothing is run and nothing is read from the machine: the paths are computed (a missing file is
/// [`missing_tools`]'s to report).
#[must_use]
pub fn environment(ndk: &Path, os: Os, triple: &str, api: u32) -> Vec<(String, String)> {
    let path = |p: PathBuf| p.to_string_lossy().into_owned();
    let cc_triple = cc_env_triple(triple);
    vec![
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
    ]
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

    #[test]
    fn the_hosts_name_the_ndk_toolchain_directory() {
        assert_eq!(host_tag(Os::Macos), "darwin-x86_64");
        assert_eq!(host_tag(Os::Linux), "linux-x86_64");
        assert_eq!(host_tag(Os::Windows), "windows-x86_64");
    }

    #[test]
    fn arm64_on_a_mac_links_with_the_ndk_clang_of_the_projects_api_level() {
        let env = environment(
            Path::new("/sdk/ndk/27.2.12479018"),
            Os::Macos,
            "aarch64-linux-android",
            26,
        );
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
        let env = environment(Path::new("/ndk"), Os::Linux, "x86_64-linux-android", 31);
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
        let arm = environment(Path::new("/ndk"), Os::Linux, "armv7-linux-androideabi", 26);
        assert_eq!(
            get(&arm, "CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER"),
            "/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/armv7a-linux-androideabi26-clang"
        );
        // The cc crate's variable keeps the Rust triple, which says armv7.
        assert!(
            get(&arm, "CC_armv7_linux_androideabi").ends_with("armv7a-linux-androideabi26-clang")
        );
        let x86 = environment(Path::new("/ndk"), Os::Linux, "i686-linux-android", 26);
        assert!(
            get(&x86, "CARGO_TARGET_I686_LINUX_ANDROID_LINKER")
                .ends_with("i686-linux-android26-clang")
        );
    }

    #[test]
    fn windows_links_with_clang_exe_and_compiles_with_the_cmd_wrappers() {
        let env = environment(
            Path::new("C:/ndk"),
            Os::Windows,
            "aarch64-linux-android",
            26,
        );
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
