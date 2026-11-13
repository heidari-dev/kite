#!/usr/bin/env bash
# Drives the debug APK on a running emulator and saves screenshots of the main screen: stopped,
# waiting for a computer, and pairing open, in light and dark. Run by
# .github/workflows/android-screens.yml, which boots the emulator.
#
# Usage: android-screens.sh APK OUT_DIR
set -euo pipefail

apk=$1
out=$2
pkg=io.github.heidaridev.kite
mkdir -p "$out"

# The screen as XML. A dump can fail while the screen is busy; callers retry.
ui() {
  adb shell uiautomator dump /sdcard/ui.xml > /dev/null 2>&1 || true
  adb exec-out cat /sdcard/ui.xml
}

# Waits up to 60 s for a node whose text starts with $1. The screen is the only signal the app gives.
wait_for() {
  local xml
  for _ in $(seq 60); do
    xml=$(ui) || xml=""
    if grep -q "text=\"$1" <<< "$xml"; then
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for: $1" >&2
  ui > "$out/last-ui.xml" || true
  adb exec-out screencap -p > "$out/last.png" || true
  return 1
}

# Taps the middle of the view with this resource id.
tap() {
  local id=$1 xml bounds
  xml=$(ui) || xml=""
  bounds=$(grep -o "resource-id=\"$pkg:id/$id\"[^>]*" <<< "$xml" | grep -o 'bounds="[^"]*"' | head -n 1 | tr -cs '0-9' ' ') || true
  if [ -z "$bounds" ]; then
    echo "no view $id on screen" >&2
    return 1
  fi
  # shellcheck disable=SC2086 # four numbers: left top right bottom
  set -- $bounds
  adb shell input tap $(( ($1 + $3) / 2 )) $(( ($2 + $4) / 2 ))
}

# A state change fades for 200 ms when animations are off, and the text is there before it ends.
settle() {
  sleep 1
}

shot() {
  adb exec-out screencap -p > "$out/$1.png"
  [ -s "$out/$1.png" ]
}

adb install -r -g "$apk" > /dev/null
# Without this the app asks about battery limits when sharing starts, and the dialog covers the screen.
adb shell dumpsys deviceidle whitelist +"$pkg" > /dev/null

# The extra lets a debuggable build skip FLAG_SECURE, which screencap would show as black.
adb shell cmd uimode night no
adb shell am start -W -n "$pkg/.MainActivity" --ez screenshots true > /dev/null

wait_for "Not sharing"; settle; shot light-stopped
tap toggle
wait_for "Waiting for a computer"; settle; shot light-waiting
tap pair
wait_for "Pairing is open"; settle; shot light-pairing

adb shell cmd uimode night yes
# Nothing on screen says the activity has been recreated in dark mode.
sleep 3
tap toggle
wait_for "Not sharing"; settle; shot dark-stopped
tap toggle
wait_for "Waiting for a computer"; settle; shot dark-waiting
tap pair
wait_for "Pairing is open"; settle; shot dark-pairing

adb shell am force-stop "$pkg"
ls -l "$out"
