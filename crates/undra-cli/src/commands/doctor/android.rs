//! The Android checks: the SDK and its environment variables, the NDK, the JDK,
//! `adb` and an attached device, the Gradle wrapper and the Rust targets of the project's ABIs.

use std::path::{Path, PathBuf};

use crate::adb;
use crate::sys::Os;
use crate::toolchain::{NDK_VARIABLES, UnusableNdk, ndk_major, newest_sdk_ndk};

use super::finding::{Check, Finding, State};
use super::rust::target;
use super::{Context, brew, version_line};

/// The Android SDK directory.
pub const SDK: Check = Check::new("android.sdk", "android-sdk");
/// `ANDROID_HOME` (or `ANDROID_SDK_ROOT`), which Gradle and Android Studio read.
pub const HOME: Check = Check::new("android.home", "android-sdk");
/// A platform to compile the app against.
pub const PLATFORM: Check = Check::new("android.platform", "android-sdk");
/// `adb`, from the SDK's `platform-tools`.
pub const ADB: Check = Check::new("android.adb", "adb-and-a-device").optional();
/// A device or an emulator that is attached and ready.
pub const DEVICE: Check = Check::new("android.device", "adb-and-a-device").optional();
/// The NDK, r27 or newer.
pub const NDK: Check = Check::new("android.ndk", "android-ndk");
/// `ANDROID_NDK_HOME`, which Gradle and `ndk-build` read (`undra build` finds the NDK without it).
pub const NDK_HOME: Check = Check::new("android.ndk-home", "android-ndk");
/// A JDK, 17 or newer, for Gradle.
pub const JDK: Check = Check::new("android.jdk", "jdk-17");
/// The Gradle wrapper of the project's Android app.
pub const GRADLE_WRAPPER: Check = Check::new("android.gradle-wrapper", "gradle-wrapper").optional();
/// The NDK's LLDB: what steps into Rust from Android Studio's "Dual (Java + Native)" debugger and
/// `ndk-lldb` (ADR-046).
pub const LLDB: Check = Check::new("android.lldb", "debugging-into-rust").optional();
/// The `undra` emulator that Undra's own device tests and benchmarks boot.
pub const AVD: Check =
    Check::new("android.avd", "the-undra-emulator-contributors").for_contributors();

/// Every check of this module (the Rust targets are in [`super::rust::ALL`]).
#[cfg(test)]
pub const ALL: &[Check] = &[
    SDK,
    HOME,
    PLATFORM,
    ADB,
    DEVICE,
    NDK,
    NDK_HOME,
    JDK,
    GRADLE_WRAPPER,
    LLDB,
    AVD,
];

/// The NDK the project is built and tested with (`docs/ONBOARDING.md`).
pub const NDK_VERSION: &str = "27.2.12479018";
/// The oldest NDK whose libraries are 16 KB page aligned, which Google Play requires.
pub const MIN_NDK: u32 = 27;
/// The platform the generated app compiles against (`compileSdk`).
pub const COMPILE_SDK: u32 = 35;
/// The JDK the Android Gradle plugin needs.
pub const MIN_JDK: u32 = 17;
/// The Gradle version of the wrapper `undra init` creates.
pub const GRADLE_VERSION: &str = "8.14.3";

