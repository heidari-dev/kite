# One browser through the phone

Kite can send a single browser through your phone and leave every other program on your normal
connection. It uses Kite's **Proxy only** mode, which needs no special rights and does not change
the computer's network settings.

## 1. Start Kite in Proxy only

In the Kite window, choose **Proxy only** under Share and press **Connect**. From a terminal,
`kite connect --usb` (or `--wifi`) without `--tun` does the same.

Kite now runs a SOCKS5 proxy at `127.0.0.1:1080` on this computer. Only programs set to use it go
through the phone.

## 2a. Firefox

Firefox has its own proxy setting, so only Firefox changes.

1. Open Settings, stay on General, scroll to Network Settings and click **Settings**.
2. Choose **Manual proxy configuration**. Put `127.0.0.1` in SOCKS Host and `1080` in Port, and
   pick **SOCKS v5**. Leave the HTTP proxy fields empty.
3. Tick **Proxy DNS when using SOCKS v5**, so that the names you look up also go through the phone.
4. Click OK.

To go back, pick **No proxy** in the same place. To keep your usual Firefox on your normal
connection, do this in a second profile (open `about:profiles`).

## 2b. Chrome, Edge, Brave

These browsers use the system's proxy setting, which other programs read too. Start them with a
flag instead. Close every window of that browser first: the flag only counts when the browser starts.

Windows:

```
"C:\Program Files\Google\Chrome\Application\chrome.exe" --proxy-server="socks5://127.0.0.1:1080"
```

Linux:

```
google-chrome --proxy-server="socks5://127.0.0.1:1080"
```

macOS:

```
open -na "Google Chrome" --args --proxy-server="socks5://127.0.0.1:1080"
```

With `socks5://` these browsers send the names you look up to the proxy as well. Start the browser
normally again to go back.

## 3. Check it

Open <https://ifconfig.me> in that browser. It shows the address your phone's VPN or mobile network
gives it. Any other browser still shows your computer's own address.

## What does not go through the phone

- Every other program on the computer.
- Calls and video chat inside the browser (WebRTC) can use the computer's own connection, and a
  page can read that address through them. If that matters, use Firefox and turn WebRTC off:
  open `about:config` and set `media.peerconnection.enabled` to `false`.

To send the whole computer through the phone instead, choose **Full** under Share.
