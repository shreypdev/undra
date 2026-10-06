//! The heavy platform builds, switched on by environment variables because they need Xcode, the
//! Android NDK and, for the app shells, network access and minutes:
//!
//! | Variable | What runs |
//! |---|---|
//! | `UNDRA_TEST_IOS=1` | `undra build --platform ios --release`: the XCFramework and its slices |
//! | `UNDRA_TEST_IOS_APP=1` | …and the generated Xcode project built for the simulator against it |
//! | `UNDRA_TEST_ANDROID=1` | `undra build --platform android --release`: a 16 KB aligned `.so` per ABI |
//! | `UNDRA_TEST_ANDROID_APP=1` | …and `./gradlew :app:assembleDebug` on the generated Android project |
//! | `UNDRA_TEST_WEB_APP=1` | `npm install && npm run build` on the generated web app |
//!
//! Without a variable a test prints why it did nothing and passes.

mod common;

use std::path::Path;
use std::process::Command;

use common::{
    flag, has_android_ndk, has_rust_target, init_project, init_project_with, path_with_undra,
    run_ok,
};

fn skipped(variable: &str) -> bool {
    if flag(variable) {
        return false;
    }
    eprintln!("skipped: set {variable}=1 to run this test");
    true
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[test]
fn ios_builds_an_xcframework_with_device_and_simulator_slices() {
    if skipped("UNDRA_TEST_IOS") && skipped("UNDRA_TEST_IOS_APP") {
        return;
    }
    assert!(
        has_rust_target("aarch64-apple-ios") && has_rust_target("aarch64-apple-ios-sim"),
        "rustup target add aarch64-apple-ios aarch64-apple-ios-sim"
    );
    let project = init_project("ios-build", "ios");
    let out = run_ok(
        project
            .undra()
            .args(["build", "--platform", "ios", "--release"]),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_ascii_lowercase().contains("android"),
        "an iOS build says nothing about Android:\n{stderr}"
    );
    // Named after the core's namespace, `ios_build_core` (ADR-044).
    let xcframework = project.root.join("build/ios/IosBuildCore.xcframework");
    for slice in ["ios-arm64", "ios-arm64-simulator"] {
        let lib = xcframework.join(slice).join("libios_build_core.a");
        assert!(lib.is_file(), "{slice} has no library:\n{stdout}");
        // A release static library of the template core: a few MB, LTO'd into one object.
        let bytes = size(&lib);
        assert!(
            (1_000_000..40_000_000).contains(&bytes),
            "{slice}: {bytes} bytes"
        );
        eprintln!("ios {slice}: {bytes} bytes");
    }
    // The core's header, without a module map: the generated Swift package declares the module.
    assert!(
        xcframework
            .join("ios-arm64/Headers/ios_build_core_undra.h")
            .is_file()
    );
    assert!(
        !xcframework
            .join("ios-arm64/Headers/module.modulemap")
            .exists()
    );
    assert!(
        stdout.contains("ios xcframework") && stdout.contains("budget 900 KB"),
        "{stdout}"
    );

    if flag("UNDRA_TEST_IOS_APP") {
        let derived = common::TempDir::new("ios-dd");
        let build = Command::new("xcodebuild")
            .args(["-quiet", "-project"])
            .arg(project.root.join("ios/IosBuild.xcodeproj"))
            .args([
                "-scheme",
                "IosBuild",
                "-configuration",
                "Release",
                "-destination",
                "generic/platform=iOS Simulator",
                "-derivedDataPath",
            ])
            .arg(derived.path())
            .arg("build")
            // The build phase of the project runs `undra build` (it finds `undra` on PATH).
            .env("PATH", path_with_undra())
            .output()
            .expect("xcodebuild runs");
        assert!(
            build.status.success(),
            "xcodebuild failed:\n{}\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let app = derived
            .path()
            .join("Build/Products/Release-iphonesimulator/IosBuild.app/IosBuild");
        assert!(app.is_file(), "no app binary at {}", app.display());
        eprintln!(
            "ios app executable (release, simulator): {} bytes",
            size(&app)
        );
    }
}

/// ADR-045: an app that supports iOS 15 builds for the simulator at that floor: the runtime and the generated
/// bindings (`ObservableObject` stores, `UndraDuration`) compile for iOS 15.0, and so do the template's views.
#[test]
fn an_ios_15_app_builds_for_the_simulator_at_its_floor() {
    if skipped("UNDRA_TEST_IOS_APP") {
        return;
    }
    assert!(
        has_rust_target("aarch64-apple-ios") && has_rust_target("aarch64-apple-ios-sim"),
        "rustup target add aarch64-apple-ios aarch64-apple-ios-sim"
    );
    let project = init_project_with("ios15app", "ios", &["--ios-deployment-target", "15.0"]);
    let derived = common::TempDir::new("ios15-dd");
    let build = Command::new("xcodebuild")
        .args(["-quiet", "-project"])
        .arg(project.root.join("ios/Ios15app.xcodeproj"))
        .args([
            "-scheme",
            "Ios15app",
            "-configuration",
            "Debug",
            "-destination",
            "generic/platform=iOS Simulator",
            "-derivedDataPath",
        ])
        .arg(derived.path())
        .arg("build")
        .env("PATH", path_with_undra())
        .output()
        .expect("xcodebuild runs");
    assert!(
        build.status.success(),
        "xcodebuild at iOS 15.0 failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let plist = derived
        .path()
        .join("Build/Products/Debug-iphonesimulator/Ios15app.app/Info.plist");
    let minimum = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :MinimumOSVersion"])
        .arg(&plist)
        .output()
        .expect("PlistBuddy runs");
    assert_eq!(
        String::from_utf8_lossy(&minimum.stdout).trim(),
        "15.0",
        "the app declares its floor"
    );
}

#[test]
fn android_builds_a_16kb_aligned_library_per_abi() {
    if skipped("UNDRA_TEST_ANDROID") && skipped("UNDRA_TEST_ANDROID_APP") {
        return;
    }
    assert!(
        has_android_ndk(),
        "install the NDK: sdkmanager \"ndk;27.2.12479018\""
    );
    assert!(
        has_rust_target("aarch64-linux-android") && has_rust_target("x86_64-linux-android"),
        "rustup target add aarch64-linux-android x86_64-linux-android"
    );
    let project = init_project("androidbuild", "android");
    let out = run_ok(
        project
            .undra()
            .args(["build", "--platform", "android", "--release"]),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for abi in ["arm64-v8a", "x86_64"] {
        let lib = project
            .root
            .join("build/android/jniLibs")
            .join(abi)
            .join("libandroidbuild_core.so");
        assert!(lib.is_file(), "{abi}: no library\n{stdout}");
        let bytes = size(&lib);
        assert!(
            bytes < 1_200_000,
            "{abi}: {bytes} bytes is over the 1.2 MB budget of the blueprint"
        );
        eprintln!("android {abi}: {bytes} bytes");
    }
    assert!(stdout.contains("16 KB aligned"), "{stdout}");
    // Only the core's library is shipped (a Cargo cross-build only builds the shim).
    assert!(
        !project
            .root
            .join("build/android/jniLibs/arm64-v8a/libundra_ffi.so")
            .exists()
    );

    if flag("UNDRA_TEST_ANDROID_APP") {
        let gradlew = project.root.join("android/gradlew");
        assert!(
            gradlew.is_file(),
            "init copies the Gradle wrapper from the checkout"
        );
        let mut gradle = Command::new(&gradlew);
        gradle
            .args([":app:assembleDebug", "--console=plain"])
            .current_dir(project.root.join("android"))
            // The `undraBuild` task runs `undra build` (it finds `undra` on PATH).
            .env("PATH", path_with_undra());
        for var in ["JAVA_HOME", "ANDROID_HOME"] {
            if let Ok(value) = std::env::var(var) {
                gradle.env(var, value);
            }
        }
        let build = gradle.output().expect("gradlew runs");
        assert!(
            build.status.success(),
            "gradle failed:\n{}\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let apk = project
            .root
            .join("android/app/build/outputs/apk/debug/app-debug.apk");
        assert!(apk.is_file());
        eprintln!("android debug apk: {} bytes", size(&apk));
    }
}

#[test]
fn a_debug_android_build_hints_at_release_and_lands_where_the_gradle_app_looks() {
    if skipped("UNDRA_TEST_ANDROID") && skipped("UNDRA_TEST_ANDROID_APP") {
        return;
    }
    assert!(
        has_android_ndk(),
        "install the NDK: sdkmanager \"ndk;27.2.12479018\""
    );
    assert!(
        has_rust_target("aarch64-linux-android") && has_rust_target("x86_64-linux-android"),
        "rustup target add aarch64-linux-android x86_64-linux-android"
    );
    let project = init_project("androidwiring", "android");

    // A debug build (the default): it says what a debug core weighs and what to package with, and
    // the template's jniLibs path is where the build wrote the libraries, so nothing else is said.
    let out = run_ok(project.undra().args(["build", "--platform", "android"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hint: this Android core is a debug build")
            && stderr.contains("`undra build --platform android --release`"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("warning:") && !stderr.contains("UnsatisfiedLinkError"),
        "the generated Gradle app packages build/android/jniLibs:\n{stderr}"
    );

    // The Gradle path drifts (relative to android/ instead of android/app/): the libraries are
    // put where the app looks, and the warning names the line to fix.
    let script = project.root.join("android/app/build.gradle.kts");
    let text = std::fs::read_to_string(&script).unwrap();
    std::fs::write(
        &script,
        text.replace(
            "\"../../build/android/jniLibs\"",
            "\"../build/android/jniLibs\"",
        ),
    )
    .unwrap();
    let out = run_ok(project.undra().args(["build", "--platform", "android"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("copied the libraries there too")
            && stderr.contains("jniLibs.srcDir(\"../../build/android/jniLibs\")"),
        "{stderr}"
    );
    for abi in ["arm64-v8a", "x86_64"] {
        let copy = project
            .root
            .join("android/build/android/jniLibs")
            .join(abi)
            .join("libandroidwiring_core.so");
        assert!(copy.is_file(), "{abi}: nothing at {}", copy.display());
        assert_eq!(
            size(&copy),
            size(
                &project
                    .root
                    .join("build/android/jniLibs")
                    .join(abi)
                    .join("libandroidwiring_core.so")
            )
        );
    }
}

#[test]
fn the_web_app_shell_type_checks_and_bundles() {
    if skipped("UNDRA_TEST_WEB_APP") {
        return;
    }
    assert!(
        has_rust_target("wasm32-unknown-unknown"),
        "rustup target add wasm32-unknown-unknown"
    );
    let project = init_project("webapp", "web");
    run_ok(project.undra().args(["build", "--platform", "web"]));
    let web = project.root.join("web");
    run_ok(
        Command::new("npm")
            .args(["install", "--no-audit", "--no-fund"])
            .current_dir(&web),
    );
    // The `undra()` Vite plugin runs `undra build` (it finds `undra` on PATH).
    run_ok(
        Command::new("npm")
            .args(["run", "build"])
            .current_dir(&web)
            .env("PATH", path_with_undra()),
    );
    let assets = std::fs::read_dir(web.join("dist/assets")).unwrap();
    assert!(
        assets
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".wasm")),
        "the wasm core is bundled"
    );
}
