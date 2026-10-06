#!/bin/sh
# The dSYM of the app holds the DWARF of the Rust core: a debugger shows `hello_core::greeting` at lib.rs:<line>, not an address.
# The core's slice carries a debug map (it names the objects the DWARF is in), and `keep_debug_objects` of //:mobile is what keeps
# those objects until the app's dSYM is made. Run it with the flags a debugging build has (see ios/BUILD.bazel).
set -eu
dwarf=""
for file in "$@"; do # the dSYM's files; the DWARF is the one below Contents/Resources/DWARF
  case "$file" in */Contents/Resources/DWARF/*) dwarf="$file" ;; esac
done
[ -n "$dwarf" ] && [ -f "$dwarf" ] || { echo "ios symbols: no dSYM DWARF file '$dwarf': build with -c dbg --apple_generate_dsym" >&2; exit 1; }
lines="$(dwarfdump --debug-line "$dwarf" 2>/dev/null | grep -c 'name: "lib.rs"' || true)"
if [ "$lines" -eq 0 ]; then
  echo "ios symbols: FAIL the dSYM has no line table for lib.rs (the core's objects were gone when it was made)" >&2
  exit 1
fi
if ! dwarfdump --name 'greeting' "$dwarf" | grep -q 'DW_AT_decl_file.*lib.rs'; then
  echo "ios symbols: FAIL the dSYM does not name the Rust function greeting at lib.rs" >&2
  exit 1
fi
echo "ios symbols: ok the dSYM resolves hello_core::greeting to lib.rs"
