#!/bin/sh
# Prints the Bazel flags that make this workspace use an NDK that is already installed (`sdkmanager "ndk;27.2.12479018"`, Android
# Studio) instead of downloading the archive MODULE.bazel pins:
#
#   flags="$(android/use-installed-ndk.sh)"
#   bazel build //:mobile_android $flags
#
# It builds a repository of symbolic links to the installed NDK, with the same BUILD file as the archive's (android/ndk.BUILD),
# and overrides the two NDK repositories with it. The build still declares the NDK as an input; only where it comes from changes.
#
#   use-installed-ndk.sh [<ndk directory> [<repository directory>]]
#
# The NDK is $1, else $ANDROID_NDK_HOME, else the newest $ANDROID_HOME/ndk/<version> (by version, not by name). The repository is
# $2, else `$XDG_CACHE_HOME/undra/example-ndk-<n>` (`~/.cache` by default), a directory of this user's alone (mode 700) that only
# this script writes: it marks what it creates (`.undra-example-ndk`) and replaces nothing without the mark. The flags are printed
# on one line for an unquoted `$flags`, so a repository directory with a space or a tab in its path is refused: name another with $2.
set -eu

here="$(cd "$(dirname "$0")" && pwd -P)"
marker=.undra-example-ndk
ndk="${1:-${ANDROID_NDK_HOME:-}}"
if [ -z "$ndk" ] && [ -d "${ANDROID_HOME:-/nonexistent}/ndk" ]; then
  # The newest version: 27.2.12479018 is newer than 9.0.0, which a plain sort would put last.
  ndk="$(find "$ANDROID_HOME/ndk" -mindepth 1 -maxdepth 1 -type d | awk -F/ '{ print $NF "\t" $0 }' |
    sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1 | cut -f2-)"
fi
[ -n "$ndk" ] && [ -f "$ndk/source.properties" ] || {
  echo "use-installed-ndk.sh: no NDK found (pass its directory, or set ANDROID_NDK_HOME or ANDROID_HOME)" >&2
  exit 1
}
ndk="$(cd "$ndk" && pwd -P)"
cache="${XDG_CACHE_HOME:-${HOME:-/nonexistent}/.cache}"
repo="${2:-$cache/undra/example-ndk-$(printf '%s' "$ndk" | cksum | cut -d' ' -f1)}"
case "$repo" in
  /*) ;;
  *) repo="$(pwd -P)/$repo" ;;
esac
case "$repo" in
  *[[:space:]]*)
    echo "use-installed-ndk.sh: the repository directory '$repo' has whitespace in its path, which the printed flags cannot carry: pass another directory as the second argument" >&2
    exit 1
    ;;
esac

# Only a directory this script made is replaced; anything else at that path is left alone and named.
if [ -e "$repo" ] && [ ! -f "$repo/$marker" ]; then
  echo "use-installed-ndk.sh: $repo exists and is not a repository this script made (no $marker in it): pass another directory as the second argument" >&2
  exit 1
fi
if [ -f "$repo/$marker" ]; then
  rm -f "$repo/toolchains" "$repo/source.properties" "$repo/BUILD.bazel" "$repo/REPO.bazel" "$repo/$marker"
  rmdir "$repo"
fi
mkdir -p "$(dirname "$repo")"
mkdir -m 700 "$repo"
: > "$repo/$marker"
ln -s "$ndk/toolchains" "$repo/toolchains"
ln -s "$ndk/source.properties" "$repo/source.properties"
cp "$here/ndk.BUILD" "$repo/BUILD.bazel"
: > "$repo/REPO.bazel"
# Each repository by two names, because Bazel reads the name differently by version and silently ignores one it does not know:
# 9.x takes the apparent name, while 8.x takes only the canonical one (`+_repo_rules+<name>`, the name of a repository that
# MODULE.bazel defines with `use_repo_rule`) and treats an apparent name as a repository that does not exist, so the override
# would do nothing and Bazel would download the archive.
flags=""
for name in android_ndk_macos android_ndk_linux; do
  flags="$flags --override_repository=$name=$repo --override_repository=+_repo_rules+$name=$repo"
done
echo "${flags# }"
