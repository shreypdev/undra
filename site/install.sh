#!/bin/sh
# Undra installer.
#
#   curl -fsSL https://shreypdev.github.io/undra/install.sh | sh
#
# Downloads the `undra` release binary for this machine from GitHub Releases, checks its sha256
# against the release's checksums.txt, and installs it to $UNDRA_HOME/bin (default ~/.undra/bin).
# It never uses sudo, never evaluates downloaded text and stops before installing anything it
# could not verify. The checksum shows the download is whole and is the file the release lists;
# who built the release is the trust you already place in github.com/shreypdev/undra over TLS.
# Read it before you run it.
#
# It prints the line that puts $UNDRA_HOME/bin on your PATH and writes no startup file of yours,
# unless UNDRA_MODIFY_PATH=1 asks it to add that one line (see --help).
#
# The whole script is one function that runs on the last line, so a download that is cut off
# part-way runs nothing.
set -eu

main() {
  repo="shreypdev/undra"
  # Where releases live: <base>/v<version>/undra-v<version>-<target>.tar.gz and .../checksums.txt.
  # UNDRA_INSTALL_BASE_URL replaces it (CI points it at a local web server). Plain http is spoken
  # only when that URL is itself http://; an https mirror keeps every transfer, the release lookup
  # included, on TLS.
  base_url=${UNDRA_INSTALL_BASE_URL:-https://github.com/$repo/releases/download}
  api_url=${UNDRA_API_URL:-https://api.github.com/repos/$repo/releases/latest}
  allow_http=0
  case "$base_url" in
    http://*) allow_http=1 ;;
  esac

  while [ $# -gt 0 ]; do
    case "$1" in
      -h | --help) usage; return 0 ;;
      *) usage >&2; die "unknown argument: $1" ;;
    esac
  done
  # Checked before anything is downloaded, so a typo does not cost an install.
  modify_path=${UNDRA_MODIFY_PATH:-0}
  case "$modify_path" in
    0 | 1) ;;
    *) die "UNDRA_MODIFY_PATH must be 1 (add the PATH line to your shell's startup file) or 0, got: $modify_path" ;;
  esac

  need curl
  need tar
  need uname
  need mktemp
  need awk
  need grep
  need sed
  need tr
  if command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | awk '{ print $1 }'; }
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | awk '{ print $1 }'; }
  else
    die "neither shasum nor sha256sum is installed, so the download cannot be verified; install one of them and run this again"
  fi

  home=${UNDRA_HOME:-}
  if [ -z "$home" ]; then
    [ -n "${HOME:-}" ] || die "HOME is not set; set UNDRA_HOME to the directory to install into"
    home=$HOME/.undra
  fi
  case "$home" in
    /*) ;;
    *) die "UNDRA_HOME must be an absolute path, got: $home" ;;
  esac
  bin_dir=$home/bin

  target=$(detect_target)

  tmp=$(mktemp -d "${TMPDIR:-/tmp}/undra-install.XXXXXX")
  staged=""
  trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT
  trap 'exit 1' HUP INT TERM

  if [ -n "${UNDRA_VERSION:-}" ]; then
    version=${UNDRA_VERSION#v}
  else
    say "Looking up the latest release..."
    version=$(latest_version)
  fi
  valid_version "$version" || die "not a release version: $version (expected something like 1.0.0)"

  asset="undra-v$version-$target.tar.gz"
  release_url="$base_url/v$version"

  say "Installing undra v$version for $target"
  fetch "$release_url/$asset" "$tmp/$asset" \
    || die "could not download $release_url/$asset (does release v$version exist, with a build for $target?)"
  fetch "$release_url/checksums.txt" "$tmp/checksums.txt" \
    || die "could not download $release_url/checksums.txt"

  expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/checksums.txt")
  [ -n "$expected" ] || die "checksums.txt has no entry for $asset; refusing to install an unverified file"
  [ "$(printf '%s\n' "$expected" | grep -c .)" = 1 ] || die "checksums.txt has more than one entry for $asset"
  expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
  case "$expected" in
    *[!0-9a-f]*) die "the checksum for $asset is not a sha256: $expected" ;;
  esac
  [ "${#expected}" = 64 ] || die "the checksum for $asset is not a sha256: $expected"
  actual=$(sha256_of "$tmp/$asset")
  if [ "$actual" != "$expected" ]; then
    die "checksum mismatch for $asset: expected $expected, got ${actual:-nothing}; nothing was installed"
  fi
  say "Checksum verified (sha256 $expected)"

  mkdir "$tmp/extract"
  tar -xzf "$tmp/$asset" -C "$tmp/extract" undra || die "$asset does not contain an undra executable"
  [ -f "$tmp/extract/undra" ] && [ ! -L "$tmp/extract/undra" ] || die "$asset does not contain an undra executable"

  mkdir -p "$bin_dir" || die "cannot create $bin_dir"
  # Copy into a fresh file next to the destination (mktemp makes it: a new regular file of this
  # process, never a name something else put there), then rename: the swap is atomic and a running
  # undra is not overwritten in place.
  staged=$(mktemp "$bin_dir/.undra.XXXXXX") || die "cannot write to $bin_dir"
  cp "$tmp/extract/undra" "$staged" || die "cannot write to $bin_dir"
  chmod 755 "$staged"
  mv -f "$staged" "$bin_dir/undra" || die "cannot install $bin_dir/undra"
  staged=""

  say "Installed $bin_dir/undra"
  if shown=$("$bin_dir/undra" --version 2>&1); then
    say "$shown"
  else
    die "$bin_dir/undra was installed but did not start: $shown"
  fi
  shadow_note
  path_hint
  say "Uninstall: rm $bin_dir/undra"
}

usage() {
  cat <<'USAGE'
Undra installer

Usage:
  curl -fsSL https://shreypdev.github.io/undra/install.sh | sh
  curl -fsSL https://shreypdev.github.io/undra/install.sh | sh -s -- --help

Downloads the undra binary for this machine from GitHub Releases, verifies its sha256 against
the release's checksums.txt and installs it to $UNDRA_HOME/bin. No sudo, nothing is installed
unless the checksum matches.

Environment:
  UNDRA_VERSION            version to install, for example 1.0.0 or v1.0.0 (default: the latest release)
  UNDRA_HOME               install root (default: ~/.undra); the binary goes to $UNDRA_HOME/bin
  UNDRA_MODIFY_PATH        1: add the line that puts $UNDRA_HOME/bin on PATH to your shell's startup
                           file (~/.zshrc; ~/.bashrc, or ~/.bash_profile on macOS; fish's
                           conf.d/undra.fish), once, on a line of its own (default: 0, print it only)
  UNDRA_INSTALL_BASE_URL   where releases are served, laid out as GitHub does:
                           <url>/v<version>/undra-v<version>-<target>.tar.gz and checksums.txt
                           (default: https://github.com/shreypdev/undra/releases/download)
  UNDRA_API_URL            where the latest release is looked up (default: GitHub's API)

Supported: macOS and Linux (glibc), on x86_64 and arm64. Not yet: Windows, Alpine (musl);
`cargo install --git https://github.com/shreypdev/undra undra-cli` builds from source.
USAGE
}

say() {
  printf '%s\n' "$*"
}

die() {
  printf 'undra-install: error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required and was not found"
}

# curl with the transport pinned: https only, TLS 1.2 or newer, also across redirects.
fetch() {
  if [ "$allow_http" = 1 ]; then
    curl -fsSL --retry 3 --connect-timeout 20 --max-time 600 --proto '=http,https' --proto-redir '=http,https' -o "$2" "$1"
  else
    curl -fsSL --retry 3 --connect-timeout 20 --max-time 600 --proto '=https' --proto-redir '=https' --tlsv1.2 -o "$2" "$1"
  fi
}

# A release version: MAJOR.MINOR.PATCH with an optional -prerelease. It becomes part of a URL
# and a file name, so nothing else gets through.
valid_version() {
  case "$1" in
    *[!0-9A-Za-z.-]*) return 1 ;; # grep matches line by line, so reject odd characters first
  esac
  printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'
}

latest_version() {
  fetch "$api_url" "$tmp/latest.json" \
    || die "could not look up the latest release at $api_url (GitHub rate-limits anonymous requests); set UNDRA_VERSION=<x.y.z> to skip the lookup"
  # tag_name is on its own line in GitHub's JSON; splitting on , and { also copes with minified
  # JSON.
  tag=$(tr ',{' '\n\n' <"$tmp/latest.json" | sed -n 's/^[[:space:]]*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*$/\1/p' | head -n 1)
  [ -n "$tag" ] || die "the reply from $api_url has no tag_name; set UNDRA_VERSION=<x.y.z>"
  printf '%s\n' "${tag#v}"
}

detect_target() {
  platform=$(uname -sm)
  os=${platform%% *}
  arch=${platform#* }
  case "$os" in
    Darwin)
      # A shell running under Rosetta reports x86_64 on Apple silicon; the native build is faster.
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
        arch=arm64
      fi
      case "$arch" in
        arm64 | aarch64) printf 'aarch64-apple-darwin\n' ;;
        x86_64 | amd64) printf 'x86_64-apple-darwin\n' ;;
        *) die "no prebuilt undra for $platform; build from source: cargo install --git https://github.com/$repo undra-cli" ;;
      esac
      ;;
    Linux)
      # The Linux builds link glibc. musl's ldd prints "musl libc" (on stderr, exit status 1).
      if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
        die "the prebuilt undra needs glibc and this system uses musl (Alpine); build from source: cargo install --git https://github.com/$repo undra-cli"
      fi
      case "$arch" in
        x86_64 | amd64) printf 'x86_64-unknown-linux-gnu\n' ;;
        aarch64 | arm64) printf 'aarch64-unknown-linux-gnu\n' ;;
        *) die "no prebuilt undra for $platform; build from source: cargo install --git https://github.com/$repo undra-cli" ;;
      esac
      ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
      die "Windows is not supported yet ($platform); use WSL, or build from source: cargo install --git https://github.com/$repo undra-cli"
      ;;
    *)
      die "no prebuilt undra for $platform; build from source: cargo install --git https://github.com/$repo undra-cli"
      ;;
  esac
}

# Names an `undra` that a shell finds before the one just installed: it would keep answering
# `undra --version` with its own version, and nothing else would say why.
shadow_note() {
  other=$(command -v undra 2>/dev/null) || return 0
  case "$other" in
    /*) ;;
    *) return 0 ;; # not a file (an alias or a function of this shell)
  esac
  [ -f "$other" ] || return 0
  # A symlink to the installed file (or a directory symlinked to $bin_dir) is the same undra.
  [ "$(real_path "$other")" != "$(real_path "$bin_dir/undra")" ] || return 0
  other_dir=${other%/*}
  say ""
  say "Note: \`undra\` in a shell runs another undra on your PATH, not the one just installed:"
  say ""
  say "  $other ($(version_of "$other"))"
  say ""
  say "Remove it:"
  say ""
  case "$other" in
    */.cargo/bin/undra | "${CARGO_HOME:-/nonexistent}/bin/undra") say "  cargo uninstall undra-cli" ;;
    *) say "  rm $other" ;;
  esac
  say ""
  case ":${PATH:-}:" in
    *":$bin_dir:"*) say "or put $bin_dir before $other_dir on your PATH." ;;
    *) say "or put $bin_dir before $other_dir on your PATH, as the PATH line below does." ;;
  esac
}

