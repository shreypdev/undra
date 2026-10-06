#!/usr/bin/env bash
# The native size gate (ADR-052, amendment "native size gates"; constitution R9: budgets are tests).
#
#   scripts/native-size.sh                       measure and gate what this machine can build (Android; iOS on a Mac)
#   scripts/native-size.sh --platform android    the two Android ABIs (any OS with the NDK)
#   scripts/native-size.sh --platform ios        the iOS device slice (macOS with Xcode)
#   scripts/native-size.sh --record              measure, gate against the budgets only, and write the record:
#                                                bench/results/native-size.jsonl and `measured_bytes` of each
#                                                [size."..."] table of bench/budgets.toml that this run measured
#                                                (commit both; the budgets test checks they agree). CI never records.
#
# Three artefacts, each gated by its `[size."<artifact>"]` table of bench/budgets.toml: at most `budget_bytes` (the
# design's number: 1.2 MB per Android ABI, 900 KB added to an iOS app), and at most `tolerance` over `measured_bytes`
# (the record), whichever is lower. The ceiling comes from the committed record, never from the build being measured.
# All of them are the hello-world core: the `undra init` template (a store with a keyed list and two computeds, a record,
# an enum, an error, a function; no query) built by `undra build --release`, which is the `release-mobile` profile of the
# generated shim (opt-level "s" with the call-path crates at 3, fat LTO, one codegen unit, panic = "unwind": ADR-052's
# amendment), exactly as an app does it.
#
# * android/hello-arm64-v8a, android/hello-x86_64: the bytes of `build/android/jniLibs/<abi>/libhello_core.so`, the
#   file Gradle packages: stripped by `undra build` (`llvm-strip --strip-debug --strip-unneeded`; the unstripped twin is
#   the symbol file and is not counted), 16 KB page aligned (the script fails if a LOAD segment is not: Google Play
#   rejects it), and free of the builder's directories (`--remap-path-prefix`).
# * ios/hello-arm64: what the device slice of the XCFramework adds to an app. The `.a` is not that number: it is a
#   prelinked object with its symbol table and debug map, and at `opt-level = "z"` it GROWS (the outlined functions'
#   local symbols) while the code the app links shrinks by a fifth. So the slice is linked the way an app's link
#   keeps it (a dylib with `-force_load` and `-dead_strip`, local symbols stripped with `strip -x`) and the gated
#   `bytes` are the sizes of the sections of its __TEXT, __DATA_CONST and __DATA segments (zero-fill excluded), page
#   padding and the dylib's own headers left out. The `.a` and the slice object's own sections are in the line too
#   (`archive_bytes`, `object_sections_bytes`), ungated.
#
# Output: one JSON line per artefact on stdout and in <dir>/native-size.jsonl, the comparison on stderr.
# Exit status: 0 within the gates, 1 over one (or misaligned, or naming the builder's machine), 2 when it could not
# measure (no NDK, no Xcode, a failed build).
#
# Environment: UNDRA_SIZE_TARGET_DIR (cargo's target directory for the template; default target/native-size/target,
# kept between runs so dependencies build once; the template itself is created afresh and rebuilt on every run, so a
# stale library cannot be measured), UNDRA_BENCH_RESULTS_DIR (where the JSON goes; default target/native-size/).
set -euo pipefail

die() { echo "native-size.sh: $*" >&2; exit 2; }

RECORD=0
PLATFORM=auto
while [ $# -gt 0 ]; do
  case "$1" in
    --record) RECORD=1; shift ;;
    --platform) PLATFORM="${2:?--platform needs android, ios or all}"; shift 2 ;;
    *) die "usage: scripts/native-size.sh [--platform android|ios|all] [--record]" ;;
  esac
done
case "$PLATFORM" in
  auto) if [ "$(uname -s)" = Darwin ]; then PLATFORM=all; else PLATFORM=android; fi ;;
  android|ios|all) ;;
  *) die "--platform is android, ios or all" ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$ROOT/target/native-size"
