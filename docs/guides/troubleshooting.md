# Troubleshooting

Find the symptom, try the fix. If nothing here helps, [open an issue](https://github.com/heidari-dev/kite/issues)
and say what the Kite window or `kite status` printed. Leave out pairing codes.

## Before anything else

1. Is Kite open on the phone, with **Start sharing** pressed? On an iPhone, Kite must also be the app
   on the screen.
2. Is the phone's screen allowed to stay on, and is Kite exempt from battery limits? Android stops
   background apps with the screen off.
3. Restart the connection: Disconnect, then Connect.

## The computer does not find the phone

**No phone found, over USB.**
- Check USB debugging is on (see the guide for your computer) and try another cable. Some cables
  only charge.
- On the phone, pull down the notification and make sure USB mode is not "No data transfer".
- Linux: unplug and replug the phone after installing Kite, and log out and in once so your
  account is in the `kite` group.

**Several phones connected.** Unplug the others, or from a terminal pick one with
`kite connect --usb --serial SERIAL` (`kite devices` lists the serials).

**The phone is "unauthorized".** The phone asked "Allow USB debugging?" and nobody tapped Allow.
Unplug, replug and tap Allow. Kite waits 60 seconds for the tap.

**The phone is "offline".** Unplug and replug it.

**The Kite app is not listening.** Open Kite on the phone and tap Start sharing.

**Over WiFi: nothing is found.**
- Switch on **Share over WiFi** in the Android app. The iPhone app needs no switch.
- Both devices must be on the same network. Some routers and most guest or hotel networks stop
  devices from seeing each other. Use the phone's hotspot instead.
- Type the phone's address in the Kite window (or `kite connect --wifi ADDRESS`). The phone shows
  it on its screen. A hotspot is usually `192.168.43.1` on Android and `172.20.10.1` on an iPhone.
- "Several phones answered": type the address of the one you want.

## Pairing fails

- **The code is wrong or expired.** Codes last two minutes. Tap Pair a computer again and start over.
  After five wrong tries the phone refuses pairing for a minute.
- **The code does not match even though you typed it right.** Something may be sitting between the
  two devices. Stop, move to a network you trust, or use a USB cable.
- **The fingerprints differ.** Tap Don't allow. Another device reached the phone first.
- **Linux says pairing is not allowed.** Pairing needs `sudo`: `sudo kite pair --usb` or
  `sudo kite pair --wifi`.
- **Windows asks for a permission and nothing happens.** Pairing needs an administrator. Run Kite
  again and say yes to the question, or use `kite pair --usb` in a terminal opened as administrator.
- **You want to start over.** `kite peers` lists the paired phones and `kite peers remove KEY`
  forgets one. Remove the computer on the phone too.

## It connects, but my address did not change

- Look under Share: **Proxy only** sends only programs that use Kite's proxy. Choose **Full**.
- Your address is the phone's address. If the phone has no VPN, that is the mobile network's
  address, which is not yours at home. If it does have one, that is the VPN's. Open the VPN app
  on the phone and check that it is on. See [the FAQ](faq.md#why-does-my-vpn-address-show-up).
- A browser that uses its own secure DNS or its own proxy setting can skip parts of this. Try
  another browser or <https://ifconfig.me> in a terminal: `curl https://ifconfig.me`.

## The connection drops

- Android: the system stopped Kite. Allow it to run without battery limits, and keep the cable in.
- iPhone: Kite left the screen. Open it again.
- A phone that changes WiFi networks gets a new address. Connect again.
- Over WiFi, a weak signal on either side shows up as stalls. Try the cable.
- After the phone drops out, Linux with the kill switch on blocks everything until it is back. Press
  Disconnect to get your normal connection. Without the switch, or on Windows and macOS, the
  computer falls back to its own connection by itself.

## Names on my home network stopped working

While connected, every name lookup goes to the phone, so a router's own name or a printer's `.lan`
name does not resolve. Disconnect to get them back. Addresses still work.

## My internet is broken after Kite crashed

Run `kite restore` (with `sudo` on Linux and macOS, in an administrator terminal on Windows). It
removes leftover routing rules and the DNS rule. Connecting again in Full mode clears them too.

## iPhone

- **Over USB, Kite does not find the iPhone.** Unlock the iPhone and tap Trust This Computer. The
  computer needs the Apple USB service: `sudo apt install usbmuxd` on Linux, Apple Mobile Device
  Support (installed with iTunes or Apple Devices) on Windows. A Mac has it. Kite says which of
  these is missing.
- **It says the Kite app is not listening.** Open Kite on the iPhone and tap Start sharing, and
  leave it on the screen.
- **The iPhone shows "wants to find devices on your local network" or computers cannot find it over
  WiFi.** Allow it in Settings, Kite, Local Network.
- **Sharing stopped when you switched apps.** That is how iOS works. Open Kite again.

## Linux

- **The window says the daemon is not running.** `sudo systemctl status kite`, and `sudo systemctl
  start kite`.
- **The window says you are not allowed.** Log out and in once after installing. To check,
  `groups` should list `kite`.
- **No tray icon.** GNOME needs the AppIndicator extension. The window still works from the menu.
- **USB permission denied.** Unplug and replug the phone after installing Kite.

## Windows

### Windows says Kite needs the WinUSB driver

Kite talks to the phone's ADB interface through a driver called WinUSB. Windows 10 and 11 attach it
to many phones by themselves; yours is one that does not. Pick one:

- Install Android's platform-tools and run `adb start-server` in a terminal. Kite then goes through
  adb.
- Or attach WinUSB to the phone's ADB interface (not the file transfer or camera one) with
  [Zadig](https://zadig.akeo.ie/): pick "Android ADB Interface", choose WinUSB, click Replace Driver.

**The USB interface is busy.** Only one program can hold it. Run `adb start-server`, or close
whatever runs `adb`.

**The window says the service is not running.** Open Services, find Kite and start it.

**The window says you are not allowed.** Your account is not in the local group `kite`. An
administrator runs `net localgroup kite NAME /add`, then you sign in again.

## macOS

- **"Kite cannot be opened" or the package will not open.** See step 3 of the
  [macOS guide](android-macos.md#3-put-kite-on-the-mac) for Open Anyway.
- **No menu bar icon.** Open Kite from Applications once.
- **USB is busy.** Run `adb kill-server`, then connect again.

More detail for every command: [cli.md](../cli.md).
