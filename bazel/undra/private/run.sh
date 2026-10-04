#!/bin/sh
# The one program every Undra Bazel action runs (ADR-061).
#
# It makes a private, offline copy of exactly the files the action declares, in a directory named by the target, then runs
# one command in it:
#
#   mode=cli     cargo build of the `undra` CLI from the Undra checkout
#   mode=build   `undra build -C <project> --platform <platform>`
#   mode=bindgen `undra bindgen -C <project> --library <the host library>`
#
# Its only argument is a parameter file of `key=value` lines (written by the rules; keys may repeat). Paths in it are
# relative to the execution root, which is the working directory. Nothing is read from the user's home, the network or
# the environment but what the lines say: Cargo's directory source is the crates.io archives named by the vendor manifest,
# and `rustc` is the toolchain Bazel resolved, with its sysroot merged from the toolchains the action was given.
set -eu

PARAMS="$1"
EXECROOT="$(pwd -P)"

# --- parameters -------------------------------------------------------------------------------------------------
MODE=""
CLI=""
RUSTC=""
CARGO=""
VENDOR_MANIFEST=""
PROJECT="."
PLATFORM="host"
RELEASE=0
SYMBOLS=0
WASM_OPT=""
LIBRARY=""
OUT_FILE=""
OUT_DIR=""
OUT_SYMBOLS=""
NAMESPACE=""
UNDRA_ROOT=""
UNDRA_FILES=""
APP_ROOT=""
APP_FILES=""
STAGE_KEY=""
SYSROOTS=""
OUT_SWIFT=""
OUT_KOTLIN=""
OUT_TS=""
OUT_KOTLIN_SRCJAR=""
ZIPPER=""
SWIFT_FILES=""
BINDGEN_PLATFORMS=""
BINDGEN_DOCS=0
EXTRA_PATH=""
INHERIT_PATH=0
ORIG_PATH="${PATH:-}"
EXTRA_ENV=""
while IFS= read -r line || [ -n "$line" ]; do
  key="${line%%=*}"
  value="${line#*=}"
  case "$key" in
    mode) MODE="$value" ;;
    cli) CLI="$value" ;;
    rustc) RUSTC="$value" ;;
    cargo) CARGO="$value" ;;
    sysroot) SYSROOTS="$SYSROOTS $value" ;;
    vendor) VENDOR_MANIFEST="$value" ;;
    project) PROJECT="$value" ;;
    platform) PLATFORM="$value" ;;
    release) RELEASE="$value" ;;
    symbols) SYMBOLS="$value" ;;
    wasm_opt) WASM_OPT="$value" ;;
    library) LIBRARY="$value" ;;
    out_file) OUT_FILE="$value" ;;
    out_dir) OUT_DIR="$value" ;;
    out_symbols) OUT_SYMBOLS="$value" ;;
    namespace) NAMESPACE="$value" ;;
    undra_root) UNDRA_ROOT="$value" ;;
    undra_files) UNDRA_FILES="$value" ;;
    app_root) APP_ROOT="$value" ;;
    app_files) APP_FILES="$value" ;;
    stage_key) STAGE_KEY="$value" ;;
    out_swift) OUT_SWIFT="$value" ;;
    out_kotlin) OUT_KOTLIN="$value" ;;
    out_ts) OUT_TS="$value" ;;
    out_kotlin_srcjar) OUT_KOTLIN_SRCJAR="$value" ;;
    zipper) ZIPPER="$value" ;;
    swift_file) SWIFT_FILES="$SWIFT_FILES