# The first line that `<file> --version` prints, or "version unknown" when it fails, prints
# nothing or has not finished within 10 seconds (an old build that waits must not hang the
# install). Its stdin is /dev/null: under `curl | sh` the shell's stdin is this script.
version_of() {
  "$1" --version </dev/null >"$tmp/other-version" 2>/dev/null &
  version_pid=$!
  # The guard's output goes nowhere, so a caller reading this script's output does not wait for it.
  (sleep 10; kill "$version_pid" 2>/dev/null) </dev/null >/dev/null 2>&1 &
  guard_pid=$!
  version_line=""
  if wait "$version_pid"; then
    version_line=$(head -n 1 "$tmp/other-version")
  fi
  kill "$guard_pid" 2>/dev/null || true
  printf '%s\n' "${version_line:-version unknown}"
}

# <absolute path> with every symlink on the way resolved (POSIX has no realpath).
real_path() {
  resolved=$1
  hops=0
  while [ -L "$resolved" ] && [ "$hops" -lt 40 ]; do
    link=$(readlink "$resolved") || break
    case "$link" in
      /*) resolved=$link ;;
      *) resolved=${resolved%/*}/$link ;;
    esac
    hops=$((hops + 1))
  done
  parent=${resolved%/*}
  if parent=$(cd -P "${parent:-/}" 2>/dev/null && pwd -P); then
    printf '%s/%s\n' "${parent%/}" "${resolved##*/}"
  else
    printf '%s\n' "$resolved"
  fi
}

