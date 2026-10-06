#!/usr/bin/env bash
# Runs one Bazel command in examples/bazel with every ruleset at the lowest version undra_rules declares (bazel/MODULE.bazel), so
# the minimums the Bazel guide states are tested, not assumed. The example's MODULE.bazel pins recent versions (what most
# applications run); this script rewrites each of its bazel_dep lines to the rules' version for the length of the command, then
# puts the file back, whatever the command's result.
#
#   examples/bazel/scripts/at-minimums.sh test //... --repository_cache="$HOME/.cache/bazel-repository"
#   USE_BAZEL_VERSION=9.2.0 examples/bazel/scripts/at-minimums.sh test //...
#
# Three flags are added to the command: --lockfile_mode=off, because the committed lock file is the one the recent versions write
# and this run must neither read nor rewrite it; --check_direct_dependencies=error, so a minimum that something else in the graph
# raises fails the run instead of quietly testing a higher version; and --deleted_packages=ios, because the debugging example
# (//ios, the one user of rules_xcodeproj, whose line this run drops: see below) is not part of what the minimums cover.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <bazel command> [targets and flags]" >&2
  exit 2
fi

example=$(cd "$(dirname "$0")/.." && pwd)
module="$example/MODULE.bazel"
rules="$example/../../bazel/MODULE.bazel"
dep='^bazel_dep\(name = "([^"]+)", version = "([^"]+)"\)$'

backup=$(mktemp)
cp "$module" "$backup"
restore() { cat "$backup" > "$module" && rm -f "$backup"; }
trap restore EXIT
trap 'exit 130' INT TERM

# name version, one per line: what the rules declare.
minimums=$(sed -nE "s/$dep/\1 \2/p" "$rules")

script=""
while read -r name version; do
  case "$name" in
    # The rules themselves, by path: not a ruleset with a minimum.
    undra_rules) continue ;;
    # The debugging example's alone (//ios, left out by --deleted_packages): its own floors for rules_apple and rules_swift
    # (4.4.0 and 3.5.0 for 4.1.0) sit above the rules' minimums, so its line is dropped rather than kept at its version.
    rules_xcodeproj)
      echo "at-minimums: $name dropped (//ios is left out)"
      script+="/^bazel_dep\\(name = \"$name\", version = \"[^\"]+\"\\)\$/d;"
      continue
      ;;
  esac
  minimum=$(awk -v n="$name" '$1 == n { print $2 }' <<<"$minimums")
  if [ -z "$minimum" ]; then
    echo "at-minimums: $name is a bazel_dep of examples/bazel but not of bazel/MODULE.bazel, so it has no minimum to test" >&2
    exit 1
  fi
  echo "at-minimums: $name $version -> $minimum"
  script+="s/^bazel_dep\\(name = \"$name\", version = \"[^\"]+\"\\)\$/bazel_dep(name = \"$name\", version = \"$minimum\")/;"
done < <(sed -nE "s/$dep/\1 \2/p" "$backup")

sed -E "$script" "$backup" > "$module"

cd "$example"
command=$1
shift
bazel "$command" --lockfile_mode=off --check_direct_dependencies=error --deleted_packages=ios "$@"
