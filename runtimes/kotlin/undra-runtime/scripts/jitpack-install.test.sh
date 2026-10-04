#!/usr/bin/env bash
# Tests jitpack-install.sh's one retry rule with a stand-in for gradle: a failed Android SDK component
# download is tried again (three attempts in all), and any other failure ends the build at once.
#
#   bash runtimes/kotlin/undra-runtime/scripts/jitpack-install.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Fails as $FAKE_MODE says for the first $FAKE_FAILS calls, then writes the six poms a real build publishes.
cat >"$tmp/gradle" <<'FAKE'
#!/usr/bin/env bash
calls=$(cat "$FAKE_STATE" 2>/dev/null || echo 0)
calls=$((calls + 1))
echo "$calls" >"$FAKE_STATE"
if [ "$calls" -le "$FAKE_FAILS" ]; then
  if [ "$FAKE_MODE" = sdk ]; then
    echo "> Failed to install the following SDK components:"
  else
    echo "e: Something.kt: (1, 1): Unresolved reference"
  fi
  exit 1
fi
for module in runtime testkit android-adapters android-work undra-compose okhttp-adapters; do
  dir="$MAVEN_REPO_LOCAL/com/github/shreypdev/undra/$module/$VERSION"
  mkdir -p "$dir"
  : >"$dir/$module-$VERSION.pom"
done
FAKE
chmod +x "$tmp/gradle"

fail() {
  printf 'not ok - %s\n' "$*" >&2
  exit 1
}

# check <what> <mode> <failing calls> <expected exit> <expected calls>
check() {
  local what="$1" mode="$2" fails="$3" want_exit="$4" want_calls="$5" got_exit=0
  rm -rf "$tmp/repo" "$tmp/state"
  FAKE_STATE="$tmp/state" FAKE_MODE="$mode" FAKE_FAILS="$fails" UNDRA_GRADLE="$tmp/gradle" \
    UNDRA_SDK_RETRY_PAUSE=0 MAVEN_REPO_LOCAL="$tmp/repo" VERSION=v9.9.9 \
    bash "$here/jitpack-install.sh" >"$tmp/out" 2>&1 || got_exit=$?
  [ "$got_exit" = "$want_exit" ] || fail "$what: exit $got_exit, expected $want_exit: $(tail -n 3 "$tmp/out")"
  [ "$(cat "$tmp/state")" = "$want_calls" ] || fail "$what: $(cat "$tmp/state") gradle call(s), expected $want_calls"
  printf 'ok - %s\n' "$what"
}

check "a build that works runs gradle once" sdk 0 0 1
check "a failed SDK component download is tried again and the build publishes" sdk 2 0 3
check "a third failed SDK component download ends the build" sdk 3 1 3
check "any other failure ends the build at once" other 1 1 1
echo "jitpack-install.sh: all checks passed"
