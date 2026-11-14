# Kite for the desktop

A small window and a tray icon for the Kite daemon, written with [Slint](https://slint.dev) (GPL-3.0
option, software renderer: no GPU, no web view). The app only shows and controls what the daemon
does, over `/run/kite/kite.sock` and the `kite-ipc` types, so closing the window or quitting the app
does not drop the connection.

```
cargo run -p kite-desktop            # open the window
cargo run -p kite-desktop -- --hidden  # tray only
```

Building on Linux needs the fontconfig headers (`libfontconfig1-dev` on Debian and Ubuntu).

- The daemon must be running and your user must be in the `kite` group. If not, the window says what
  to do.
- Pairing a phone changes where all of your traffic can go, so the daemon allows it only for root
  and its own user. As a member of the `kite` group the app shows the daemon's refusal; pair with
  `sudo kite pair --usb` instead (or `--wifi`, if the phone shares over WiFi).
- The phone is reached by USB or over WiFi. For WiFi, Share over WiFi must be on in the phone app and
  both must be on the same WiFi or hotspot. Type the phone's address, or leave it empty to find it
  automatically. The kill switch needs an IP address there, not a name.
- The window remembers the mode, the kill switch ("Block traffic if the phone disconnects", off
  until you turn it on), USB or WiFi and the address in `kite/desktop.json` in your config
  directory.
- The tray icon needs a desktop that shows StatusNotifier icons. GNOME needs the AppIndicator
  extension.

## On Windows

The app talks to the Kite service over the named pipe `\\.\pipe\kite`. If the service is not
running, or your user is not in the local group `kite`, the window says so (an administrator adds
you with `net localgroup kite YOUR_NAME /add`; sign in again afterwards). Pairing needs an
administrator, so pair from an administrator prompt with `kite pair --usb` (or `--wifi`). The
settings file is `%APPDATA%\kite\desktop.json`.

The tray icon is Slint's own (no extra crate: Slint uses the Windows notification area API and
`ksni` is only for Linux). Windows 11 puts a new icon in the overflow flyout of the taskbar; drag
it out to keep it visible. There is no kill switch on Windows yet, so the switch is not shown.
Reduced motion and text scaling are not read from the system on Windows; `--reduced-motion` works.

## Look and feel

The kite follows [docs/design/motion.md](../../docs/design/motion.md). With the window closed to
the tray, at rest and in reduced motion nothing is scheduled. Reduced motion is read from GNOME's
`enable-animations` setting each time the window opens, or forced with `--reduced-motion`. Text
follows GNOME's `text-scaling-factor`. The window follows the system's light or dark setting.

Every control works from the keyboard: Tab and Shift+Tab move, Space and Enter act, and the focus
ring is 3:1 against its background in both themes. Controls have names and roles for screen
readers (through AT-SPI); the state of the connection is read with the main button.

## Trying the window without a phone

A debug build can talk to a stand-in for the daemon, which answers from a JSON file you edit while
it runs (see the header of `examples/fake-daemon.rs`):

```
cargo run -p kite-desktop --example fake-daemon -- /tmp/fake.sock /tmp/fake.json
KITE_SOCKET=/tmp/fake.sock cargo run -p kite-desktop
```

A release build always uses `/run/kite/kite.sock`.
