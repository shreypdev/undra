"""`undra_core`: the core of an Undra app, built for each platform it ships to (ADR-061)."""

load("//undra/private:android_std.bzl", "VERSION_FILE")
load("//undra/private:actions.bzl", "RUNNER_ATTRS", "below", "short_dirname", "stage_manifest", "vendor_params", "write_params")

# The Bazel platforms (undra/platforms) each target platform of `undra build` selects the Rust toolchain for. A platform
# with several is built for all of them in one action: iOS is a device slice and a simulator slice. Android has none: its core
# is a Cargo cross-build linked by the NDK, which is an input of the action (`ndk`), so it is built in the host configuration
# (a target platform of Android would need a C++ toolchain for it, which only `rules_android_ndk` provides) and its standard
# library is `android_std`, not a Rust toolchain resolved for a platform (ADR-065).
_PLATFORMS = {
    "host": [],
    "web": ["wasm32"],
    "ios": ["ios_arm64", "ios_sim_arm64"],
    "android": [],
}

# The marker of an NDK's root directory among the files of the `ndk` label.
_NDK_MARKER = "source.properties"

def _sysroot_of(file):
    """The sysroot a Rust standard library file lies in: the directory above `lib/rustlib/`."""
    marker = "/lib/rustlib/"
    index = file.path.find(marker)
    return file.path[:index] if index >= 0 else None

def _target_platforms_impl(settings, attr):
    names = _PLATFORMS[attr.platform]
    if not names:
        return {"host": {"//command_line_option:platforms": settings["//command_line_option:platforms"]}}
    return {
        name: {"//command_line_option:platforms": [str(Label("//undra/platforms:" + name))]}
        for name in names
    }

_target_platforms = transition(
    implementation = _target_platforms_impl,
    inputs = ["//command_line_option:platforms"],
    outputs = ["//command_line_option:platforms"],
)

def _project(config_dir, app_root, label):
    """The directory of undra.toml below the app's root (`.` when it is the root), both short paths."""
    if config_dir == app_root:
        return "."
    project = below(config_dir, app_root)
    if project == None:
        fail("{}: undra.toml is in {}, which is not below the Cargo workspace root {} (`workspace`)".format(
            label,
            config_dir or "the repository root",
            app_root or "the repository root",
        ))
    return project

