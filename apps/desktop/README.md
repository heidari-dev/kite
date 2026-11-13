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
  `sudo kite pair --usb` instead.
- The window remembers the mode and the kill switch ("Block traffic if the phone disconnects", off
  until you turn it on) in `kite/desktop.json` in your config directory.
- The tray icon needs a desktop that shows StatusNotifier icons. GNOME needs the AppIndicator
  extension.
