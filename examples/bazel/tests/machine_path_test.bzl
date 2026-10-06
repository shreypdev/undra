"""The parameter file of each platform's build action: `extra_path` reaches the iOS build alone (ADR-065)."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")

def _params(env):
    """The lines of the parameter file `run.sh` reads, written by the target under test."""
    for action in analysistest.target_actions(env):
        for output in action.outputs.to_list():
            if output.basename.endswith(".params"):
                return action.content.split("\n")
    fail("the target under test wrote no parameter file")

def _no_machine_path_impl(ctx):
    env = analysistest.begin(ctx)
    lines = _params(env)
    machine = [line for line in lines if line.startswith("path=") or line == "inherit_path=1"]
    asserts.equals(env, [], machine, "a directory of the machine reached a build that must see none: " + "\n".join(lines))
    return analysistest.end(env)

def _ios_machine_path_impl(ctx):
    env = analysistest.begin(ctx)
    lines = _params(env)
    asserts.true(env, "path=/machine/bin" in lines, "the iOS build was not given extra_path: " + "\n".join(lines))
    asserts.true(env, "inherit_path=1" in lines, "the iOS build does not inherit the PATH it was started with")
    return analysistest.end(env)

_no_machine_path_test = analysistest.make(_no_machine_path_impl)
_ios_machine_path_test = analysistest.make(_ios_machine_path_impl)

def machine_path_test_suite(name):
    """One test per platform of `:machine_path*`, as a suite called `name`."""
    tests = []
    for platform in ["host", "web"]:
        test = "{}_{}_test".format(name, platform)
        _no_machine_path_test(name = test, target_under_test = ":machine_path_" + platform)
        tests.append(test)
    _no_machine_path_test(name = name + "_android_test", target_under_test = ":machine_path_android")
    tests.append(name + "_android_test")
    # The iOS target is macOS's (its rule target is incompatible elsewhere, and so is this test, which `//...` then skips).
    _ios_machine_path_test(name = name + "_ios_test", target_under_test = ":machine_path_ios")
    tests.append(name + "_ios_test")
    native.test_suite(name = name, tests = [":" + t for t in tests])