PROJECT="$WORK/hello"
export CARGO_TARGET_DIR="${UNDRA_SIZE_TARGET_DIR:-$WORK/target}"
if [ "$RECORD" = 1 ]; then
  OUT_DIR="$ROOT/bench/results"
else
  OUT_DIR="${UNDRA_BENCH_RESULTS_DIR:-$WORK}"
fi
mkdir -p "$WORK" "$OUT_DIR"

command -v cargo >/dev/null || die "cargo is not on PATH (source scripts/env.sh)"
command -v python3 >/dev/null || die "python3 is needed to read the ELF headers and the budgets"
WANT_ANDROID=0; WANT_IOS=0
case "$PLATFORM" in android) WANT_ANDROID=1 ;; ios) WANT_IOS=1 ;; all) WANT_ANDROID=1; WANT_IOS=1 ;; esac
if [ "$WANT_IOS" = 1 ] && [ "$(uname -s)" != Darwin ]; then
  if [ "$PLATFORM" = ios ]; then die "the iOS slice is linked with Apple's toolchain: run this on a Mac"; fi
  WANT_IOS=0
fi
if [ "$WANT_ANDROID" = 1 ]; then
  [ -d "${ANDROID_NDK_HOME:-/nonexistent}" ] || [ -d "${ANDROID_HOME:-/nonexistent}/ndk" ] \
    || die "the Android NDK was not found (set ANDROID_NDK_HOME, or ANDROID_HOME so that ndk/<version> is found): the Android core is linked with it"
fi
if [ "$WANT_IOS" = 1 ]; then
  for tool in xcrun size strip; do command -v "$tool" >/dev/null || die "$tool is not on PATH (Xcode's tools; docs/ONBOARDING.md)"; done
fi

# 1. The CLI of this checkout, and a fresh template project that takes Undra from it.
echo "==> building undra-cli" >&2
cargo build -q -p undra-cli --manifest-path "$ROOT/Cargo.toml" --target-dir "$ROOT/target"
UNDRA="$ROOT/target/debug/undra"
rm -rf "$PROJECT"
PLATFORMS=""
[ "$WANT_ANDROID" = 1 ] && PLATFORMS="android"
[ "$WANT_IOS" = 1 ] && PLATFORMS="${PLATFORMS:+$PLATFORMS,}ios"
echo "==> undra init hello --platforms $PLATFORMS" >&2
"$UNDRA" init hello --platforms "$PLATFORMS" --undra-path "$ROOT" --dir "$WORK" >/dev/null

# 2. The builds, as an app developer runs them.
if [ "$WANT_ANDROID" = 1 ]; then
  echo "==> undra build --platform android --release" >&2
  "$UNDRA" build --platform android --release -C "$PROJECT" >"$WORK/build-android.log" 2>&1 \
    || { tail -40 "$WORK/build-android.log" >&2; die "undra build --platform android --release failed (log: $WORK/build-android.log); it needs the NDK r27 (docs/ONBOARDING.md)"; }
fi
if [ "$WANT_IOS" = 1 ]; then
  echo "==> undra build --platform ios --release" >&2
  "$UNDRA" build --platform ios --release -C "$PROJECT" >"$WORK/build-ios.log" 2>&1 \
    || { tail -40 "$WORK/build-ios.log" >&2; die "undra build --platform ios --release failed (log: $WORK/build-ios.log); it needs Xcode (docs/ONBOARDING.md)"; }
fi

# 3. What ships must not name the machine it was built on, nor the directory it was built in (`undra build` remaps the
# home directory, the project, the Undra checkout and Cargo's sources out of every release build: ADR-052), so the size does
# not move with where this checkout lives.
check_unnamed() { # FILE
  for NAMED in "${HOME:-}" "$ROOT" "$PROJECT"; do
    if [ -n "$NAMED" ] && [ "$NAMED" != "/" ] && LC_ALL=C grep -q -a -F -- "$NAMED" "$1"; then
      echo "native-size.sh: $1 contains $NAMED: the release build's --remap-path-prefix is missing" >&2
      exit 1
    fi
  done
}