$value" ;;
    bindgen_platforms) BINDGEN_PLATFORMS="$value" ;;
    bindgen_docs) BINDGEN_DOCS="$value" ;;
    path) case "$value" in /*) EXTRA_PATH="$EXTRA_PATH:$value" ;; *) EXTRA_PATH="$EXTRA_PATH:$EXECROOT/$value" ;; esac ;;
    inherit_path) INHERIT_PATH="$value" ;;
    env) EXTRA_ENV="$EXTRA_ENV
$value" ;;
    '') ;;
    *) echo "undra bazel: unknown parameter '$key'" >&2; exit 2 ;;
  esac
done < "$PARAMS"

die() { echo "undra bazel: $*" >&2; exit 1; }
abs() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$EXECROOT" "$1" ;; esac; }

# --- where the build happens --------------------------------------------------------------------------------------
# Cargo hashes the absolute path of a path dependency that lies outside the workspace it builds (the core, the Undra crates) into
# the crate's disambiguator, and `undra build` names the shim crate after a hash of the project's path (`undra_core_<hash>`);
# LLVM and `wasm-opt` then fold and order functions by those names, so a build in another directory is another file (measured:
# 274,053 against 274,598 bytes for one core). So the build happens in a directory named by the target, the same on every
# machine and in every checkout: the same inputs give the same bytes, and a change to an input does not move the directory
# (which would rename every symbol). It is not named by the inputs' contents, for that reason.
#
# The directory is its own lock. On Linux the sandbox's /tmp is private and nothing ever waits; macOS shares /tmp between
# builds and users, so it is created with `mkdir -m 700` (atomic; /tmp's sticky bit stops anyone else renaming or removing
# it), a second build of the same target waits for the first, a directory a dead build of this user's left is taken over, and
# one another user holds sends this build to a directory of its own user's (its bytes then name that directory).
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; }
[ -n "$STAGE_KEY" ] || die "no stage_key"
KEY="$(printf '%s|%s' "$MODE" "$STAGE_KEY" | sha | cut -c1-16)"
STAGE_BASE=/tmp
[ -d "$STAGE_BASE" ] && [ -w "$STAGE_BASE" ] || STAGE_BASE="${TMPDIR:-/tmp}"
WORK="$STAGE_BASE/undra-bazel-$KEY"
alive() { # <pid>: whether that process exists, whoever owns it
  if command -v ps >/dev/null 2>&1; then ps -p "$1" >/dev/null 2>&1; else kill -0 "$1" 2>/dev/null; fi
}
waited=0
until mkdir -m 700 "$WORK" 2>/dev/null; do
  owner="$(cat "$WORK/.undra-bazel-owner" 2>/dev/null || true)"
  if [ ! -O "$WORK" ] && [ -e "$WORK" ]; then
    if [ "$WORK" = "$STAGE_BASE/undra-bazel-$KEY" ]; then
      echo "undra bazel: $WORK belongs to another user; building in $WORK-u$(id -u) instead" >&2
      WORK="$WORK-u$(id -u)"
      continue
    fi
    die "$WORK belongs to another user"
  fi
  if [ -n "$owner" ] && ! alive "$owner"; then rm -rf "$WORK"; continue; fi # left by a build of ours that died
  if [ -z "$owner" ] && [ -n "$(find "$WORK" -maxdepth 0 -mmin +2 2>/dev/null)" ]; then rm -rf "$WORK"; continue; fi
  waited=$((waited + 1))
  [ "$waited" -le 1800 ] || die "waited 30 minutes for $WORK, held by process '$owner'"
  sleep 1
done
echo "$$" > "$WORK/.undra-bazel-owner"
trap 'rm -rf "$WORK"' EXIT
trap 'exit 1' HUP INT TERM

# --- a private copy of the declared files -----------------------------------------------------------------------
# Exactly the files the rule declared (a manifest of `<path below the root>|<execution path>` lines), never the directory they
# are in: with --spawn_strategy=local the execution root is the whole source tree (a `.cargo/config.toml` included) and the
# repositories of every toolchain. `tar -h` (bsdtar and GNU tar both spell it so) follows the sandbox's symlinks, so what
# lands in $WORK is real files and a build that writes next to its sources (`undra build` writes `build/`) never writes into
# the user's tree. Sources below the root go through one `tar`; a generated file is copied on its own.
stage() { # <execution path of the root> <manifest> <destination directory>
  mkdir -p "$3"
  while IFS='|' read -r rel src; do
    [ -n "$rel" ] || continue
    if [ "$src" = "$1/$rel" ] || { [ "$1" = . ] && [ "$src" = "$rel" ]; }; then
      printf '%s\n' "$rel"
    else
      case "$rel" in */*) mkdir -p "$3/${rel%/*}" ;; esac
      cp "$(abs "$src")" "$3/$rel"
    fi
  done < "$(abs "$2")" > "$WORK/.stage.list"
  if [ -s "$WORK/.stage.list" ]; then
    (cd "$(abs "$1")" && tar -chf - -T "$WORK/.stage.list") | (cd "$3" && tar -xf -)
  fi
  rm -f "$WORK/.stage.list"
}

