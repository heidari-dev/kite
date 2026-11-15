# iPhone

Share an iPhone's connection with a Linux, Windows or Mac computer over WiFi: the iPhone's Personal
Hotspot, or a WiFi network that both are on. You need an iPhone with iOS 17 or newer.

Kite's own link over the USB cable is not in this version of the iPhone app (see the
[roadmap](../../ROADMAP.md)). Until it is, use WiFi. The cable still works as a network connection: an
iPhone with Personal Hotspot on can share over USB, and Kite then talks to the iPhone across that
connection.

## 1. Put Kite on the iPhone

There is no App Store version. The project builds an unsigned app and you sign it for yourself, with
your own Apple ID, using [SideStore](https://sidestore.io) or [AltStore](https://altstore.io). With a
free Apple ID the signature lasts seven days and SideStore or AltStore renews it. The steps are in
[apps/ios](../../apps/ios/README.md#install-with-sidestore-or-altstore).

The first time Kite shares, iOS asks to let it find devices on the local network. Allow it, or the
computer cannot find the iPhone.

## 2. Put Kite on the computer

- **Linux:** [step 3 of the Android and Linux guide](android-linux.md#3-put-kite-on-the-computer)
- **Windows:** [step 3 of the Android and Windows guide](android-windows.md#3-put-kite-on-the-computer)
- **Mac:** [step 3 of the Android and macOS guide](android-macos.md#3-put-kite-on-the-mac)

## 3. Join the iPhone's network

Pick one:

- **Personal Hotspot.** On the iPhone: Settings, Personal Hotspot, then switch on Allow Others to
  Join. Join that WiFi network from the computer, or plug the iPhone in with its cable.
- **A WiFi network you both use,** such as your home WiFi. No hotspot needed.

## 4. Start sharing on the iPhone

Open Kite on the iPhone and tap **Start sharing**. It shows the address it listens on.

**Keep Kite open on the screen.** iOS suspends apps in the background, so sharing stops when you
leave Kite and starts again when you return. The screen stays on while sharing.

## 5. Pair, once

1. On the iPhone, tap **Pair a computer**. It stays open for two minutes.
2. On the computer:
   - **Linux:** in a terminal, run `sudo kite pair --wifi`.
   - **Windows:** open Kite, choose WiFi under Phone, click **Pair a phone** and say yes to the
     permission question.
   - **Mac:** open Kite, choose WiFi under Phone and click **Pair a phone**.
3. The computer shows a short fingerprint, like `3F2A 91C0 77D4 0B1E`. The iPhone asks "Allow this
   computer?" with a fingerprint. If it is the same, tap **Allow**. If not, tap Don't allow:
   something else reached the iPhone first.
4. The iPhone shows a 6-digit code. Type it on the computer.

Pair on a network you trust. On a public WiFi, everyone there can see the pairing window open.

## 6. Connect

In the Kite window choose WiFi under Phone, leave the address empty (Kite finds the iPhone itself),
leave Share on **Full**, and press **Connect**. If it cannot find the iPhone, type the address that
the iPhone shows. On a Personal Hotspot it is usually `172.20.10.1`.

From a terminal: `kite connect --wifi --tun`, or `kite connect --wifi 172.20.10.1 --tun`.

Check it: before and after connecting, open <https://ifconfig.me>. After, the address is the one the
iPhone's VPN or mobile network gives it. Press **Disconnect** to stop.

## Good to know

- Without Kite, a computer on the iPhone's hotspot uses the mobile network directly and does not use
  the iPhone's VPN. With Kite it does. That is the reason to use it.
- Kite on the iPhone only listens on WiFi, ethernet and hotspot addresses, never on mobile data or a
  VPN. Turn off the hotspot and leave public WiFi when you do not need it.
- The kill switch (block traffic if the phone disconnects) exists on Linux only so far.

Stuck? [Troubleshooting](troubleshooting.md).