# 4. The iOS number: the device slice linked the way an app's link keeps it (see the header).
IOS_SLICE=""
IOS_LINKED=""
if [ "$WANT_IOS" = 1 ]; then
  IOS_SLICE="$PROJECT/build/ios/HelloCore.xcframework/ios-arm64/libhello_core.a"
  [ -f "$IOS_SLICE" ] || die "undra build did not write $IOS_SLICE"
  IOS_SDK="$(xcrun --sdk iphoneos --show-sdk-path 2>/dev/null)" || die "the iphoneos SDK is not installed (Xcode, not just the command line tools)"
  IOS_LINKED="$WORK/hello-linked.dylib"
  rm -f "$IOS_LINKED"
  xcrun --sdk iphoneos clang -arch arm64 -isysroot "$IOS_SDK" -miphoneos-version-min=17.0 -dynamiclib \
    -Wl,-force_load,"$IOS_SLICE" -Wl,-dead_strip -o "$IOS_LINKED" 2>"$WORK/link-ios.log" \
    || { tail -20 "$WORK/link-ios.log" >&2; die "linking the iOS slice failed (log: $WORK/link-ios.log)"; }
  strip -x "$IOS_LINKED"
  xcrun size -m "$IOS_LINKED" >"$WORK/ios-linked.size"
  xcrun size -m "$IOS_SLICE" >"$WORK/ios-slice.size"
fi

# 5. Sizes, the gate and the JSON lines.
# "-dirty" when what the library is built from differs from the commit (the record itself, the budgets file and this
# script do not count).
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD)"
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no -- crates runtimes Cargo.toml Cargo.lock)" ] \
  || COMMIT="$COMMIT-dirty"
RUSTC="$(rustc --version | awk '{print $2}')"
NDK=""
if [ "$WANT_ANDROID" = 1 ]; then
  NDK="${ANDROID_NDK_HOME:-}"
  if [ -z "$NDK" ] && [ -d "${ANDROID_HOME:-/nonexistent}/ndk" ]; then
    NDK="$(ls -d "$ANDROID_HOME"/ndk/*/ 2>/dev/null | sort -V | tail -1)"
  fi
  NDK="$(basename "${NDK%/}")"
fi
XCODE=""
if [ "$WANT_IOS" = 1 ]; then XCODE="$(xcodebuild -version 2>/dev/null | tr '\n' ' ' | sed 's/ *$//')"; fi

