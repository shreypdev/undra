"""`undra_bindings_test`: committed bindings that the build keeps honest (ADR-064)."""

load("@bazel_lib//lib:write_source_files.bzl", "write_source_file")

# The trees `undra_bindings` declares, by the output group and the directory each is committed under: the names `undra bindgen`
# gives them below its `--out`.
_LANGUAGES = ("swift", "kotlin", "ts")

def _committed_tree_impl(ctx):
    groups = ctx.attr.bindings[OutputGroupInfo]
    trees = []
    for language in _LANGUAGES:
        if not hasattr(groups, language):
            continue
        files = getattr(groups, language).to_list()
        if len(files) != 1 or not files[0].is_directory:
            fail("{}: the `{}` output group of {} is not one tree artifact".format(ctx.label, language, ctx.attr.bindings.label))
        trees.append((language, files[0]))
    if not trees:
        fail("{}: {} has no `swift`, `kotlin` or `ts` output group: `bindings` is an `undra_bindings` target".format(ctx.label, ctx.attr.bindings.label))

    out = ctx.actions.declare_directory(ctx.label.name)
    commands = ["set -eu", "mkdir -p \"$1\""]
    arguments = [out.path]
    for index, (language, tree) in enumerate(trees):
        commands.append("mkdir -p \"$1/{language}\" && cp -R \"${n}\"/. \"$1/{language}/\"".format(language = language, n = index + 2))
        arguments.append(tree.path)
    ctx.actions.run_shell(
        command = "\n".join(commands),
        arguments = arguments,
        inputs = [tree for _, tree in trees],
        outputs = [out],
        mnemonic = "UndraCommittedBindings",
        progress_message = "Laying out the bindings of {} as they are committed".format(ctx.attr.bindings.label),
        use_default_shell_env = False,
        env = {"PATH": "/usr/bin:/bin"},
    )
    return [DefaultInfo(files = depset([out]))]

_committed_tree = rule(
    implementation = _committed_tree_impl,
    attrs = {
        "bindings": attr.label(
            doc = "An `undra_bindings` target.",
            mandatory = True,
        ),
    },
    doc = """The trees of an `undra_bindings` target as one directory: `swift/`, `kotlin/` and `ts/`, the layout `undra bindgen --out` writes
(without its `.undra-generated` manifest, which only the CLI's own pruning reads). The languages the bindings were generated for
are the subdirectories.""",
)

def undra_bindings_test(name, bindings, committed, **kwargs):
    """Keeps a committed copy of the bindings equal to what the schema generates (ADR-064).

    For a repository whose policy puts generated code in the tree, where `undra_bindings` alone would leave it out. The committed
    directory holds one subdirectory per language the bindings were generated for, `swift/`, `kotlin/` and `ts/`: what `undra
    bindgen --out <committed>` writes, less the manifest `.undra-generated`. Targets:

    * `<name>`: a test that fails when the committed directory is not what `bindings` produces: a file that differs, one that is
      missing, one that is not generated. Byte for byte, with nothing normalised, as `undra bindgen --check` compares the files it
      generates. The failure names the update command.
    * `<name>.update`: `bazel run //<package>:<name>.update` writes the generated trees into `committed` in the source tree, and
      removes everything else in it.

    The directory belongs to the rule: do not keep other files in it. `undra bindgen --check` ignores a file beside the generated
    ones that its manifest does not list; this test does not, because it compares the directory as a whole, and the update removes
    it. It is built on `write_source_file` of `bazel_lib`, so the diff, the update and the test are the standard ones.

    Args:
        name: the test.
        bindings: the `undra_bindings` target to compare against.
        committed: the directory, relative to this package, the bindings are committed to (`"generated"`): in the same package as
            this target, as `write_source_file` requires.
        **kwargs: `visibility`, `tags`.
    """
    update = name + ".update"
    update_command = "bazel run //{}:{}".format(native.package_name(), update)
    tree = name + "_tree"
    _committed_tree(
        name = tree,
        bindings = bindings,
        tags = ["manual"] + kwargs.get("tags", []),
        visibility = ["//visibility:private"],
    )

    # The messages are put in a shell double-quoted string by `diff_test`: no backquote, dollar sign or quotation mark in them.
    differs = """

The committed bindings in {committed} are not what the core's schema generates (undra bindgen --check compares the same files).
They are generated: do not edit them. To regenerate them from the core, then commit the result, run:

    {command}

"""
    missing = """

The committed bindings {committed} do not exist yet. To generate and write them, then commit them, run:

    {command}

"""
    write_source_file(
        name = update,
        in_file = ":" + tree,
        out_file = committed,
        diff_test_failure_message = differs.format(committed = committed, command = update_command),
        file_missing_failure_message = missing.format(committed = committed, command = update_command),
        verbosity = "short",
        **kwargs
    )

    # `write_source_file` names its test `<update>_test`; the target a user runs is `<name>`.
    native.test_suite(
        name = name,
        tests = [":" + update + "_test"],
        **kwargs
    )
