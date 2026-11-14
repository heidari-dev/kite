#!/usr/bin/env bash
# Saves screenshots of the iPhone app's screen in every state, light and dark, from a simulator.
# The debug build shows fixed data when started with -demo NAME, so no relay or computer is
# needed. Run by .github/workflows/ios.yml after the simulator build.
#
# Usage: ios-screens.sh APP OUT_DIR
set -euo pipefail

app=$1
out=$2
bundle=io.github.heidaridev.kite
mkdir -p "$out"

udid=$(xcrun simctl list devices available -j |
  jq -r '[.devices | to_entries[] | select(.key | contains("iOS")) | .value[] | select(.name | startswith("iPhone"))][0].udid')
if [ -z "$udid" ] || [ "$udid" = null ]; then
  echo "no iPhone simulator" >&2
  exit 1
fi
xcrun simctl boot "$udid"
xcrun simctl bootstatus "$udid" -b > /dev/null
xcrun simctl status_bar "$udid" override --time 9:41 --batteryState charged --batteryLevel 100
xcrun simctl install "$udid" "$app"

shot() {
  xcrun simctl launch --terminate-running-process "$udid" "$bundle" -demo "$2" > /dev/null
  # The kite lands and an alert slides in within a second.
  sleep 3
  xcrun simctl io "$udid" screenshot "$out/$1-$2.png" > /dev/null 2>&1
}

for appearance in light dark; do
  xcrun simctl ui "$udid" appearance "$appearance"
  for state in stopped waiting pairing allow code connected error worst; do
    shot "$appearance" "$state"
  done
done
# The longest text at the largest accessibility size.
xcrun simctl ui "$udid" appearance light
xcrun simctl ui "$udid" content_size accessibility-extra-extra-extra-large
shot largest-text worst
shot largest-text code

xcrun simctl shutdown "$udid"
ls -l "$out"
