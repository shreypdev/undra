"""The public API of the Undra Bazel rules (ADR-061).

`undra_ts_library` is in `ts.bzl` and `undra_android_library` in `android.bzl`, loaded on their own: the TypeScript rules
(aspect_rules_js) report usage to Aspect unless told not to, and the Android rules need the Android SDK, so neither is loaded by
a build that does not use it.

    load("@undra_rules//undra:defs.bzl", "undra_bindings", "undra_bindings_test", "undra_core", "undra_kt_jvm_library", "undra_swift_library")
    load("@undra_rules//undra:ts.bzl", "undra_ts_library")
"""

load("//undra:bindings.bzl", _undra_bindings = "undra_bindings")
load("//undra:bindings_test.bzl", _undra_bindings_test = "undra_bindings_test")
load("//undra:cli.bzl", _undra_cli = "undra_cli")
load("//undra:core.bzl", _undra_core = "undra_core")
load("//undra:kotlin.bzl", _undra_kt_jvm_library = "undra_kt_jvm_library")
load("//undra:swift.bzl", _undra_swift_library = "undra_swift_library")

undra_cli = _undra_cli
undra_core = _undra_core
undra_bindings = _undra_bindings
undra_bindings_test = _undra_bindings_test
undra_kt_jvm_library = _undra_kt_jvm_library
undra_swift_library = _undra_swift_library
