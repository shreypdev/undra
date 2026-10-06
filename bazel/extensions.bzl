"""The `undra` module extension: where the Undra sources and the crates they need come from."""

load("//undra/private:android_std.bzl", "undra_android_std")
load("//undra/private:binaryen.bzl", "undra_binaryen")
load("//undra/private:cargo_vendor.bzl", "undra_vendor")
load("//undra/private:source.bzl", "undra_source")

_source = tag_class(
    doc = "Where Undra is: a checkout (`path`) or a source archive (`urls`, `integrity`, `strip_prefix`).",
    attrs = {
        "path": attr.string(doc = "A checkout of the Undra repository, relative to the root module or absolute."),
        "urls": attr.string_list(doc = "Where a source archive of the Undra repository is downloaded from (one or more mirrors), when it is not a checkout."),
        "integrity": attr.string(doc = "The archive's checksum, as Bazel's `http_archive` takes it (`sha256-<base64>`): a download that differs fails."),
        "strip_prefix": attr.string(doc = "The directory of the archive the repository is in (`undra-1.1.0` for a tagged source archive)."),
    },
)

_vendor = tag_class(
    doc = "The Cargo.lock files whose crates.io packages are downloaded (by checksum) for the offline builds.",
    attrs = {
        "lockfiles": attr.label_list(allow_files = True, doc = "The application's Cargo.lock, when it has one."),
    },
)

_android_std = tag_class(
    doc = "The Rust standard library of the Android targets, by checksum: what `undra_core(platforms = [\"android\"])` links against " +
          "(ADR-065). `version` must be the Rust release of the application's `rust.toolchain`.",
    attrs = {
        "version": attr.string(mandatory = True, doc = "The Rust release, such as `1.99.0`."),
        "sha256s": attr.string_dict(
            mandatory = True,
            doc = "The SHA-256 of `rust-std-<version>-<triple>.tar.xz` for each of `aarch64-linux-android`, `x86_64-linux-android`.",
        ),
    },
)

def _impl(mctx):
    source = None
    lockfiles = []
    android_std = None
    for module in mctx.modules:
        for tag in module.tags.source:
            if source == None or module.is_root:
                source = tag
        for tag in module.tags.vendor:
            lockfiles.extend(tag.lockfiles)
        for tag in module.tags.android_std:
            if android_std == None or module.is_root:
                android_std = tag

    runtimes = {}
    if source != None:
        runtimes = _runtime_builds()
    undra_source(
        name = "undra",
        path = source.path if source else "",
        urls = source.urls if source else [],
        integrity = source.integrity if source else "",
        strip_prefix = source.strip_prefix if source else "",
        runtimes = runtimes,
    )
    undra_vendor(
        name = "undra_vendor",
        lockfiles = lockfiles + ([Label("@undra//:Cargo.lock")] if source != None else []),
    )
    undra_binaryen(name = "undra_binaryen")
    undra_android_std(
        name = "undra_android_std",
        version = android_std.version if android_std else "",
        sha256s = android_std.sha256s if android_std else {},
    )
    return mctx.extension_metadata(reproducible = True)

def _runtime_builds():
    # The BUILD file of each language runtime's package in @undra (templates, so a package that is not used is not loaded).
    return {
        "kotlin": Label("//undra/private/runtimes:kotlin.BUILD"),
        "ts": Label("//undra/private/runtimes:ts.BUILD"),
        "swift": Label("//undra/private/runtimes:swift.BUILD"),
    }

undra = module_extension(
    implementation = _impl,
    tag_classes = {
        "source": _source,
        "vendor": _vendor,
        "android_std": _android_std,
    },
    doc = "Configures where the Undra crates and runtimes come from.",
)
