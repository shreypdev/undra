#!/usr/bin/env bash
# Tests site/install.sh (the curl installer) against a release served from this machine.
#
# It builds a release layout the way GitHub serves one (v<version>/undra-v<version>-<target>.tar.gz
# and checksums.txt), serves it with `python3 -m http.server` on 127.0.0.1 and points the installer
# at it with UNDRA_INSTALL_BASE_URL. Checked: a good install and upgrade under several POSIX
# shells, the latest-release lookup, and that a tampered tarball, a missing, duplicated or
# malformed checksum, a missing sha256 tool, a bad version, an unsupported platform and musl are
# all refused without installing anything.
# Also checked: the PATH line it prints (a printf that cannot join the last line of a startup file),
# UNDRA_MODIFY_PATH=1 writing that line itself (once, on a line of its own, into the shell's file)
# and the note that names an older undra found first on PATH, with its version.
#
#   bash packaging/test-install.sh                       # builds undra-cli (debug), or $UNDRA_BIN
#   bash packaging/test-install.sh --release-dir <dir>   # a real release's tarballs + checksums.txt
# shellcheck disable=SC2086  # the tool and target lists are split into words on purpose
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: bash packaging/test-install.sh [--release-dir <dir>] [-h | --help]

With no option: take the binary from $UNDRA_BIN, or build `cargo build -p undra-cli` and use
target/debug/undra, pack it for all four targets and serve that as the release.

  --release-dir <dir>  test a real release instead: <dir> holds undra-v<version>-<target>.tar.gz for
                       the four targets and checksums.txt (the release workflow's artifacts)
  -h, --help           print this text

Environment: UNDRA_BIN (an existing undra binary), KEEP_TMP=1 (keep the scratch directory).
Needs python3, curl and tar.
USAGE
}

release_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --release-dir) [ $# -ge 2 ] || { echo "test-install.sh: --release-dir needs a directory" >&2; exit 2; }; release_dir=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; echo "test-install.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
if [ -n "$release_dir" ]; then
  [ -d "$release_dir" ] || { echo "test-install.sh: no such directory: $release_dir" >&2; exit 2; }
  release_dir=$(cd "$release_dir" && pwd)
fi
script=$root/site/install.sh
tmp=$(mktemp -d "${TMPDIR:-/tmp}/undra-install-test.XXXXXX")
server_pid=""
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [ "${KEEP_TMP:-0}" = 1 ]; then echo "kept $tmp"; else rm -rf "$tmp"; fi
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; [ -z "${OUT:-}" ] || printf '%s\n' "--- installer output ---" "$OUT" "------------------------" >&2; exit 1; }
pass() { echo "ok: $*"; }

for tool in python3 curl tar; do command -v "$tool" >/dev/null || fail "$tool is required"; done
[ -f "$script" ] || fail "missing $script"

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
case "$(uname -sm)" in
  "Darwin arm64") target=aarch64-apple-darwin ;;
  "Darwin x86_64") target=x86_64-apple-darwin ;;
  "Linux x86_64") target=x86_64-unknown-linux-gnu ;;
  "Linux aarch64" | "Linux arm64") target=aarch64-unknown-linux-gnu ;;
  *) fail "no prebuilt undra for $(uname -sm), so the installer cannot be tested here" ;;
esac
targets="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"

# --- the release layout -----------------------------------------------------------------------
www=$tmp/www
mkdir -p "$www/v$version" "$tmp/bin" "$tmp/packed"

if [ -n "$release_dir" ]; then
  [ -f "$release_dir/checksums.txt" ] || fail "$release_dir/checksums.txt is missing"
  real=$release_dir/undra-v$version-$target.tar.gz
  [ -f "$real" ] || fail "$real is missing"
  for t in $targets; do cp "$release_dir/undra-v$version-$t.tar.gz" "$www/v$version/"; done
  cp "$release_dir/checksums.txt" "$www/v$version/checksums.txt"
  tar -xzf "$real" -C "$tmp/bin" undra
