//! `undra init`: a new project.
//!
//! The project is written from templates compiled into the binary, then its bindings are
//! generated from the template core's schema (embedded too, and checked against a real build by
//! the tests), so what `init` leaves behind is complete: the core compiles, the three app shells
//! import bindings that exist, and `undra bindgen --check` passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use undra_bindgen::Generator;
use undra_bindgen::naming::CoreNames;

use crate::bindgen::{self, Plan, canonicalize_lenient};
use crate::cli::InitArgs;
use crate::config::{Platform, ProjectConfig, UNDRA_RELEASE_TAG, UNDRA_VERSION};
use crate::dist::{self, Dist};
use crate::error::{CliError, Code, Result};
use crate::fsutil::{self, create_dir_all, is_empty_dir, make_executable, write_if_changed};
use crate::names::{Names, portable, relative_path, validate_app_id, validate_project_name};
use crate::project::{CONFIG_FILE, Project};
use crate::render::{TemplateFile, Vars};
use crate::runtimes::{KOTLIN_IN_REPO, RuntimeRef, Runtimes, enclosing_checkout, require_checkout};
use crate::schema::parse_schema_json;
use crate::templates;

use super::Env;

/// The Gradle version the wrapper is created with when there is no checkout to copy one from
/// (the Kotlin runtime's own wrapper version).
const GRADLE_VERSION: &str = "8.14.3";

/// What `init` decided before writing anything.
pub(super) struct Setup {
    // (fields below are read by `undra adopt` too)
    pub(super) root: PathBuf,
    pub(super) names: Names,
    pub(super) config: ProjectConfig,
    pub(super) repo: Option<PathBuf>,
    /// Where a released project's dependencies are fetched from (ADR-063): GitHub, or the mirror the environment names.
    pub(super) dist: Dist,
}

/// Runs `undra init`.
///
/// # Errors
///
/// `C0009` for a bad name, platform list or id, `C0008` when the directory is not empty, `C0010`
/// when a file cannot be written.
pub fn run(env: &Env<'_>, args: &InitArgs) -> Result<()> {
    let setup = prepare(env, args)?;
    let ui = env.ui;
    ui.step(&format!(
        "Creating {} in {}",
        setup.names.project,
        setup.root.display()
    ));
    let files = scaffold(&setup)?;
    let bindings = generate_bindings(&setup)?;
    if setup.config.platforms.contains(&Platform::Android) {
        gradle_wrapper(env, &setup);
    }

    ui.line(&format!(
        "Created {} ({} files, bindings for schema hash {:#018x}).",
        setup.root.display(),
        files + bindings.files,
        bindings.hash
    ));
    ui.line("");
    ui.line(&next_steps(&setup));
    Ok(())
}

/// Validates the arguments and works out where everything goes.
fn prepare(env: &Env<'_>, args: &InitArgs) -> Result<Setup> {
    validate_project_name(&args.name)?;
    let dist = Dist::announced(env.sys, &env.ui)?;
    let platforms = Platform::parse_list(&args.platforms)?;
    let names = Names::derive(&args.name);
    let id = args.id.clone().unwrap_or_else(|| names.default_app_id());
    validate_app_id(&id)?;

    let parent = match &args.dir {
        Some(dir) => dir.clone(),
        None => env.start_dir()?,
    };
    let root = canonicalize_lenient(&parent.join(&args.name));
    if !is_empty_dir(&root) {
        return Err(CliError::new(
            Code::WouldOverwrite,
            format!("{} already exists and is not empty", root.display()),
            "`undra init` creates a new project and will not write into a directory that has files in it",
            format!(
                "choose another name, or another parent with `--dir`; to add Undra to an existing app use `undra adopt {}`",
                root.display()
            ),
        ));
    }

    let mut config = ProjectConfig::new(&args.name, &id, platforms);
    if let Some(target) = &args.ios_deployment_target {
        crate::config::check_ios_target(target)?;
        config.ios.deployment_target.clone_from(target);
    }
    let repo = match &args.undra_path {
        Some(path) => {
            let repo = path.canonicalize().map_err(|e| {
                CliError::new(
                    Code::BadArgument,
                    format!("--undra-path {}: {e}", path.display()),
                    "the path has to be a checkout of the Undra repository",
                    "check the path, or leave --undra-path out to use released versions",
                )
            })?;
            require_checkout(&repo)?;
            let shown = relative_path(&root, &repo).unwrap_or_else(|| repo.clone());
            config.undra_path = Some(portable(&shown));
            Some(repo)
        }
        // A project created inside a checkout of the Undra repository uses the checkout, as with
        // `--undra-path`; anywhere else it pins the CLI's release (see `variables`).
        None => enclosing_checkout(&root).inspect(|repo| {
            let shown = relative_path(&root, repo).unwrap_or_else(|| repo.clone());
            config.undra_path = Some(portable(&shown));
            env.ui.line(&format!(
                "Using the Undra checkout at {} (the project is inside it); `--undra-path` names another.",
                repo.display()
            ));
        }),
    };
    Ok(Setup {
        root,
        names,
        config,
        repo,
        dist,
    })
}

/// The path from `from` (a directory of the project) to `to`, as text for a config file.
fn rel(from: &Path, to: &Path) -> String {
    portable(&relative_path(from, to).unwrap_or_else(|| to.to_path_buf()))
}

