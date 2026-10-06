#!/bin/bash
# Runs a test script that is expected to fail (the diff test `undra_bindings_test` makes) and checks what it said.
#
#   expect_failure.sh <test script> <text its output must contain>...
set -u
script="$1"
shift
# The script writes a JUnit file for Bazel to XML_OUTPUT_FILE; this test has its own.
output="$(XML_OUTPUT_FILE="$TEST_TMPDIR/inner.xml" "$script" 2>&1)"
status=$?
if [ "$status" -eq 0 ]; then
  echo "expected $script to fail, and it passed" >&2
  exit 1
fi
for expected in "$@"; do
  case "$output" in
    *"$expected"*) ;;
    *)
      printf 'the output of %s does not say "%s":\n%s\n' "$script" "$expected" "$output" >&2
      exit 1
      ;;
  esac
done
echo "failed as expected:"
echo "$output"