else
  binary=${UNDRA_BIN:-}
  if [ -z "$binary" ]; then
    echo "Building undra-cli (debug)..."
    (cd "$root" && cargo build -p undra-cli >&2)
    binary=${CARGO_TARGET_DIR:-$root/target}/debug/undra
  fi
  [ -x "$binary" ] || fail "no executable at $binary"
  cp "$binary" "$tmp/bin/undra"
  for t in $targets; do
    sh "$root/packaging/pack-release.sh" --version "$version" --target "$t" --binary "$tmp/bin/undra" --out "$tmp/packed" >/dev/null
  done
  cat "$tmp"/packed/*.sha256 >"$www/v$version/checksums.txt"
  cp "$tmp"/packed/*.tar.gz "$www/v$version/"
fi

# Releases that are wrong in one way each, made from the same binary.
bad() { mkdir -p "$www/v$1"; } # <version>
pack_into() { # <version> -> tarball path (in $tmp/packed2)
  sh "$root/packaging/pack-release.sh" --version "$1" --target "$target" --binary "$tmp/bin/undra" --out "$tmp/packed2" >/dev/null
}
asset_of() { printf 'undra-v%s-%s.tar.gz' "$1" "$target"; }

# 9.9.9: the tarball was changed after its checksum was written.
bad 9.9.9; pack_into 9.9.9
cp "$tmp/packed2/$(asset_of 9.9.9)" "$www/v9.9.9/"; cp "$tmp/packed2/$(asset_of 9.9.9).sha256" "$www/v9.9.9/checksums.txt"
printf 'tampered' >>"$www/v9.9.9/$(asset_of 9.9.9)"
# 9.9.8: checksums.txt has no entry for this machine's tarball.
bad 9.9.8; pack_into 9.9.8
cp "$tmp/packed2/$(asset_of 9.9.8)" "$www/v9.9.8/"; echo "0000000000000000000000000000000000000000000000000000000000000000  undra-v9.9.8-other-target.tar.gz" >"$www/v9.9.8/checksums.txt"
# 9.9.7: two entries for the tarball.
bad 9.9.7; pack_into 9.9.7
cp "$tmp/packed2/$(asset_of 9.9.7)" "$www/v9.9.7/"; cat "$tmp/packed2/$(asset_of 9.9.7).sha256" "$tmp/packed2/$(asset_of 9.9.7).sha256" >"$www/v9.9.7/checksums.txt"
# 9.9.6: the "hash" is not a sha256.
bad 9.9.6; pack_into 9.9.6
cp "$tmp/packed2/$(asset_of 9.9.6)" "$www/v9.9.6/"; echo "not-a-sha256  $(asset_of 9.9.6)" >"$www/v9.9.6/checksums.txt"
# 9.9.5: a checksums.txt but no tarball.
bad 9.9.5; pack_into 9.9.5
cp "$tmp/packed2/$(asset_of 9.9.5).sha256" "$www/v9.9.5/checksums.txt"
# 9.9.4: the tarball is intact but holds no undra.
bad 9.9.4; mkdir -p "$tmp/empty" && : >"$tmp/empty/readme" && tar -czf "$www/v9.9.4/$(asset_of 9.9.4)" -C "$tmp/empty" readme
(cd "$www/v9.9.4" && { shasum -a 256 "$(asset_of 9.9.4)" 2>/dev/null || sha256sum "$(asset_of 9.9.4)"; } >checksums.txt)

# What GitHub's "latest release" reply looks like: long, nested, tag_name among other fields.
mkdir -p "$www/api"
cat >"$www/api/latest.json" <<JSON
{
  "url": "https://api.github.com/repos/shreypdev/undra/releases/1",
  "assets_url": "https://api.github.com/repos/shreypdev/undra/releases/1/assets",
  "id": 1,
  "author": { "login": "shreypdev", "id": 2, "type": "User" },
  "tag_name": "v$version",
  "target_commitish": "main",
  "name": "Undra v$version",
  "draft": false,
  "prerelease": false,
  "assets": [ { "name": "checksums.txt", "tag_name": "decoy" } ],
  "body": "notes, with a comma, and a \"quoted\" word"
}
JSON
printf '{"id":1,"tag_name":"v%s","name":"Undra","assets":[]}' "$version" >"$www/api/minified.json"

# --- serve it ---------------------------------------------------------------------------------
python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$www" >"$tmp/server.log" 2>&1 &
server_pid=$!
port=""
for _ in $(seq 1 100); do
  port=$(sed -n 's/^Serving HTTP on 127.0.0.1 port \([0-9]*\).*/\1/p' "$tmp/server.log" | head -n 1)
  [ -z "$port" ] || break
  kill -0 "$server_pid" 2>/dev/null || fail "the web server exited: $(cat "$tmp/server.log")"
  sleep 0.1
done
[ -n "$port" ] || fail "the web server did not start: $(cat "$tmp/server.log")"
base=http://127.0.0.1:$port
curl -fsS "$base/v$version/checksums.txt" >/dev/null || fail "the served release is not reachable at $base"