/// Every placeholder the templates use.
pub(super) fn variables(setup: &Setup) -> Vars {
    let Setup {
        root,
        names,
        config,
        repo,
        dist,
    } = setup;
    let generated = root.join(&config.generated);
    let build = root.join(&config.build);
    let kotlin_package = format!("{}.core", config.id);
    // ADR-044: the core's namespace names its symbol, its libraries and the bindings' entry.
    let core_names = CoreNames::new(
        &config
            .core_namespace
            .clone()
            .unwrap_or_else(|| CoreNames::default_namespace(&names.core_package)),
    );
    let ts_package = format!("@app/{}", names.core_package);
    let id_path = config.id.replace('.', "/");

    // Where Undra comes from, as the core's Cargo.toml and the shells say it.
    let undra_dep = match repo {
        Some(repo) => format!(
            "{{ path = {} }}",
            crate::toml_lite::quote(&rel(&root.join("core"), &repo.join("crates/undra")))
        ),
        // Not in a checkout: the crates of the release this CLI belongs to, from its git tag
        // (crates.io publishing is later; ADR-063).
        None => format!(
            "{{ git = \"{}\", tag = \"{UNDRA_RELEASE_TAG}\" }}",
            dist.git_url
        ),
    };
    let runtimes = match repo {
        Some(repo) => Runtimes::in_repo(repo),
        None => Runtimes::released(UNDRA_VERSION, dist),
    };

    let mut vars = Vars::new()
        .with("NAME", names.project.clone())
        .with("KEBAB", names.kebab.clone())
        .with("SNAKE", names.snake.clone())
        .with("PASCAL", names.pascal.clone())
        .with("APP", names.pascal.clone())
        .with("APP_ID", config.id.clone())
        .with("APP_ID_PATH", id_path)
        .with("CORE_PACKAGE", names.core_package.clone())
        .with("CORE_LIB", names.core_lib.clone())
        .with("SWIFT_MODULE", names.swift_module.clone())
        .with("KOTLIN_PACKAGE", kotlin_package)
        .with("TS_PACKAGE", ts_package)
        .with("UNDRA_DEP", undra_dep)
        .with("UNDRA_VERSION", UNDRA_VERSION)
        .with("NAMESPACE", core_names.namespace().to_owned())
        .with("CORE_ENTRY", core_names.entry())
        .with("CORE_BUNDLE", core_names.bundle());

    // iOS
    let ios_dir = root.join("ios");
    let slice_sim = if config.ios.simulator_archs.len() > 1 {
        "ios-arm64_x86_64-simulator"
    } else if config.ios.simulator_archs[0] == "x86_64" {
        "ios-x86_64-simulator"
    } else {
        "ios-arm64-simulator"
    };
    let xcframework = build.join(format!("ios/{}.xcframework", core_names.bundle()));
    let xcframework_rel = rel(&ios_dir, &xcframework);
    // The prelinked core links like any archive: the bindings reference its entry, which pulls the one
    // object that holds the whole core (ADR-044), so no -force_load.
    let library = format!("lib{}.a", core_names.namespace());
    let link_flags = format!(
        "\"OTHER_LDFLAGS[sdk=iphoneos*]\" = (\n\t\t\t\t\t\"$(inherited)\",\n\t\t\t\t\t\"$(SRCROOT)/{xcframework_rel}/ios-arm64/{library}\",\n\t\t\t\t);\n\t\t\t\t\"OTHER_LDFLAGS[sdk=iphonesimulator*]\" = (\n\t\t\t\t\t\"$(inherited)\",\n\t\t\t\t\t\"$(SRCROOT)/{xcframework_rel}/{slice_sim}/{library}\",\n\t\t\t\t);"
    );
    // A generic simulator destination builds every architecture Xcode knows; the XCFramework has
    // only the slices of `[ios] simulator_archs`, so exclude the others instead of failing to link.
    let excluded = match config.ios.simulator_archs.as_slice() {
        [only] if only == "arm64" => "\n\t\t\t\t\"EXCLUDED_ARCHS[sdk=iphonesimulator*]\" = x86_64;",
        [only] if only == "x86_64" => "\n\t\t\t\t\"EXCLUDED_ARCHS[sdk=iphonesimulator*]\" = arm64;",
        _ => "",
    };
    vars.set("XCFRAMEWORK_PATH", xcframework_rel);
    // The Run Script phase that builds the core (`undra build --platform ios`) and its file lists.
    vars.set("ROOT_REL", rel(&ios_dir, root));
    vars.set("CORE_REL", rel(&ios_dir, &root.join(&config.core_path)));
    vars.set("BUILD_REL", rel(&ios_dir, &build));
    vars.set("SIM_SLICE", slice_sim);
    vars.set("LINK_FLAGS", format!("{link_flags}{excluded}"));
    vars.set("DEPLOYMENT_TARGET", config.ios.deployment_target.clone());
    vars.set(
        "PACKAGE_REFERENCE_SECTIONS",
        package_references(
            &rel(&ios_dir, &generated.join("swift")),
            &runtimes.swift,
            &ios_dir,
        ),
    );

    // Android
    let android_dir = root.join("android");
    vars.set(
        "GENERATED_KOTLIN_PATH",
        rel(&android_dir, &generated.join("kotlin")),
    );
    vars.set(
        "JNI_LIBS_PATH",
        rel(&android_dir.join("app"), &build.join("android/jniLibs")),
    );
    vars.set("PROJECT_ROOT_FROM_APP", rel(&android_dir.join("app"), root));
    vars.set(
        "CORE_FROM_APP",
        rel(&android_dir.join("app"), &root.join(&config.core_path)),
    );
    vars.set("MIN_SDK", config.android.min_sdk.to_string());
    vars.set(
        "ABI_FILTERS",
        config
            .android
            .abis
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", "),
    );
    match &runtimes.kotlin {
        RuntimeRef::Path(dir) => {
            vars.set(
                "KOTLIN_RUNTIME_BUILD",
                format!(
                    "\n// The Kotlin runtime, built from the Undra checkout (a composite build: `dev.undra:runtime` below\n// resolves to it, so runtime edits show up without publishing).\nincludeBuild(\"{}\")\n",
                    rel(&android_dir, dir)
                ),
            );
            vars.set("UNDRA_MAVEN_GROUP", "dev.undra");
            vars.set("KOTLIN_RUNTIME_VERSION", "0.1.0-SNAPSHOT");
            vars.set("MAVEN_REPOSITORY", "");
        }
        // ADR-063: the modules of the release, from the repository `dist::MAVEN` names (JitPack builds them from the tag).
        RuntimeRef::Release { version, dist } => {
            vars.set("KOTLIN_RUNTIME_BUILD", "");
            vars.set("UNDRA_MAVEN_GROUP", dist::MAVEN.group);
            vars.set("KOTLIN_RUNTIME_VERSION", dist::maven_version(version));
            vars.set(
                "MAVEN_REPOSITORY",
                dist.gradle_repository("        ", true).unwrap_or_default(),
            );
        }
    }

    // Web
    let web_dir = root.join("web");
    vars.set("GENERATED_TS_PATH", rel(&web_dir, &generated.join("ts")));
    vars.set("PROJECT_ROOT_PATH", rel(&web_dir, root));
    vars.set(
        "WASM_IMPORT",
        rel(
            &web_dir.join("src"),
            &build.join(format!("web/{}.wasm", core_names.namespace())),
        ),
    );
    // What `vite dev` serves instead of the shipped module: the debug build (ADR-046).
    vars.set(
        "WASM_DEBUG_PATH",
        rel(
            &web_dir,
            &build.join(format!("symbols/web/{}.dwarf.wasm", core_names.namespace())),
        ),
    );
    match &runtimes.ts {
        RuntimeRef::Path(dir) => {
            let runtime_src = rel(&web_dir, &dir.join("src/index.ts"));
            // vite.config.ts is bundled by Vite and run by Node, so it names the plugin's source by
            // path (a checkout has no `node_modules/@undra/runtime`).
            vars.set("RUNTIME_VITE_IMPORT", rel(&web_dir, &dir.join("src/vite")));
            vars.set("RUNTIME_DEPENDENCY", "");
            vars.set(
                "RUNTIME_ALIAS",
                format!("      // The Undra runtime, from the Undra checkout's sources.\n      \"@undra/runtime\": here(\"{runtime_src}\"),\n"),
            );
            vars.set(
                "RUNTIME_PATHS",
                format!(",\n      \"@undra/runtime\": [\"{runtime_src}\"]"),
            );
            vars.set(
                "EXTRA_FS_ALLOW",
                format!(", here(\"{}\")", rel(&web_dir, dir)),
            );
            vars.set("RUNTIME_DEDUPE", "");
        }
        // ADR-063: the runtime's tarball, an asset of the release on GitHub.
        RuntimeRef::Release { version, dist } => {
            vars.set("RUNTIME_VITE_IMPORT", "@undra/runtime/vite");
            vars.set(
                "RUNTIME_DEPENDENCY",
                format!(
                    "\"@undra/runtime\": \"{}\",\n    ",
                    dist.npm_url("undra-runtime", version)
                ),
            );
            vars.set("RUNTIME_ALIAS", "");
            // The generated bindings live outside web/ (`generated/ts`) and import `@undra/runtime`, which is installed in
            // web/node_modules: Node's lookup from their directory would never get there. TypeScript is pointed at the
            // installed package, and Vite resolves it from this app whoever imports it.
            vars.set(
                "RUNTIME_PATHS",
                ",\n      \"@undra/runtime\": [\"./node_modules/@undra/runtime\"]",
            );
            vars.set(
                "RUNTIME_DEDUPE",
                "    // The generated bindings (outside web/) import @undra/runtime: resolve it from this app's node_modules.\n    dedupe: [\"@undra/runtime\"],\n",
            );
            vars.set("EXTRA_FS_ALLOW", "");
        }
    }
    vars
}

