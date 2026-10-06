#!/bin/sh
# ADR-061: the generated Kotlin does not break a repository that lints everything.
#
# ktlint (the version rules_kotlin pins) runs over the Kotlin tree `undra_bindings` generated, five ways: as generated, and with
# each of the two exclusions `undra bindgen` writes taken away in turn, and with both. The first must be clean, the fourth must not
# be (otherwise the test would pass whatever the exclusions do), and each exclusion alone must be enough. The fifth takes both
# away and puts the root `.editorconfig` section of the Bazel guide in the repository's own file, which must be enough too. Usage:
#
#   lint_test.sh <ktlint> <the generated Kotlin tree>
set -eu
KTLINT="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
TREE="$(cd "$2" && pwd)"
WORK="${TEST_TMPDIR:-/tmp}/undra-lint-$$"
mkdir -p "$WORK"

# The repository's own configuration: it lints every Kotlin file with the default rules, like a repository that "lints everything".
repository() { # <name> -> a directory holding the tree and that configuration
  mkdir -p "$WORK/$1"
  printf 'root = true\n\n[*.{kt,kts}]\nindent_size = 4\n' > "$WORK/$1/.editorconfig"
  mkdir -p "$WORK/$1/generated"
  cp -RL "$TREE/." "$WORK/$1/generated/"
  chmod -R u+w "$WORK/$1"
}

# 0 when ktlint finds nothing in the tree, 1 when it reports something.
lint() { # <name>
  if (cd "$WORK/$1" && "$KTLINT" --relative 'generated/src/main/kotlin/**/*.kt' > "$WORK/$1.out" 2>&1); then
    echo "kotlin lint: $1: clean"
    return 0
  fi
  echo "kotlin lint: $1: $(grep -c '\.kt:' "$WORK/$1.out") findings, e.g. $(grep -m1 '\.kt:' "$WORK/$1.out")"
  return 1
}

drop_suppress() { find "$WORK/$1/generated" -name '*.kt' -exec sed -i.bak '/^@file:Suppress/d' {} + && find "$WORK/$1" -name '*.bak' -delete; }
drop_editorconfig() { rm -f "$WORK/$1/generated/.editorconfig"; }

failed=0
expect() { # <clean|findings> <name>
  if lint "$2"; then got=clean; else got=findings; fi
  if [ "$got" != "$1" ]; then echo "kotlin lint: FAIL $2 should have $1, it has $got"; failed=1; fi
}

repository as-generated
expect clean as-generated

repository editorconfig-only
drop_suppress editorconfig-only
expect clean editorconfig-only

repository suppress-only
drop_editorconfig suppress-only
expect clean suppress-only

repository neither
drop_suppress neither
drop_editorconfig neither
expect findings neither

# `[bindings] lint_exclusions = "none"`: no file beside the tree, the section the guide gives in the repository's own
# .editorconfig instead. (An EditorConfig section with a slash is anchored to its file's directory, so `[**/generated/**]`
# misses a `generated/` right beside it: ktlint 1.8 reports everything with it.)
repository root-section
drop_suppress root-section
drop_editorconfig root-section
printf '\n[generated/**]\nktlint_standard = disabled\nktlint_experimental = disabled\n' >> "$WORK/root-section/.editorconfig"
expect clean root-section

echo "kotlin lint: $([ "$failed" = 0 ] && echo passed || echo FAILED)"
exit "$failed"
