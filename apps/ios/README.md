# Kite for iPhone

The phone side of Kite on iOS: a SwiftUI app that runs the relay (`crates/kite-relay`, through
`crates/kite-ffi`) while it is open. Computers reach it over TCP on the iPhone's Personal Hotspot or
on a WiFi network both are on. iOS 17 or newer.

The relay runs only while Kite is on the screen. iOS suspends apps in the background, so sharing
stops when you leave the app and starts again when you come back. The screen stays on while
sharing.

## Install with SideStore or AltStore

There is no App Store build. Each CI run of the `iOS` workflow attaches an unsigned
`Kite-unsigned.ipa` (artifact `kite-ios-unsigned-ipa`).

1. Install [SideStore](https://sidestore.io) or [AltStore](https://altstore.io) on the iPhone,
   following their guide. They sign apps with your own Apple ID.
2. Download `Kite-unsigned.ipa` to the iPhone (unzip the artifact first) and open it with SideStore
   or AltStore, or add it from the Files app inside SideStore or AltStore.
3. With a free Apple ID the signature lasts 7 days; SideStore or AltStore refreshes it.
4. Open Kite. The first time it shares, iOS asks to let Kite find and connect to devices on the
   local network: allow it, or computers cannot find the iPhone.

## Try it

1. Turn on Personal Hotspot with Allow Others to Join, and join it from the computer over WiFi
   or USB. A WiFi network both are on works too.
2. Open Kite and tap Start sharing. The screen shows the address it listens on (TCP port 47810).
3. The first time, tap Pair a computer and run `kite pair --wifi` on the computer. Kite asks
   whether to allow the computer and shows its fingerprint, which `kite pair` printed too; tap Allow,
   then type the code Kite shows.
4. Then, each time:

```
kite connect --wifi
```

Without an address the computer finds the iPhone through mDNS (`_kite._tcp`); give it as
`--wifi 172.20.10.1` if that fails. Kite only listens on the private IPv4 addresses of WiFi,
ethernet and the hotspot, never on mobile data or a VPN. Traffic the computer sends leaves through
the iPhone, so it uses the iPhone's VPN if one is on.

The relay's private key and the paired computers' public keys are in the Keychain, readable only
while the iPhone is unlocked and never copied to backups or other devices.

## Build

You need a Mac with Xcode 26 and Rust with the iOS targets:

```
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
apps/ios/build-rust.sh
open apps/ios/Kite.xcodeproj
```

`build-rust.sh` builds `kite-ffi` as a static library for iPhones and the Apple silicon simulator,
packs both into `build/KiteFFI.xcframework` and generates the Swift bindings into `Kite/FFI`. Both
are build outputs and ignored by git. The generator (`uniffi-bindgen`) is installed by the script
on first use, the same way as for Android.

`apps/ios/Tests/main.swift` checks the motion state machine and the address filter; CI compiles it
with `KiteMotion.swift` and `Network.swift` as a macOS program and runs it.

## The screen

The same single screen as the Android app: the kite, a line saying what it is doing, the live
speed, Start or Stop, pairing, the addresses it listens on and the paired computers. The kite
follows [docs/design/motion.md](../../docs/design/motion.md): one `Shape` per layer, moved with
offsets, rotations and opacities, with the numbers in `KiteMotion.swift`. The loops run in a
`TimelineView` at most 30 times a second, only while a loop runs and the app is in front. With
Reduce Motion on, it fades between states instead of moving. Light and dark follow the system.

While the app is not active (app switcher, Control Center) the screen is covered, so the pairing
code stays out of the app switcher's picture. iOS does not let an app block screenshots; the code
is hidden while the screen is recorded or mirrored.

## Screenshots

The `iOS` workflow boots a simulator, installs the debug build and saves the screen in every state,
light and dark, plus the longest text at the largest accessibility size. They are the `ios-screens`
artifact of the run. `scripts/ios-screens.sh` starts the app with `-demo NAME`, which only debug
builds understand and which shows fixed data with no relay behind it.