/// The `XCLocalSwiftPackageReference` / `XCRemoteSwiftPackageReference` sections of the Xcode
/// project: the generated package is always local, the runtime is local in a checkout and a
/// versioned remote otherwise.
fn package_references(generated: &str, runtime: &RuntimeRef, ios_dir: &Path) -> String {
    let mut out = format!(
        "/* Begin XCLocalSwiftPackageReference section */\n\t\tA0A0A0A0A0A0A0A000000120 /* generated Swift package */ = {{\n\t\t\tisa = XCLocalSwiftPackageReference;\n\t\t\trelativePath = \"{generated}\";\n\t\t}};\n"
    );
    match runtime {
        RuntimeRef::Path(dir) => {
            out.push_str(&format!(
                "\t\tA0A0A0A0A0A0A0A000000121 /* UndraRuntime package */ = {{\n\t\t\tisa = XCLocalSwiftPackageReference;\n\t\t\trelativePath = \"{}\";\n\t\t}};\n/* End XCLocalSwiftPackageReference section */",
                rel(ios_dir, dir)
            ));
        }
        // ADR-063: the Swift package at the root of the Undra repository, from the project's release on.
        RuntimeRef::Release { version, dist } => {
            out.push_str("/* End XCLocalSwiftPackageReference section */\n\n/* Begin XCRemoteSwiftPackageReference section */\n");
            out.push_str(&format!(
                "\t\tA0A0A0A0A0A0A0A000000121 /* UndraRuntime package */ = {{\n\t\t\tisa = XCRemoteSwiftPackageReference;\n\t\t\trepositoryURL = \"{}\";\n\t\t\trequirement = {{\n\t\t\t\tkind = upToNextMajorVersion;\n\t\t\t\tminimumVersion = {version};\n\t\t\t}};\n\t\t}};\n/* End XCRemoteSwiftPackageReference section */",
                dist.git_url
            ));
        }
    }
    out
}

/// Writes `undra.toml`, the Cargo workspace and the core crate: what a project is without app
/// shells (`undra adopt` stops here). Returns how many files.
pub(super) fn scaffold_core(setup: &Setup, vars: &Vars) -> Result<usize> {
    create_dir_all(&setup.root)?;
    write_if_changed(&setup.root.join(CONFIG_FILE), &setup.config.render())?;
    let mut count = 1;
    count += write_set(&setup.root, templates::PROJECT, vars)?;
    count += write_set(&setup.root, templates::CORE, vars)?;
    Ok(count)
}

/// Writes the project's own files, the core and the shells. Returns how many files.
fn scaffold(setup: &Setup) -> Result<usize> {
    let vars = variables(setup);
    let mut count = scaffold_core(setup, &vars)?;
    for platform in &setup.config.platforms {
        count += match platform {
            Platform::Ios => {
                // The Run Script phase's input list is made from the core just written: the
                // same function refreshes it on every iOS build (`builds::xcode`).
                crate::builds::xcode::write_inputs(
                    &setup.root,
                    &[setup.root.join(&setup.config.core_path)],
                )?;
                // The views differ with how the generated stores are observed (ADR-045).
                let observation = setup.config.swift_observation(None)?;
                1 + write_set(&setup.root, &templates::ios(observation), &vars)?
            }
            Platform::Android => write_set(&setup.root, templates::ANDROID, &vars)?,
            Platform::Web => write_set(&setup.root, templates::WEB, &vars)?,
        };
    }
    if has_ci(setup) {
        let text = crate::ci::workflow(&setup.config, &setup.names, crate::ci::current_version());
        write_if_changed(&setup.root.join(crate::ci::WORKFLOW_PATH), &text)?;
        count += 1;
    }
    count += write_set(
        &setup.root,
        std::slice::from_ref(&templates::LLDBINIT),
        &Vars::new(),
    )?;
    count += write_set(
        &setup.root,
        std::slice::from_ref(&templates::README),
        &readme_vars(setup, vars),
    )?;
    Ok(count)
}

/// Whether the project gets a CI workflow: not when it uses a local checkout of Undra, which a
/// CI machine cannot see (the workflow installs a released `undra` and the core pins a release).
fn has_ci(setup: &Setup) -> bool {
    setup.repo.is_none()
}

/// Renders every file of `set` below `root`.
pub(super) fn write_set(root: &Path, set: &[TemplateFile], vars: &Vars) -> Result<usize> {
    for file in set {
        let path = vars.render(file.path).map_err(template_bug)?;
        let contents = vars.render(file.contents).map_err(template_bug)?;
        let dest = root.join(&path);
        write_if_changed(&dest, &contents)?;
        if file.executable {
            make_executable(&dest)?;
        }
    }
    Ok(set.len())
}

fn template_bug(name: String) -> CliError {
    CliError::new(
        Code::ToolFailed,
        format!("an undra-cli template uses the placeholder @@{name}@@ and nothing sets it"),
        "this is a bug in undra-cli, not in your arguments",
        "report it at https://github.com/shreypdev/undra/issues",
    )
}

/// Variables of the README on top of the shared ones.
fn readme_vars(setup: &Setup, vars: Vars) -> Vars {
    let has = |p: Platform| setup.config.platforms.contains(&p);
    let mut vars = vars;
    vars.set(
        "CI_LINE",
        if has_ci(setup) {
            ".github/       the CI workflow (undra.yml): the core, and a job per app\n"
        } else {
            ""
        },
    );
    vars.set(
        "PLATFORM_LIST",
        setup
            .config
            .platforms
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", "),
    );
    // The per-platform sections are templates themselves: render them with the shared values.
    let section = |vars: &Vars, platform: Platform, text: &str| {
        if has(platform) {
            vars.render(text).unwrap_or_default()
        } else {
            String::new()
        }
    };
    let (ios, android, web) = (
        section(&vars, Platform::Ios, README_IOS),
        section(&vars, Platform::Android, README_ANDROID),
        section(&vars, Platform::Web, README_WEB),
    );
    vars.set("README_IOS", ios);
    vars.set("README_ANDROID", android);
    vars.set("README_WEB", web);
    vars.set("UNDRA_SOURCE_NOTE", match &setup.repo {
        Some(repo) => format!(
            "The core uses the Undra crates, and the apps the Undra runtimes, from the checkout at `{}` (`[undra] path` in undra.toml).",
            repo.display()
        ),
        None => format!(
            "The core depends on the Undra crates at the git tag `{UNDRA_RELEASE_TAG}` of {} (core/Cargo.toml), and the apps on the runtimes of the same release, all from that repository: the Swift package at its root, the Kotlin artifacts JitPack builds from the tag (`{}`), and the npm package attached to the GitHub Release (`web/package.json`). `undra upgrade` moves them all to a newer release together.",
            setup.dist.git_url,
            dist::maven_coordinate("runtime", UNDRA_VERSION)
        ),
    });
    vars
}

const README_IOS: &str = "
## iOS

```sh
xcodebuild -project ios/@@APP@@.xcodeproj -scheme @@APP@@ \\
    -destination 'generic/platform=iOS Simulator' build
```

Open `ios/@@APP@@.xcodeproj` in Xcode 16 or newer to run it. There is no `undra build` to run first: the **Build the
Undra core** Run Script phase, before Compile Sources, runs `undra build --platform ios --configuration $CONFIGURATION`
(a Debug build makes a debug core, a Release build a release one, `build/ios/@@CORE_BUNDLE@@.xcframework`). Xcode skips it
while the files in `ios/Config/undra-core-inputs.xcfilelist` are older than the ones in
`ios/Config/undra-core-outputs.xcfilelist`; `undra build` keeps the input list in step with the core's sources, so
commit it. The phase finds `undra` on `PATH` or in `~/.undra/bin`, `~/.cargo/bin` and Homebrew's directories (Xcode
started from the Dock has a short `PATH`) and says how to install it when it is missing; it turns user script
sandboxing off for the target, since it runs Cargo. The app links the core's library, `lib@@NAMESPACE@@.a`, by its path
in Other Linker Flags, not as a framework: Xcode reads an XCFramework while it plans the build, before the phase could
have made it. Each slice is one prelinked object whose only global symbol is the core's entry, which the bindings call
(`@@CORE_ENTRY@@.load()`), so it needs no `-force_load` and another Undra core can sit next to it in the app.