# --- running the installer --------------------------------------------------------------------
# The runs' PATH: this machine's without the directories that hold an undra, which every run would
# otherwise name as an older undra found first on PATH.
run_path=""
IFS=: read -r -a path_dirs <<<"$PATH"
for d in "${path_dirs[@]}"; do
  if [ -n "$d" ] && [ ! -e "$d/undra" ]; then run_path=${run_path:+$run_path:}$d; fi
done
n=0
OUT=""
STATUS=0
# run_install <shell> [VAR=value ...]: a fresh HOME, a clean environment, output and status kept.
run_install() {
  local shell=$1; shift
  n=$((n + 1))
  HOME_DIR=$tmp/home-$n
  mkdir -p "$HOME_DIR" "$tmp/tmpdir"
  OUT=$(env -i PATH="$run_path" HOME="$HOME_DIR" TMPDIR="$tmp/tmpdir" SHELL=/bin/zsh "$@" "$shell" "$script" 2>&1) && STATUS=0 || STATUS=$?
}
next_home() { printf '%s/home-%s\n' "$tmp" "$((n + 1))"; } # the HOME the next run_install gets
expect_installed() { # [dir]
  local bin=${1:-$HOME_DIR/.undra}/bin/undra
  [ -x "$bin" ] || fail "$bin was not installed"
  local out; out=$("$bin" --version) || fail "$bin does not run"
  case "$out" in *"$version"*) ;; *) fail "$bin --version printed '$out', expected $version" ;; esac
}
expect_refused() { # <message fragment>
  [ "$STATUS" -ne 0 ] || fail "the installer succeeded but should have refused"
  case "$OUT" in *"$1"*) ;; *) fail "the installer's message does not contain '$1'" ;; esac
  [ ! -e "$HOME_DIR/.undra/bin/undra" ] || fail "a refused install left a binary behind"
  if [ -d "$HOME_DIR/.undra/bin" ]; then
    [ -z "$(find "$HOME_DIR/.undra/bin" -type f)" ] || fail "a refused install left files in bin/"
  fi
}

# What a zsh user is told to run, and the line it adds.
zsh_hint='  printf '\''\nexport PATH="$HOME/.undra/bin:$PATH"\n'\'' >> ~/.zshrc && exec zsh'
export_line='export PATH="$HOME/.undra/bin:$PATH"'

# 1. A good install, under every POSIX shell we can find.
shells=""
for s in sh dash bash; do
  if p=$(command -v "$s" 2>/dev/null); then shells="$shells $p"; fi
done
for sh_path in $shells; do
  run_install "$sh_path" UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="v$version"
  [ "$STATUS" = 0 ] || fail "install under $sh_path failed ($STATUS)"
  expect_installed
  case "$OUT" in *"Checksum verified"*) ;; *) fail "no 'Checksum verified' line under $sh_path" ;; esac
  case "$OUT" in *"$zsh_hint"*) ;; *) fail "no zsh PATH line under $sh_path" ;; esac
  pass "install under $sh_path: runs, checksum verified, PATH line printed"
done

# 2. The PATH line follows $SHELL, and is left out when the directory is already on PATH.
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" SHELL=/usr/bin/fish
case "$OUT" in *'fish_add_path "$HOME/.undra/bin"'*) ;; *) fail "no fish PATH line" ;; esac
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" SHELL=/bin/bash
bash_rc=.bashrc
[ "$(uname -s)" != Darwin ] || bash_rc=.bash_profile
case "$OUT" in *"  printf '\nexport PATH=\"\$HOME/.undra/bin:\$PATH\"\n' >> ~/$bash_rc && exec bash"*) ;; *) fail "no bash PATH line" ;; esac
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" SHELL=/bin/zsh PATH="$(next_home)/.undra/bin:$run_path"
case "$OUT" in *"not on your PATH"* | *"another undra"*) fail "told the user to add a directory that is already on PATH" ;; esac
pass "PATH hint: fish, bash, zsh, and silent when already on PATH"

# 3. UNDRA_HOME, and an upgrade over an existing install.
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" UNDRA_HOME="$tmp/custom home"
[ "$STATUS" = 0 ] || fail "install into UNDRA_HOME failed"
expect_installed "$tmp/custom home"
case "$OUT" in *"$tmp/custom home/bin"*) ;; *) fail "the PATH hint does not name the custom directory" ;; esac
UPG=$tmp/custom\ home
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" UNDRA_HOME="$UPG"
[ "$STATUS" = 0 ] || fail "upgrade over an existing install failed"
expect_installed "$UPG"
[ "$(find "$UPG/bin" -type f | wc -l | tr -d ' ')" = 1 ] || fail "an upgrade left extra files in bin/: $(find "$UPG/bin" -type f)"
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" UNDRA_HOME="relative/dir"
expect_refused "UNDRA_HOME must be an absolute path"
pass "UNDRA_HOME is honoured, an upgrade replaces the binary and leaves nothing else, relative paths are refused"

