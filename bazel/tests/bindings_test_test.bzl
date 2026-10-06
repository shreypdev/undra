"""Tests of `undra_bindings_test` against stand-in bindings (ADR-064): the rule, its test and its update target."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
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

def _absent_message_impl(ctx):
    env = analysistest.begin(ctx)
    asserts.expect_failure(env, "do not exist yet")
    asserts.expect_failure(env, "bazel run //tests:absent.update")
    return analysistest.end(env)

_absent_message_test = analysistest.make(_absent_message_impl, expect_failure = True)

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

    # `committed` that does not exist yet fails analysis of the test (so `bazel run` of the update target can create it),
    # with a message that names the update target.
    undra_bindings_test(name = "absent", bindings = ":all_languages", committed = "committed/absent", tags = ["manual"])
    _absent_message_test(name = "absent_message_test", target_under_test = ":absent.update_test")

    tests = ["identical", "identical_ts_only", "absent_message_test"]
    for case in ["changed", "extra", "missing_file", "stale_language"]:
        expected = ["bazel run //tests:{}.update".format(case), "differ"]
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
    native.test_suite(name = name, tests = [":" + t for t in tests])