# --- the Rust toolchain ------------------------------------------------------------------------------------------
# rustc finds its standard library from `--sysroot`, and the toolchain Bazel resolves for a target is the sysroot of that
# target alone, so a wasm build (whose build scripts and proc macros run on the host) is given the host's sysroot and the
# wasm one, merged here file by file.
SYSROOT="$WORK/sysroot"
mkdir -p "$SYSROOT"
for root in $SYSROOTS; do
  root="$(abs "$root")"
  (cd "$root" && find . \( -type f -o -type l \) -print) | while IFS= read -r file; do
    file="${file#./}"
    [ -e "$SYSROOT/$file" ] || [ -L "$SYSROOT/$file" ] && continue
    mkdir -p "$SYSROOT/$(dirname "$file")"
    ln -s "$root/$file" "$SYSROOT/$file"
  done
done
REAL_RUSTC="$(abs "$RUSTC")"
mkdir -p "$WORK/bin"
cat > "$WORK/bin/rustc" <<EOF
#!/bin/sh
exec "$REAL_RUSTC" --sysroot "$SYSROOT" "\$@"
EOF
chmod +x "$WORK/bin/rustc"
ln -s "$(abs "$CARGO")" "$WORK/bin/cargo"

# --- Cargo, offline ----------------------------------------------------------------------------------------------
export CARGO_HOME="$WORK/cargo-home"
mkdir -p "$CARGO_HOME" "$WORK/home"
if [ -n "$VENDOR_MANIFEST" ]; then
  # Where Cargo's own registry would unpack the crates (`registry/src/<index>-<hash>`), under the name every Cargo of 1.85 or
  # later gives crates.io's index: `undra build` remaps `$CARGO_HOME/registry/src` to a fixed label in a release build, so a
  # crate's path in the binary is the same here as in a build that downloaded it.
  VENDOR_DIR="$CARGO_HOME/registry/src/index.crates.io-1949cf8c6b5b557f"
  VENDOR_BASE="$(dirname "$(abs "$VENDOR_MANIFEST")")"
  mkdir -p "$VENDOR_DIR"
  while IFS='|' read -r name version checksum archive; do
    [ -n "$name" ] || continue
    tar -xzf "$VENDOR_BASE/$archive" -C "$VENDOR_DIR"
    printf '{"files":{},"package":"%s"}\n' "$checksum" > "$VENDOR_DIR/$name-$version/.cargo-checksum.json"
  done < "$(abs "$VENDOR_MANIFEST")"
fi

if [ -n "$UNDRA_FILES" ]; then
  stage "$UNDRA_ROOT" "$UNDRA_FILES" "$WORK/undra"
  # Only the library crates are members: the examples and benchmarks of the checkout are not inputs of any build here, and
  # Cargo reads every member's manifest when it loads a workspace.
  sed -e '/"examples\//d' -e '/"bench"/d' "$WORK/undra/Cargo.toml" > "$WORK/undra/Cargo.toml.pruned"
  mv "$WORK/undra/Cargo.toml.pruned" "$WORK/undra/Cargo.toml"
fi

