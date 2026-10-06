"""A stand-in for `undra_bindings` with files written by the test: the rules under test read its output groups, nothing else."""

load("@bazel_skylib//lib:paths.bzl", "paths")
load("@bazel_skylib//lib:shell.bzl", "shell")

_LANGUAGES = ("swift", "kotlin", "ts")

def _fake_bindings_impl(ctx):
    groups = {}
    outputs = []
    for language in _LANGUAGES:
        files = getattr(ctx.attr, language)
        if not files:
            continue
        tree = ctx.actions.declare_directory("{}_{}".format(ctx.label.name, language))
        commands = ["set -eu", "mkdir -p " + shell.quote(tree.path)]
        for path, content in sorted(files.items()):
            target = shell.quote(paths.join(tree.path, path))
            commands.append("mkdir -p {} && printf '%s' {} > {}".format(shell.quote(paths.dirname(paths.join(tree.path, path))), shell.quote(content), target))
        ctx.actions.run_shell(command = "\n".join(commands), outputs = [tree], use_default_shell_env = False, env = {"PATH": "/usr/bin:/bin"})
        groups[language] = depset([tree])
        outputs.append(tree)
    return [DefaultInfo(files = depset(outputs)), OutputGroupInfo(**groups)]

fake_bindings = rule(
    implementation = _fake_bindings_impl,
    attrs = {language: attr.string_dict(doc = "The files of the {} tree: path to content.".format(language)) for language in _LANGUAGES},
    doc = "Declares one tree per language that has files, in the output groups `undra_bindings` has.",
)
