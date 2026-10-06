//! Machines for the tests of the checks: described by data, never by the host that runs them.

use std::path::Path;

use crate::config::Platform;
use crate::sys::fake::FakeSys;
use crate::toolchain::{Toolchain, XCODE_DEVELOPER_DIR};

use super::finding::{Finding, Report};
use super::{Context, check};

const SDK: &str = "/opt/homebrew/share/android-commandlinetools";

/// What a described machine has. Start from [`Spec::mac`] or [`Spec::linux`] and take things away.
#[derive(Clone)]
pub struct Spec {
    pub mac: bool,
    pub rust: bool,
    pub targets: bool,
    pub xcode: bool,
    pub sdk: bool,
    pub platform: bool,
    pub adb: bool,
    pub ndk: Option<&'static str>,
    pub emulator: bool,
    pub jdk: bool,
    pub node: bool,
    pub kotlinc: bool,
    pub undra: bool,
}

impl Spec {
    /// A Mac that has everything the platforms need, with no environment variables set.
    pub fn mac() -> Spec {
        Spec {
            mac: true,
            rust: true,
            targets: true,
            xcode: true,
            sdk: true,
            platform: true,
            adb: true,
            ndk: Some("27.2.12479018"),
            emulator: true,
            jdk: true,
            node: true,
            kotlinc: true,
            undra: true,
        }
    }

    /// A Linux box with the same tools, minus Xcode.
    pub fn linux() -> Spec {
        Spec {
            mac: false,
            xcode: false,
            ..Spec::mac()
        }
    }