**Against `undra dev`.** In the scheme's Run environment variables set `UNDRA_DEV_URL` to the `ws://` URL
`undra dev` prints (a simulator can use `ws://127.0.0.1:7443`; a device needs your computer's address and
`undra dev --addr 0.0.0.0:7443`). Debug builds then use that core instead of the linked one: edit the Rust,
save, and the app reconnects and comes back to the screen it was on, with its state (`undra dev` carries the
core's state across a rebuild; the bar at the top says `Reloaded, state kept`, and shows the connection;
`core.connectionState` and `core.connection` are the API). See docs/DEV_LOOP.md in the Undra repository.
";

const README_ANDROID: &str = "
## Android

```sh
cd android && ./gradlew :app:assembleDebug    # or open android/ in Android Studio
```

There is no `undra build` to run first: `android/app/build.gradle.kts` has an `undraBuild` task that runs
`undra build --platform android`, `preBuild` depends on it, and Gradle skips it while the core's sources
(`core/src/**`, the Cargo manifests) and `build/android/jniLibs` are unchanged. The variant decides the profile:
`assembleRelease` and `bundleRelease` build a release core (about 1.5 MB per ABI), anything else a debug core (fast to
build, right for the dev loop, but tens of megabytes per ABI: 42 MB for the playground, and the APK carries every
byte of it). `-PundraRelease=true` or `false` overrides, `-PundraSkipBuild=true` (or `UNDRA_SKIP_BUILD=1`) skips
the task when the core was built in an earlier step. Sources outside `core/` (a path dependency in a monorepo) go in
with `undraBuild { sources.from(\"../../shared/src\") }`. The task finds `undra` on `PATH` or in `~/.undra/bin`,
`~/.cargo/bin` and Homebrew's directories (a GUI-launched Gradle has a short `PATH`), `UNDRA_BIN` names one
explicitly, and it says how to install it when it is missing.

The app module packages `build/android/jniLibs` (the path in `android/app/build.gradle.kts` is relative to
the module, `android/app`; after a build `undra` checks that it still names the directory it wrote) and
depends on the generated Kotlin module (`generated/kotlin`, included by `android/settings.gradle.kts`) and on
the Undra runtime (`@@UNDRA_MAVEN_GROUP@@:runtime`).

**Against `undra dev`.** Debug builds can run against the core `undra dev` serves instead of the one in the
APK, so a Rust change needs no rebuild of the app and `undra build --platform android` is not needed at all:

```sh
undra dev --android                                            # prints the addresses; adb reverse for attached devices
cd android && ./gradlew -PundraDevUrl=ws://10.0.2.2:7443 :app:installDebug    # the emulator's name for this machine
adb shell am start -n @@APP_ID@@/.MainActivity --es undra_dev_url ws://127.0.0.1:7443   # or switch at run time (USB device)
```

`10.0.2.2` is how the emulator reaches this machine; a USB device uses `adb reverse tcp:7443 tcp:7443` (which
`undra dev --android` runs) and `127.0.0.1`. Cleartext traffic and the dev URL exist in debug builds only
(`android/app/src/debug/AndroidManifest.xml`; release builds get neither). Edit the Rust, save, and the app
reconnects and comes back to the screen it was on, with its state (`undra dev` carries the core's state across a
rebuild; the bar at the top says `Reloaded, state kept` and shows the connection, `core.connectionState` is a
`StateFlow`). See docs/DEV_LOOP.md in the Undra repository.
";

const README_WEB: &str = "
## Web

```sh
cd web && npm install && npm run dev          # http://localhost:5173
npm run build                                 # type-checks and bundles
```

There is no `undra build` to run first: the `undra()` plugin of `web/vite.config.ts` (`@undra/runtime/vite`) runs
`undra build --platform web` when Vite starts, for `npm run build` as for `npm run dev`, and under `npm run dev` it
rebuilds the core and reloads the page onto it whenever `core/src` changes (a page opened with `?undra=`, below, is left on the
core `undra dev` serves, which keeps its state). It finds `undra` on `PATH` (or `UNDRA_BIN`) and says
how to install it when it is missing; `UNDRA_SKIP_BUILD=1` skips it when the core was built in an earlier step.

The page loads the wasm core and runs it on the main thread. Add `?undra=ws://127.0.0.1:7443` to the URL
(with `undra dev` running) to use the core that `undra dev` serves instead: edit the Rust, save, and the page is
on the rebuilt core with its state, no reload (a dropped connection is reconnected by the runtime; a bar at the top
shows what it is doing, `Reloaded, state kept` after a rebuild, and `core.connection` is the signal behind it). Only
`npm run dev` reads `?undra=`: a production build ignores it.
";

/// What `init` generated from the embedded schema.
pub(super) struct Generated {
    pub(super) files: usize,
    pub(super) hash: u64,
}

/// Generates the bindings of the template core into `generated/`, exactly as `undra bindgen`
/// would.
pub(super) fn generate_bindings(setup: &Setup) -> Result<Generated> {
    let schema = parse_schema_json(templates::CORE_SCHEMA, &setup.names.core_package)?;
    let project = Project {
        root: setup.root.clone(),
        config: setup.config.clone(),
    };
    let mut generator = Generator::for_crate(&setup.names.core_package);
    generator.swift_module = project.swift_module();
    generator.kotlin_package = project.kotlin_package();
    bindgen::configure_swift(&mut generator, &setup.config, None, None)?;
    let plan = Plan {
        generator,
        platforms: setup.config.platforms.clone(),
        runtimes: Runtimes::for_project(&project, &setup.dist),
        out: canonicalize_lenient(&project.generated_dir()),
        lint_exclusions: setup.config.bindings.lint_exclusions,
    };
    let files = bindgen::plan_files(&schema, &plan)?;
    bindgen::apply(&plan.out, &files)?;
    Ok(Generated {
        files: files.len(),
        hash: schema.hash(),
    })
}

/// Gives the Android project a Gradle wrapper: copied from the Undra checkout's Kotlin runtime
/// when there is one, else created with `gradle wrapper` when Gradle is installed.
fn gradle_wrapper(env: &Env<'_>, setup: &Setup) {
    let android = setup.root.join("android");
    if let Some(repo) = &setup.repo {
        let source = repo.join(KOTLIN_IN_REPO);
        let mut copied = true;
        for file in [
            "gradlew",
            "gradlew.bat",
            "gradle/wrapper/gradle-wrapper.jar",
            "gradle/wrapper/gradle-wrapper.properties",
        ] {
            let from = source.join(file);
            if from.is_file() {
                if fsutil::copy_file(&from, &android.join(file)).is_err() {
                    copied = false;
                }
            } else {
                copied = false;
            }
        }
        if copied {
            let _ = make_executable(&android.join("gradlew"));
            return;
        }
    }
    let Some(gradle) = env.sys.which("gradle", &[]) else {
        env.ui.warn("Gradle is not installed, so android/ has no ./gradlew; Android Studio creates one when it opens the project (or run `gradle wrapper` in android/)");
        return;
    };
    let status = Command::new(gradle)
        .args([
            "wrapper",
            "--gradle-version",
            GRADLE_VERSION,
            "--no-validate-url",
        ])
        .current_dir(&android)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !status.is_ok_and(|s| s.success()) {
        env.ui.warn("could not create the Gradle wrapper; Android Studio creates one when it opens android/");
    }
}

