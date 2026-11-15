# Android and Linux

Share an Android phone's connection with a Linux computer, over a USB cable or over WiFi. You need
Debian 12, Ubuntu 22.04 or newer (64-bit Intel or AMD) and an Android phone with Android 8 or newer.

## 1. Put Kite on the phone

1. On the phone, open the [releases page](https://github.com/heidari-dev/kite/releases) and download
   the file ending in `.apk`.
2. Open it. Android asks whether your browser may install apps. Allow it for this one install, then
   tap Install.

## 2. Turn on USB debugging

Skip this if you only want WiFi. Kite uses USB debugging as its way into the phone; it does not
give the computer access to your files.

1. Settings, About phone, then tap Build number seven times. Android says you are a developer.
2. Settings, System, Developer options, then switch on USB debugging. The menu names differ a
   little between brands.

## 3. Put Kite on the computer

1. Download `kite_VERSION_amd64.deb` from the same releases page.
2. Open it with your software centre and install, or in a terminal:

   ```
   sudo apt install ./kite_*.deb
   ```

3. Log out and back in once. This lets your account use Kite.

Kite is now in your application menu, and its icon shows in the tray. GNOME needs the AppIndicator
extension to show tray icons; the window works without it.

## 4. Start sharing on the phone

Open Kite on the phone and tap **Start sharing**. If Android asks to let Kite run without battery
limits, say yes: without it, Android can stop Kite when the screen is off.

## 5. Pair, once

Pairing tells each side which device to trust. It changes where all of your traffic can go, so
Linux asks for `sudo`.

1. Plug the phone into the computer. The phone may ask "Allow USB debugging?". Tap Allow. Leave
   "Always allow" off.
2. On the phone, tap **Pair a computer**. It stays open for two minutes.
3. On the computer, open a terminal and run:

   ```
   sudo kite pair --usb
   ```

   Kite prints a short fingerprint, like `3F2A 91C0 77D4 0B1E`.
4. The phone asks "Allow this computer?" and shows a fingerprint. If it is the same as the one on
   the computer, tap **Allow**. If not, tap Don't allow: something else reached the phone first.
5. The phone now shows a 6-digit code. Type it in the terminal and press Enter.

Over WiFi instead of a cable: switch on **Share over WiFi** in the phone app, put both devices on
the same WiFi (or the phone's hotspot), and run `sudo kite pair --wifi`.

## 6. Connect

Open **Kite** from the menu, choose USB (or WiFi) under Phone, leave Share on **Full**, and press
**Connect**. The phone says it is sharing with one computer.

Check it: before and after pressing Connect, open <https://ifconfig.me>. After, the address is the
one your phone's VPN or mobile network gives it. Press **Disconnect** to stop.

## Next times

Plug in the phone, tap Start sharing, press Connect.

## Options worth knowing

- **Proxy only** (under Share) sends only the programs you point at Kite's local proxy through the
  phone, and needs no special rights. Everything else uses your normal connection.
- **Block traffic if the phone disconnects** stops the computer from falling back to its own
  connection if the cable comes out. It is off until you turn it on. It is available on Linux only
  for now.
- From a terminal: `kite connect --usb --tun` and `kite disconnect`. Every command is in
  [cli.md](../cli.md).

## Taking Kite off

```
sudo apt remove kite
```

This also forgets the paired phones. On the phone, open Kite and remove the computer from Paired
computers. More about the packages: [packaging/linux](../../packaging/linux/README.md).

Stuck? [Troubleshooting](troubleshooting.md).