    pub fn build(&self) -> FakeSys {
        let mut sys = if self.mac {
            FakeSys::macos()
        } else {
            FakeSys::linux()
        };
        let home = if self.mac { "/Users/dev" } else { "/home/dev" };
        sys = sys.with_free_disk(500_000_000_000);
        if self.rust {
            sys = sys
                .with_dir(&format!("{home}/.cargo/bin"))
                .with_tool("rustup", &format!("{home}/.cargo/bin/rustup"))
                .with_tool("rustc", &format!("{home}/.cargo/bin/rustc"))
                .with_tool("cargo", &format!("{home}/.cargo/bin/cargo"))
                .with_output("rustup", "--version", "rustup 1.28.2 (e4f3ad6f8 2025-04-28)\ninfo: This is the version for the rustup toolchain manager, not the rustc compiler.\n")
                .with_output("rustup", "show active-toolchain", "stable-aarch64-apple-darwin (default)\n")
                .with_output("rustc", "--version", "rustc 1.98.1 (48a229cea 2026-09-01)\n")
                .with_output("cargo", "--version", "cargo 1.98.1 (797e8a9bc 2026-08-05)\n")
                .with_output("rustc", "--print sysroot", &format!("{home}/.rustup/toolchains/stable\n"))
                .with_file(&format!(
                    "{home}/.rustup/toolchains/stable/lib/rustlib/etc/lldb_lookup.py"
                ));
            if self.targets {
                let mut triples = vec![
                    "wasm32-unknown-unknown",
                    "aarch64-linux-android",
                    "x86_64-linux-android",
                ];
                if self.mac {
                    triples.extend(["aarch64-apple-ios", "aarch64-apple-ios-sim"]);
                }
                for t in triples {
                    sys =
                        sys.with_dir(&format!("{home}/.rustup/toolchains/stable/lib/rustlib/{t}"));
                }
            }
        }
        if self.mac {
            // Homebrew, on PATH: every brew fix of a described Mac is then one `brew install`.
            sys = sys.with_tool("brew", "/opt/homebrew/bin/brew");
        }
        if self.mac && self.xcode {
            sys = sys
                .with_dir(XCODE_DEVELOPER_DIR)
                .with_tool("xcodebuild", "/usr/bin/xcodebuild")
                .with_tool("xcode-select", "/usr/bin/xcode-select")
                .with_tool("xcrun", "/usr/bin/xcrun")
                .with_tool("lipo", "/usr/bin/lipo")
                .with_output("xcodebuild", "-version", "Xcode 26.6\nBuild version 17F113\n")
                .with_output("xcode-select", "-p", &format!("{XCODE_DEVELOPER_DIR}\n"))
                .with_output(
                    "xcrun",
                    "simctl list runtimes",
                    "== Runtimes ==\niOS 26.5 (26.5 - 23F77) - com.apple.CoreSimulator.SimRuntime.iOS-26-5\n",
                );
        }
        if self.sdk {
            sys = sys
                .with_dir(SDK)
                .with_file(&format!("{SDK}/cmdline-tools/latest/bin/sdkmanager"));
            if self.platform {
                sys = sys.with_dir(&format!("{SDK}/platforms/android-35"));
            }
            if let Some(ndk) = self.ndk {
                sys = sys
                    .with_dir(&format!("{SDK}/ndk/{ndk}"))
                    .with_file(&format!(
                        "{SDK}/ndk/{ndk}/toolchains/llvm/prebuilt/darwin-x86_64/bin/lldb"
                    ));
            }
            if self.adb {
                sys = sys
                    .with_file(&format!("{SDK}/platform-tools/adb"))
                    .with_output(
                        "adb",
                        "--version",
                        "Android Debug Bridge version 1.0.41\nVersion 36.0.0-13206524\n",
                    )
                    .with_output(
                        "adb",
                        "devices",
                        "List of devices attached\nemulator-5554\tdevice\n\n",
                    );
            }
            if self.emulator {
                sys = sys
                    .with_file(&format!("{SDK}/emulator/emulator"))
                    .with_output("emulator", "-list-avds", "undra\n");
            }
        }
        if self.jdk {
            sys = sys.with_tool("java", "/usr/bin/java").with_stderr_output(
                "java",
                "-version",
                "openjdk version \"17.0.12\" 2024-07-16\nOpenJDK Runtime Environment (build 17.0.12+7)\n",
            );
        }
        if self.node {
            sys = sys
                .with_tool("node", "/usr/local/bin/node")
                .with_tool("npm", "/usr/local/bin/npm")
                .with_tool("wasm-opt", "/opt/homebrew/bin/wasm-opt")
                .with_output("node", "--version", "v22.3.0\n")
                .with_output("npm", "--version", "10.8.2\n")
                .with_output("wasm-opt", "--version", "wasm-opt version 133\n");
        }
        if self.kotlinc {
            sys = sys
                .with_tool("kotlinc", "/opt/homebrew/bin/kotlinc")
                .with_stderr_output(
                    "kotlinc",
                    "-version",
                    "info: kotlinc-jvm 2.4.20 (JRE 17.0.20.1+0)\n",
                );
        }
        if self.undra {
            sys = sys
                .with_tool("undra", &format!("{home}/.undra/bin/undra"))
                .with_output(
                    "undra",
                    "--version",
                    &format!("undra {} (abc1234)\n", crate::version::SEMVER),
                );
        }
        sys
    }
}

/// A Mac with everything installed, and no environment variables set.
pub fn good_machine() -> FakeSys {
    Spec::mac().build()
}

/// A Linux box with everything but Xcode.
pub fn good_linux_machine() -> FakeSys {
    Spec::linux().build()
}

/// A Mac with everything installed and every variable set: nothing for doctor to say.
pub fn fully_configured_machine() -> FakeSys {
    good_machine()
        .with_env("ANDROID_HOME", SDK)
        .with_env("ANDROID_NDK_HOME", &format!("{SDK}/ndk/27.2.12479018"))
        .with_dir("/Users/dev/.rustup/toolchains/stable/lib/rustlib/x86_64-apple-ios")
}

/// A Linux machine with nothing on it.
pub fn bare_machine() -> FakeSys {
    FakeSys::linux().with_free_disk(500_000_000_000)
}

