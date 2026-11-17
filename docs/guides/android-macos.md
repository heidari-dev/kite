# Android and macOS

Share an Android phone's connection with a Mac, over a USB cable or over WiFi. You need macOS 12 or
newer (Apple silicon or Intel) and an Android phone with Android 8 or newer.

Kite on macOS needs testers: it has only been run on GitHub's test machines, not on many real Macs.
If something does not work, please [open an issue](https://github.com/heidari-dev/kite/issues).

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

## 3. Put Kite on the Mac

1. Download `Kite-VERSION.pkg` from the same releases page and open it.
2. macOS says it cannot verify the package. Kite's package is not signed with an Apple developer
   certificate, because that costs money the project does not have
   ([how to check the file yourself](faq.md#why-is-the-installer-not-signed)). Open System
   Settings, Privacy & Security, scroll to the message about the package and click **Open
   Anyway**.
3. Enter an administrator's password when the installer asks.

Kite is now in Applications and shows an icon in the menu bar after you log in.

## 4. Start sharing on the phone

Open Kite on the phone and tap **Start sharing**. If Android asks to let Kite run without battery
limits, say yes: without it, Android can stop Kite when the screen is off.

## 5. Pair, once

Pairing tells each side which device to trust.

1. Plug the phone into the Mac. The phone may ask "Allow USB debugging?". Tap Allow. Leave
   "Always allow" off.
2. On the phone, tap **Pair a computer**. It stays open for two minutes.
3. On the Mac, open **Kite** from Applications and click **Pair a phone**. A window opens that
   shows a short fingerprint, like `3F2A 91C0 77D4 0B1E`. (Or in Terminal: `kite pair --usb`.)
4. The phone asks "Allow this computer?" and shows a fingerprint. If it is the same, tap **Allow**.
   If not, tap Don't allow: something else reached the phone first.
5. The phone now shows a 6-digit code. Type it on the Mac and press Enter.

Over WiFi instead of a cable: switch on **Share over WiFi** in the phone app, put both devices on
the same WiFi (or the phone's hotspot), and choose WiFi under Phone in the Kite window.

## 6. Connect

In the Kite window choose USB (or WiFi) under Phone, leave Share on **Full**, and click **Connect**.
The phone says it is sharing with one computer.

Check it: before and after clicking Connect, open <https://ifconfig.me>. After, the address is the
one your phone's VPN or mobile network gives it. Click **Disconnect** to stop.

## Next times

Plug in the phone, tap Start sharing, click Connect. To skip the last step, turn on **Connect when
the phone is plugged in** in the Kite window (or run `kite auto-connect on`): the computer then
repeats your last USB connection by itself, and it keeps trying while you tap Start sharing. If the
phone does not answer for about a minute, Kite stops trying until you plug it in again, and so it
does after you click Disconnect.

## Good to know

- **Proxy only** (under Share) sends only the programs you point at Kite's local proxy through the
  phone. Everything else uses your normal connection.
  To send just one browser: [One browser through the phone](browser.md).
- If the phone drops out, the Mac goes back to its normal connection, with your own address, unless
  you turned on **Block traffic if the phone disconnects**. With it on, nothing leaves until the
  phone is back or you press Disconnect. If Kite itself crashes, the block stays; press Disconnect,
  run `sudo kite restore` or restart the Mac to get your normal connection back.
- While connected, names that only your home network knows stop resolving. Disconnect to get them
  back.
- Only one program can hold the phone's USB connection. If you also use `adb`, run
  `adb kill-server` first.

## Taking Kite off

```
sudo "/Library/Application Support/Kite/uninstall.sh"
```

On the phone, open Kite and remove the computer from Paired computers. More about what the package
does: [macos.md](../macos.md).

Stuck? [Troubleshooting](troubleshooting.md).
