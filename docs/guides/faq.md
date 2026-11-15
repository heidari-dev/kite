# FAQ

## What does Kite do?

It lets a computer use a phone's internet connection, the way tethering does, with one difference:
the computer's traffic leaves through whatever the phone itself uses, which can be a VPN, a proxy or
the plain mobile network. Start with the [guides](README.md).

## Why does my VPN address show up?

That is the point. When you tether normally, the computer's packets are forwarded by the phone's
operating system, and on most phones they do not enter the VPN. Kite works differently. The Kite app
on the phone receives each request from the computer and opens a real connection of its own, so the
connection is treated like one from any other app on the phone. If the phone's VPN is on, the
website sees the VPN's address. Names are looked up on the phone too, so DNS follows the VPN.

If you do not want that, turn the VPN off on the phone. Kite then uses the mobile network.

## Does it need root on the phone?

No. On Android you install an app and turn on USB debugging if you want the cable. On an iPhone you
sign the app with your own Apple ID. Nothing is jailbroken or rooted.

## Is it safe?

Short version: only a phone and a computer that you paired can talk to each other, the link is
encrypted, and the computer cannot reach the phone's own files or apps.

- **Pairing.** You pair once, with a 6-digit code that both sides compute from the encrypted
  connection itself. The phone shows a fingerprint of the computer first and you compare it to the
  one on the computer. Pairing stays open for two minutes after you tap Pair a computer, and five
  wrong tries lock it for a minute.
- **Encryption.** Every link, cable or WiFi, uses [Noise](https://noiseprotocol.org/) (25519,
  ChaChaPoly, BLAKE2s) through the `snow` library, with no home-made cryptography. After pairing,
  each side accepts only the other's key.
- **What the phone cannot be made to reach.** The relay on the phone refuses connections to the
  phone itself: its loopback address, every address of its own network interfaces, link-local and
  multicast addresses. It checks after the name lookup, so a trick with DNS does not get around it.
- **What it can reach, by design.** The computer can open connections to other devices on the networks
  the phone is on, such as other devices on a hotspot, because your traffic needs them. A paired
  computer can use all of the phone's data; there is no quota yet.
- **No middleman.** Kite has no server and no account. Traffic goes only between your phone and your
  computer, and then out to the Internet from the phone. Kite does not touch HTTPS, so websites stay
  encrypted from end to end. It logs no keys, codes, payloads or hostnames.
- **WiFi sharing is off until you turn it on.** The phone only listens on WiFi, hotspot and
  ethernet addresses, never on mobile data or a VPN. Turn it off on public WiFi.

Kite is early software. The [threat model](../threat-model.md) lists what is done and a section of
known gaps, including the ones that matter on a shared network.

## Why is the installer not signed?

A Windows code signing certificate and an Apple developer account cost money, and Kite is a free
project with none. So Windows shows "unknown publisher" and macOS says it cannot verify the package.
Signing would tell you who built the file; it is not the only way to know. You can check that the
file is the one the project built:

- Compare its SHA-256 with `SHA256SUMS` on the release page.
- Run `gh attestation verify FILE --repo heidari-dev/kite`. It proves the file was built by the
  project's release workflow from a tagged commit.

The steps for each system are in [windows.md](../windows.md#the-smartscreen-warning) and
[macos.md](../macos.md#the-gatekeeper-warning). The Android app is signed with the project's own
key. The iPhone app is signed by you.

## What about my carrier's tethering limits?

Here is what is known, and what is not.

- Kite's traffic leaves the phone the way the phone's own apps' traffic does: the Kite app opens the
  connections itself, so they come from the phone with its own network settings.
- Kite is not built to hide tethering and does not promise to. Nobody has tested it against any
  particular carrier. How a carrier spots tethering is its own business, and it can use more than
  how packets are forwarded: how much data you use, which sites you visit, and what your traffic
  looks like.
- The data counts against your plan like any other data on the phone. If your plan caps or forbids
  tethering, its terms still apply to what you do.
- With a VPN on the phone, the carrier sees the encrypted VPN traffic and not the sites.

Check your plan's terms before relying on Kite for something the plan does not allow.

## What about native tethering?

Native USB and WiFi tethering is simpler and faster to set up, and needs nothing on the computer.
Its drawback is that on most phones the tethered traffic skips the phone's VPN. Use it when you do
not need the VPN. Use Kite when you do. There is a short comparison in the
[README](../../README.md#kite-and-native-tethering).

## Does it use my mobile data and battery?

Yes. The computer's traffic goes through the phone's connection, so it counts against the data plan.
The phone does the work of relaying it, which costs some battery. Keep it plugged in for long
sessions.

## Can I use more than one computer?

Yes, once each is paired. A phone serves up to four computers at a time.

## Is there a kill switch?

Linux and Windows have one: **Block traffic if the phone disconnects**, off until you turn it on.
macOS does not have one yet ([#101](https://github.com/heidari-dev/kite/issues/101)). Without it,
when the link to the phone drops, the computer goes back to its own connection.

## Why must Kite stay open on an iPhone?

iOS suspends apps in the background, which would end the connection. Kite runs while it is on the
screen and keeps the screen on while sharing. See the [iPhone guide](iphone.md).

## How do I remove a computer or a phone?

On the phone, remove the computer under Paired computers. On the computer, `kite peers` lists the
paired phones and `kite peers remove KEY` forgets one. Remove the pairing on both sides.
Removing a computer in the app also ends the tunnel it has open.

## It does not work

Try [Troubleshooting](troubleshooting.md), then ask in
[Discussions](https://github.com/heidari-dev/kite/discussions) or
[open an issue](https://github.com/heidari-dev/kite/issues). Security problems go to
[SECURITY.md](../../SECURITY.md), not a public issue.
