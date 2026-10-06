"""`undra_bindings`: the Swift, Kotlin and TypeScript bindings of a core, as outputs of the build (ADR-061)."""

load("//undra/private:actions.bzl", "short_dirname", "stage_manifest", "write_params")

# Language -> (the platform name `undra bindgen --platforms` takes, the directory it writes).
_LANGUAGES = {
    "swift": "ios",
    "kotlin": "android",
    "ts": "web",
}

def _undra_bindings_impl(ctx):
    library = ctx.file.core
    if library.extension not in ("dylib", "so", "dll"):
        fail("{}: `core` is {}, which is not a host library: the schema is read from the build for this machine, `core = \":<name>_host\"` of an undra_core whose platforms include \"host\"".format(
            ctx.label,
            library.short_path,
        ))
    languages = ctx.attr.languages
    for language in languages:
        if language not in _LANGUAGES:
            fail("{}: unknown language `{}`; use any of {}".format(ctx.label, language, sorted(_LANGUAGES.keys())))

    config = ctx.file.config
    app_manifest, app_exec_root = stage_manifest(
        ctx,
        ctx.label.name + ".app_files",
        short_dirname(config),
        [config],
        "the directory of undra.toml",
    )
    lines = [
        "mode=bindgen",
        "cli=" + ctx.executable.cli.path,
        "app_root=" + app_exec_root,
        "app_files=" + app_manifest.path,
        "stage_key={}".format(ctx.label),
        "project=.",
        "library=" + library.path,
        "bindgen_platforms=" + ",".join([_LANGUAGES[l] for l in languages]),
        "bindgen_docs=" + ("1" if ctx.attr.docs else "0"),
    ]
    outputs = []
    groups = {}
    for language in languages:
        tree = ctx.actions.declare_directory("{}_{}".format(ctx.label.name, language))
        lines.append("out_{}={}".format(language, tree.path))
        outputs.append(tree)
        groups[language] = depset([tree])
    tools = [ctx.executable.cli]
    if ctx.attr.swift_files:
        if "swift" not in languages:
            fail("{}: swift_files lists Swift files, but `swift` is not among the languages".format(ctx.label))
        swift_groups = {
            "swift_sources": [],
            "swift_ffi_sources": [],
            "swift_ffi_headers": [],
            "swift_ffi_modulemap": [],
        }
        for path in ctx.attr.swift_files:
            declared = ctx.actions.declare_file("{}_swift_files/{}".format(ctx.label.name, path))
            lines.append("swift_file={}|{}".format(path, declared.path))
            outputs.append(declared)
            if path.endswith(".swift"):
                swift_groups["swift_sources"].append(declared)
            elif path.endswith(".c"):
                swift_groups["swift_ffi_sources"].append(declared)
            elif path.endswith(".h"):
                swift_groups["swift_ffi_headers"].append(declared)
            elif path.endswith("module.modulemap"):
                swift_groups["swift_ffi_modulemap"].append(declared)
            else:
                fail("{}: swift_files lists {}, which is none of .swift, .c, .h and module.modulemap".format(ctx.label, path))
        for group, files in swift_groups.items():
            groups[group] = depset(files)
    if "kotlin" in languages:
        srcjar = ctx.actions.declare_file(ctx.label.name + "_kotlin.srcjar")
        lines += [
            "out_kotlin_srcjar=" + srcjar.path,
            "zipper=" + ctx.executable._zipper.path,
        ]
        outputs.append(srcjar)
        tools.append(ctx.executable._zipper)
        groups["kotlin_srcjar"] = depset([srcjar])

    params = write_params(ctx, ctx.label.name + ".params", lines)
    ctx.actions.run(
        executable = ctx.file._runner,
        arguments = [params.path],
        inputs = [params, app_manifest, library, config],
        tools = tools,
        outputs = outputs,
        mnemonic = "UndraBindgen",
        progress_message = "Generating the {} bindings of {}".format(", ".join(languages), ctx.label),
        env = {"LC_ALL": "C"},
    )
    return [
        DefaultInfo(files = depset(outputs)),
        OutputGroupInfo(**groups),
    ]

_undra_bindings = rule(
    implementation = _undra_bindings_impl,
    attrs = {
        "core": attr.label(
            doc = "The host build of the core (an `undra_core` target for the `host` platform): the library the schema is read from.",
            allow_single_file = True,
            mandatory = True,
            # The library is loaded by the bindgen action, which runs on the execution platform: built for it whatever the target
            # platform is (an iOS application depends on the bindings, and its host library is still a Mac's).
            cfg = "exec",
        ),
        "config": attr.label(
            doc = "The project's undra.toml: its `[bindings]` names the generated modules, packages and scope.",
            allow_single_file = True,
            mandatory = True,
        ),
        "languages": attr.string_list(
            doc = "Any of `swift`, `kotlin`, `ts`.",
            default = ["swift", "kotlin", "ts"],
        ),
        "docs": attr.bool(
            doc = "Keep the core's doc comments in the bindings (`undra bindgen --docs`).",
            default = False,
        ),
        "swift_files": attr.string_list(
            doc = "The files of the Swift tree to declare as outputs, by their path in it (`Sources/HelloCore/Generated/Core.swift`, " +
                  "the C source, header and module.modulemap of `Sources/<Ns>CoreFFI`): `rules_swift` compiles files, not a directory. " +
                  "The set depends on the schema; the build fails, naming the actual list, when this one differs.",
        ),
        "cli": attr.label(default = Label("@undra//:cli"), executable = True, cfg = "exec"),
        "_runner": attr.label(
            default = Label("//undra/private:run.sh"),
            allow_single_file = True,
            cfg = "exec",
        ),
        "_zipper": attr.label(
            default = Label("@bazel_tools//tools/zip:zipper"),
            executable = True,
            cfg = "exec",
        ),
    },
    doc = """Generates the bindings of an Undra core with `undra bindgen --library`.

One tree artifact per language, `<name>_swift`, `<name>_kotlin` and `<name>_ts`, each the directory `undra bindgen` writes for
that language (the Swift package, the Gradle module, the npm package), and, with Kotlin, `<name>_kotlin.srcjar` of its sources
(the output group `kotlin_srcjar`). Nothing is committed and nothing can be stale: the bindings are a function of the core library.""",
)

def undra_bindings(name, core, config = "undra.toml", languages = ["swift", "kotlin", "ts"], docs = False, swift_files = [], **kwargs):
    """Generates the bindings of an Undra core (ADR-061).

    Outputs: one tree artifact per language, `<name>_swift`, `<name>_kotlin` and `<name>_ts`, each the directory `undra bindgen`
    writes for that language (the Swift package, the Gradle module, the npm package, lint exclusions beside them), and, with
    Kotlin, `<name>_kotlin.srcjar` of its sources. The output groups are `swift`, `kotlin`, `ts` and `kotlin_srcjar`.

    Args:
        name: the target.
        core: the host build of the core: `":core_host"` of an `undra_core` that lists `host` among its platforms.
        config: the project's undra.toml.
        languages: any of `swift`, `kotlin`, `ts`.
        docs: keep the core's doc comments in the bindings.
        swift_files: the files of the Swift tree `undra_swift_library` compiles (see the rule); empty when no Swift library is built.
        **kwargs: `visibility`, `tags`.
    """
    _undra_bindings(
        name = name,
        core = core,
        config = config,
        languages = languages,
        docs = docs,
        swift_files = swift_files,
        **kwargs
    )