{
  echo '[net]'
  echo 'offline = true'
  echo '[term]'
  echo 'color = "never"'
  if [ -n "$VENDOR_MANIFEST" ]; then
    echo '[source.crates-io]'
    echo 'replace-with = "vendored-sources"'
    echo '[source.vendored-sources]'
    echo "directory = \"$VENDOR_DIR\""
  fi
  if [ -n "$UNDRA_FILES" ] && [ "$MODE" != cli ]; then
    # The core depends on the `undra` crate by version like any app; this is where the checkout stands in for the registry.
    echo '[patch.crates-io]'
    for crate in "$WORK"/undra/crates/*/; do
      crate="${crate%/}"
      [ -f "$crate/Cargo.toml" ] || continue
      case "$(basename "$crate")" in
        # Never a dependency of an app's core: patching them only makes Cargo warn that the patch is unused.
        undra-cli | undra-bindgen | undra-transport) continue ;;
      esac
      echo "$(basename "$crate") = { path = \"$crate\" }"
    done
  fi
} > "$CARGO_HOME/config.toml"

# The toolchain's cargo and rustc first, then the system's; a platform whose toolchain is the machine's (Xcode, the NDK and
# cargo-ndk) also gets the PATH the build was started with, after them.
export PATH="$WORK/bin${EXTRA_PATH}:/usr/bin:/bin:/usr/sbin:/sbin"
if [ "$INHERIT_PATH" = 1 ] && [ -n "$ORIG_PATH" ]; then export PATH="$PATH:$ORIG_PATH"; fi
export RUSTC="$WORK/bin/rustc"
export HOME="$WORK/home"
export CARGO_TARGET_DIR="$WORK/target"
export CARGO_INCREMENTAL=0
export CARGO_NET_OFFLINE=true
export CARGO_TERM_COLOR=never
export LC_ALL=C
export TZ=UTC
unset RUSTUP_TOOLCHAIN RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
if [ -n "$EXTRA_ENV" ]; then
  # key=value lines
  OLD_IFS="$IFS"
  IFS='
'
  for pair in $EXTRA_ENV; do
    [ -n "$pair" ] && export "$pair"
  done
  IFS="$OLD_IFS"
fi

# --- the command -------------------------------------------------------------------------------------------------
case "$MODE" in
  cli)
    (cd "$WORK/undra" && cargo build --offline -p undra-cli --bin undra)
    cp "$WORK/target/debug/undra" "$(abs "$OUT_FILE")"
    ;;
  build)
    [ -n "$APP_FILES" ] || die "no app_files"
    stage "$APP_ROOT" "$APP_FILES" "$WORK/app"
    PROJECT_DIR="$WORK/app/$PROJECT"
    [ -f "$PROJECT_DIR/undra.toml" ] || die "there is no undra.toml in $PROJECT of the app's files"
    # A lock file in the project seeds the shim's (undra build); the checkout's stands in for it when the app has none.
    if [ ! -f "$WORK/app/Cargo.lock" ] && [ ! -f "$PROJECT_DIR/Cargo.lock" ] && [ -f "$WORK/undra/Cargo.lock" ]; then
      cp "$WORK/undra/Cargo.lock" "$PROJECT_DIR/Cargo.lock"
    fi
    if [ -n "$WASM_OPT" ]; then
      mkdir -p "$WORK/tools"
      ln -s "$(abs "$WASM_OPT")" "$WORK/tools/wasm-opt"
      export PATH="$WORK/tools:$PATH"
    fi
    set -- build -C "$PROJECT_DIR" --platform "$PLATFORM"
    [ "$RELEASE" = 1 ] && set -- "$@" --release
    [ "$SYMBOLS" = 1 ] || set -- "$@" --no-symbols
    # From the project, as a developer runs it: Cargo looks for configuration from its working directory up, and the execution
    # root's parents are the output base's (on Linux, below the home directory and its ~/.cargo/config.toml).
    (cd "$PROJECT_DIR" && "$(abs "$CLI")" "$@") || {
      echo "undra bazel: the build sees only the files the target declares (undra.toml, \`srcs\`, \`workspace\`), copied to $WORK/app: a file the error above says is missing is one to add to \`srcs\`" >&2
      exit 1
    }
    BUILD_DIR="$PROJECT_DIR/build"
    case "$PLATFORM" in
      host)
        found=""
        for candidate in "$BUILD_DIR/host/lib$NAMESPACE.dylib" "$BUILD_DIR/host/lib$NAMESPACE.so"; do
          [ -f "$candidate" ] && found="$candidate"
        done
        [ -n "$found" ] || die "undra build did not produce lib$NAMESPACE below build/host (it produced: $(ls "$BUILD_DIR/host" 2>/dev/null | tr '\n' ' ')): undra_core(namespace = \"$NAMESPACE\") must be the [core] namespace of undra.toml"
        cp "$found" "$(abs "$OUT_FILE")"
        ;;
      web)
        found="$BUILD_DIR/web/$NAMESPACE.wasm"
        [ -f "$found" ] || die "undra build did not produce $NAMESPACE.wasm below build/web (it produced: $(ls "$BUILD_DIR/web" 2>/dev/null | tr '\n' ' ')): undra_core(namespace = \"$NAMESPACE\") must be the [core] namespace of undra.toml"
        cp "$found" "$(abs "$OUT_FILE")"
        ;;
      ios)
        mkdir -p "$(abs "$OUT_DIR")"
        cp -R "$BUILD_DIR"/ios/. "$(abs "$OUT_DIR")/"
        ;;
      android)
        mkdir -p "$(abs "$OUT_DIR")"
        cp -R "$BUILD_DIR"/android/. "$(abs "$OUT_DIR")/"
        ;;
      *) die "unknown platform '$PLATFORM'" ;;
    esac
    if [ -n "$OUT_SYMBOLS" ]; then
      mkdir -p "$(abs "$OUT_SYMBOLS")"
      [ -d "$BUILD_DIR/symbols" ] && cp -R "$BUILD_DIR"/symbols/. "$(abs "$OUT_SYMBOLS")/"
    fi
    ;;
  bindgen)
    [ -n "$APP_FILES" ] || die "no app_files"
    stage "$APP_ROOT" "$APP_FILES" "$WORK/app"
    PROJECT_DIR="$WORK/app/$PROJECT"
    [ -f "$PROJECT_DIR/undra.toml" ] || die "there is no undra.toml in $PROJECT of the app's files"
    GEN="$WORK/generated"
    set -- bindgen -C "$PROJECT_DIR" --library "$(abs "$LIBRARY")" --out "$GEN"
    [ -n "$BINDGEN_PLATFORMS" ] && set -- "$@" --platforms "$BINDGEN_PLATFORMS"
    [ "$BINDGEN_DOCS" = 1 ] && set -- "$@" --docs
    (cd "$PROJECT_DIR" && "$(abs "$CLI")" "$@")
    # One declared tree per language: the directory `undra bindgen` wrote for it.
    for language in swift kotlin ts; do
      case "$language" in
        swift) dest="$OUT_SWIFT" ;;
        kotlin) dest="$OUT_KOTLIN" ;;
        ts) dest="$OUT_TS" ;;
      esac
      [ -n "$dest" ] || continue
      [ -d "$GEN/$language" ] || die "undra bindgen wrote no $language tree"
      mkdir -p "$(abs "$dest")"
      cp -R "$GEN/$language"/. "$(abs "$dest")/"
    done
    if [ -n "$SWIFT_FILES" ]; then
      # The Swift files `rules_swift` compiles, one declared output each. The set depends on the schema, so it is checked
      # both ways: a file that is listed and not generated, and one that is generated and not listed.
      LISTED="$WORK/swift.listed"
      ACTUAL="$WORK/swift.actual"
      printf '%s\n' "$SWIFT_FILES" | sed -e '/^$/d' -e 's#|.*##' | LC_ALL=C sort > "$LISTED"
      (cd "$GEN/swift" && find Sources -type f \( -name '*.swift' -o -name '*.c' -o -name '*.h' -o -name module.modulemap \) | LC_ALL=C sort) > "$ACTUAL"
      if ! cmp -s "$LISTED" "$ACTUAL"; then
        echo "undra bazel: the Swift files in swift_files are not the ones undra bindgen generated for this core." >&2
        echo "undra bazel: set swift_files to:" >&2
        sed -e 's#^#    "#' -e 's#$#",#' "$ACTUAL" >&2
        exit 1
      fi
      printf '%s\n' "$SWIFT_FILES" | sed -e '/^$/d' | while IFS='|' read -r rel dest; do
        mkdir -p "$(dirname "$(abs "$dest")")"
        cp "$GEN/swift/$rel" "$(abs "$dest")"
      done
    fi
    if [ -n "$OUT_KOTLIN_SRCJAR" ]; then
      # The Kotlin sources as a source jar (what rules_kotlin takes), entries named by their package path.
      SRC="$GEN/kotlin/src/main/kotlin"
      ENTRIES="$WORK/srcjar.entries"
      (cd "$SRC" && find . -type f -name '*.kt' | sort) | sed -e 's#^\./##' | while IFS= read -r entry; do
        printf '%s=%s\n' "$entry" "$SRC/$entry"
      done > "$ENTRIES"
      # shellcheck disable=SC2046
      "$(abs "$ZIPPER")" cC "$(abs "$OUT_KOTLIN_SRCJAR")" $(cat "$ENTRIES")
    fi
    ;;
  *) die "unknown mode '$MODE'" ;;
esac
