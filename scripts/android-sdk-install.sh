#!/usr/bin/env bash
# Installs Android SDK packages with the runner's sdkmanager, and tries a failed download again.
#
#   bash scripts/android-sdk-install.sh "ndk;27.2.12479018" "platforms;android-35"
#   bash scripts/android-sdk-install.sh emulator "system-images;android-34;google_apis;x86_64"
#
# dl.google.com answers an HTTP 502 now and then, and one failed download used to fail the whole job
# (the Two cores emulator job, inside reactivecircus/android-emulator-runner). This accepts the
# licenses, then runs `sdkmanager --install` up to 4 times, 15, 30 and 60 s apart (ANDROID_SDK_INSTALL_DELAY
# sets the first wait), and checks
# `--list_installed` for every package after each try (sdkmanager's exit status alone has not always
# said whether a download failed). What is installed already is not downloaded again, so a job that
# runs this before the emulator action leaves the action nothing to download.
set -euo pipefail

[ $# -gt 0 ] || { echo "usage: android-sdk-install.sh <package>..." >&2; exit 2; }
sdkmanager="${ANDROID_HOME:?ANDROID_HOME is not set}/cmdline-tools/latest/bin/sdkmanager"
[ -x "$sdkmanager" ] || { echo "android-sdk-install.sh: no sdkmanager at $sdkmanager" >&2; exit 2; }

yes | "$sdkmanager" --licenses >/dev/null 2>&1 || true

missing() { # prints every package of "$@" that sdkmanager does not list as installed
  local installed
  installed=$("$sdkmanager" --list_installed 2>/dev/null | awk -F'|' 'NF >= 3 { gsub(/^ +| +$/, "", $1); print $1 }')
  local package
  for package in "$@"; do
    printf '%s\n' "$installed" | grep -Fxq -- "$package" || printf '%s\n' "$package"
  done
}

attempts=4
delay=${ANDROID_SDK_INSTALL_DELAY:-15} # seconds before the second try; doubled each time
log=$(mktemp "${TMPDIR:-/tmp}/sdkmanager.XXXXXX")
trap 'rm -f "$log"' EXIT
for attempt in $(seq 1 "$attempts"); do
  "$sdkmanager" --install "$@" >"$log" 2>&1 || true
  left=$(missing "$@")
  if [ -z "$left" ]; then
    echo "android-sdk-install.sh: installed $* (attempt $attempt)"
    exit 0
  fi
  tail -n 5 "$log" >&2
  if [ "$attempt" -lt "$attempts" ]; then
    echo "::warning title=sdkmanager::not installed after attempt $attempt of $attempts: ${left//$'\n'/ }; again in ${delay} s"
    sleep "$delay"
    delay=$((delay * 2))
  fi
done
echo "::error title=sdkmanager::not installed after $attempts attempts: ${left//$'\n'/ }"
exit 1
