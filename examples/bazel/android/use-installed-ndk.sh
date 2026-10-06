#!/bin/sh
# Prints the Bazel flags that make this workspace use an NDK that is already installed (`sdkmanager "ndk;27.2.12479018"`, Android
# Studio) instead of downloading the archive MODULE.bazel pins:
#
#   bazel build //:mobile_android $(android/use-installed-ndk.sh)
#
# It builds a repository of symbolic links to the installed NDK, with the same BUILD file as the archive's (android/ndk.BUILD),
# and overrides the two NDK repositories with it. The build still declares the NDK as an input; only where it comes from changes.
#
#   use-installed-ndk.sh [<ndk directory> [<repository directory>]]
#
# The NDK is $1, else $ANDROID_NDK_HOME, else the newest $ANDROID_HOME/ndk/<version>.
set -eu

here="$(cd "$(dirname "$0")" && pwd -P)"
ndk="${1:-${ANDROID_NDK_HOME:-}}"
if [ -z "$ndk" ] && [ -d "${ANDROID_HOME:-/nonexistent}/ndk" ]; then
  ndk="$(find "$ANDROID_HOME/ndk" -mindepth 1 -maxdepth 1 -type d | sort | tail -n 1)"
fi
[ -n "$ndk" ] && [ -f "$ndk/source.properties" ] || {
  echo "use-installed-ndk.sh: no NDK found (pass its directory, or set ANDROID_NDK_HOME or ANDROID_HOME)" >&2
  exit 1
}
ndk="$(cd "$ndk" && pwd -P)"
repo="${2:-${TMPDIR:-/tmp}/undra-example-ndk-$(printf '%s' "$ndk" | cksum | cut -d' ' -f1)}"

rm -rf "$repo"
mkdir -p "$repo"
ln -s "$ndk/toolchains" "$repo/toolchains"
ln -s "$ndk/source.properties" "$repo/source.properties"
cp "$here/ndk.BUILD" "$repo/BUILD.bazel"
: > "$repo/REPO.bazel"
echo "--override_repository=android_ndk_macos=$repo --override_repository=android_ndk_linux=$repo"
