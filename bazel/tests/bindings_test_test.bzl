"""Tests of `undra_bindings_test` against stand-in bindings (ADR-064): the rule, its test and its update target."""

load("@rules_shell//shell:sh_test.bzl", "sh_test")
load("//undra:bindings_test.bzl", "undra_bindings_test")
load(":fake_bindings.bzl", "fake_bindings")

_ALL = {
    "swift": {
        ".gitattributes": "* linguist-generated=true\n",
        "Package.swift": "// swift-tools-version: 6.0\n",
        "Sources/Demo/Generated/Core.swift": "struct Core {}\n",
    },
    "kotlin": {
        "build.gradle.kts": "plugins {}\n",
        "src/main/kotlin/demo/Core.kt": "package demo\n\nclass Core\n",
    },
    "ts": {
        "package.json": "{}\n",
        "src/core.ts": "export {};\n",
    },
}

def bindings_test_suite(name):
    """The tests of `undra_bindings_test`, as a test suite called `name`."""
    fake_bindings(name = "all_languages", swift = _ALL["swift"], kotlin = _ALL["kotlin"], ts = _ALL["ts"])
    fake_bindings(name = "ts_only", ts = _ALL["ts"])

    # Equal to the stand-in: passes, in this suite.
    undra_bindings_test(name = "identical", bindings = ":all_languages", committed = "committed/identical")
    undra_bindings_test(name = "identical_ts_only", bindings = ":ts_only", committed = "committed/ts_only")

    # Each of these is a way to be out of date: the tests run them and expect them to fail, so they are not part of the suite.
    for case in ["changed", "extra", "missing_file"]:
        undra_bindings_test(name = case, bindings = ":all_languages", committed = "committed/" + case, tags = ["manual"])
    undra_bindings_test(name = "stale_language", bindings = ":ts_only", committed = "committed/identical", tags = ["manual"])

    # The update target writes into a scratch workspace: what is below `committed` is the stand-in's trees and nothing else.
    undra_bindings_test(name = "to_update", bindings = ":all_languages", committed = "committed/to_update", tags = ["manual"])

    # `committed` that does not exist yet: the test fails when it runs, with a message that names the update target. Its
    # analysis succeeds (the test below depends on it), so it does not stop a `bazel test //...` of everything else.
    undra_bindings_test(name = "absent", bindings = ":all_languages", committed = "committed/absent", tags = ["manual"])

    tests = ["identical", "identical_ts_only"]
    cases = {case: "differ" for case in ["changed", "extra", "missing_file", "stale_language"]}
    cases["absent"] = "do not exist yet"
    for case, says in cases.items():
        expected = ["bazel run //tests:{}.update".format(case), says]
        sh_test(
            name = case + "_fails_test",
            srcs = ["expect_failure.sh"],
            args = ["$(rootpath :{}.update_test)".format(case)] + expected,
            data = [":{}.update_test".format(case)],
        )
        tests.append(case + "_fails_test")
    sh_test(
        name = "update_writes_test",
        srcs = ["expect_update.sh"],
        args = [
            "$(rootpath :to_update.update)",
            "tests/committed/to_update",
            "$(rootpath :committed/identical)",
        ],
        data = [":to_update.update", ":committed/identical"],
    )
    tests.append("update_writes_test")

    # The laid-out directory holds copies: in a sandbox the input trees' files are links into the execroot, which must not
    # end up in an output (a disk or remote cache would hand them to another machine).
    sh_test(
        name = "copies_test",
        srcs = ["expect_copies.sh"],
        args = ["$(rootpath :identical_tree)", "identical_tree"],
        data = [":identical_tree"],
    )
    tests.append("copies_test")
    native.test_suite(name = name, tests = [":" + t for t in tests])