# 4. The latest release, from the API reply.
for reply in latest.json minified.json; do
  run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_API_URL="$base/api/$reply"
  [ "$STATUS" = 0 ] || fail "latest-release install from $reply failed"
  expect_installed
done
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_API_URL="$base/api/missing.json"
expect_refused "could not look up the latest release"
case "$OUT" in *"UNDRA_VERSION"*) ;; *) fail "the lookup failure does not say how to skip it" ;; esac
pass "latest release: tag_name read from a pretty-printed and a minified reply; a failed lookup says how to pin a version"

# 5. Transport: without the base-URL override only https is spoken, even for the API lookup; an
#    https mirror does not open plain http either.
run_install sh UNDRA_API_URL="$base/api/latest.json"
expect_refused "could not look up the latest release"
run_install sh UNDRA_INSTALL_BASE_URL="https://127.0.0.1:$port" UNDRA_API_URL="$base/api/latest.json"
expect_refused "could not look up the latest release"
pass "plain http is refused unless UNDRA_INSTALL_BASE_URL is itself http://"

# 6. Refusals, each leaving nothing installed.
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION=9.9.9
expect_refused "checksum mismatch"
pass "a tampered tarball is rejected"
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION=9.9.8
expect_refused "no entry for"
pass "a checksums.txt without the tarball's entry is rejected"
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION=9.9.7
expect_refused "more than one entry"
pass "a duplicated checksum entry is rejected"
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION=9.9.6
expect_refused "is not a sha256"
pass "a malformed checksum is rejected"
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION=9.9.5
expect_refused "could not download"
pass "a missing tarball is reported"
run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION=9.9.4
expect_refused "does not contain an undra executable"
pass "a tarball without an undra executable is rejected"
for v in "1.0" "1.0.0/../../x" "1.0.0 2" 'v1.0.0;id' "latest" $'x\n1.0.0'; do
  run_install sh UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$v"
  expect_refused "not a release version"
done
pass "a version that is not MAJOR.MINOR.PATCH never reaches a URL"