# Says what to add to PATH, in the user's shell's syntax, unless it is already there. Nothing is
# written to any startup file unless UNDRA_MODIFY_PATH=1 asks for it (add_path_line).
path_hint() {
  case ":${PATH:-}:" in
    *":$bin_dir:"*)
      [ "$modify_path" = 0 ] || say "$bin_dir is already on your PATH; no startup file was changed."
      return 0
      ;;
  esac
  shown=$bin_dir
  # shellcheck disable=SC2016 # the literal text $HOME is what the user's shell should see
  [ "$bin_dir" != "${HOME:-}/.undra/bin" ] || shown='$HOME/.undra/bin'
  export_line="export PATH=\"$shown:\$PATH\""
  shell_name=${SHELL:-}
  shell_name=${shell_name##*/}
  # The startup file, relative to the home directory.
  case "$shell_name" in
    zsh) rc=.zshrc ;;
    bash)
      rc=.bashrc
      [ "$(uname -s)" != Darwin ] || rc=.bash_profile
      ;;
    fish) rc=.config/fish/conf.d/undra.fish ;;
    *) rc="" ;;
  esac
  if [ "$modify_path" = 1 ]; then
    add_path_line
    return 0
  fi
  say ""
  say "$bin_dir is not on your PATH yet. Add it:"
  say ""
  case "$shell_name" in
    fish)
      say "  fish_add_path \"$shown\""
      ;;
    zsh | bash)
      # printf, not echo: the leading \n ends a last line that has no newline of its own, which
      # the export would otherwise join (and the shell would then fail to read the file).
      say "  printf '\\nexport PATH=\"$(printf_text "$shown"):\$PATH\"\\n' >> ~/$rc && exec $shell_name"
      ;;
    *)
      say "  $export_line    # and put that line in your shell's startup file"
      ;;
  esac
  say ""
}

