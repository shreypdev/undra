#!/bin/sh
# The build directory of an action (`/tmp/undra-bazel-<key>`) is its own lock, and a core built with `keep_debug_objects`
# leaves it behind with the objects a debugger needs. What stays must be free for the next build of the target to take:
# its owner file says `kept`, not the pid of the build that made it, since that pid could be another process's by then and
# the next build would wait for it; and what stays is the archives the debug map names and the sources the DWARF names,
# not the rest of the build. Here run.sh runs with a stand-in CLI that writes the files a build would, twice over
# the same stage key, then once with an owner left by a build that died, then once without the attribute. The test's timeout
# is the hang bound: a run that waits for a lock nobody holds does not come back.
set -u

runner="$TEST_SRCDIR/$TEST_WORKSPACE/undra/private/run.sh"
[ -f "$runner" ] || runner="$(find "$TEST_SRCDIR" -path '*undra/private/run.sh' | head -n 1)"
[ -f "$runner" ] || { echo "FAIL: run.sh not found below $TEST_SRCDIR" >&2; exit 1; }

dir="${TEST_TMPDIR:-$(mktemp -d)}"
cd "$dir" || exit 1
key="undra-stage-lock-test-$$"
# Where run.sh builds for this key (its own rule: /tmp when writable, else TMPDIR; the key hashes the mode and the stage key).
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; }
base=/tmp
[ -d "$base" ] && [ -w "$base" ] || base="${TMPDIR:-/tmp}"
work="$base/undra-bazel-$(printf '%s|%s' build "$key" | sha | cut -c1-16)"
trap 'rm -rf "$work"' EXIT

# The stand-in CLI: what `undra build --platform ios` leaves in Cargo's target directory and below build/ios.
cat > fake-undra <<'CLI'
#!/bin/sh
set -eu
for triple in aarch64-apple-ios aarch64-apple-ios-sim; do
  for profile in debug release-mobile; do
    mkdir -p "$CARGO_TARGET_DIR/$triple/$profile/deps"
    echo "an archive with the core's DWARF" > "$CARGO_TARGET_DIR/$triple/$profile/libundra_core_0123.a"
    echo "the copy Cargo keeps, which the debug map does not name" > "$CARGO_TARGET_DIR/$triple/$profile/deps/libundra_core_0123-4567.a"
    echo "an object the debug map does not name" > "$CARGO_TARGET_DIR/$triple/$profile/deps/other.o"
  done
done
mkdir -p "$CARGO_TARGET_DIR/debug/build"
echo "a build script of the host" > "$CARGO_TARGET_DIR/debug/build/script"
mkdir -p build/ios/Fake.xcframework
echo slice > build/ios/Fake.xcframework/libfake.a
mkdir -p "$CARGO_HOME/registry/src/index/serde-1.0.0" "$CARGO_HOME/registry/cache"
echo "pub fn serde() {}" > "$CARGO_HOME/registry/src/index/serde-1.0.0/lib.rs"
echo "an archive" > "$CARGO_HOME/registry/cache/serde-1.0.0.crate"
CLI
chmod +x fake-undra
: > rustc
: > cargo
mkdir -p app
printf '[core]\nnamespace = "fake"\n' > app/undra.toml
printf 'undra.toml|app/undra.toml\n' > app.manifest

params() { # <keep_debug_objects>
  cat <<PARAMS
mode=build
platform=ios
cli=fake-undra
rustc=rustc
cargo=cargo
app_root=app
app_files=app.manifest
project=.
namespace=fake
out_dir=out
stage_key=$key
keep_debug_objects=$1
PARAMS
}
params 1 > keep.params
params 0 > plain.params

run() { # <params> <what>
  rm -rf out
  out="$(sh "$runner" "$1" 2>&1)" || { echo "FAIL: $2: run.sh failed: $out" >&2; exit 1; }
  [ -f out/Fake.xcframework/libfake.a ] || { echo "FAIL: $2: the output was not copied: $out" >&2; exit 1; }
}
kept() { # <what>: the directory holds the archives the debug map names, the sources, the owner mark, and nothing else
  [ "$(cat "$work/.undra-bazel-owner" 2>/dev/null)" = kept ] || { echo "FAIL: $1: the owner file does not say kept: '$(cat "$work/.undra-bazel-owner" 2>/dev/null)'" >&2; exit 1; }
  for profile in debug release-mobile; do
    [ -f "$work/target/aarch64-apple-ios/$profile/libundra_core_0123.a" ] || { echo "FAIL: $1: the core's $profile archive was not kept" >&2; exit 1; }
    [ ! -e "$work/target/aarch64-apple-ios/$profile/deps" ] || { echo "FAIL: $1: the $profile/deps copies were kept" >&2; exit 1; }
  done
  [ ! -e "$work/target/debug" ] || { echo "FAIL: $1: the host profile directory was kept" >&2; exit 1; }
  [ "$(find "$work/target" -type f | wc -l | tr -d ' ')" = 4 ] || { echo "FAIL: $1: not the four archives: $(find "$work/target" -type f)" >&2; exit 1; }
  [ -f "$work/app/undra.toml" ] || { echo "FAIL: $1: the project's sources were not kept" >&2; exit 1; }
  [ ! -e "$work/app/build" ] || { echo "FAIL: $1: the project's build output was kept" >&2; exit 1; }
  [ ! -e "$work/cargo-home" ] || { echo "FAIL: $1: Cargo's home (the vendored crates, 295 MB unpacked for the example) was kept" >&2; exit 1; }
  rest="$(ls -A "$work" | grep -v -e '^target$' -e '^app$' -e '^\.undra-bazel-owner$')"
  [ -z "$rest" ] || { echo "FAIL: $1: more than the archives and the sources stayed: $rest" >&2; exit 1; }
}

run keep.params "the first build"
kept "the first build"
run keep.params "the second build, over the kept directory"
kept "the second build"

# A build that died mid-way: its owner file names a process that is gone.
echo "$(sh -c 'echo $$')" > "$work/.undra-bazel-owner"
run keep.params "the build after one that died"
kept "the build after one that died"

# Without the attribute the directory goes with the build.
run plain.params "a plain build"
[ ! -e "$work" ] || { echo "FAIL: a plain build left $work behind" >&2; exit 1; }
echo PASS
