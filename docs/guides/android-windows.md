# Android and Windows

Share an Android phone's connection with a Windows computer, over a USB cable or over WiFi. You need
Windows 10 or 11 (64-bit) and an Android phone with Android 8 or newer.

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

1. Download `kite-VERSION-x86_64.msi` from the same releases page and open it.
2. Windows shows a blue "Windows protected your PC" window that says the publisher is unknown.
   Kite's installer is not signed, because a signing certificate costs money the project does not
   have ([why this is safe to continue, and how to check the file](faq.md#why-is-the-installer-not-signed)).
   Click **More info**, then **Run anyway**.
3. Windows asks once whether to let the installer make changes. Say yes. There are no other
   questions.

Kite is now in the Start menu and starts hidden in the tray when you sign in.

## 4. Start sharing on the phone

Open Kite on the phone and tap **Start sharing**. If Android asks to let Kite run without battery
limits, say yes: without it, Android can stop Kite when the screen is off.

## 5. Pair, once

Pairing tells each side which device to trust. It changes where all of your traffic can go, so
Windows asks for administrator permission.

1. Plug the phone into the computer. The phone may ask "Allow USB debugging?". Tap Allow. Leave
   "Always allow" off.
2. On the phone, tap **Pair a computer**. It stays open for two minutes.
3. On the computer, open **Kite** from the Start menu (its icon sits in the tray; Windows 11 hides
   new icons behind the arrow at the right of the taskbar) and click **Pair a phone**. Say yes to
   the permission question.
4. A window shows a short fingerprint, like `3F2A 91C0 77D4 0B1E`. The phone asks "Allow this
   computer?" and shows a fingerprint. If it is the same, tap **Allow**. If not, tap Don't allow:
   something else reached the phone first.
5. The phone now shows a 6-digit code. Type it into the window and click **Pair**.

Over WiFi instead of a cable: switch on **Share over WiFi** in the phone app, put both devices on
the same WiFi (or the phone's hotspot), and choose WiFi under Phone in the Kite window.

## 6. Connect

In the Kite window choose USB (or WiFi) under Phone, leave Share on **Full**, and click **Connect**.
The phone says it is sharing with one computer.

Check it: before and after clicking Connect, open <https://ifconfig.me>. After, the address is the
one your phone's VPN or mobile network gives it. Click **Disconnect** to stop.

## Next times

Plug in the phone, tap Start sharing, click Connect.

## Good to know

- **Proxy only** (under Share) sends only the programs you point at Kite's local proxy through the
  phone. Everything else uses your normal connection.
- There is no kill switch on Windows yet. If the phone drops out, Windows goes back to its normal
  connection, with your own address. Watch the tray icon if that matters to you.
- While connected, names that only your home network knows (a router's `.lan` name) stop resolving.
  Disconnect to get them back.
- If Windows has no driver for the phone, Kite says it needs the WinUSB driver. The fix is in
  [troubleshooting](troubleshooting.md#windows-says-kite-needs-the-winusb-driver).

## Taking Kite off

Settings, Apps, Kite, Uninstall. This also forgets the paired phones. On the phone, open Kite and
remove the computer from Paired computers. More about what the installer does:
[windows.md](../windows.md).

Stuck? [Troubleshooting](troubleshooting.md).
