# Kite on Windows

Kite shares your phone's connection with a Windows computer, with the phone's VPN or proxy intact.
This page is about installing it, using it from the Start menu and removing it. The command line is in
[cli.md](cli.md), what Kite does to the system and why in [threat-model.md](threat-model.md).

Windows 10 or 11, 64-bit.

## Install

Download `kite-VERSION-x86_64.msi` from the [releases page](https://github.com/heidari-dev/kite/releases)
and open it. Windows asks once whether to let the installer make changes (the UAC prompt); there are
no other questions.

### The SmartScreen warning

The installer is not signed yet, because a code signing certificate costs money the project does not
have. Windows therefore shows a blue "Windows protected your PC" window that says the publisher is
unknown. This is expected. To continue, click **More info**, then **Run anyway**.

You do not have to trust the download on faith. Compare it with what the project published:

```
Get-FileHash kite-VERSION-x86_64.msi -Algorithm SHA256       # compare with SHA256SUMS on the release
gh attestation verify kite-VERSION-x86_64.msi --repo heidari-dev/kite
```

The release also holds a Sigstore bundle for the file (`kite-VERSION-x86_64.msi.sigstore.json`), see
[releasing.md](releasing.md). The installer is built by the release workflow from the tagged commit and
by nothing else.

### What it installs

- `C:\Program Files\Kite`: `kite.exe` (the command line and the service), `kite-desktop.exe` (the
  window and tray icon), `wintun.dll` with its license, and Kite's license.
- The Windows service **Kite**, started at boot and restarted if it fails. It runs as LocalSystem,
  because making the network adapter and changing routes need administrator rights; the installer cuts
  its token down to the four privileges it uses.
- A Start menu entry, and the app starts hidden in the tray at every sign-in.
- The local group **kite**, with you (the person who installed) in it, and a place for the service's
  key and settings in `C:\ProgramData\Kite`, which only administrators can open.

Installing again over an older version upgrades it and keeps your key and the paired phones.

## Use

1. Open **Kite** from the Start menu. Its icon sits in the tray; Windows 11 puts a new icon in the
   overflow flyout (the arrow at the right of the taskbar), and you can drag it out.
2. Install the Kite app on the phone, turn on USB debugging, connect the phone by USB or put it on the
   same WiFi, and start sharing on the phone. USB needs `adb` for now: install Android's
   platform-tools and run `adb start-server` (the installer will not ship adb).
3. Click **Pair a phone**. Pairing decides where all of your traffic may go, so only an administrator
   may do it: Windows asks for permission, and a window opens in which you allow this computer on the
   phone and type the 6-digit code. Do this once per phone.
4. Click **Connect**. All of this computer's traffic and DNS go through the phone.

From a terminal the same things are `kite pair --usb` (in an administrator prompt), `kite connect --usb
--tun` and `kite disconnect`.

There is no kill switch on Windows yet ([#88](https://github.com/heidari-dev/kite/issues/88)): if the
link to the phone drops, the tunnel ends and Windows returns to its own network.

### Letting someone else use it

Everyone who may start and stop the tunnel has to be allowed. As an administrator, either add the
person to the group (they sign in again afterwards):

```
net localgroup kite NAME /add
```

or allow them at once, which takes effect when the service starts again:

```
kite allow NAME
sc stop Kite
sc start Kite
```

### If something does not work

- The window says the service is not running: open **Services**, find **Kite** and start it, or run
  `sc start Kite` as an administrator.
- It says you are not allowed to use the service: you are not in the group `kite` (see above).
- Names do not resolve on the local network while connected (a router's own name, say): the tunnel
  sends every lookup to the phone. Disconnect to get them back.
- After a crash of the service the DNS rule can stay for a moment; the service removes it when it
  starts again, or run `kite restore` as an administrator.

## Remove

**Settings, Apps, Kite, Uninstall** (or `msiexec /x` on the MSI). It stops the service, runs
`kite restore`, and removes the programs, the service, the Start menu entry, the autostart entry, the
group `kite` and `C:\ProgramData\Kite` (the service's key and paired phones). The app's own
settings in `%APPDATA%\kite` are yours and stay.

## Wintun

Kite's TUN mode uses [Wintun](https://www.wintun.net/), the driver of WireGuard, which the installer
puts next to `kite.exe` unmodified together with its license. Kite checks that the DLL is signed by
WireGuard LLC before it loads it. For a build from source, `scripts\fetch-wintun.ps1` downloads it and
checks its hash.