# <text> as it must appear inside printf's single-quoted format: % and \ doubled, ' closed and
# reopened around an escaped one.
printf_text() {
  printf '%s' "$1" | sed -e 's/[\\%]/&&/g' -e "s/'/'\\\\''/g"
}

# UNDRA_MODIFY_PATH=1: appends the PATH line to the shell's startup file, on a line of its own,
# unless a line there already puts $bin_dir on PATH. Called by path_hint, which sets rc.
add_path_line() {
  if [ -z "$rc" ] || [ -z "${HOME:-}" ]; then
    say ""
    say "UNDRA_MODIFY_PATH=1, but this installer knows no startup file for the shell '${shell_name:-unknown}' (\$SHELL), so nothing was written. Put this line in your shell's startup file:"
    say ""
    say "  $export_line"
    say ""
    return 0
  fi
  need tail
  file=$HOME/$rc
  line=$export_line
  [ "$shell_name" != fish ] || line="fish_add_path \"$shown\""
  if [ -f "$file" ] && has_path_line "$file"; then
    say ""
    say "$file already puts $bin_dir on PATH; nothing was written. A new shell reads it: exec $shell_name"
    say ""
    return 0
  fi
  [ "$shell_name" != fish ] || mkdir -p "${file%/*}" || die "undra is installed, but ${file%/*} could not be created; add this line to $file yourself: $line"
  # A file whose last byte is not a newline gets one first, so the line is not joined to its
  # last line.
  newline=""
  if [ -s "$file" ] && [ -n "$(tail -c 1 "$file")" ]; then
    newline='
'
  fi
  printf '%s%s\n' "$newline" "$line" >>"$file" || die "undra is installed, but $file could not be written; add this line to it yourself: $line"
  say ""
  say "Added this line to $file:"
  say ""
  say "  $line"
  say ""
  say "A new shell picks it up: exec $shell_name"
  say ""
}

# Whether a line of <file>, comments aside, already names $bin_dir in a PATH setting or in
# fish_add_path, spelt in full or from the home directory with $HOME, ${HOME} or ~.
has_path_line() {
  with_var=""
  with_braces=""
  with_tilde=""
  case "$bin_dir" in
    "$HOME"/*)
      from_home=${bin_dir#"$HOME"/}
      # shellcheck disable=SC2016,SC2088 # the literal text $HOME, ${HOME} or ~ is what a startup file holds
      with_var='$HOME/'$from_home with_braces='${HOME}/'$from_home with_tilde='~/'$from_home
      ;;
  esac
  UNDRA_N1=$bin_dir UNDRA_N2=$with_var UNDRA_N3=$with_braces UNDRA_N4=$with_tilde awk '
    /^[[:space:]]*#/ { next }
    /PATH|fish_add_path/ {
      for (k = 1; k <= 4; k++) {
        name = ENVIRON["UNDRA_N" k]
        if (name != "" && index($0, name)) found = 1
      }
    }
    END { exit !found }' "$1"
}

main "$@"