# 7. No sha256 tool, an unsupported platform, musl: a PATH of symlinks to exactly the tools the
#    installer needs, plus stand-ins for uname and ldd.
tools_dir() { # <dir> <tool>... : symlinks to those tools
  local dir=$1; shift
  mkdir -p "$dir"
  local t p
  for t in "$@"; do
    p=$(command -v "$t") || continue
    case "$p" in /*) ln -sf "$p" "$dir/$t" ;; esac
  done
}
base_tools="curl tar uname mktemp awk grep sed tr head cat rm mkdir cp chmod mv sysctl"
tools_dir "$tmp/path-nosha" $base_tools
sh_bin=$(command -v sh)
run_install "$sh_bin" PATH="$tmp/path-nosha" UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version"
expect_refused "neither shasum nor sha256sum"
pass "without shasum or sha256sum the installer fails closed, before downloading"

fake_uname() { # <dir> <"uname -sm" output>
  mkdir -p "$1"
  rm -f "$1/uname" # tools_dir may have linked the real one; never write through a symlink
  cat >"$1/uname" <<EOF
#!/bin/sh
case "\$*" in
  -sm) echo "$2" ;;
  -s) echo "${2%% *}" ;;
  *) echo "$2" ;;
esac
EOF
  chmod +x "$1/uname"
}
shatool=$(command -v shasum || command -v sha256sum)
for plat in "Linux riscv64" "Darwin ppc" "FreeBSD amd64" "MINGW64_NT-10.0-19045 x86_64"; do
  d=$tmp/path-uname-$(printf '%s' "$plat" | tr -c 'A-Za-z0-9' '_')
  tools_dir "$d" $base_tools "$(basename "$shatool")"
  fake_uname "$d" "$plat"
  run_install "$sh_bin" PATH="$d" UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version"
  case "$plat" in
    MINGW*) expect_refused "Windows is not supported yet" ;;
    *) expect_refused "no prebuilt undra for $plat" ;;
  esac
done
pass "unsupported platforms (Linux riscv64, Darwin ppc, FreeBSD, Windows) are refused with the way out"

d=$tmp/path-musl
tools_dir "$d" $base_tools "$(basename "$shatool")"
fake_uname "$d" "Linux x86_64"
rm -f "$d/ldd"
printf '#!/bin/sh\necho "musl libc (x86_64)" >&2\necho "Version 1.2.4" >&2\nexit 1\n' >"$d/ldd"; chmod +x "$d/ldd"
run_install "$sh_bin" PATH="$d" UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version"
expect_refused "uses musl"
pass "musl (Alpine) is refused with the way out"

# 8. --help and unknown arguments.
n=$((n + 1)); HOME_DIR=$tmp/home-$n; mkdir -p "$HOME_DIR"
OUT=$(env -i PATH="$PATH" HOME="$HOME_DIR" sh "$script" --help 2>&1) || fail "--help exited non-zero"
case "$OUT" in *"Usage:"*"UNDRA_INSTALL_BASE_URL"*) ;; *) fail "--help does not document the installer" ;; esac
OUT=$(env -i PATH="$PATH" HOME="$HOME_DIR" sh "$script" --bogus 2>&1) && fail "an unknown argument must fail" || true
case "$OUT" in *"unknown argument: --bogus"*) ;; *) fail "unknown-argument message is wrong" ;; esac
[ ! -e "$HOME_DIR/.undra" ] || fail "--help or a bad argument installed something"
pass "--help prints the usage and installs nothing; an unknown argument fails"

# 9. A truncated download runs nothing: cut inside main() the shell reports a syntax error before
#    executing a line; cut after it, only function definitions have run.
total=$(wc -l <"$script" | tr -d ' ')
for keep in 30 $((total - 1)); do
  head -n "$keep" "$script" >"$tmp/truncated.sh"
  mkdir -p "$tmp/home-trunc"
  OUT=$(env -i PATH="$PATH" HOME="$tmp/home-trunc" TMPDIR="$tmp/tmpdir" UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version" sh "$tmp/truncated.sh" 2>&1) && STATUS=0 || STATUS=$?
  [ ! -e "$tmp/home-trunc/.undra" ] || fail "a script cut after $keep lines installed something"
  if [ "$keep" = "$((total - 1))" ]; then
    [ "$STATUS" = 0 ] && [ -z "$OUT" ] || fail "a script without its last line should do nothing, got ($STATUS): $OUT"
  else
    [ "$STATUS" != 0 ] || fail "a script cut inside main() should be a syntax error"
  fi
done
pass "a truncated download does nothing"

# 10. UNDRA_MODIFY_PATH=1: the installer writes the PATH line itself, once, on a line of its own.
#     Unset, it writes nothing, and the line it prints is safe on the file that broke a user's zsh.
install_env=(UNDRA_INSTALL_BASE_URL="$base" UNDRA_VERSION="$version")
expect_file() { # <file> <expected contents, printf format> [args]: byte for byte
  local file=$1 format=$2; shift 2
  [ -f "$file" ] || fail "$file was not written"
  # shellcheck disable=SC2059 # the format is the expectation
  printf "$format" "$@" >"$tmp/expected"
  cmp -s "$file" "$tmp/expected" || fail "$file is not what was expected; it holds: $(od -c "$file")"
}
expect_only_undra() { # the run created nothing in HOME but .undra
  [ "$(ls -A "$HOME_DIR")" = .undra ] || fail "the run wrote to HOME: $(ls -A "$HOME_DIR")"
}

# a. A .zshrc whose last byte is not a newline: its line stays whole, the export goes on a line of
#    its own and the file ends with a newline.
h=$(next_home); mkdir -p "$h"; printf 'alias ll="ls -l"' >"$h/.zshrc"
run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1
[ "$STATUS" = 0 ] || fail "UNDRA_MODIFY_PATH=1 failed ($STATUS)"
expect_file "$h/.zshrc" 'alias ll="ls -l"\n%s\n' "$export_line"
case "$OUT" in *"Added this line to $h/.zshrc:"*"  $export_line"*"exec zsh"*) ;; *) fail "the run does not say what it wrote where" ;; esac
pass "UNDRA_MODIFY_PATH=1: a .zshrc without a final newline keeps its last line, the export is on a line of its own"

# b. The line is there already: nothing is written, and the run says so. Spelt with ~, or in
#    full, it counts too; commented out, it does not.
run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1 HOME="$h"
[ "$STATUS" = 0 ] || fail "a second UNDRA_MODIFY_PATH=1 run failed ($STATUS)"
expect_file "$h/.zshrc" 'alias ll="ls -l"\n%s\n' "$export_line"
case "$OUT" in *"$h/.zshrc already puts $h/.undra/bin on PATH; nothing was written"*) ;; *) fail "the second run does not say it wrote nothing" ;; esac
for spelling in tilde full; do
  h=$(next_home); mkdir -p "$h"
  case "$spelling" in
    tilde) existing='export PATH=~/.undra/bin:$PATH' ;;
    full) existing="export PATH=\"\$PATH:$h/.undra/bin\"" ;;
  esac
  printf '%s\n' "$existing" >"$h/.zshrc"
  run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1
  expect_file "$h/.zshrc" '%s\n' "$existing"
  case "$OUT" in *"nothing was written"*) ;; *) fail "'$existing' was not taken for the PATH line" ;; esac
done
h=$(next_home); mkdir -p "$h"; printf '# %s\n' "$export_line" >"$h/.zshrc"
run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1
expect_file "$h/.zshrc" '# %s\n%s\n' "$export_line" "$export_line"
# Another directory whose name begins with ours is not ours; a subdirectory is not either.
for other in '~/.undra/bin-old' '$HOME/.undra/bin2' '$HOME/.undra/bin/sub'; do
  h=$(next_home); mkdir -p "$h"; printf 'export PATH="%s:$PATH"\n' "$other" >"$h/.zshrc"
  run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1
  expect_file "$h/.zshrc" 'export PATH="%s:$PATH"\n%s\n' "$other" "$export_line"
done
# zsh's path array, and a trailing slash, name the same directory.
for existing in 'path=(~/.undra/bin $path)' 'export PATH="$HOME/.undra/bin/:$PATH"'; do
  h=$(next_home); mkdir -p "$h"; printf '%s\n' "$existing" >"$h/.zshrc"
  run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1
  expect_file "$h/.zshrc" '%s\n' "$existing"
  case "$OUT" in *"nothing was written"*) ;; *) fail "'$existing' was not taken for the PATH line" ;; esac
done
pass "UNDRA_MODIFY_PATH=1: a file that already sets the PATH entry is left as it is, and the run says so; bin-old, bin2 and bin/sub are other directories"

# c. A missing startup file is created holding the one line (bash's is per platform).
run_install sh "${install_env[@]}" SHELL=/bin/bash UNDRA_MODIFY_PATH=1
[ "$STATUS" = 0 ] || fail "UNDRA_MODIFY_PATH=1 under bash failed ($STATUS)"
expect_file "$HOME_DIR/$bash_rc" '%s\n' "$export_line"
[ "$(ls -A "$HOME_DIR" | sort | tr '\n' ' ')" = "$bash_rc .undra " ] || fail "bash: other files were written: $(ls -A "$HOME_DIR")"
case "$OUT" in *"Added this line to $HOME_DIR/$bash_rc:"*"exec bash"*) ;; *) fail "bash: the run does not say what it wrote" ;; esac
pass "UNDRA_MODIFY_PATH=1: a missing ~/$bash_rc is created with the one line"

# d. fish: its conf.d file, the directories made.
run_install sh "${install_env[@]}" SHELL=/usr/local/bin/fish UNDRA_MODIFY_PATH=1
[ "$STATUS" = 0 ] || fail "UNDRA_MODIFY_PATH=1 under fish failed ($STATUS)"
expect_file "$HOME_DIR/.config/fish/conf.d/undra.fish" 'fish_add_path "$HOME/.undra/bin"\n'
pass "UNDRA_MODIFY_PATH=1: fish gets ~/.config/fish/conf.d/undra.fish with fish_add_path"

# e. A shell it knows no file for: nothing is written, the line is printed. Already on PATH:
#    nothing to write. A value that is not 0 or 1: refused before anything is downloaded.
run_install sh "${install_env[@]}" SHELL=/bin/tcsh UNDRA_MODIFY_PATH=1
[ "$STATUS" = 0 ] || fail "UNDRA_MODIFY_PATH=1 under tcsh failed ($STATUS)"
case "$OUT" in *"knows no startup file for the shell 'tcsh'"*"nothing was written"*"  $export_line"*) ;; *) fail "tcsh: no refusal with the line to add" ;; esac
expect_only_undra
run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1 PATH="$(next_home)/.undra/bin:$run_path"
case "$OUT" in *"already on your PATH; no startup file was changed"*) ;; *) fail "on PATH: the run does not say it changed nothing" ;; esac
expect_only_undra
run_install sh "${install_env[@]}" UNDRA_MODIFY_PATH=yes
expect_refused "UNDRA_MODIFY_PATH must be 1"
case "$OUT" in *"Installing"*) fail "a bad UNDRA_MODIFY_PATH was found only after the download" ;; esac
pass "UNDRA_MODIFY_PATH=1: an unknown shell and a directory already on PATH write nothing; a bad value is refused first"

# f. Unset: nothing is written, the printf line is printed, and run as printed (but the exec) on
#    the .zshrc of (a), by sh and by bash, it leaves what (a) leaves.
for runner in sh bash; do
  h=$(next_home); mkdir -p "$h"; printf 'alias ll="ls -l"' >"$h/.zshrc"
  run_install sh "${install_env[@]}" SHELL=/bin/zsh
  [ "$STATUS" = 0 ] || fail "the default run failed ($STATUS)"
  expect_file "$h/.zshrc" 'alias ll="ls -l"'
  case "$OUT" in *"$zsh_hint"*) ;; *) fail "the default run does not print the printf line" ;; esac
  printed=$(printf '%s\n' "$OUT" | sed -n 's/^  \(printf .*\) && exec zsh$/\1/p')
  [ -n "$printed" ] || fail "no printf line to run"
  HOME=$h "$runner" -c "$printed" || fail "the printed line failed under $runner"
  expect_file "$h/.zshrc" 'alias ll="ls -l"\n%s\n' "$export_line"
done
pass "unset: nothing is written; the printed line, run, puts the export on a line of its own"

# g. UNDRA_HOME elsewhere: the written line names that directory in full, not $HOME/.undra/bin,
#    and a second run finds it.
h=$(next_home)
run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1 UNDRA_HOME="$h/tools/undra"
[ "$STATUS" = 0 ] || fail "UNDRA_MODIFY_PATH=1 with UNDRA_HOME failed ($STATUS)"
expect_installed "$h/tools/undra"
expect_file "$h/.zshrc" 'export PATH="%s/tools/undra/bin:$PATH"\n' "$h"
run_install sh "${install_env[@]}" SHELL=/bin/zsh UNDRA_MODIFY_PATH=1 UNDRA_HOME="$h/tools/undra" HOME="$h"
expect_file "$h/.zshrc" 'export PATH="%s/tools/undra/bin:$PATH"\n' "$h"
case "$OUT" in *"nothing was written"*) ;; *) fail "the UNDRA_HOME line written by the first run was not found by the second" ;; esac
pass "UNDRA_MODIFY_PATH=1: a custom UNDRA_HOME is written in full, once"

# 11. An older undra found first on PATH is named, with its version and how to remove it, before
#     the PATH line; one that is the installed file, or comes after it, is not.
stale=$tmp/stale-bin
mkdir -p "$stale"
printf '#!/bin/sh\necho "undra 0.1.0"\necho "second line"\n' >"$stale/undra"; chmod +x "$stale/undra"
run_install sh "${install_env[@]}" PATH="$stale:$run_path"
[ "$STATUS" = 0 ] || fail "install with an older undra on PATH failed ($STATUS)"
case "$OUT" in
  *"runs another undra on your PATH, not the one just installed"*"  $stale/undra (undra 0.1.0)"*"  rm $stale/undra"*"before $stale on your PATH"*"not on your PATH yet"*) ;;
  *) fail "an older undra first on PATH is not named with its version and the fix, before the PATH line" ;;
esac
case "$OUT" in *"second line"*) fail "more than the first line of the old --version was printed" ;; esac
pass "an older undra first on PATH is named with its version, before the PATH line, with rm as the fix"

h=$(next_home); mkdir -p "$h/.cargo/bin"; cp "$stale/undra" "$h/.cargo/bin/undra"
run_install sh "${install_env[@]}" PATH="$h/.undra/bin:$run_path:$h/.cargo/bin"
case "$OUT" in *"another undra"*) fail "named an undra that comes after the installed one" ;; esac
run_install sh "${install_env[@]}" PATH="$h/.cargo/bin:$h/.undra/bin:$run_path" HOME="$h"
case "$OUT" in *"  $h/.cargo/bin/undra (undra 0.1.0)"*"  cargo uninstall undra-cli"*"or put $h/.undra/bin before $h/.cargo/bin on your PATH."*) ;; *) fail "a cargo-installed undra is not named with cargo uninstall" ;; esac
pass "a cargo-installed undra first on PATH: cargo uninstall undra-cli; after the installed one: nothing said"

broken=$tmp/broken-bin
mkdir -p "$broken"
printf '#!/bin/sh\nexit 3\n' >"$broken/undra"; chmod +x "$broken/undra"
run_install sh "${install_env[@]}" PATH="$broken:$run_path"
case "$OUT" in *"  $broken/undra (version unknown)"*) ;; *) fail "an undra whose --version fails is not 'version unknown'" ;; esac
linked=$tmp/linked-bin
mkdir -p "$linked"
ln -s "$(next_home)/.undra/bin/undra" "$linked/undra"
run_install sh "${install_env[@]}" PATH="$linked:$run_path"
[ "$STATUS" = 0 ] || fail "install with a symlink to it on PATH failed ($STATUS)"
case "$OUT" in *"another undra"*) fail "a symlink to the installed file was named as another undra" ;; esac
pass "an undra whose --version fails is 'version unknown'; a symlink to the installed file is not another undra"

# 12. The 10 s guard on `--version`. The installer finds `sleep` on PATH, so a fake one stands in:
#     one that records its pid and sleeps for real shows the guard's sleep is killed once the
#     version is in (not left for the rest of its 10 s); one that returns at once fires the guard
#     without the wait, so an undra that hangs is "version unknown", is killed, and no shell says
#     "Terminated" (dash does, when wait reports the kill).
fake_sleep=$tmp/fake-sleep
mkdir -p "$fake_sleep"
printf '#!/bin/sh\necho $$ >"%s/sleep.pid"\nexec /bin/sleep "$@"\n' "$tmp" >"$fake_sleep/sleep"; chmod +x "$fake_sleep/sleep"
gone() { # <pid>: no longer a process, within a second
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 "$1" 2>/dev/null || return 0
    sleep 0.1
  done
  return 1
}
# This older undra answers once the guard's sleep has recorded its pid (within 2 s), so the sleep
# is known to be running when the guard is told to stop. No pid file means the sleep was killed
# before its shell got that far, which is gone too.
waiting=$tmp/waiting-bin
mkdir -p "$waiting"
printf '#!/bin/sh\ni=0\nwhile [ ! -f "%s/sleep.pid" ] && [ $i -lt 200 ]; do /bin/sleep 0.01; i=$((i + 1)); done\necho "undra 0.1.0"\n' "$tmp" >"$waiting/undra"; chmod +x "$waiting/undra"
rm -f "$tmp/sleep.pid"
run_install sh "${install_env[@]}" PATH="$waiting:$fake_sleep:$run_path"
case "$OUT" in *"  $waiting/undra (undra 0.1.0)"*) ;; *) fail "with the fake sleep on PATH the older undra is not named" ;; esac
if [ -f "$tmp/sleep.pid" ]; then
  gone "$(cat "$tmp/sleep.pid")" || { kill "$(cat "$tmp/sleep.pid")" 2>/dev/null; fail "the guard's sleep was left running after the version was in"; }
fi
instant_sleep=$tmp/instant-sleep
mkdir -p "$instant_sleep"
printf '#!/bin/sh\nexit 0\n' >"$instant_sleep/sleep"; chmod +x "$instant_sleep/sleep"
hung=$tmp/hung-bin
mkdir -p "$hung"
printf '#!/bin/sh\necho $$ >"%s/hung.pid"\nexec /bin/sleep 600\n' "$tmp" >"$hung/undra"; chmod +x "$hung/undra"
for sh_path in $shells; do
  rm -f "$tmp/hung.pid"
  run_install "$sh_path" "${install_env[@]}" PATH="$hung:$instant_sleep:$run_path"
  [ "$STATUS" = 0 ] || fail "install with a hanging undra on PATH failed under $sh_path ($STATUS)"
  case "$OUT" in *"  $hung/undra (version unknown)"*) ;; *) fail "under $sh_path a hanging undra is not 'version unknown'" ;; esac
  case "$OUT" in *Terminated* | *Killed*) fail "under $sh_path the guard's kill was reported to the user: $OUT" ;; esac
  [ -f "$tmp/hung.pid" ] || fail "the hanging undra did not run"
  gone "$(cat "$tmp/hung.pid")" || { kill "$(cat "$tmp/hung.pid")" 2>/dev/null; fail "under $sh_path the hanging undra was left running"; }
done
pass "the --version guard: its sleep is killed once the version is in; a hanging undra is 'version unknown', killed, and no 'Terminated' is printed (${shells# })"

echo "curl installer: all checks passed"
