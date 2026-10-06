#!/bin/sh
# The Android standard library is pinned in MODULE.bazel (`undra.android_std(version = ..)`) and rustc rejects one built by
# another release with E0514. run.sh compares the two before it builds, and the message names the tag. Here run.sh gets a
# fake rustc and a version file, and stops right after the check ("no app_files") when they agree.
set -u

runner="$TEST_SRCDIR/$TEST_WORKSPACE/undra/private/run.sh"
[ -f "$runner" ] || runner="$(find "$TEST_SRCDIR" -path '*undra/private/run.sh' | head -n 1)"
[ -f "$runner" ] || { echo "FAIL: run.sh not found below $TEST_SRCDIR" >&2; exit 1; }

dir="${TEST_TMPDIR:-$(mktemp -d)}"
cd "$dir" || exit 1
cat > rustc <<'RUSTC'
#!/bin/sh
echo "rustc 1.99.0 (0123456789 2026-07-01)"
RUSTC
cat > cargo <<'CARGO'
#!/bin/sh
exit 0
CARGO
chmod +x rustc cargo

params() { # <version file>
  cat <<PARAMS
mode=build
rustc=rustc
cargo=cargo
platform=android
stage_key=undra-std-version-test-$$
std_version=$1
PARAMS
}

printf '1.98.0\n' > std-wrong
printf '1.99.0\n' > std-right

params std-wrong > wrong.params
out="$(sh "$runner" wrong.params 2>&1)"
status=$?
[ "$status" -ne 0 ] || { echo "FAIL: a standard library of another release was accepted: $out" >&2; exit 1; }
for want in 'undra.android_std(version = "1.98.0")' 'MODULE.bazel' '1.99.0'; do
  case "$out" in
    *"$want"*) ;;
    *) echo "FAIL: the message lacks '$want': $out" >&2; exit 1 ;;
  esac
done

params std-right > right.params
out="$(sh "$runner" right.params 2>&1)"
status=$?
[ "$status" -ne 0 ] || { echo "FAIL: expected to stop at the missing app_files: $out" >&2; exit 1; }
case "$out" in
  *"no app_files"*) ;;
  *) echo "FAIL: the matching release did not get past the version check: $out" >&2; exit 1 ;;
esac
echo PASS
