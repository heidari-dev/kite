# Kite guides

Pick the page that matches your phone and your computer. Each one takes about ten minutes the first
time and about ten seconds after that.

| Phone | Computer | Guide |
|-------|----------|-------|
| Android | Linux | [Android and Linux](android-linux.md) |
| Android | Windows | [Android and Windows](android-windows.md) |
| Android | Mac | [Android and macOS](android-macos.md) |
| iPhone | Linux, Windows or Mac | [iPhone](iphone.md) |

Something went wrong: [Troubleshooting](troubleshooting.md). Wondering what Kite does, whether it is
safe or what it costs you: [FAQ](faq.md).

## The idea in one minute

Your phone shares its internet connection with your computer. Normal tethering hands the computer a
direct line to the mobile network, and on most phones that line skips the phone's VPN. Kite sends
the computer's traffic to an app on the phone, and the app makes each connection itself. The
websites see whatever the phone itself would show them: its VPN, its proxy, or its mobile network.

You need three things:

1. The Kite app on the phone.
2. Kite on the computer.
3. A one-time pairing, so that the two only talk to each other. You type a 6-digit code that the
   phone shows.

How it works inside is in [architecture.md](../architecture.md).
