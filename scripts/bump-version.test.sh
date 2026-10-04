#!/usr/bin/env bash
# Tests scripts/bump-version.sh on a copy of this repository's tracked files with a throwaway version:
# every file it owns moves (the workspace, Cargo.lock, the three npm packages and their locks, every
# "@undra/runtime" range), --check agrees, a second run has nothing to do, the fixtures of older
# releases are left, and a version that is not semver writes nothing. Needs git and cargo (Cargo.lock
# is refreshed offline, so the workspace's dependencies must be in Cargo's cache, as after a build).
#
#   bash scripts/bump-version.test.sh
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/bump-version-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
copy="$tmp/repo"
mkdir -p "$copy"

fail() {
  printf 'bump-version.test.sh: FAIL: %s\n' "$*" >&2
  exit 1
}
pass() { printf 'ok - %s\n' "$*"; }

# The tracked files as they are in the working tree (uncommitted edits included).
(cd "$repo" && git ls-files -z | while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done) |
  (cd "$repo" && tar --null -T - -cf -) | tar -xf - -C "$copy"
git -C "$copy" init -q
git -C "$copy" -c core.autocrlf=false add -A
git -C "$copy" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -qm base

old=$(awk '/^\[/ { s = $0 } s == "[workspace.package]" && /^version = "/ { gsub(/^version = "|"$/, ""); print; exit }' "$copy/Cargo.toml")
new=9.8.7-test.1
bump() { (cd "$copy" && bash scripts/bump-version.sh "$@"); }

bump --check >/dev/null || fail "--check fails on the tree as it is"
pass "--check agrees with the tree ($old)"

if out=$(bump 1.0 2>&1); then fail "a two-part version was accepted"; fi
case "$out" in *"not a semantic version"*) ;; *) fail "the refusal does not say why: $out" ;; esac
[ -z "$(git -C "$copy" status --porcelain)" ] || fail "a refused version wrote something"
pass "a version that is not semver is refused and writes nothing"

out=$(bump "$new") || fail "bump $new failed: $out"
for f in Cargo.toml Cargo.lock \
  runtimes/ts/@undra/runtime/package.json runtimes/ts/@undra/testkit/package.json \
  runtimes/rn/@undra/react-native/package.json; do
  case "$out" in *"  $f"*) ;; *) fail "$f is not in the list of changed files:\n$out" ;; esac
done
pass "it lists what it changed"

grep -q "^version = \"$new\"" "$copy/Cargo.toml" || fail "Cargo.toml"
awk -v v="$new" '/^name = "undra-cli"$/ { getline; if ($0 == "version = \"" v "\"") found = 1 } END { exit !found }' "$copy/Cargo.lock" ||
  fail "Cargo.lock does not record undra-cli at $new"
for dir in runtimes/ts/@undra/runtime runtimes/ts/@undra/testkit runtimes/rn/@undra/react-native; do
  grep -q "^  \"version\": \"$new\"" "$copy/$dir/package.json" || fail "$dir/package.json"
  if [ -f "$copy/$dir/package-lock.json" ]; then
    [ "$(grep -c "\"version\": \"$new\"" "$copy/$dir/package-lock.json")" -ge 2 ] || fail "$dir/package-lock.json"
  fi
done
for dir in runtimes/ts/@undra/testkit runtimes/rn/@undra/react-native; do
  grep -q "\"@undra/runtime\": \"^$new\"" "$copy/$dir/package.json" || fail "$dir/package.json's peer range"
done
grep -qx "export const RUNTIME_VERSION = \"$new\";" "$copy/runtimes/ts/@undra/runtime/src/version.ts" ||
  fail "runtimes/ts/@undra/runtime/src/version.ts does not say $new"
grep -qx "internal const val UNDRA_RUNTIME_VERSION: String = \"$new\"" \
  "$copy/runtimes/kotlin/undra-runtime/runtime/src/main/kotlin/dev/undra/runtime/UndraLog.kt" ||
  fail "the Kotlin runtime's UNDRA_RUNTIME_VERSION does not say $new"
# Every tracked manifest, so one that names the range but is missing from the script's list fails here.
stale=$(cd "$copy" && git ls-files -- '*package.json' '*package-lock.json' | grep -v '/tests/fixtures/' |
  xargs grep -l "\"@undra/runtime\": \"^$old\"" || true)
