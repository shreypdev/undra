# The BUILD file of the Android NDK repository (the `build_file` of the `http_archive`s in MODULE.bazel, ADR-065).
#
# `undra_core(ndk = ..)` takes the NDK as a tree of declared files: what `cargo` needs to cross-build a core for Android is the
# NDK's LLVM toolchain of the host (clang and its per-API wrappers, llvm-ar, llvm-strip, the sysroot with the platform
# libraries), and `source.properties`, which marks the root the action points ANDROID_NDK_HOME at.
package(default_visibility = ["//visibility:public"])

filegroup(
    name = "ndk",
    srcs = ["source.properties"] + glob(["toolchains/llvm/prebuilt/**"]),
)
