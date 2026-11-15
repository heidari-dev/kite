# iPhone

Share an iPhone's connection with a Linux, Windows or Mac computer over a USB cable, or over WiFi:
the iPhone's Personal Hotspot, or a WiFi network that both are on. You need an iPhone with iOS 17 or
newer.

## 1. Put Kite on the iPhone

There is no App Store version. The project builds an unsigned app and you sign it for yourself, with
your own Apple ID, using [SideStore](https://sidestore.io) or [AltStore](https://altstore.io). With a
free Apple ID the signature lasts seven days and SideStore or AltStore renews it. The steps are in
[apps/ios](../../apps/ios/README.md#install-with-sidestore-or-altstore).

The first time Kite shares over WiFi, iOS asks to let it find devices on the local network. Allow it,
or the computer cannot find the iPhone. The cable does not need this.

## 2. Put Kite on the computer

- **Linux:** [step 3 of the Android and Linux guide](android-linux.md#3-put-kite-on-the-computer)
- **Windows:** [step 3 of the Android and Windows guide](android-windows.md#3-put-kite-on-the-computer)
- **Mac:** [step 3 of the Android and macOS guide](android-macos.md#3-put-kite-on-the-mac)

## 3. Choose how they connect

### By USB cable

The computer talks to the iPhone through a small system service that handles Apple devices over USB.
Check that yours has it:

- **Mac:** built in. Nothing to do.
- **Linux:** install it with `sudo apt install usbmuxd`.
- **Windows:** you need Apple Mobile Device Support. It comes with iTunes (from the Microsoft Store)
  or the Apple Devices app.

Plug the iPhone in, unlock it and tap **Trust This Computer** when it asks.

### Over WiFi

Pick one:

- **Personal Hotspot.** On the iPhone: Settings, Personal Hotspot, then switch on Allow Others to
  Join. Join that WiFi network from the computer.
- **A WiFi network you both use,** such as your home WiFi. No hotspot needed.

## 4. Start sharing on the iPhone

Open Kite on the iPhone and tap **Start sharing**.

**Keep Kite open on the screen.** iOS suspends apps in the background, so sharing stops when you
leave Kite and starts again when you return. The screen stays on while sharing.

## 5. Pair, once

1. On the iPhone, tap **Pair a computer**. It stays open for two minutes.
2. On the computer, in the same way for both transports, with `--usb` for the cable or `--wifi` for
   WiFi:
   - **Linux:** in a terminal, run `sudo kite pair --usb` (or `--wifi`).
   - **Windows:** open Kite, choose USB or WiFi under Phone, click **Pair a phone** and say yes to
     the permission question.
   - **Mac:** open Kite, choose USB or WiFi under Phone and click **Pair a phone**.
3. The computer shows a short fingerprint, like `3F2A 91C0 77D4 0B1E`. The iPhone asks "Allow this
   computer?" with a fingerprint. If it is the same, tap **Allow**. If not, tap Don't allow:
   something else reached the iPhone first.
4. The iPhone shows a 6-digit code. Type it on the computer.

Pairing is the same whichever way the iPhone is connected. On a public WiFi, everyone there can see
the pairing window open, so pair on a network you trust or use the cable.

## 6. Connect

In the Kite window choose **USB** (or **WiFi**) under Phone, leave Share on **Full**, and press
**Connect**. From a terminal, `kite connect --usb --tun` (or `kite connect --wifi --tun`). With the
cable plugged in, `--usb` picks the iPhone by itself. If several phones are connected, `kite devices`
lists them and `--serial` chooses one.

Over WiFi, leave the address empty and Kite finds the iPhone itself. If it cannot, type the address
the iPhone shows. On a Personal Hotspot it is usually `172.20.10.1`.

Check it: before and after connecting, open <https://ifconfig.me>. After, the address is the one the
iPhone's VPN or mobile network gives it. Press **Disconnect** to stop.

## Good to know

- Without Kite, a computer on the iPhone's hotspot uses the mobile network directly and does not use
  the iPhone's VPN. With Kite it does. That is the reason to use it.
- Over USB, the iPhone's mobile data or its WiFi carries the traffic, and so does its VPN if one is on.
- Kite on the iPhone listens on WiFi, ethernet and hotspot addresses and, for the cable, on the
  iPhone itself, never on mobile data or a VPN. Turn the hotspot off when you do not need it.
- The kill switch (block traffic if the phone disconnects) exists on Linux and Windows so far.

Stuck? [Troubleshooting](troubleshooting.md#iphone).
