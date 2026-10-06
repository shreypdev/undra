#!/usr/bin/env bash
# at-minimums.sh puts MODULE.bazel back whatever happens during the command: a `bazel` on PATH stands in for the real one and
# checks what it is given, deletes the script's backup copy, and fails or passes as asked; afterwards MODULE.bazel must be the
# file it was, byte for byte, and the backup gone. A backup a killed run left behind is put back before anything else.
#
#   bash examples/bazel/scripts/at-minimums.test.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
example=$(cd "$here/.." && pwd)
module="$example/MODULE.bazel"
backup="$module.at-minimums~"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cp "$module" "$scratch/original"
[ ! -e "$backup" ] || { echo "FAIL: $backup exists before the test" >&2; exit 1; }
pass() { echo "ok - $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# The stand-in: records the rewritten MODULE.bazel, removes the backup, then exits as FAKE_BAZEL_STATUS says.
mkdir -p "$scratch/bin"
cat > "$scratch/bin/bazel" <<EOF
#!/bin/sh
cp "$module" "$scratch/seen"
printf '%s\n' "\$@" > "$scratch/args"
rm -f "$backup"
exit "\${FAKE_BAZEL_STATUS:-0}"
EOF
chmod +x "$scratch/bin/bazel"

# 1. The command runs at the minimums, the backup is deleted under the script, and MODULE.bazel comes back intact.
status=0
PATH="$scratch/bin:$PATH" FAKE_BAZEL_STATUS=0 bash "$here/at-minimums.sh" test //... --nothing >"$scratch/out" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "the script did not pass the command's success through (exit $status): $(cat "$scratch/out")"
grep -q 'bazel_dep(name = "rules_rust", version = "0.70.0")' "$scratch/seen" || fail "bazel did not see rules_rust at the rules' minimum"
! grep -q 'rules_xcodeproj' "$scratch/seen" || fail "bazel still saw the rules_xcodeproj line"
grep -qx -- '--deleted_packages=ios' "$scratch/args" && grep -qx -- '--lockfile_mode=off' "$scratch/args" && grep -qx -- '--check_direct_dependencies=error' "$scratch/args" || fail "the three flags were not all passed: $(cat "$scratch/args")"
cmp -s "$module" "$scratch/original" || fail "MODULE.bazel is not the file it was after the backup was deleted under the script"
[ ! -e "$backup" ] || fail "the backup was left behind"
pass "the module is intact after the backup was deleted during the command"

# 2. A failing command: its status comes through, and the module is intact.
status=0
PATH="$scratch/bin:$PATH" FAKE_BAZEL_STATUS=3 bash "$here/at-minimums.sh" build //:x >"$scratch/out" 2>&1 || status=$?
[ "$status" -eq 3 ] || fail "a failing command's status was not passed through (got $status)"
cmp -s "$module" "$scratch/original" || fail "MODULE.bazel is not the file it was after a failing command"
pass "a failing command's status passes through and the module is intact"

# 3. A backup a killed run left behind (with the module rewritten) is put back before the command runs.
printf 'rewritten by a run that was killed\n' > "$module"
cp "$scratch/original" "$backup"
status=0
PATH="$scratch/bin:$PATH" FAKE_BAZEL_STATUS=0 bash "$here/at-minimums.sh" test //... >"$scratch/out" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "the run after a killed one failed: $(cat "$scratch/out")"
grep -q "previous run was cut short" "$scratch/out" || fail "the leftover backup was not reported: $(cat "$scratch/out")"
grep -q 'bazel_dep(name = "rules_rust", version = "0.70.0")' "$scratch/seen" || fail "bazel did not see the restored module at the minimums"
cmp -s "$module" "$scratch/original" || fail "MODULE.bazel is not the original after a leftover backup"
pass "a leftover backup is put back first"
echo "at-minimums.test.sh: all checks passed"