def _undra_core_impl(ctx):
    platform = ctx.attr.platform
    namespace = ctx.attr.namespace
    release = ctx.attr.release or platform == "web"  # `undra build` always builds the web core optimised

    host = ctx.attr._host_rust[platform_common.ToolchainInfo]
    lines = [
        "mode=build",
        "platform=" + platform,
        "release=" + ("1" if release else "0"),
        "symbols=" + ("1" if ctx.attr.symbols else "0"),
        "cli=" + ctx.executable.cli.path,
        "rustc=" + host.rustc.path,
        "cargo=" + host.cargo.path,
        "sysroot=" + host.sysroot,
    ]
    tool_files = [host.all_files]
    sysroots = {host.sysroot: True}
    for target in ctx.split_attr._target_rust.values():
        info = target[platform_common.ToolchainInfo]
        if info.sysroot not in sysroots:
            sysroots[info.sysroot] = True
            lines.append("sysroot=" + info.sysroot)
        tool_files.append(info.all_files)

    config = ctx.file.config
    config_dir = short_dirname(config)
    app_root = short_dirname(ctx.file.workspace) if ctx.file.workspace else config_dir
    project = _project(config_dir, app_root, ctx.label)
    app_files = [config] + ctx.files.srcs + ([ctx.file.workspace] if ctx.file.workspace else [])
    app_manifest, app_exec_root = stage_manifest(
        ctx,
        ctx.label.name + ".app_files",
        app_root,
        app_files,
        "the root of the Cargo workspace (the directory of `workspace`, else of undra.toml)",
    )
    lines += [
        "app_root=" + app_exec_root,
        "app_files=" + app_manifest.path,
        "project=" + project,
        "namespace=" + namespace,
        # The private copy is built in a directory named by the target, never by when or where it runs (run.sh says why).
        "stage_key={}|{}".format(ctx.label, platform),
    ]

    outputs = []
    out_symbols = None
    if platform == "host":
        macos = ctx.target_platform_has_constraint(ctx.attr._macos[platform_common.ConstraintValueInfo])
        out = ctx.actions.declare_file("{}/lib{}.{}".format(ctx.label.name, namespace, "dylib" if macos else "so"))
        lines.append("out_file=" + out.path)
    elif platform == "web":
        out = ctx.actions.declare_file("{}/{}.wasm".format(ctx.label.name, namespace))
        lines.append("out_file=" + out.path)
    else:
        out = ctx.actions.declare_directory(ctx.label.name)
        lines.append("out_dir=" + out.path)
    outputs.append(out)
    if release and ctx.attr.symbols:
        out_symbols = ctx.actions.declare_directory(ctx.label.name + ".symbols")
        lines.append("out_symbols=" + out_symbols.path)
        outputs.append(out_symbols)

    inputs = app_files + [app_manifest]
    for directory in ctx.attr.extra_path:
        lines.append("path=" + directory)
    if platform == "ios":
        # Xcode is the machine's (the documented exception, ADR-065): the PATH the build was started with comes along.
        lines.append("inherit_path=1")
    extra_tools = []
    android_inputs = []
    if platform == "android":
        # The NDK and the standard library of the Android targets are declared inputs: nothing of the machine's is read.
        if not ctx.attr.ndk or not ctx.attr.android_std:
            fail("{}: the android platform needs `ndk` and `android_std` (`undra_core` passes them)".format(ctx.label))
        markers = [f for f in ctx.files.ndk if f.basename == _NDK_MARKER]
        shallowest = min([len(f.path) for f in markers]) if markers else 0
        markers = [f for f in markers if len(f.path) == shallowest]
        if len(markers) != 1:
            fail("{}: `ndk` must name the files of one Android NDK, with `{}` at its root (found {} such files)".format(ctx.label, _NDK_MARKER, len(markers)))
        lines.append("ndk=" + markers[0].dirname)
        android_inputs.append(ctx.attr.ndk[DefaultInfo].files)
        sysroots_of_std = {}
        for f in ctx.files.android_std:
            root = _sysroot_of(f)
            if root:
                sysroots_of_std[root] = True
        if not sysroots_of_std:
            fail("{}: the Rust standard library of the Android targets is not declared: add `undra.android_std(version = .., sha256s = ..)` to MODULE.bazel and `use_repo(undra, \"undra_android_std\")`".format(ctx.label))
        for root in sorted(sysroots_of_std):
            lines.append("sysroot=" + root)
        versions = [f for f in ctx.files.android_std if f.basename == VERSION_FILE]
        if len(versions) != 1:
            fail("{}: `android_std` has no `{}` (it is `@undra_android_std//:std`)".format(ctx.label, VERSION_FILE))
        lines.append("std_version=" + versions[0].path)
        android_inputs.append(ctx.attr.android_std[DefaultInfo].files)
    if ctx.attr.wasm_opt:
        tool = [f for f in ctx.files.wasm_opt if f.basename == "wasm-opt"]
        if len(tool) != 1:
            fail("`wasm_opt` of {} must name the files of one binaryen (exactly one of them called wasm-opt)".format(ctx.label))
        lines.append("path=" + tool[0].dirname)
        extra_tools.extend(ctx.files.wasm_opt)

    vendor_lines, vendor_files = vendor_params(ctx)
    lines += vendor_lines
    undra_files = [ctx.file._undra_manifest] + ctx.files._undra_sources
    undra_manifest, undra_exec_root = stage_manifest(
        ctx,
        ctx.label.name + ".undra_files",
        short_dirname(ctx.file._undra_manifest),
        undra_files,
        "the Undra checkout",
    )
    lines += [
        "undra_root=" + undra_exec_root,
        "undra_files=" + undra_manifest.path,
    ]
    params = write_params(ctx, ctx.label.name + ".params", lines)

    ctx.actions.run(
        executable = ctx.file._runner,
        arguments = [params.path],
        inputs = depset(
            [params, undra_manifest] + inputs + extra_tools,
            transitive = [depset(undra_files), vendor_files] + tool_files + android_inputs,
        ),
        tools = [ctx.executable.cli],
        outputs = outputs,
        mnemonic = "UndraBuild",
        progress_message = "Building the {} core of {}".format(platform, ctx.label),
        env = {"LC_ALL": "C"},
        use_default_shell_env = platform == "ios",
        execution_requirements = ctx.attr.execution_requirements,
    )
    groups = {"core": depset([out])}
    if out_symbols:
        groups["symbols"] = depset([out_symbols])
    return [
        DefaultInfo(files = depset([out])),
        OutputGroupInfo(**groups),
    ]