/// The text printed after a successful `init`.
fn next_steps(setup: &Setup) -> String {
    let name = &setup.names.project;
    let mut out = format!(
        "Next:\n  cd {name}\n  undra doctor                 check this machine\n  undra dev                    serve the core to a running app, rebuilding on change\n"
    );
    out.push_str("\nThen run an app:\n");
    for platform in &setup.config.platforms {
        match platform {
            Platform::Ios => {
                out.push_str("  iOS      open ios/ in Xcode, or xcodebuild (README.md)\n")
            }
            Platform::Android => out.push_str(
                "  Android  open android/ in Android Studio, or ./gradlew :app:installDebug\n",
            ),
            Platform::Web => out.push_str("  web      cd web && npm install && npm run dev\n"),
        }
    }
    out.push_str("\nREADME.md has the details for each platform.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::RealSys;
    use crate::ui::Ui;

    fn env(dir: &Path) -> Env<'static> {
        static SYS: RealSys = RealSys;
        Env {
            sys: &SYS,
            ui: Ui::plain(),
            project_dir: Some(dir.to_path_buf()),
        }
    }

    fn args(name: &str, platforms: &str) -> InitArgs {
        InitArgs {
            name: name.to_owned(),
            platforms: platforms.to_owned(),
            id: None,
            undra_path: None,
            dir: None,
            ios_deployment_target: None,
        }
    }

    #[test]
    fn an_ios_15_project_gets_observable_object_stores_and_views_that_observe_them() {
        let parent = fsutil::unique_temp_dir("init-ios15");
        create_dir_all(&parent).unwrap();
        let mut floor = args("floor-app", "ios");
        floor.ios_deployment_target = Some("15.0".to_owned());
        run(&env(&parent), &floor).unwrap();
        let root = parent.canonicalize().unwrap().join("floor-app");
        let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
        // The config says so, and Xcode gets the same target.
        assert!(read("undra.toml").contains("deployment_target = \"15.0\""));
        assert!(
            read("ios/FloorApp.xcodeproj/project.pbxproj")
                .contains("IPHONEOS_DEPLOYMENT_TARGET = 15.0;")
        );
        // The bindings follow the floor: ObservableObject stores, the runtime's UndraDuration, an iOS 15 package.
        let stores = read("generated/swift/Sources/FloorAppCore/Generated/Stores.swift");
        assert!(
            stores.contains("import Combine")
                && stores.contains("ObservableObject, @unchecked Sendable")
        );
        assert!(stores.contains("@Published public private(set) var todos: [Todo]"));
        assert!(!stores.contains("@Observable"));
        assert!(
            read("generated/swift/Package.swift").contains("platforms: [.iOS(.v15), .macOS(.v12)]")
        );
        // The views observe an ObservableObject, use nothing newer than iOS 15 and say why.
        let views = [
            read("ios/FloorApp/MainApp.swift"),
            read("ios/FloorApp/ContentView.swift"),
            read("ios/FloorApp/DevStatusBar.swift"),
        ]
        .join("\n");
        assert!(views.contains("@ObservedObject var todos: Todos"));
        assert!(views.contains("@ObservedObject var connection: UndraConnectionObject"));
        assert!(
            views.contains("NavigationView {") && views.contains(".navigationViewStyle(.stack)")
        );
        for newer in [
            "NavigationStack",
            "@Observable",
            "Task.sleep(for:",
            "core.connection.state",
        ] {
            assert!(!views.contains(newer), "{newer} is newer than iOS 15");
        }
        // The bootstrap is shared by both modes.
        assert!(read("ios/FloorApp/UndraBootstrap.swift").contains("UndraBootstrap"));
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn an_ios_target_below_the_runtimes_floor_is_refused_before_anything_is_written() {
        let parent = fsutil::unique_temp_dir("init-ios14");
        create_dir_all(&parent).unwrap();
        for (target, needle) in [("14.0", "15.0"), ("latest", "not an iOS version")] {
            let mut bad = args("old-app", "ios");
            bad.ios_deployment_target = Some(target.to_owned());
            let e = run(&env(&parent), &bad).unwrap_err();
            assert!(e.what.contains(needle) || e.fix.contains(needle), "{e:?}");
        }
        assert!(!parent.join("old-app").exists());
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn init_creates_a_complete_project_from_the_templates() {
        let parent = fsutil::unique_temp_dir("init-unit");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("todo-app", "ios,android,web")).unwrap();
        let root = parent.canonicalize().unwrap().join("todo-app");
        let files = fsutil::list_files(&root);
        for expected in [
            "undra.toml",
            "Cargo.toml",
            ".gitignore",
            ".lldbinit",
            "README.md",
            "core/Cargo.toml",
            "core/src/lib.rs",
            "ios/TodoApp.xcodeproj/project.pbxproj",
            "ios/TodoApp/MainApp.swift",
            "android/app/src/main/kotlin/com/example/todoapp/MainActivity.kt",
            "android/settings.gradle.kts",
            "web/package.json",
            "web/src/App.tsx",
            "generated/swift/Package.swift",
            "generated/swift/Sources/TodoAppCore/Generated/Stores.swift",
            "generated/kotlin/src/main/kotlin/com/example/todoapp/core/Stores.kt",
            "generated/ts/src/stores.ts",
            "generated/.undra-generated",
        ] {
            assert!(
                files.iter().any(|f| f == expected),
                "missing {expected}; have {files:#?}"
            );
        }
        // No placeholder survives rendering.
        for file in &files {
            if let Ok(text) = std::fs::read_to_string(root.join(file)) {
                assert!(!text.contains("@@"), "{file} still has a placeholder");
            }
        }
        // The Rust formatters for LLDB are loaded from the toolchain on the machine, not from a path
        // of this one (ADR-046), so the file can be committed.
        let lldbinit = std::fs::read_to_string(root.join(".lldbinit")).unwrap();
        assert!(
            lldbinit.contains("rustc\", \"--print\", \"sysroot\"")
                && lldbinit.contains("lldb_lookup.py"),
            "{lldbinit}"
        );
        assert!(
            !lldbinit.contains(&parent.display().to_string()),
            "{lldbinit}"
        );
        // The project reads back as a project.
        let project = Project::open(&root).unwrap();
        assert_eq!(project.config.name, "todo-app");
        assert_eq!(project.config.id, "com.example.todoapp");
        assert_eq!(project.config.platforms, Platform::ALL.to_vec());
        let core = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
        assert!(
            core.contains("name = \"todo-app-core\"")
                && core.contains(&format!(
                    "undra = {{ git = \"https://github.com/shreypdev/undra\", tag = \"v{}\" }}",
                    env!("CARGO_PKG_VERSION")
                )),
            "{core}"
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn only_the_chosen_platforms_get_shells() {
        let parent = fsutil::unique_temp_dir("init-web");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("site", "web")).unwrap();
        let root = parent.canonicalize().unwrap().join("site");
        assert!(root.join("web/package.json").is_file());
        assert!(!root.join("ios").exists() && !root.join("android").exists());
        assert!(root.join("generated/ts/src/index.ts").is_file());
        assert!(!root.join("generated/swift").exists());
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(
            readme.contains("## Web") && !readme.contains("## iOS"),
            "{readme}"
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn a_non_empty_directory_is_refused_with_the_way_out() {
        let parent = fsutil::unique_temp_dir("init-exists");
        create_dir_all(&parent.join("taken")).unwrap();
        std::fs::write(parent.join("taken/file"), "x").unwrap();
        let e = run(&env(&parent), &args("taken", "web")).unwrap_err();
        assert_eq!(e.code, Code::WouldOverwrite);
        assert!(e.fix.contains("undra adopt"), "{e}");
        assert_eq!(
            std::fs::read_to_string(parent.join("taken/file")).unwrap(),
            "x"
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn bad_arguments_are_explained_before_anything_is_written() {
        let parent = fsutil::unique_temp_dir("init-bad");
        create_dir_all(&parent).unwrap();
        assert_eq!(
            run(&env(&parent), &args("1bad", "web")).unwrap_err().code,
            Code::BadArgument
        );
        assert_eq!(
            run(&env(&parent), &args("ok", "tv")).unwrap_err().code,
            Code::BadArgument
        );
        let mut with_id = args("ok", "web");
        with_id.id = Some("Bad_Id".into());
        assert_eq!(
            run(&env(&parent), &with_id).unwrap_err().code,
            Code::BadArgument
        );
        let mut with_path = args("ok", "web");
        with_path.undra_path = Some(parent.join("not-a-checkout"));
        assert_eq!(
            run(&env(&parent), &with_path).unwrap_err().code,
            Code::BadArgument
        );
        assert!(fsutil::is_empty_dir(&parent), "nothing was written");
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_embedded_schema_is_canonical_and_generates_bindings() {
        let schema = parse_schema_json(templates::CORE_SCHEMA, "demo-core").unwrap();
        assert_eq!(schema.canonical_json(), templates::CORE_SCHEMA.trim());
        assert!(schema.objects.iter().any(|o| o.name == "Todos"));
    }

    #[test]
    fn checkout_mode_writes_relative_paths_the_shells_can_follow() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let parent = fsutil::unique_temp_dir("init-path");
        create_dir_all(&parent).unwrap();
        let mut a = args("demo", "ios,android,web");
        a.undra_path = Some(repo.clone());
        run(&env(&parent), &a).unwrap();
        let root = parent.canonicalize().unwrap().join("demo");
        let core = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
        // The path in core/Cargo.toml resolves to the checkout's `undra` crate.
        let dep = core.lines().find(|l| l.starts_with("undra = ")).unwrap();
        let path = dep.split('"').nth(1).unwrap();
        assert!(
            root.join("core").join(path).join("Cargo.toml").is_file(),
            "{dep}"
        );
        let project = Project::open(&root).unwrap();
        assert_eq!(project.undra_repo().unwrap(), repo);
        let pbx = std::fs::read_to_string(root.join("ios/Demo.xcodeproj/project.pbxproj")).unwrap();
        assert!(
            pbx.contains("XCLocalSwiftPackageReference")
                && pbx.contains("runtimes/swift/UndraRuntime"),
            "pbxproj"
        );
        let settings = std::fs::read_to_string(root.join("android/settings.gradle.kts")).unwrap();
        assert!(
            settings.contains("includeBuild(")
                && settings.contains("runtimes/kotlin/undra-runtime")
                && !settings.contains("jitpack"),
            "{settings}"
        );
        // The composite build substitutes the repository's own coordinates; nothing names a release.
        let app = std::fs::read_to_string(root.join("android/app/build.gradle.kts")).unwrap();
        assert!(
            app.contains("implementation(\"dev.undra:runtime:0.1.0-SNAPSHOT\")")
                && !app.contains("com.github.shreypdev"),
            "{app}"
        );
        assert!(
            root.join("android/gradlew").is_file(),
            "the wrapper is copied from the checkout"
        );
        let vite = std::fs::read_to_string(root.join("web/vite.config.ts")).unwrap();
        assert!(
            vite.contains("@undra/runtime")
                && vite.contains("runtimes/ts/@undra/runtime/src/index.ts"),
            "{vite}"
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    /// The dependency lines of every template of a released project (ADR-063): one release, all from GitHub.
    fn assert_release_lines(root: &Path, app: &str, git: &str, releases: &str, maven: &str) {
        let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
        let v = UNDRA_VERSION;
        let core = read("core/Cargo.toml");
        assert!(
            core.contains(&format!("undra = {{ git = \"{git}\", tag = \"v{v}\" }}")),
            "{core}"
        );
        assert!(
            read("undra.toml").contains(&format!("version = \"{v}\"\n")),
            "undra.toml names the full release"
        );
        let pbx = read(&format!("ios/{app}.xcodeproj/project.pbxproj"));
        assert!(
            pbx.contains(&format!("isa = XCRemoteSwiftPackageReference;\n\t\t\trepositoryURL = \"{git}\";\n\t\t\trequirement = {{\n\t\t\t\tkind = upToNextMajorVersion;\n\t\t\t\tminimumVersion = {v};")),
            "{pbx}"
        );
        let swift = read("generated/swift/Package.swift");
        assert!(
            swift.contains(&format!(".package(url: \"{git}\", from: \"{v}\")"))
                && swift.contains(".product(name: \"UndraRuntime\", package: \"undra\")"),
            "{swift}"
        );
        let app_gradle = read("android/app/build.gradle.kts");
        for module in ["runtime", "android-adapters"] {
            assert!(
                app_gradle.contains(&format!(
                    "implementation(\"com.github.shreypdev.undra:{module}:v{v}\")"
                )),
                "{app_gradle}"
            );
        }
        assert!(
            app_gradle.contains(&format!(
                "// implementation(\"com.github.shreypdev.undra:android-work:v{v}\")"
            )),
            "{app_gradle}"
        );
        assert!(
            read("generated/kotlin/build.gradle.kts")
                .contains(&format!("api(\"com.github.shreypdev.undra:runtime:v{v}\")"))
        );
        let settings = read("android/settings.gradle.kts");
        assert!(
            settings.contains(&format!(
                "        mavenCentral()\n        // Undra's Kotlin runtime, built from the release tag (ADR-063). Only its group is looked up here.\n        maven {{\n            url = uri(\"{maven}\")\n            content {{ includeGroup(\"com.github.shreypdev.undra\") }}\n        }}\n    }}\n"
            )),
            "{settings}"
        );
        assert!(!settings.contains("includeBuild("), "{settings}");
        let pkg = read("web/package.json");
        assert!(
            pkg.contains(&format!(
                "\"@undra/runtime\": \"{releases}/v{v}/undra-runtime-{v}.tgz\","
            )),
            "{pkg}"
        );
        // The generated bindings (outside web/) find the installed runtime: TypeScript through `paths`, Vite by `dedupe`
        // (without them `npm run build` fails to resolve `@undra/runtime` from generated/ts; the launch rehearsal found it).
        let tsconfig = read("web/tsconfig.json");
        assert!(
            tsconfig.contains("\"@undra/runtime\": [\"./node_modules/@undra/runtime\"]"),
            "{tsconfig}"
        );
        let vite = read("web/vite.config.ts");
        assert!(vite.contains("dedupe: [\"@undra/runtime\"],"), "{vite}");
        let readme = read("README.md");
        assert!(
            readme.contains(&format!("com.github.shreypdev.undra:runtime:v{v}")),
            "{readme}"
        );
        // Nothing names what does not exist: the old Swift repository, Maven Central's group, a registry range.
        for file in [
            format!("ios/{app}.xcodeproj/project.pbxproj"),
            "generated/swift/Package.swift".to_owned(),
            "android/app/build.gradle.kts".to_owned(),
            "generated/kotlin/build.gradle.kts".to_owned(),
            "web/package.json".to_owned(),
            "README.md".to_owned(),
        ] {
            let text = read(&file);
            for gone in ["undra-swift", "\"dev.undra:", "\"^0.", "\"^1."] {
                assert!(!text.contains(gone), "{file} still has {gone}");
            }
        }
    }

    #[test]
    fn a_released_project_names_the_release_on_github_on_every_platform() {
        let parent = fsutil::unique_temp_dir("init-release");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("demo", "ios,android,web")).unwrap();
        let root = parent.canonicalize().unwrap().join("demo");
        assert_release_lines(
            &root,
            "Demo",
            "https://github.com/shreypdev/undra",
            "https://github.com/shreypdev/undra/releases/download",
            "https://jitpack.io",
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn a_mirror_replaces_every_address_and_nothing_else() {
        let parent = fsutil::unique_temp_dir("init-mirror");
        create_dir_all(&parent).unwrap();
        let root = parent.canonicalize().unwrap().join("demo");
        let dist = Dist {
            git_url: "file:///tmp/rehearsal/remote/undra.git".to_owned(),
            release_url: "http://127.0.0.1:8123/releases".to_owned(),
            maven_repo: Some("file:///tmp/rehearsal/m2".to_owned()),
        };
        let setup = Setup {
            root: root.clone(),
            names: Names::derive("demo"),
            config: ProjectConfig::new("demo", "com.example.demo", Platform::ALL.to_vec()),
            repo: None,
            dist,
        };
        scaffold(&setup).unwrap();
        generate_bindings(&setup).unwrap();
        assert_release_lines(
            &root,
            "Demo",
            "file:///tmp/rehearsal/remote/undra.git",
            "http://127.0.0.1:8123/releases",
            "file:///tmp/rehearsal/m2",
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_readme_documents_the_gradle_task_and_the_release_variant() {
        let parent = fsutil::unique_temp_dir("init-readme");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("demo", "android")).unwrap();
        let root = parent.canonicalize().unwrap().join("demo");
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(
            readme.contains("`undraBuild` task")
                && readme.contains("`assembleRelease` and `bundleRelease` build a release core")
                && readme.contains("anything else a debug core")
                && readme.contains("-PundraSkipBuild=true"),
            "{readme}"
        );
        // The Gradle line says what its path is relative to, and is the one `undra build` checks.
        let app = std::fs::read_to_string(root.join("android/app/build.gradle.kts")).unwrap();
        assert!(
            app.contains("relative to this module")
                && app.contains("jniLibs.srcDir(\"../../build/android/jniLibs\")"),
            "{app}"
        );
        let _ = std::fs::remove_dir_all(parent);
    }
    /// The 24-digit object ids of a pbxproj that are defined (`ID /* name */ = {`) and the ones that appear.
    fn pbx_ids(
        text: &str,
    ) -> (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    ) {
        let mut defined = std::collections::BTreeSet::new();
        let mut seen = std::collections::BTreeSet::new();
        for line in text.lines() {
            let mut rest = line;
            while let Some(at) = rest.find("A0A0A0A0A0A0A0A0") {
                let id = &rest[at..(at + 24).min(rest.len())];
                if id.len() == 24 && id.chars().all(|c| c.is_ascii_hexdigit()) {
                    seen.insert(id.to_owned());
                    let after = rest[at + 24..].trim_start();
                    let after = after.strip_prefix("/*").map_or(after, |c| {
                        c.split_once("*/").map_or("", |(_, tail)| tail.trim_start())
                    });
                    if line.trim_start().starts_with(id) && after.starts_with("= {") {
                        defined.insert(id.to_owned());
                    }
                }
                rest = &rest[at + 24..];
            }
        }
        (defined, seen)
    }

    #[test]
    fn the_xcode_project_builds_the_core_in_a_run_script_phase_before_compile_sources() {
        let parent = fsutil::unique_temp_dir("init-xcode");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("phased", "ios")).unwrap();
        let root = parent.canonicalize().unwrap().join("phased");
        let pbx =
            std::fs::read_to_string(root.join("ios/Phased.xcodeproj/project.pbxproj")).unwrap();

        // The phase exists, runs before Sources, and has file lists.
        let phases = pbx
            .split("buildPhases = (")
            .nth(1)
            .unwrap()
            .split(");")
            .next()
            .unwrap();
        let order: Vec<&str> = phases
            .lines()
            .filter_map(|l| l.split("/*").nth(1)?.split("*/").next())
            .map(str::trim)
            .collect();
        assert_eq!(
            order,
            ["Build the Undra core", "Sources", "Frameworks", "Resources"],
            "{phases}"
        );
        for needle in [
            "isa = PBXShellScriptBuildPhase;",
            "inputFileListPaths = (\n\t\t\t\t\"$(SRCROOT)/Config/undra-core-inputs.xcfilelist\",",
            "outputFileListPaths = (\n\t\t\t\t\"$(SRCROOT)/Config/undra-core-outputs.xcfilelist\",",
            "shellPath = /bin/sh;",
        ] {
            assert!(pbx.contains(needle), "no {needle:?}:\n{pbx}");
        }
        // The script: finds undra, teaches when it is missing, hands it a clean environment, passes the configuration.
        let script = pbx
            .lines()
            .find(|l| l.trim_start().starts_with("shellScript = "))
            .unwrap();
        for needle in [
            "export PATH=\\\"$HOME/.undra/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\\\"",
            "command -v undra",
            "error: [undra::C0003] 'undra' was not found",
            "curl -fsSL https://shreypdev.github.io/undra/install.sh | sh",
            "exec env -i HOME=",
            "undra -C \\\"$SRCROOT/..\\\" build --platform ios --configuration \\\"$CONFIGURATION\\\"",
        ] {
            assert!(
                script.contains(needle),
                "the script lacks {needle:?}:\n{script}"
            );
        }
        // (A backtick in an echoed string would run a command; the comments may have them.)
        for line in script
            .split("\\n")
            .filter(|l| l.trim_start().starts_with("echo"))
        {
            assert!(
                !line.contains('`'),
                "a backtick would run a command: {line}"
            );
        }
        // Sandboxing is off for the app target (the phase runs Cargo), and the core is not a framework of the project.
        assert_eq!(
            pbx.matches("ENABLE_USER_SCRIPT_SANDBOXING = NO;").count(),
            2
        );
        assert!(
            !pbx.contains("PhasedCore.xcframework in Frameworks"),
            "Xcode would read it before the phase made it"
        );
        // ADR-044: the prelinked library is linked by its path, without -force_load.
        assert!(
            pbx.contains("\"$(SRCROOT)/../build/ios/PhasedCore.xcframework/ios-arm64/libphased_core.a\"")
                && pbx.contains(
                    "\"$(SRCROOT)/../build/ios/PhasedCore.xcframework/ios-arm64-simulator/libphased_core.a\""
                ),
            "{pbx}"
        );
        assert!(!pbx.contains("\"-force_load\""), "{pbx}");
        // Every object the file refers to is defined.
        let (defined, seen) = pbx_ids(&pbx);
        assert!(defined.contains("A0A0A0A0A0A0A0A000000092"));
        assert_eq!(
            seen.difference(&defined).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "ids used but never defined"
        );

        // The lists: inputs from the core, outputs the XCFramework slices and the configuration's stamp.
        let inputs =
            std::fs::read_to_string(root.join("ios/Config/undra-core-inputs.xcfilelist")).unwrap();
        assert_eq!(
            inputs,
            "$(SRCROOT)/../Cargo.toml\n$(SRCROOT)/../core/Cargo.toml\n$(SRCROOT)/../core/src\n$(SRCROOT)/../core/src/lib.rs\n$(SRCROOT)/../undra.toml\n"
        );
        let outputs =
            std::fs::read_to_string(root.join("ios/Config/undra-core-outputs.xcfilelist")).unwrap();
        assert_eq!(
            outputs,
            "$(SRCROOT)/../build/ios/PhasedCore.xcframework/Info.plist\n$(SRCROOT)/../build/ios/PhasedCore.xcframework/ios-arm64/libphased_core.a\n$(SRCROOT)/../build/ios/PhasedCore.xcframework/ios-arm64-simulator/libphased_core.a\n$(SRCROOT)/../build/ios/.undra-configuration-$(CONFIGURATION)\n"
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn an_intel_simulator_slice_is_a_listed_output() {
        let setup = |archs: &[&str]| {
            let parent = fsutil::unique_temp_dir("init-xcode-archs");
            create_dir_all(&parent).unwrap();
            run(&env(&parent), &args("slices", "ios")).unwrap();
            let root = parent.canonicalize().unwrap().join("slices");
            let toml = std::fs::read_to_string(root.join("undra.toml")).unwrap();
            let list = archs
                .iter()
                .map(|a| format!("\"{a}\""))
                .collect::<Vec<_>>()
                .join(", ");
            std::fs::write(
                root.join("undra.toml"),
                toml.replace(
                    "simulator_archs = [\"arm64\"]",
                    &format!("simulator_archs = [{list}]"),
                ),
            )
            .unwrap();
            // Render again from the changed project, the way `variables` reads it.
            let project = Project::open(&root).unwrap();
            let vars = variables(&Setup {
                root: root.clone(),
                names: Names::derive("slices"),
                config: project.config,
                repo: None,
                dist: Dist::github(),
            });
            let outputs = vars.render(templates::IOS_OUTPUT_LIST).unwrap();
            let _ = std::fs::remove_dir_all(parent);
            outputs
        };
        assert!(
            setup(&["arm64", "x86_64"]).contains("/ios-arm64_x86_64-simulator/libslices_core.a")
        );
        assert!(setup(&["x86_64"]).contains("/ios-x86_64-simulator/libslices_core.a"));
    }

    #[test]
    fn the_gradle_app_builds_the_core_with_a_task_prebuild_depends_on() {
        let parent = fsutil::unique_temp_dir("init-gradle");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("tasked", "android")).unwrap();
        let root = parent.canonicalize().unwrap().join("tasked");
        let app = std::fs::read_to_string(root.join("android/app/build.gradle.kts")).unwrap();
        for needle in [
            "abstract class UndraBuild @Inject constructor(private val execOps: ExecOperations) : DefaultTask()",
            "tasks.register<UndraBuild>(\"undraBuild\")",
            "tasks.named(\"preBuild\") { dependsOn(undraBuild) }",
            // The inputs are the core's sources and manifests, the output the directory `undra build` writes.
            "@get:InputFiles",
            "@get:OutputDirectory",
            "layout.projectDirectory.dir(\"../../core\")",
            "include(\"src/**\", \"Cargo.toml\", \"Cargo.lock\", \"build.rs\")",
            "layout.projectDirectory.dir(\"../..\").file(\"undra.toml\")",
            "libraries.set(layout.projectDirectory.dir(\"../../build/android/jniLibs\"))",
            // The command, the variant and the escape hatches.
            "\"-C\", projectRoot.get().asFile.absolutePath, \"build\", \"--platform\", \"android\"",
            "if (release.get()) command.add(\"--release\")",
            "it.name.contains(\"Release\")",
            "undraRelease",
            "undraSkipBuild",
            "UNDRA_SKIP_BUILD",
            "UNDRA_BIN",
            // A missing undra teaches, in the shape of the CLI's errors.
            "error[undra::C0003]: `undra` was not found",
            "curl -fsSL https://shreypdev.github.io/undra/install.sh | sh",
        ] {
            assert!(
                app.contains(needle),
                "the Gradle script lacks {needle:?}:\n{app}"
            );
        }
        // The packaging line that `undra build` verifies is still the only `jniLibs.srcDir` there is.
        assert_eq!(app.matches("jniLibs.srcDir(").count(), 1, "{app}");
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_vite_config_uses_the_undra_plugin() {
        let parent = fsutil::unique_temp_dir("init-vite");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("plugged", "web")).unwrap();
        let root = parent.canonicalize().unwrap().join("plugged");
        let vite = std::fs::read_to_string(root.join("web/vite.config.ts")).unwrap();
        assert!(
            vite.contains("import { undra } from \"@undra/runtime/vite\";"),
            "{vite}"
        );
        assert!(vite.contains("plugins: [undra(), react()],"), "{vite}");
        // `vite dev` serves the debug build of the core (ADR-046); a production build names none.
        assert!(
            vite.contains("export default defineConfig(({ command }) => ({")
                && vite.contains("command === \"serve\" ? \"/@fs\" + here(\"../build/symbols/web/")
                && vite.contains(".dwarf.wasm\")"),
            "{vite}"
        );
        let entry = std::fs::read_to_string(root.join("web/src/undra.ts")).unwrap();
        assert!(
            entry.contains("declare const __UNDRA_DEBUG_WASM__: string;")
                && entry.contains("import.meta.env.DEV && __UNDRA_DEBUG_WASM__ !== \"\""),
            "{entry}"
        );
        let _ = std::fs::remove_dir_all(parent);

        // In a checkout there is no node_modules/@undra/runtime: the config names the plugin's source.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let parent = fsutil::unique_temp_dir("init-vite-path");
        create_dir_all(&parent).unwrap();
        let mut a = args("plugged", "web");
        a.undra_path = Some(repo);
        run(&env(&parent), &a).unwrap();
        let root = parent.canonicalize().unwrap().join("plugged");
        let vite = std::fs::read_to_string(root.join("web/vite.config.ts")).unwrap();
        let import = vite
            .lines()
            .find(|l| l.starts_with("import { undra }"))
            .unwrap();
        let path = import.split('"').nth(1).unwrap();
        assert!(
            root.join("web").join(path).with_extension("ts").is_file(),
            "{import} does not lead to src/vite.ts"
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn a_project_outside_a_checkout_pins_the_release_by_git_tag() {
        let parent = fsutil::unique_temp_dir("init-pinned");
        create_dir_all(&parent).unwrap();
        run(&env(&parent), &args("pinned", "web")).unwrap();
        let root = parent.canonicalize().unwrap().join("pinned");
        let core = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
        let dep = core.lines().find(|l| l.starts_with("undra = ")).unwrap();
        assert_eq!(
            dep,
            format!(
                "undra = {{ git = \"https://github.com/shreypdev/undra\", tag = \"v{}\" }}",
                env!("CARGO_PKG_VERSION")
            )
        );
        let project = Project::open(&root).unwrap();
        assert_eq!(project.config.undra_path, None);
        assert_eq!(project.config.undra_version, UNDRA_VERSION);
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(readme.contains(UNDRA_RELEASE_TAG), "{readme}");
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn a_project_created_inside_a_checkout_uses_it_by_path() {
        let checkout = fsutil::unique_temp_dir("init-inside");
        for file in crate::runtimes::CHECKOUT_FILES {
            let path = checkout.join(file);
            create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let examples = checkout.join("examples");
        create_dir_all(&examples).unwrap();
        run(&env(&examples), &args("inside", "web")).unwrap();
        let root = examples.canonicalize().unwrap().join("inside");
        let core = std::fs::read_to_string(root.join("core/Cargo.toml")).unwrap();
        let dep = core.lines().find(|l| l.starts_with("undra = ")).unwrap();
        assert_eq!(dep, "undra = { path = \"../../../crates/undra\" }");
        let toml = std::fs::read_to_string(root.join("undra.toml")).unwrap();
        assert!(toml.contains("path = \"../..\""), "{toml}");
        assert!(!core.contains("git ="), "{core}");
        let _ = std::fs::remove_dir_all(checkout);
    }
}
