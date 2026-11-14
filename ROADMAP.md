# Roadmap

What is planned, in the order it is likely to happen. Dates are not promised; each item links to the
issue where it is tracked. What exists today is in the [README](README.md#status).

## 0.1.0: Android and Linux

The first release candidate, `0.1.0-rc.1`, is on `main`. The release itself waits for the Android
signing key to be set up in CI ([docs/releasing.md](docs/releasing.md)), then goes through a
candidate, a check of the draft, and a tag. Open work is on the
[v0.1.0 milestone](https://github.com/heidari-dev/kite/milestone/1).

## 0.2.0: Windows

A Windows computer with the same features as Linux: TUN mode with Wintun, the daemon as a Windows
service, a desktop app, a kill switch ([#88](https://github.com/heidari-dev/kite/issues/88)) and an
MSI installer. Tracked in [#15](https://github.com/heidari-dev/kite/issues/15).

The installer should not ask people to install Android platform-tools, so talking to the phone over
USB without an adb server comes with it ([#52](https://github.com/heidari-dev/kite/issues/52)).

## 0.3.0: iPhone

The phone relay as an iOS app, reached over usbmux or WiFi. The relay core is shared with Android
through `kite-ffi`. Tracked in [#16](https://github.com/heidari-dev/kite/issues/16).

## Not scheduled

- **macOS** as a computer, with `utun`: [#17](https://github.com/heidari-dev/kite/issues/17).
- **User guides and a website**: [#18](https://github.com/heidari-dev/kite/issues/18).

## Not planned

- A hosted service, account or server of ours in the data path. Traffic goes only between your
  phone and your computer.
- A WebView or Electron app. The apps stay native so that they use little memory.

## Ongoing

- Keeping the supply chain honest: dependency advisories ([#63](https://github.com/heidari-dev/kite/issues/63)),
  Gradle dependency verification ([#60](https://github.com/heidari-dev/kite/issues/60)), and the
  [threat model](docs/threat-model.md)'s list of known gaps.
- A second maintainer, so that the project does not depend on one person
  ([GOVERNANCE.md](GOVERNANCE.md)).

Ideas go in [Discussions](https://github.com/heidari-dev/kite/discussions) or a feature issue.