/// A Mac with nothing on it.
pub fn bare_mac() -> FakeSys {
    FakeSys::macos().with_free_disk(500_000_000_000)
}

/// A Mac with nothing on it but Homebrew (on PATH).
pub fn bare_mac_with_brew() -> FakeSys {
    bare_mac().with_tool("brew", "/opt/homebrew/bin/brew")
}

/// A Mac without any Rust target.
pub fn good_machine_without_targets() -> FakeSys {
    Spec {
        targets: false,
        ..Spec::mac()
    }
    .build()
}

/// A Mac whose SDK has no platform.
pub fn good_machine_without_platform() -> FakeSys {
    Spec {
        platform: false,
        ..Spec::mac()
    }
    .build()
}

/// A Mac whose SDK has no `platform-tools`.
pub fn good_machine_without_adb() -> FakeSys {
    Spec {
        adb: false,
        ..Spec::mac()
    }
    .build()
}

/// A Mac with the NDK `version`.
pub fn good_machine_with_ndk(version: &'static str) -> FakeSys {
    Spec {
        ndk: Some(version),
        ..Spec::mac()
    }
    .build()
}

/// A Mac with an SDK and no NDK.
pub fn good_machine_without_ndk() -> FakeSys {
    Spec {
        ndk: None,
        ..Spec::mac()
    }
    .build()
}

#[derive(Default)]
struct Opts {
    in_project: bool,
    contributor: bool,
    sim_archs: Option<Vec<String>>,
    abis: Option<Vec<String>>,
}

fn go(sys: &FakeSys, platforms: &[&str], opts: Opts) -> Report {
    let toolchain = Toolchain::detect(sys);
    let scope: Vec<Platform> = platforms
        .iter()
        .map(|p| Platform::parse(p).expect("a platform"))
        .collect();
    let abis = opts
        .abis
        .unwrap_or_else(|| vec!["arm64-v8a".to_owned(), "x86_64".to_owned()]);
    let archs = opts.sim_archs.unwrap_or_else(|| vec!["arm64".to_owned()]);
    let root = Path::new("/work/app");
    let cx = Context {
        sys,
        toolchain: &toolchain,
        scope: &scope,
        android_abis: &abis,
        simulator_archs: &archs,
        project: opts.in_project.then_some(root),
        contributor: opts.contributor,
    };
    check(&cx)
}

/// Runs the checks of `platforms` outside a project.
pub fn scan(sys: &FakeSys, platforms: &[&str]) -> Report {
    go(sys, platforms, Opts::default())
}

/// Runs them inside a project at `/work/app` whose iOS simulator architectures are `archs`.
pub fn run_in_project(sys: &FakeSys, platforms: &[&str], archs: &[&str]) -> Report {
    go(
        sys,
        platforms,
        Opts {
            in_project: true,
            sim_archs: Some(archs.iter().map(|a| (*a).to_owned()).collect()),
            ..Opts::default()
        },
    )
}

/// Runs the Android checks for a project that builds `abis`.
pub fn run_with_abis(sys: &FakeSys, abis: &[&str]) -> Report {
    go(
        sys,
        &["android"],
        Opts {
            in_project: true,
            abis: Some(abis.iter().map(|a| (*a).to_owned()).collect()),
            ..Opts::default()
        },
    )
}

/// Runs the checks for someone who works on Undra.
pub fn run_as_contributor(sys: &FakeSys, platforms: &[&str]) -> Report {
    go(
        sys,
        platforms,
        Opts {
            contributor: true,
            ..Opts::default()
        },
    )
}

/// The finding with `id`; panics, listing what there is, when the report has none.
pub fn by_id<'a>(report: &'a Report, id: &str) -> &'a Finding {
    report.findings().find(|f| f.id == id).unwrap_or_else(|| {
        panic!(
            "no finding {id}; the report has {:?}",
            report.findings().map(|f| f.id).collect::<Vec<_>>()
        )
    })
}