_undra_core = rule(
    implementation = _undra_core_impl,
    attrs = dict(RUNNER_ATTRS, **{
        "platform": attr.string(values = ["host", "web", "ios", "android"], mandatory = True),
        "namespace": attr.string(mandatory = True),
        "config": attr.label(allow_single_file = True, mandatory = True),
        "workspace": attr.label(allow_single_file = True),
        "srcs": attr.label_list(allow_files = True),
        "release": attr.bool(default = False),
        "symbols": attr.bool(default = True),
        "wasm_opt": attr.label(allow_files = True, cfg = "exec"),
        "execution_requirements": attr.string_dict(),
        "extra_path": attr.string_list(),
        "ndk": attr.label(allow_files = True),
        "android_std": attr.label(allow_files = True),
        "cli": attr.label(default = Label("@undra//:cli"), executable = True, cfg = "exec"),
        "_undra_sources": attr.label(default = Label("@undra//:sources")),
        "_undra_manifest": attr.label(default = Label("@undra//:Cargo.toml"), allow_single_file = True),
        "_macos": attr.label(default = Label("@platforms//os:macos")),
        "_target_rust": attr.label(
            default = Label("@rules_rust//rust/toolchain:current_rust_toolchain"),
            cfg = _target_platforms,
            providers = [platform_common.ToolchainInfo],
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    }),
    doc = "One platform's build of an Undra core: `undra build --platform <platform>` in a hermetic, offline action.",
)

def undra_core(
        name,
        namespace,
        config = "undra.toml",
        srcs = [],
        workspace = None,
        platforms = ["host", "web"],
        release = False,
        symbols = True,
        wasm_opt = None,
        ndk = None,
        extra_path = [],
        tags = [],
        visibility = None,
        **kwargs):
    """Builds the core of an Undra app for each of `platforms`, with the same cargo profiles `undra build` uses.

    One target per platform, named `<name>_<platform>` (`core_host`, `core_web`, `core_ios`, `core_android`), and `<name>`, a
    filegroup of all of them. What each makes, the same files `undra build` writes below `build/`:

    * `host`: `<name>_host/lib<namespace>.dylib` (`.so` on Linux), for the JVM and for `undra_bindings`;
    * `web`: `<name>_web/<namespace>.wasm`, the release-wasm profile then `wasm-opt -Oz` when `wasm_opt` is given;
    * `ios`: the directory `<name>_ios` holding `<Namespace>Core.xcframework` (macOS with Xcode only);
    * `android`: the directory `<name>_android` holding `jniLibs/<abi>/lib<namespace>.so`, a Cargo cross-build per ABI linked
      with the NDK of `ndk` (ADR-065; no `cargo-ndk`). It is built in the host configuration, with the host's Rust toolchain: no
      Android platform, no C++ toolchain for one. The standard library of the Android targets is the application's
      `undra.android_std(..)` (a checksum-pinned `rust-std`), and the API level of the linker is `[android] min_sdk` of undra.toml.

    Each release target (`release = True`, and `web`, which always is) also has an output group `symbols`, a directory
    `<target>.symbols` laid out as `undra build --release` lays out `build/`, restricted to the symbol files: `symbols/` is
    `build/symbols/` (`manifest.json`, `android/..`, `web/..`) and, for `host`, `host/` holds the library's symbols
    (`lib<namespace>.dylib.dSYM`, `.so.debug` on Linux), which the CLI keeps next to the library and the manifest names as
    `../host/..`. A crash report's addresses resolve to file and line with them: `undra symbolicate --symbols
    <target>.symbols/symbols report.json`. iOS writes no symbol file of its own (its group holds the manifest): the Rust
    frames are in the app's own dSYM (ADR-046). A debug target has no `symbols` group, and a `filegroup` of it is empty.

    ```starlark
    undra_core(name = "core_release", namespace = "hello_core", srcs = [...], platforms = ["host"], release = True)
    filegroup(name = "core_release_symbols", srcs = [":core_release_host"], output_group = "symbols")  # the directory
    ```

    Args:
        name: the filegroup of every platform's build.
        namespace: the core's `[core] namespace` in undra.toml; the build fails, naming both, if undra.toml says another.
        config: the project's undra.toml.
        srcs: every file of the project the build reads: its Cargo manifests and lock file and the core's sources.
        workspace: the Cargo workspace's root `Cargo.toml` when it is above undra.toml's directory (default: the same directory).
        platforms: any of `host`, `web`, `ios`, `android`.
        release: build `host`, `ios` and `android` with the release profile (LTO, one codegen unit). `web` always is.
        symbols: also write the symbol files of a release build, as `undra build` does by default (the shipped web module differs
            by about 0.1% when it does not: `wasm-opt` sees the names, ADR-046), so the default is the CLI's own bytes.
        wasm_opt: an executable `wasm-opt` (binaryen); without it the web module is not shrunk further, as the CLI says.
        ndk: the Android NDK as a label (required by `android`): a target whose files are the NDK's root directory tree, with
            its `source.properties` at the root, such as the repository an `http_archive` of the NDK zip makes, pinned by sha256,
            with a `build_file` that exports `source.properties` and `toolchains/llvm/prebuilt/**` (`examples/bazel` has it), or
            the one `rules_android_ndk` makes. The action sets `ANDROID_NDK_HOME` to that directory itself: no `--action_env`
            and no `extra_path` for Android. The archive is per host OS, so a `select()` on `@platforms//os:*` is the usual
            value.
        extra_path: directories (absolute) put on the action's `PATH` for the `ios` build, whose toolchain is the machine's
            (the documented exception: `DEVELOPER_DIR` comes in through `--action_env` and the `PATH` the build was started with
            is inherited). Android does not use it.
        tags: tags of the generated targets.
        visibility: the visibility of every generated target.
        **kwargs: passed to the generated rule instances (`execution_requirements`).
    """
    if "android" in platforms and ndk == None:
        fail("undra_core({}): `platforms` has \"android\", which needs `ndk`: the label of the Android NDK's files (an http_archive of the NDK, see examples/bazel)".format(name))
    targets = []
    for platform in platforms:
        target = "{}_{}".format(name, platform)
        _undra_core(
            name = target,
            platform = platform,
            namespace = namespace,
            config = config,
            srcs = srcs,
            workspace = workspace,
            release = release,
            symbols = symbols,
            wasm_opt = wasm_opt if platform == "web" else None,
            ndk = ndk if platform == "android" else None,
            android_std = Label("@undra_android_std//:std") if platform == "android" else None,
            extra_path = extra_path,
            # Xcode is the machine's, not Bazel's (ADR-061), and the NDK is a large download: both build when asked for by name.
            tags = tags + (["manual", "requires-darwin"] if platform == "ios" else []) + (["manual"] if platform == "android" else []),
            target_compatible_with = ["@platforms//os:macos"] if platform == "ios" else [],
            visibility = visibility,
            **kwargs
        )
        targets.append(":" + target)
    native.filegroup(
        name = name,
        srcs = targets,
        tags = tags + (["manual"] if "ios" in platforms or "android" in platforms else []),
        visibility = visibility,
    )
