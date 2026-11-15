#!/usr/bin/env bash
# Test of the container image, with relay-dev on the Docker host standing in for the phone: pairing
# into a named volume, proxy mode used from the container's network namespace the way a sidecar
# container (network_mode: service:kite) uses it, and TUN mode with the flags docs/docker.md gives.
#
# Usage: bash scripts/container-test.sh IMAGE path/to/relay-dev
# Needs: docker, sudo, nsenter, curl, dig.
set -euo pipefail

IMAGE=$1
VOLUME=kite-test-$$
LOG=$(mktemp -d)
GATEWAY=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
RELAY=$GATEWAY:7000
relay_pid=

cleanup() {
  set +e
  docker rm -f kite-proxy kite-tun >/dev/null 2>&1
  docker volume rm "$VOLUME" >/dev/null 2>&1
  [ -n "$relay_pid" ] && kill "$relay_pid"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  for f in "$LOG"/*.log; do echo "--- $f" >&2; cat "$f" >&2; done
  for c in kite-proxy kite-tun; do
    echo "--- docker logs $c" >&2
    docker logs "$c" >&2 2>&1 || true
  done
  exit 1
}

# Waits until the tunnel of container $1 is up.
wait_connected() {
  for _ in $(seq 50); do
    docker logs "$1" 2>&1 | grep -q '^connected to ' && return
    [ "$(docker inspect -f '{{.State.Running}}' "$1")" = true ] || fail "$1 exited"
    sleep 0.2
  done
  fail "$1 did not connect"
}

# Runs a command in the network namespace of container $1, as another container sharing it would.
in_netns() {
  local pid
  pid=$(docker inspect -f '{{.State.Pid}}' "$1")
  shift
  sudo nsenter -t "$pid" -n "$@"
}

[ "$(docker image inspect -f '{{.Config.User}}' "$IMAGE")" = 65532:65532 ] || fail "the image does not run as 65532"
docker run --rm "$IMAGE" --version | grep -q '^kite ' || fail "kite --version"

"$2" "$RELAY" >"$LOG/relay.log" 2>&1 &
relay_pid=$!
for _ in $(seq 50); do
  grep -q '^relay-dev listening' "$LOG/relay.log" && break
  sleep 0.1
done

# Pair into a new named volume, typing the code relay-dev prints where the phone would show it.
shown_code() { sed -n 's/^pairing code \([0-9]\{6\}\)$/\1/p' "$LOG/relay.log" | tail -1; }
for _ in $(seq 100); do
  c=$(shown_code)
  if [ -n "$c" ]; then echo "$c"; break; fi
  sleep 0.2
done | docker run -i --rm -v "$VOLUME:/data" "$IMAGE" pair --tcp "$RELAY" >"$LOG/pair.log" 2>&1 ||
  fail "kite pair"

# The volume starts as a copy of the image's /data, so the image's user owns it and the keys in it.
data=$(docker volume inspect -f '{{.Mountpoint}}' "$VOLUME")
perms=$(sudo stat -c '%u:%g %a' "$data" "$data/kite" "$data/kite/key" "$data/kite/phones" | tr '\n' ' ')
[ "$perms" = "65532:65532 700 65532:65532 700 65532:65532 600 65532:65532 600 " ] ||
  fail "owner and mode of the volume, its directory, key and phones: $perms"

# Proxy mode.
docker run -d --name kite-proxy -v "$VOLUME:/data" "$IMAGE" connect --tcp "$RELAY" >/dev/null
wait_connected kite-proxy
in_netns kite-proxy curl -fsS -m 20 --retry 3 --retry-all-errors -o /dev/null \
  -x socks5h://127.0.0.1:1080 https://example.com || fail "proxy mode"
docker exec kite-proxy kite status | grep -q '^relay: ' || fail "kite status"
ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' kite-proxy)
if curl -sS -m 5 -o /dev/null -x "socks5h://$ip:1080" https://example.com 2>/dev/null; then
  fail "the proxy answers outside the container's loopback"
fi
docker stop -t 5 kite-proxy >/dev/null
[ "$(docker inspect -f '{{.State.ExitCode}}' kite-proxy)" = 0 ] || fail "kite-proxy did not stop cleanly"

# TUN mode: root in the container with NET_ADMIN for the device and routes, and DAC_OVERRIDE to
# read the volume, which belongs to the image's user. Nothing else.
docker run -d --name kite-tun --user 0 --cap-drop ALL --cap-add NET_ADMIN --cap-add DAC_OVERRIDE \
  --device /dev/net/tun -v "$VOLUME:/data" "$IMAGE" connect --tcp "$RELAY" --tun >/dev/null
wait_connected kite-tun
for _ in $(seq 50); do
  [ "$(in_netns kite-tun ip rule | grep -c 'lookup 5300')" -ge 3 ] && break
  sleep 0.2
done
in_netns kite-tun curl -fsS -m 20 --retry 3 --retry-all-errors -o /dev/null \
  https://1.1.1.1/cdn-cgi/trace || fail "TUN mode"
[ -n "$(in_netns kite-tun dig +short +time=5 +tries=2 @9.9.9.9 example.com)" ] ||
  fail "DNS in TUN mode"
docker stop -t 5 kite-tun >/dev/null
[ "$(docker inspect -f '{{.State.ExitCode}}' kite-tun)" = 0 ] || fail "kite-tun did not stop cleanly"

echo "container test passed"
