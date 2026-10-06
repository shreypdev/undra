#!/bin/bash
# Runs the update target of an `undra_bindings_test` against a scratch workspace and checks what it wrote.
#
#   expect_update.sh <update script> <the workspace-relative directory it writes> <the directory it must be equal to>
set -eu
script="$1"
destination="$2"
expected="$3"
workspace="$TEST_TMPDIR/workspace"
mkdir -p "$workspace/$destination"
printf 'left over\n' > "$workspace/$destination/stale.txt"
BUILD_WORKSPACE_DIRECTORY="$workspace" "$script"
# Equal as a whole: the generated files are there, dotfiles included, and what was there before is gone.
diff -r "$expected" "$workspace/$destination"
echo "the update target wrote $destination"
