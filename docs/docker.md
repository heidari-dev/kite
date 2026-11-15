# Kite in a container

The image `ghcr.io/heidari-dev/kite` holds the `kite` command and nothing else. Use it to give a
Docker stack or a home server the phone's connection: Kite runs in its own container, and the
containers that should go through the phone join its network namespace. For one computer, the
packages on the [releases page](https://github.com/heidari-dev/kite/releases) are simpler.

Tags: `edge` follows the main branch, each release gets its version (such as `0.1.0`), and `latest`
is the newest release that is not a pre-release. The examples use `edge`; pin a version for a setup
that should not change under you. The images are for `linux/amd64` and `linux/arm64`, built only by
the [container workflow](../.github/workflows/container.yml), with build provenance you can check:

```
gh attestation verify oci://ghcr.io/heidari-dev/kite:edge --repo heidari-dev/kite
```

## How it runs

The container runs `kite --foreground`: there is no daemon, every command runs in the container's
own process. It runs as user 65532. Its key and the list of paired phones are in `/data/kite`,
which Kite creates readable by that user only and refuses to use if others can read or write it.
Keep `/data` on a named volume, as below, so pairing survives a new container; a new named volume
starts out owned by 65532. A bind mount works if the directory belongs to 65532.

`docker exec kite kite status` shows the running tunnel.

## Pair

Open pairing in the app on the phone, then:

```
docker run --rm -it -v kite:/data ghcr.io/heidari-dev/kite:edge pair --wifi 192.168.1.20
```

Compare the fingerprint the command prints with the one the phone shows, allow it on the phone and
type the 6-digit code. Pair over USB with the flags from [USB](#usb).

## Proxy mode

Kite serves SOCKS5 on `127.0.0.1:1080` in its own network namespace. `--listen` only takes a loopback
address, so publishing the port with `-p` does not reach it, and nothing outside that namespace can
use the phone. Containers that should use it join the namespace and point at
`socks5://127.0.0.1:1080` (`socks5h://` makes the phone look up the names too).

With Compose:

```yaml
services:
  kite:
    image: ghcr.io/heidari-dev/kite:edge
    command: connect --wifi 192.168.1.20
    volumes:
      - kite:/data
    restart: unless-stopped

  app:
    image: curlimages/curl
    network_mode: service:kite
    environment:
      ALL_PROXY: socks5h://127.0.0.1:1080
    command: https://ifconfig.me

volumes:
  kite:
```

`ALL_PROXY` covers curl and many other programs; give the others the proxy in their own settings.
With `docker run` it is `--network container:kite`:

```
docker run -d --name kite -v kite:/data ghcr.io/heidari-dev/kite:edge connect --wifi 192.168.1.20
docker run --rm --network container:kite curlimages/curl -x socks5h://127.0.0.1:1080 https://ifconfig.me
```

A container that shares Kite's namespace loses its network when Kite's container stops, restarts
or is recreated, and gets it back only when it is restarted too.

## TUN mode

TUN mode sends everything in the shared namespace through the phone, DNS included, so the other
containers need no proxy settings. It needs `CAP_NET_ADMIN` and `/dev/net/tun`. Docker does not
give capabilities to a user other than root, so TUN mode runs as root in the container with every
other capability dropped; `DAC_OVERRIDE` lets root read the volume, which belongs to user 65532.

```
docker run -d --name kite --user 0 --cap-drop ALL --cap-add NET_ADMIN --cap-add DAC_OVERRIDE \
  --device /dev/net/tun -v kite:/data ghcr.io/heidari-dev/kite:edge connect --wifi 192.168.1.20 --tun
```

With Compose:

```yaml
services:
  kite:
    image: ghcr.io/heidari-dev/kite:edge
    command: connect --wifi 192.168.1.20 --tun
    user: "0"
    cap_drop: [ALL]
    cap_add: [NET_ADMIN, DAC_OVERRIDE]
    devices: [/dev/net/tun]
    volumes:
      - kite:/data
    restart: unless-stopped
```

Pair first, as the image's own user: a file that a root run creates belongs to root, and the
image's user cannot read it later. Do not combine TUN mode with `--network host`, which would hand
the host's routing to Kite; the [Linux package](guides/android-linux.md) is the way to send a whole
computer through the phone.

## WiFi

`--wifi ADDRESS` works from Docker's default network: Docker forwards the connection out of the
host. Leaving the address out makes Kite look for the phone with mDNS, and that question only
reaches the WiFi with `--network host`. The proxy then listens on the host's loopback, where every
local user can use it. A fixed address for the phone (a DHCP reservation on the router) avoids
both. `--tcp HOST:PORT` dials a relay at that address.

## USB

Kite talks to an Android phone over USB directly, without an adb server. Give the container the USB
devices and the group that may open the phone:

```
docker run -d --name kite --device /dev/bus/usb --group-add "$(getent group kite | cut -d: -f3)" \
  -v kite:/data ghcr.io/heidari-dev/kite:edge connect --usb
```

The `kite` group and the udev rule `51-kite-adb.rules` that gives it the phone come with the
[Linux package](../packaging/linux/README.md); without the package, create the group and install
the rule by hand. Docker passes in the devices that exist when the container starts, so plug the
phone in first and restart the container after plugging it in again. The phone asks to allow USB
debugging for the key in `/data/kite/adbkey`; leave "Always allow" off unless you trust the server
(see [cli.md](cli.md)).

An iPhone goes through the host's usbmuxd: use `-v /var/run/usbmuxd:/var/run/usbmuxd` in place of
`--device` and `--group-add`.

## Build it yourself

```
docker build -f packaging/container/Dockerfile -t kite .
```

The build cross-compiles a static binary, so `--platform linux/arm64` works on an x86 machine
without emulation.