[ -z "$stale" ] || fail "still naming ^$old: $stale"
pass "the workspace, Cargo.lock, the three packages, their locks, every runtime range and the runtimes' Hello versions say $new"

# The copy has no tags, so $old was never released: the migration notes filed under it now arrive with $new.
if grep -q "^    version: \"$old\",$" "$repo/crates/undra-cli/src/migrations.rs"; then
  grep -q "^    version: \"$new\",$" "$copy/crates/undra-cli/src/migrations.rs" ||
    fail "the migration notes filed under the unreleased $old did not move to $new"
  pass "the notes filed under the unreleased $old arrive with $new"
fi

fixtures=$(git -C "$copy" status --porcelain -- crates/undra-cli/tests/fixtures)
[ -z "$fixtures" ] || fail "the fixtures of older releases changed: $fixtures"
pass "the fixtures of older releases are left"

bump --check "$new" >/dev/null || fail "--check $new fails after the bump"
# One file left behind (the runtime's Hello version, which no manifest shows) and --check names it.
cp "$copy/runtimes/ts/@undra/runtime/src/version.ts" "$tmp/version.ts"
sed -i.bak "s/\"$new\"/\"$old\"/" "$copy/runtimes/ts/@undra/runtime/src/version.ts"
rm -f "$copy/runtimes/ts/@undra/runtime/src/version.ts.bak"
if out=$(bump --check "$new" 2>&1); then fail "--check passed with version.ts left at $old"; fi
case "$out" in *"runtimes/ts/@undra/runtime/src/version.ts"*) ;; *) fail "--check does not name the file it found: $out" ;; esac
cp "$tmp/version.ts" "$copy/runtimes/ts/@undra/runtime/src/version.ts"
pass "--check names a file that was left behind"
if bump --check "$old" >/dev/null 2>&1; then fail "--check $old passes after the bump"; fi
out=$(bump "$new")
case "$out" in "nothing to change"*) ;; *) fail "a second run changed something: $out" ;; esac
pass "--check agrees, and a second run has nothing to do"

# A released version keeps its notes: tagged v$new, the next bump leaves them where they are.
git -C "$copy" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -qam "$new"
git -C "$copy" tag "v$new"
bump 9.8.8 >/dev/null || fail "bump 9.8.8 failed"
if grep -q "^    version: \"$old\",$" "$repo/crates/undra-cli/src/migrations.rs"; then
  grep -q "^    version: \"$new\",$" "$copy/crates/undra-cli/src/migrations.rs" ||
    fail "the notes of the released $new moved"
fi
pass "the notes of a released version stay where they are"

# Back to $new on a slow runner, planted: a copy of the script that pauses for a second between
# truncating the React Native host's package.json and writing it. Nothing may read a file the script is
# rewriting (the list of files used to be made while the files were written, and found this one empty).
slow="$copy/scripts/bump-version.slow.sh"
awk -v line="      printf '%s' \"\${new%x}\" >\"\$file\"" '
  $0 == line { print "      { case $file in runtimes/rn/*/package.json) sleep 1 ;; esac; printf '\''%s'\'' \"${new%x}\"; } >\"$file\""; n++; next }
  { print }
  END { exit n != 1 }' "$copy/scripts/bump-version.sh" >"$slow" || fail "the write the slow copy pauses is not in bump-version.sh"
(cd "$copy" && bash scripts/bump-version.slow.sh "$new" >/dev/null) || fail "bump back to $new failed"
rm "$slow"
for dir in runtimes/ts/@undra/testkit runtimes/rn/@undra/react-native; do
  grep -q "\"@undra/runtime\": \"^$new\"" "$copy/$dir/package.json" || fail "$dir/package.json's peer range was left behind by a slow write"
done
bump --check "$new" >/dev/null || fail "--check $new fails after a slow bump"
pass "a pause in the middle of a write leaves nothing behind"

# Outside a git checkout (an unpacked source archive) the script visits the same files: its list is fixed.
rm -rf "$copy/.git"
bump --check "$new" >/dev/null || fail "--check without git"
pass "it works without git"

echo "bump-version.sh: all checks passed"
