"""`undra_android_library`: the generated Kotlin bindings as an Android library (ADR-061).

It needs the Android SDK (`ANDROID_HOME`) and `rules_android`, which a machine without them cannot configure, so it is its own file,
not part of `defs.bzl`: nothing that loads `defs.bzl` loads the Android rules. CI builds it on Linux, next to the Android core
(`undra_core(platforms = ["android"], ndk = ..)`, whose NDK is a declared input and needs no SDK).

    load("@undra_rules//undra:android.bzl", "undra_android_library")
"""

load("@rules_kotlin//kotlin:android.bzl", "kt_android_library")

_RUNTIME = Label("@undra//kotlin:runtime")

def undra_android_library(name, bindings, custom_package, manifest = None, deps = [], runtime = _RUNTIME, **kwargs):
    """Compiles the Kotlin bindings of an `undra_bindings` target as an Android library.

    The bindings use no Android API (they are a JVM library: `undra_kt_jvm_library`), so this is the same sources in a library an
    `android_binary` can depend on. The core's native libraries are not part of it: `undra_core`'s `android` output is a directory
    `jniLibs/<abi>/lib<namespace>.so`, which an app packages (Gradle's `jniLibs`; under Bazel, put the `.so` files in the
    `android_binary`'s `cc_library` deps or `native_libs`). The Android runtime adapters (`android-adapters`) are a separate library of
    the Undra checkout that an app adds.

    Args:
        name: the library.
        bindings: an `undra_bindings` target that lists `kotlin` among its languages.
        custom_package: the Android package of the library (the `kotlin_package` of undra.toml's `[bindings]`).
        manifest: an AndroidManifest.xml, when the library has one.
        deps: more dependencies.
        runtime: the Kotlin runtime library.
        **kwargs: passed to `kt_android_library`.
    """
    srcjar = name + "_srcjar"
    native.filegroup(
        name = srcjar,
        srcs = [bindings],
        output_group = "kotlin_srcjar",
        tags = kwargs.get("tags", []),
    )
    kt_android_library(
        name = name,
        srcs = [":" + srcjar],
        custom_package = custom_package,
        manifest = manifest,
        deps = [runtime] + deps,
        **kwargs
    )
