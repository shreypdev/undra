#!/bin/bash
# Checks that a directory an `undra_bindings_test` lays out holds copies of the generated files, not links back into the
# trees it was laid out from: a link would point into the output base of the machine that built it, which a remote or disk
# cache hands to other machines.
#
#   expect_copies.sh <the laid-out directory> <its target name>
set -eu
tree="$1"
name="$2"
count=0
while IFS= read -r -d '' file; do
  count=$((count + 1))
  resolved="$(cd "$(dirname "$file")" && pwd -P)/$(basename "$file")"
  while [ -L "$resolved" ]; do
    target="$(readlink "$resolved")"
    case "$target" in
      /*) resolved="$target" ;;
      *) resolved="$(dirname "$resolved")/$target" ;;
    esac
  done
  case "$resolved" in
    */"$name"/*) ;;
    *)
      echo "$file is a link to $resolved, outside $name: the layout must copy the files" >&2
      exit 1
      ;;
  esac
done < <(find -L "$tree" -type f -print0)
[ "$count" -gt 0 ] || { echo "$tree has no files" >&2; exit 1; }
echo "$count files, each a copy in $name"