for LIB in "$PROJECT"/build/android/jniLibs/*/libhello_core.so; do
  [ -f "$LIB" ] && check_unnamed "$LIB"
done

python3 - "$ROOT/bench/budgets.toml" "$PROJECT" "$WORK" "$IOS_SLICE" "$OUT_DIR/native-size.jsonl" \
  "$RECORD" "$COMMIT" "$RUSTC" "$NDK" "$XCODE" "$WANT_ANDROID" "$WANT_IOS" <<'PY'
import datetime, json, math, os, re, struct, sys

(budgets_path, project, work, ios_slice, out, record, commit, rustc, ndk, xcode, want_android, want_ios) = sys.argv[1:13]
record = record == "1"
want_android, want_ios = want_android == "1", want_ios == "1"
today = datetime.date.today().isoformat()
text = open(budgets_path).read()

def size_table(name):
    """The keys of `[size."name"]` (the budgets file's strict subset: `key = number` lines)."""
    m = re.search(r'^\[size\."' + re.escape(name) + r'"\]\s*$(.*?)(?=^\[|\Z)', text, re.M | re.S)
    if not m:
        sys.exit(f'native-size.sh: bench/budgets.toml has no [size."{name}"] table')
    table = {}
    for line in m.group(1).splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            key, value = (part.strip() for part in line.split("=", 1))
            table[key] = float(value.replace("_", ""))
    return table

def gate(name, size):
    """The gate of `name` for a measured size: (ceiling, ok, table)."""
    table = size_table(name)
    budget = table["budget_bytes"]
    recorded = size if record else table.get("measured_bytes")
    tolerance = table.get("tolerance")
    ceiling = budget if recorded is None or tolerance is None else min(budget, math.floor(recorded * (1 + tolerance)))
    return int(ceiling), size <= ceiling, table

def min_load_alignment(path):
    """The smallest p_align of the PT_LOAD segments of a 64-bit little-endian ELF file."""
    data = open(path, "rb").read(4096 * 4)
    if data[:4] != b"\x7fELF" or data[4] != 2:
        sys.exit(f"native-size.sh: {path} is not a 64-bit ELF library")
    phoff, = struct.unpack_from("<Q", data, 32)
    phentsize, phnum = struct.unpack_from("<HH", data, 54)
    aligns = []
    for i in range(phnum):
        p_type, _flags, _off, _va, _pa, _fsz, _msz, p_align = struct.unpack_from("<IIQQQQQQ", data, phoff + i * phentsize)
        if p_type == 1:
            aligns.append(p_align)
    return min(aligns) if aligns else 0

def sections_bytes(size_m_output, segments=None):
    """Sum of the section sizes in `size -m` output, zero-fill sections left out; only those of `segments` when given.

    A linked image prints `Segment __TEXT: n` and then `Section __text: n`; an object or archive member prints
    `Section (__TEXT, __text): n` under a nameless segment."""
    total, current = 0, None
    for line in size_m_output.splitlines():
        seg = re.match(r"Segment (\S+):", line)
        if seg:
            current = seg.group(1)
            continue
        sec = re.match(r"\s+Section (?:\((\S+), \S+\)|\S+): (\d+)( \(zerofill\))?", line)
        if sec and not sec.group(3):
            if segments is None or (sec.group(1) or current) in segments:
                total += int(sec.group(2))
    return total

lines, results = [], []
WHAT_ANDROID = ("undra init template (android), undra build --platform android --release (the release-mobile profile): the "
                "shipped libhello_core.so (stripped, 16 KB aligned, a hello-world core with the standard surface)")
if want_android:
    for abi in ("arm64-v8a", "x86_64"):
        lib = os.path.join(project, "build", "android", "jniLibs", abi, "libhello_core.so")
        if not os.path.isfile(lib):
            sys.exit(f"native-size.sh: undra build did not write {lib}")
        nbytes = os.path.getsize(lib)
        align = min_load_alignment(lib)
        name = f"android/hello-{abi}"
        ceiling, ok, table = gate(name, nbytes)
        if abi == "arm64-v8a" and align < 16384:
            print(f"native-size.sh: {lib} has LOAD segments aligned to {align} bytes, not 16 KB: Google Play rejects it", file=sys.stderr)
            sys.exit(1)
        line = {
            "artifact": name, "bytes": nbytes, "budget": int(table["budget_bytes"]), "ceiling": ceiling,
            "tolerance": table.get("tolerance"), "load_alignment": align,
            "what": WHAT_ANDROID,
            "command": "undra init hello --platforms android,ios --undra-path <checkout>; undra build --platform android --release -C hello (scripts/native-size.sh)",
            "commit": commit, "date": today, "rustc": rustc, "ndk": ndk,
        }
        lines.append(line)
        results.append((line, ok, table))

if want_ios:
    slice_size = open(os.path.join(work, "ios-slice.size")).read()
    linked_size = open(os.path.join(work, "ios-linked.size")).read()
    nbytes = sections_bytes(linked_size, {"__TEXT", "__DATA_CONST", "__DATA"})
    if nbytes <= 0:
        sys.exit("native-size.sh: could not read the section sizes of the linked iOS slice")
    name = "ios/hello-arm64"
    ceiling, ok, table = gate(name, nbytes)
    line = {
        "artifact": name, "bytes": nbytes, "budget": int(table["budget_bytes"]), "ceiling": ceiling,
        "tolerance": table.get("tolerance"),
        "archive_bytes": os.path.getsize(ios_slice),
        "object_sections_bytes": sections_bytes(slice_size),
        "what": ("undra init template (ios), undra build --platform ios --release (the release-mobile profile): the device slice of the XCFramework "
                 "linked the way an app's link keeps it (dylib, -force_load, -dead_strip, strip -x), the sizes of the sections of its __TEXT, "
                 "__DATA_CONST and __DATA segments, zero-fill left out; archive_bytes is the .a itself, object_sections_bytes the sections of "
                 "the slice's prelinked object before the app's link"),
        "command": "undra init hello --platforms android,ios --undra-path <checkout>; undra build --platform ios --release -C hello (scripts/native-size.sh)",
        "commit": commit, "date": today, "rustc": rustc, "xcode": xcode,
    }
    lines.append(line)
    results.append((line, ok, table))

for line in lines:
    print(json.dumps(line, separators=(",", ":")))

kb = lambda n: f"{n / 1000:.1f} KB"
failed = []
for line, passed, tbl in results:
    name, size, ceil = line["artifact"], line["bytes"], line["ceiling"]
    print(f"{name}: {size:,} bytes ({kb(size)})", file=sys.stderr)
    if name.startswith("ios/"):
        print(f"  beside it: the .a is {line['archive_bytes']:,} bytes, the slice object's sections {line['object_sections_bytes']:,} (not gated)", file=sys.stderr)
    rec, tol = (size if record else tbl.get("measured_bytes")), tbl.get("tolerance")
    print(f"  gate: <= {ceil:,} (budget {line['budget']:,}" + (f", record {int(rec):,} + {tol:.0%}" if rec and tol is not None else "") + f"): {'ok' if passed else 'OVER'}", file=sys.stderr)
    if not passed:
        over = "the budget" if size > line["budget"] else f"{tol:.0%} over the record"
        failed.append(f"{name} is {size - ceil:,} bytes over its gate ({over})")

# The JSON behind the numbers: always for a gate run (CI uploads it, pass or fail); for --record only when everything
# was within its budget, so a failed run never rewrites the record. A record keeps the lines this run did not measure (a Linux
# run records the Android rows and leaves the iOS one as it was); the JSON of a gate run has this run's lines only.
if not record or not failed:
    kept = []
    if record and os.path.exists(out):
        measured = {line["artifact"] for line in lines}
        for old in open(out):
            old = old.strip()
            if old and json.loads(old)["artifact"] not in measured:
                kept.append(json.loads(old))
    order = ["android/hello-arm64-v8a", "android/hello-x86_64", "ios/hello-arm64"]
    everything = sorted(kept + lines, key=lambda l: order.index(l["artifact"]) if l["artifact"] in order else len(order))
    with open(out, "w") as f:
        for line in everything:
            f.write(json.dumps(line, separators=(",", ":")) + "\n")

if record and not failed:
    new = text
    for line, _, _ in results:
        block = re.compile(r'(^\[size\."' + re.escape(line["artifact"]) + r'"\]\s*$)(.*?)(?=^\[|\Z)', re.M | re.S)
        m = block.search(new)
        body, n = re.subn(r'^(measured_bytes\s*=\s*)\d[\d_]*', lambda mm: mm.group(1) + str(line["bytes"]), m.group(2), count=1, flags=re.M)
        if n != 1:
            sys.exit(f'native-size.sh: [size."{line["artifact"]}"] has no measured_bytes line to write the record into')
        new = new[:m.start(2)] + body + new[m.end(2):]
    open(budgets_path, "w").write(new)
    print(f"recorded: {out} and measured_bytes of each measured [size] table in bench/budgets.toml", file=sys.stderr)

if failed:
    for message in failed:
        print(message, file=sys.stderr)
    print("Find what grew (ADR-052's native amendment says how: llvm-size -A and llvm-nm --size-sort on the unstripped twin in "
          "hello/build/symbols/android/<abi>/, `size -m` on the slice); if the growth is intended, re-record with "
          "scripts/native-size.sh --record in the same commit, where review sees it.", file=sys.stderr)
    if any(line["bytes"] > line["budget"] for line, _, _ in results):
        # --record moves the record, never the budget: a row over its budget needs room made or a decision.
        print("A row is over its budget, which --record cannot raise: make room in the same change, or restate the budget in an "
              "ADR (ADR-052).", file=sys.stderr)
    sys.exit(1)
PY