/// `openjdk version "17.0.12" ...` / `java version "1.8.0"` into the major version.
#[must_use]
pub fn java_major(line: &str) -> Option<u32> {
    let quoted = line.split('"').nth(1)?;
    let mut parts = quoted.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// The highest `android-<N>` platform in `<sdk>/platforms`.
fn newest_platform(cx: &Context<'_>, sdk: &Path) -> Option<u32> {
    cx.sys
        .list_dir(&sdk.join("platforms"))
        .iter()
        .filter_map(|name| name.strip_prefix("android-")?.parse::<u32>().ok())
        .max()
}

fn quoted_path(path: &str) -> String {
    if path.contains(' ') {
        format!("\"{path}\"")
    } else {
        path.to_owned()
    }
}

/// The `sdkmanager` command line to install `packages`, using the SDK's own when it is known.
fn sdkmanager(sdk: Option<&Path>, packages: &[&str]) -> String {
    let tool = sdk.map_or_else(
        || "\"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager\"".to_owned(),
        |sdk| {
            quoted_path(
                &sdk.join("cmdline-tools/latest/bin/sdkmanager")
                    .to_string_lossy(),
            )
        },
    );
    let list = packages
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{tool} {list}")
}

/// The commands that install the Android SDK's command line tools.
fn install_sdk(cx: &Context<'_>) -> Vec<String> {
    if cx.sys.os() == Os::Macos {
        // Where the cask goes is Homebrew's prefix: /opt/homebrew on Apple silicon, /usr/local on Intel.
        let mut out = brew(cx, "install --cask android-commandlinetools");
        out.extend([
            "export ANDROID_HOME=\"$(brew --prefix)/share/android-commandlinetools\"".to_owned(),
            "yes | \"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager\" --licenses".to_owned(),
        ]);
        out
    } else {
        vec![
            "mkdir -p \"$HOME/Android/Sdk/cmdline-tools\"".to_owned(),
            "curl -fsSLo /tmp/cmdline-tools.zip https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip".to_owned(),
            "unzip -q /tmp/cmdline-tools.zip -d \"$HOME/Android/Sdk/cmdline-tools\" && mv \"$HOME/Android/Sdk/cmdline-tools/cmdline-tools\" \"$HOME/Android/Sdk/cmdline-tools/latest\"".to_owned(),
            "export ANDROID_HOME=\"$HOME/Android/Sdk\"".to_owned(),
            "yes | \"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager\" --licenses".to_owned(),
        ]
    }
}

/// Every Android finding.
#[must_use]
pub fn check(cx: &Context<'_>) -> Vec<Finding> {
    let mut out = Vec::new();
    let sdk = cx.toolchain.android_sdk.clone();
    out.extend(sdk_findings(cx, sdk.as_deref()));
    out.extend(adb_findings(cx, sdk.as_deref()));
    out.push(ndk(cx, sdk.as_deref()));
    if let Some(unusable) = &cx.toolchain.android_ndk_unusable {
        out.push(unusable_ndk_home(cx, unusable, sdk.as_deref()));
    }
    if let Some(ndk) = &cx.toolchain.android_ndk {
        out.push(ndk_home(cx, ndk));
        out.push(lldb(cx, ndk, sdk.as_deref()));
    }
    for abi in cx.android_abis {
        let triple = crate::builds::android::triple_of(abi);
        out.push(target(cx, triple, &format!("the {abi} ABI"), false));
    }
    out.push(jdk(cx));
    out.push(gradle_wrapper(cx));
    out
}

fn sdk_findings(cx: &Context<'_>, sdk: Option<&Path>) -> Vec<Finding> {
    let Some(sdk) = sdk else {
        return vec![
            SDK.missing(
                "no Android SDK found (ANDROID_HOME is not set and the usual locations are empty)",
                &install_sdk(cx)
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ),
        ];
    };
    let mut out = vec![SDK.ok_with(
        format!("Android SDK at {}", sdk.display()),
        sdk.display().to_string(),
    )];
    let env_value = cx
        .sys
        .env("ANDROID_HOME")
        .or_else(|| cx.sys.env("ANDROID_SDK_ROOT"));
    out.push(match env_value {
        Some(value) => HOME.ok_with(format!("ANDROID_HOME is {value}"), value),
        None => HOME.warn(
            State::Missing,
            None,
            format!(
                "ANDROID_HOME is not set (undra finds the SDK at {} anyway; Gradle and Android Studio may not)",
                sdk.display()
            ),
            &[&format!("export ANDROID_HOME={}", quoted_path(&sdk.to_string_lossy()))],
        ),
    });
    out.push(match newest_platform(cx, sdk) {
        Some(n) if n >= COMPILE_SDK => {
            PLATFORM.ok_with(format!("Android platform {n} installed"), format!("android-{n}"))
        }
        Some(n) => PLATFORM.warn(
            State::WrongVersion,
            Some(format!("android-{n}")),
            format!("the newest Android platform in the SDK is {n}; the app shell compiles against {COMPILE_SDK}"),
            &[&sdkmanager(Some(sdk), &[&format!("platforms;android-{COMPILE_SDK}")])],
        ),
        None => PLATFORM.warn(
            State::Missing,
            None,
            format!("no Android platform in the SDK (the app shell compiles against {COMPILE_SDK})"),
            &[&sdkmanager(Some(sdk), &[&format!("platforms;android-{COMPILE_SDK}")])],
        ),
    });
    out
}

fn adb_findings(cx: &Context<'_>, sdk: Option<&Path>) -> Vec<Finding> {
    let platform_tools = sdkmanager(sdk, &["platform-tools"]);
    let Some(adb_path) = adb::find(cx.sys, cx.toolchain) else {
        return vec![ADB.warn_missing(
            "adb was not found (it is in the SDK's platform-tools; `undra dev --android` and running the app on a device need it)",
            &[&platform_tools],
        )];
    };
    let version = version_line(cx, &adb_path, &["--version"])
        .unwrap_or_else(|| format!("adb at {}", adb_path.display()));
    let mut out = vec![ADB.ok_with(version, adb_path.display().to_string())];
    out.push(device(cx, &adb_path, sdk));
    out
}

fn device(cx: &Context<'_>, adb_path: &Path, sdk: Option<&Path>) -> Finding {
    let listing = cx
        .sys
        .run(adb_path, &["devices"], &cx.toolchain.env_pairs())
        .filter(|o| o.success);
    let Some(listing) = listing else {
        return DEVICE.warn(
            State::Missing,
            None,
            "`adb devices` failed",
            &[&restart_adb(adb_path)],
        );
    };
    let devices = adb::parse_devices(&listing.stdout);
    let ready: Vec<String> = devices
        .iter()
        .filter(|d| d.is_ready())
        .map(|d| d.serial.clone())
        .collect();
    if !ready.is_empty() {
        return DEVICE.ok_with(
            format!(
                "{} attached: {}",
                if ready.len() == 1 {
                    "1 device or emulator".to_owned()
                } else {
                    format!("{} devices or emulators", ready.len())
                },
                ready.join(", ")
            ),
            ready.join(", "),
        );
    }
    if !devices.is_empty() {
        let states: Vec<String> = devices
            .iter()
            .map(|d| format!("{} is {}", d.serial, d.state))
            .collect();
        return DEVICE.warn(
            State::Missing,
            Some(states.join(", ")),
            format!("no ready device: {} (accept the USB debugging prompt on the phone, or restart adb)", states.join(", ")),
            &[&restart_adb(adb_path)],
        );
    }
    DEVICE.warn_missing(
        "no device or emulator is attached (needed to run the app, not to build it)",
        &start_emulator(cx, sdk)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
}

/// Restarts the adb server with the `adb` that was found (the SDK's need not be on `PATH`).
fn restart_adb(adb_path: &Path) -> String {
    let adb = quoted_path(&adb_path.to_string_lossy());
    format!("{adb} kill-server && {adb} start-server")
}

/// The `emulator` binary: the SDK's, else one on `PATH`.
fn emulator_path(cx: &Context<'_>, sdk: Option<&Path>) -> Option<PathBuf> {
    if let Some(sdk) = sdk {
        let candidate = sdk.join("emulator/emulator");
        if cx.sys.is_file(&candidate) {
            return Some(candidate);
        }
    }
    cx.toolchain.which(cx.sys, "emulator")
}

/// The names of the virtual devices (`emulator -list-avds`).
fn avds(cx: &Context<'_>, sdk: Option<&Path>) -> Option<Vec<String>> {
    let emulator = emulator_path(cx, sdk)?;
    let out = cx
        .sys
        .run(&emulator, &["-list-avds"], &cx.toolchain.env_pairs())
        .filter(|o| o.success)?;
    Some(
        out.stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("INFO") && !l.contains(' '))
            .map(ToOwned::to_owned)
            .collect(),
    )
}

/// The system image `docs/ONBOARDING.md` creates the `undra` emulator from.
fn system_image() -> String {
    let abi = if std::env::consts::ARCH == "aarch64" {
        "arm64-v8a"
    } else {
        "x86_64"
    };
    format!("system-images;android-{COMPILE_SDK};google_apis;{abi}")
}

/// The commands that create the `undra` emulator.
fn create_avd(sdk: Option<&Path>) -> Vec<String> {
    let image = system_image();
    let avdmanager = sdk.map_or_else(
        || "\"$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager\"".to_owned(),
        |sdk| {
            quoted_path(
                &sdk.join("cmdline-tools/latest/bin/avdmanager")
                    .to_string_lossy(),
            )
        },
    );
    vec![
        sdkmanager(sdk, &["emulator", &image]),
        format!("{avdmanager} create avd -n undra -k \"{image}\" -d pixel_7"),
    ]
}

/// What starts an emulator: the first virtual device there is, or the commands that make one.
fn start_emulator(cx: &Context<'_>, sdk: Option<&Path>) -> Vec<String> {
    match (avds(cx, sdk), emulator_path(cx, sdk)) {
        (Some(names), Some(emulator)) if !names.is_empty() => vec![format!(
            "{} -avd {} &",
            quoted_path(&emulator.to_string_lossy()),
            names[0]
        )],
        _ => create_avd(sdk),
    }
}

fn ndk(cx: &Context<'_>, sdk: Option<&Path>) -> Finding {
    let install = sdkmanager(sdk, &[&format!("ndk;{NDK_VERSION}")]);
    match &cx.toolchain.android_ndk {
        Some(ndk) => match ndk_major(ndk) {
            Some(major) if major >= MIN_NDK => NDK.ok_with(
                format!("Android NDK r{major} at {}", ndk.display()),
                ndk.display().to_string(),
            ),
            Some(major) => NDK.wrong_version(
                ndk.display().to_string(),
                format!(
                    "Android NDK r{major} is too old: libraries need r{MIN_NDK}+ for 16 KB page alignment"
                ),
                &[&install],
            ),
            None => NDK.ok_with(
                format!("Android NDK at {}", ndk.display()),
                ndk.display().to_string(),
            ),
        },
        None => match &cx.toolchain.android_ndk_unusable {
            Some(unusable) => NDK.missing(
                format!(
                    "no Android NDK: {} is {}, which is not a directory (`undra build` stops on it rather than use another NDK)",
                    unusable.variable,
                    unusable.path.display()
                ),
                &[&unusable_fix(cx, unusable, sdk).unwrap_or(install)],
            ),
            None => NDK.missing(
                "no Android NDK found (ANDROID_NDK_HOME is not set and the SDK has no ndk/ directory)",
                &[&install],
            ),
        },
    }
}

/// The NDK variable set to a path that is not a directory: a failure, which `undra build --platform
/// android` stops on too ([`UnusableNdk::error`]). The fix points the variable at the SDK's newest NDK
/// when there is one, else unsets it.
fn unusable_ndk_home(cx: &Context<'_>, unusable: &UnusableNdk, sdk: Option<&Path>) -> Finding {
    let variable = unusable.variable;
    let fix = unusable_fix(cx, unusable, sdk).unwrap_or_else(|| format!("unset {variable}"));
    NDK_HOME.fail(
        State::Missing,
        Some(unusable.path.display().to_string()),
        format!(
            "{variable} is {}, which is not a directory: `undra build --platform android` stops on it (C0003) rather than use an NDK the variable did not choose",
            unusable.path.display()
        ),
        &[&fix],
    )
}

/// The NDK's LLDB, which `ndk-lldb` and Android Studio's native debugger use to step into the core.
fn lldb(cx: &Context<'_>, ndk: &Path, sdk: Option<&Path>) -> Finding {
    let found = crate::symbols::tools::ndk_bin(cx.sys, ndk).and_then(|bin| {
        ["lldb", "lldb.sh"]
            .iter()
            .map(|name| bin.join(name))
            .find(|path| cx.sys.is_file(path))
    });
    match found {
        Some(path) => LLDB.ok_with(
            format!("the NDK's LLDB: {}", path.display()),
            path.display().to_string(),
        ),
        None => LLDB.warn_missing(
            format!(
                "the NDK at {} has no LLDB, so native debugging of the core from Android Studio or `ndk-lldb` has none to use",
                ndk.display()
            ),
            &[&sdkmanager(sdk, &["lldb;3.1"])],
        ),
    }
}

fn ndk_home(cx: &Context<'_>, ndk: &Path) -> Finding {
    let set = NDK_VARIABLES
        .iter()
        .find_map(|key| cx.sys.env(key).map(|v| ((*key).to_owned(), v)));
    match set {
        Some((key, value)) => NDK_HOME.ok_with(format!("{key} is {value}"), value),
        None => NDK_HOME.warn(
            State::Missing,
            None,
            format!(
                "ANDROID_NDK_HOME is not set (undra finds the NDK at {} anyway; Gradle and other tools may not)",
                ndk.display()
            ),
            &[&format!("export ANDROID_NDK_HOME={}", quoted_path(&ndk.to_string_lossy()))],
        ),
    }
}

/// The command that points an unusable NDK variable at the SDK's newest NDK, when the SDK has one.
fn unusable_fix(cx: &Context<'_>, unusable: &UnusableNdk, sdk: Option<&Path>) -> Option<String> {
    let found = newest_sdk_ndk(cx.sys, sdk?)?;
    Some(format!(
        "export {}={}",
        unusable.variable,
        quoted_path(&found.to_string_lossy())
    ))
}

/// The JDK check: Gradle and the Android Gradle plugin need one, 17 or newer.
fn jdk(cx: &Context<'_>) -> Finding {
    // A gap blocks the app build when there is an app (a project); elsewhere it is advice.
    let in_project = cx.project.is_some();
    let install_list = if cx.sys.os() == Os::Macos {
        // Homebrew's openjdk@17 is keg-only: installed, it is still not on PATH.
        let mut out = brew(cx, "install openjdk@17");
        out.extend([
            "export JAVA_HOME=\"$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home\""
                .to_owned(),
            "export PATH=\"$JAVA_HOME/bin:$PATH\"".to_owned(),
        ]);
        out
    } else {
        vec!["sudo apt-get install -y openjdk-17-jdk".to_owned()]
    };
    let install: &[&str] = &install_list.iter().map(String::as_str).collect::<Vec<_>>();
    if let Some(java) = cx.toolchain.which(cx.sys, "java") {
        let out = cx.sys.run(&java, &["-version"], &cx.toolchain.env_pairs());
        if let Some(out) = out.filter(|o| o.success) {
            let text = out.text();
            let first = text.lines().next().unwrap_or("java").to_owned();
            return match java_major(&first) {
                Some(major) if major >= MIN_JDK => JDK.ok(first),
                Some(major) => {
                    let message = format!(
                        "{first}: Android Gradle plugins need JDK {MIN_JDK} or newer (Java {major} found)"
                    );
                    if in_project {
                        JDK.wrong_version(first, message, install)
                    } else {
                        JDK.warn_version(first, message, install)
                    }
                }
                None => JDK.ok(first),
            };
        }
    }
    // A JDK that is installed but not on PATH (Homebrew's openjdk is keg-only).
    for candidate in [
        "/opt/homebrew/opt/openjdk@17",
        "/opt/homebrew/opt/openjdk",
        "/usr/local/opt/openjdk@17",
        "/usr/local/opt/openjdk",
    ] {
        if cx.sys.is_file(&Path::new(candidate).join("bin/java")) {
            let fix = [
                &format!("export JAVA_HOME={candidate}/libexec/openjdk.jdk/Contents/Home") as &str,
                "export PATH=\"$JAVA_HOME/bin:$PATH\"",
            ];
            let message = format!("a JDK is installed at {candidate} but `java` is not on PATH");
            // `./gradlew` cannot start without `java` on PATH or JAVA_HOME: in a project, a failure.
            return if in_project {
                JDK.fail(State::Missing, Some(candidate.to_owned()), message, &fix)
            } else {
                JDK.warn(State::Missing, Some(candidate.to_owned()), message, &fix)
            };
        }
    }
    let message =
        "no JDK found (Gradle and Android Studio builds need one; `undra build` does not)";
    if in_project {
        JDK.missing(message, install)
    } else {
        JDK.warn_missing(message, install)
    }
}

fn gradle_wrapper(cx: &Context<'_>) -> Finding {
    let Some(root) = cx.project else {
        return GRADLE_WRAPPER.skip(
            "inside a project, doctor checks that android/gradlew exists (`undra init` creates it)",
        );
    };
    let wrapper = root.join("android/gradlew");
    if cx.sys.is_file(&wrapper) {
        return GRADLE_WRAPPER.ok_with("android/gradlew is there", wrapper.display().to_string());
    }
    let create = format!("(cd android && gradle wrapper --gradle-version {GRADLE_VERSION})");
    if cx.toolchain.which(cx.sys, "gradle").is_some() {
        GRADLE_WRAPPER.warn_missing(
            "android/gradlew is missing (Android Studio creates one when it opens android/, or Gradle can)",
            &[&create],
        )
    } else {
        let mut fix = if cx.sys.os() == Os::Macos {
            brew(cx, "install gradle")
        } else {
            // A distribution's own Gradle is too old for JDK 17; SDKMAN has the current one.
            vec![
                "curl -s \"https://get.sdkman.io\" | bash".to_owned(),
                "source \"$HOME/.sdkman/bin/sdkman-init.sh\"".to_owned(),
                "sdk install gradle".to_owned(),
            ]
        };
        fix.push(create);
        GRADLE_WRAPPER.warn_missing(
            "android/gradlew is missing and Gradle is not installed to create it (Android Studio also creates one when it opens android/)",
            &fix.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }
}

/// The `undra` virtual device, for contributors.
#[must_use]
pub fn undra_avd(cx: &Context<'_>) -> Finding {
    let sdk = cx.toolchain.android_sdk.clone();
    match avds(cx, sdk.as_deref()) {
        None => AVD.warn_missing(
            "the Android emulator is not installed (Undra's device tests and benchmarks boot the AVD named undra)",
            &create_avd(sdk.as_deref()).iter().map(String::as_str).collect::<Vec<_>>(),
        ),
        Some(names) if names.iter().any(|n| n == "undra") => {
            AVD.ok_with("AVD undra exists".to_owned(), names.join(", "))
        }
        Some(names) => AVD.warn(
            State::Missing,
            Some(names.join(", ")),
            "no AVD named undra (Undra's device tests and benchmarks boot it)",
            &create_avd(sdk.as_deref()).iter().map(String::as_str).collect::<Vec<_>>(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::*;
    use crate::commands::doctor::finding::Status;

    const SDK_DIR: &str = "/opt/homebrew/share/android-commandlinetools";

    #[test]
    fn the_ndks_lldb_is_checked_for_native_debugging() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.lldb").clone();
        assert_eq!(f.status, Status::Ok, "{f:?}");
        assert!(f.message.contains("bin/lldb"), "{f:?}");
        // An NDK directory with nothing in it: a warning with the sdkmanager line, never a failure.
        let sys = good_machine_without_ndk().with_dir(&format!("{SDK_DIR}/ndk/27.2.12479018"));
        let f = by_id(&scan(&sys, &["android"]), "android.lldb").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing), "{f:?}");
        assert!(f.fix[0].contains("lldb;3.1"), "{f:?}");
        assert_eq!(f.anchor, "debugging-into-rust");
    }

    #[test]
    fn versions_are_parsed() {
        assert_eq!(
            java_major("openjdk version \"17.0.12\" 2024-07-16"),
            Some(17)
        );
        assert_eq!(java_major("java version \"1.8.0_292\""), Some(8));
        assert_eq!(java_major("nonsense"), None);
    }

    #[test]
    fn the_sdk_is_found_or_installed_with_the_commands_of_the_os() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.sdk").clone();
        assert_eq!(f.status, Status::Ok);
        assert_eq!(f.observed.as_deref(), Some(SDK_DIR));

        let f = by_id(&scan(&bare_mac_with_brew(), &["android"]), "android.sdk").clone();
        assert_eq!((f.status, f.state), (Status::Fail, State::Missing));
        assert_eq!(f.fix[0], "brew install --cask android-commandlinetools");
        assert!(
            f.fix
                .iter()
                .any(|c| c.contains("sdkmanager") && c.contains("--licenses")),
            "{f:?}"
        );

        let f = by_id(&scan(&bare_machine(), &["android"]), "android.sdk").clone();
        assert!(
            f.fix.iter().any(|c| c.contains("commandlinetools-linux")),
            "{f:?}"
        );
    }

    #[test]
    fn android_home_should_be_set() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.home").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert_eq!(f.fix, [format!("export ANDROID_HOME={SDK_DIR}")]);

        let set = good_machine().with_env("ANDROID_HOME", SDK_DIR);
        let f = by_id(&scan(&set, &["android"]), "android.home").clone();
        assert_eq!(f.status, Status::Ok);
        assert!(f.message.contains("ANDROID_HOME is"), "{f:?}");
    }

    #[test]
    fn the_app_compiles_against_platform_35() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.platform").clone();
        assert_eq!(f.status, Status::Ok);

        let old =
            good_machine_without_platform().with_dir(&format!("{SDK_DIR}/platforms/android-34"));
        let f = by_id(&scan(&old, &["android"]), "android.platform").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::WrongVersion));
        assert!(f.fix[0].contains("platforms;android-35"), "{f:?}");

        let none = good_machine_without_platform();
        let f = by_id(&scan(&none, &["android"]), "android.platform").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));

        let newer = good_machine().with_dir(&format!("{SDK_DIR}/platforms/android-36"));
        assert_eq!(
            by_id(&scan(&newer, &["android"]), "android.platform").status,
            Status::Ok
        );
    }

    #[test]
    fn adb_comes_from_the_path_or_the_sdk_and_is_optional() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.adb").clone();
        assert_eq!(f.status, Status::Ok);
        assert!(f.message.contains("Android Debug Bridge version"), "{f:?}");

        let without = good_machine_without_adb();
        let f = by_id(&scan(&without, &["android"]), "android.adb").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert!(f.fix[0].contains("platform-tools"), "{f:?}");
        // No adb, so no device finding either.
        assert!(
            scan(&without, &["android"])
                .findings()
                .all(|f| f.id != "android.device")
        );
    }

    #[test]
    fn an_attached_device_or_emulator_is_reported() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.device").clone();
        assert_eq!(f.status, Status::Ok);
        assert_eq!(f.observed.as_deref(), Some("emulator-5554"));

        let two = good_machine().with_output(
            "adb",
            "devices",
            "List of devices attached\nemulator-5554\tdevice\nphone1\tdevice\n\n",
        );
        let f = by_id(&scan(&two, &["android"]), "android.device").clone();
        assert!(
            f.message.contains("2 devices or emulators attached"),
            "{f:?}"
        );

        let unauthorized = good_machine().with_output(
            "adb",
            "devices",
            "List of devices attached\nphone1\tunauthorized\n\n",
        );
        let f = by_id(&scan(&unauthorized, &["android"]), "android.device").clone();
        assert_eq!(f.status, Status::Warn);
        assert!(f.message.contains("phone1 is unauthorized"), "{f:?}");

        let broken =
            good_machine().with_failing_output("adb", "devices", "cannot connect to daemon");
        let f = by_id(&scan(&broken, &["android"]), "android.device").clone();
        assert!(f.message.contains("`adb devices` failed"), "{f:?}");
    }

    #[test]
    fn no_device_says_how_to_start_an_emulator() {
        let none = good_machine().with_output("adb", "devices", "List of devices attached\n\n");
        let f = by_id(&scan(&none, &["android"]), "android.device").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        // The good machine has the undra AVD: the fix boots it.
        assert!(
            f.fix[0].contains("emulator") && f.fix[0].contains("-avd undra"),
            "{f:?}"
        );

        // Without an AVD the fix creates one.
        let bare_avds = none.with_output("emulator", "-list-avds", "");
        let f = by_id(&scan(&bare_avds, &["android"]), "android.device").clone();
        assert!(
            f.fix
                .iter()
                .any(|c| c.contains("avdmanager") && c.contains("create avd -n undra")),
            "{f:?}"
        );
    }

    #[test]
    fn the_ndk_must_be_r27_or_newer() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.ndk").clone();
        assert_eq!(f.status, Status::Ok);
        assert!(f.message.contains("NDK r27"), "{f:?}");

        let old = good_machine_with_ndk("25.2.9519653");
        let f = by_id(&scan(&old, &["android"]), "android.ndk").clone();
        assert_eq!((f.status, f.state), (Status::Fail, State::WrongVersion));
        assert!(f.message.contains("r25"), "{f:?}");
        assert!(f.fix[0].contains("ndk;27.2.12479018"), "{f:?}");

        let none = good_machine_without_ndk();
        let f = by_id(&scan(&none, &["android"]), "android.ndk").clone();
        assert_eq!((f.status, f.state), (Status::Fail, State::Missing));
        assert!(
            f.fix[0].contains("cmdline-tools/latest/bin/sdkmanager"),
            "{f:?}"
        );
        // No NDK, so nothing to say about its variable.
        assert!(
            scan(&none, &["android"])
                .findings()
                .all(|f| f.id != "android.ndk-home")
        );
    }

    #[test]
    fn android_ndk_home_should_be_set() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.ndk-home").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert!(f.fix[0].starts_with("export ANDROID_NDK_HOME="), "{f:?}");

        let set =
            good_machine().with_env("ANDROID_NDK_HOME", &format!("{SDK_DIR}/ndk/27.2.12479018"));
        let f = by_id(&scan(&set, &["android"]), "android.ndk-home").clone();
        assert_eq!(f.status, Status::Ok);
    }

    #[test]
    fn an_ndk_variable_that_names_no_directory_is_a_problem_not_ok() {
        let typo = good_machine().with_env("ANDROID_NDK_HOME", "/typo/ndk");
        let report = scan(&typo, &["android"]);
        let home = by_id(&report, "android.ndk-home").clone();
        assert_eq!(
            (home.status, home.state),
            (Status::Fail, State::Missing),
            "{home:?}"
        );
        assert!(
            home.message
                .starts_with("ANDROID_NDK_HOME is /typo/ndk, which is not a directory"),
            "{home:?}"
        );
        assert_eq!(home.observed.as_deref(), Some("/typo/ndk"));
        // The fix points it at the NDK the SDK has.
        assert_eq!(
            home.fix,
            [format!(
                "export ANDROID_NDK_HOME={SDK_DIR}/ndk/27.2.12479018"
            )]
        );
        // The NDK itself is not reported as found at the SDK's directory.
        let ndk = by_id(&report, "android.ndk").clone();
        assert_eq!(
            (ndk.status, ndk.state),
            (Status::Fail, State::Missing),
            "{ndk:?}"
        );
        assert!(
            ndk.message.contains("ANDROID_NDK_HOME is /typo/ndk"),
            "{ndk:?}"
        );
        assert_eq!(
            ndk.fix, home.fix,
            "the NDK is there: the variable is the fix"
        );
        assert!(!report.passed());

        // Without an NDK in the SDK, the fix unsets the variable.
        let nothing = good_machine_without_ndk().with_env("ANDROID_NDK_ROOT", "/gone");
        let home = by_id(&scan(&nothing, &["android"]), "android.ndk-home").clone();
        assert_eq!(home.status, Status::Fail);
        assert_eq!(home.fix, ["unset ANDROID_NDK_ROOT"]);
        let ndk = by_id(&scan(&nothing, &["android"]), "android.ndk").clone();
        assert!(
            ndk.fix[0].contains("ndk;27.2.12479018"),
            "no NDK at all: install one: {ndk:?}"
        );
    }

    #[test]
    fn the_build_needs_the_ndk_and_nothing_that_wraps_it() {
        // ADR-065: Cargo links with the NDK's clang itself, so `cargo-ndk` is no prerequisite and no
        // finding mentions it, whether or not it happens to be installed.
        let report = scan(&good_machine(), &["android"]);
        let text = format!("{report:?}");
        assert!(
            !text.contains("cargo-ndk") && !text.contains("cargo ndk"),
            "{text}"
        );
        assert_eq!(by_id(&report, "android.ndk").status, Status::Ok);
    }

    #[test]
    fn the_targets_follow_the_projects_abis() {
        let report = scan(&good_machine(), &["android"]);
        assert_eq!(
            by_id(&report, "rust.target.aarch64-linux-android").status,
            Status::Ok
        );
        assert_eq!(
            by_id(&report, "rust.target.x86_64-linux-android").status,
            Status::Ok
        );
        assert!(
            report
                .findings()
                .all(|f| f.id != "rust.target.armv7-linux-androideabi")
        );

        let report = run_with_abis(&good_machine(), &["armeabi-v7a", "x86"]);
        let armv7 = by_id(&report, "rust.target.armv7-linux-androideabi");
        assert_eq!((armv7.status, armv7.state), (Status::Fail, State::Missing));
        assert_eq!(armv7.fix, ["rustup target add armv7-linux-androideabi"]);
        assert_eq!(
            by_id(&report, "rust.target.i686-linux-android").status,
            Status::Fail
        );
    }

    #[test]
    fn the_jdk_must_be_17_or_newer() {
        let f = by_id(&scan(&good_machine(), &["android"]), "android.jdk").clone();
        assert_eq!(f.status, Status::Ok);

        // Too old: advice outside a project, a failure inside one.
        let old = good_machine().with_stderr_output(
            "java",
            "-version",
            "java version \"1.8.0_292\"\nJava(TM) SE Runtime Environment\n",
        );
        let f = by_id(&scan(&old, &["android"]), "android.jdk").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::WrongVersion));
        let f = by_id(
            &run_in_project(&old, &["android"], &["arm64"]),
            "android.jdk",
        )
        .clone();
        assert_eq!((f.status, f.state), (Status::Fail, State::WrongVersion));
        assert_eq!(
            f.fix,
            [
                "brew install openjdk@17",
                "export JAVA_HOME=\"$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home\"",
                "export PATH=\"$JAVA_HOME/bin:$PATH\"",
            ]
        );

        // Installed but keg-only.
        let keg = bare_mac().with_file("/opt/homebrew/opt/openjdk@17/bin/java");
        let f = by_id(&scan(&keg, &["android"]), "android.jdk").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert!(
            f.fix[0].contains("JAVA_HOME=/opt/homebrew/opt/openjdk@17"),
            "{f:?}"
        );
        // Inside a project with an Android app `./gradlew` cannot start without it: a failure there,
        // as for a JDK that is missing (review, 2026-10-01).
        let f = by_id(
            &run_in_project(&keg, &["android"], &["arm64"]),
            "android.jdk",
        )
        .clone();
        assert_eq!((f.status, f.state), (Status::Fail, State::Missing));

        // Not installed.
        let f = by_id(&scan(&bare_machine(), &["android"]), "android.jdk").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert_eq!(f.fix, ["sudo apt-get install -y openjdk-17-jdk"]);
        let f = by_id(
            &run_in_project(&bare_machine(), &["android"], &["arm64"]),
            "android.jdk",
        )
        .clone();
        assert_eq!(f.status, Status::Fail);
    }

    #[test]
    fn the_gradle_wrapper_is_checked_inside_a_project() {
        // No project: not applicable.
        let f = by_id(
            &scan(&good_machine(), &["android"]),
            "android.gradle-wrapper",
        )
        .clone();
        assert_eq!((f.status, f.state), (Status::Skip, State::NotApplicable));

        let with_wrapper = good_machine().with_file("/work/app/android/gradlew");
        let f = by_id(
            &run_in_project(&with_wrapper, &["android"], &["arm64"]),
            "android.gradle-wrapper",
        )
        .clone();
        assert_eq!(f.status, Status::Ok);

        // Missing, Gradle installed: create it.
        let with_gradle = good_machine().with_tool("gradle", "/opt/homebrew/bin/gradle");
        let f = by_id(
            &run_in_project(&with_gradle, &["android"], &["arm64"]),
            "android.gradle-wrapper",
        )
        .clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert_eq!(
            f.fix,
            ["(cd android && gradle wrapper --gradle-version 8.14.3)"]
        );

        // Missing, no Gradle either: install it first.
        let f = by_id(
            &run_in_project(&good_machine(), &["android"], &["arm64"]),
            "android.gradle-wrapper",
        )
        .clone();
        assert_eq!(f.fix[0], "brew install gradle");
        assert_eq!(f.fix.len(), 2);
    }

    #[test]
    fn the_undra_avd_is_for_contributors() {
        // Not a contributor: not listed at all.
        assert!(
            scan(&good_machine(), &["android"])
                .findings()
                .all(|f| f.id != "android.avd")
        );

        let f = by_id(
            &run_as_contributor(&good_machine(), &["android"]),
            "android.avd",
        )
        .clone();
        assert_eq!(f.status, Status::Ok);
        assert_eq!(
            f.audience,
            crate::commands::doctor::finding::Audience::Contributors
        );

        let other = good_machine().with_output("emulator", "-list-avds", "Pixel_8\n");
        let f = by_id(&run_as_contributor(&other, &["android"]), "android.avd").clone();
        assert_eq!((f.status, f.state), (Status::Warn, State::Missing));
        assert_eq!(f.observed.as_deref(), Some("Pixel_8"));
        assert!(
            f.fix.iter().any(|c| c.contains("create avd -n undra")),
            "{f:?}"
        );

        let no_emulator = bare_mac();
        let f = by_id(
            &run_as_contributor(&no_emulator, &["android"]),
            "android.avd",
        )
        .clone();
        assert!(f.message.contains("emulator is not installed"), "{f:?}");
    }
}
