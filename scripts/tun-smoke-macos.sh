#!/usr/bin/env bash
# Smoke test of TUN mode on macOS, with relay-dev standing in for the phone on the loopback
# address. It checks what kite changes on the system and that all of it is gone again, after a
# clean disconnect and after the daemon was killed. The relay runs on this machine, so it cannot
# carry real traffic: relay-dev refuses every target but the loopback. What it can show is that
# packets enter the tunnel (the TCP handshake with an outside address is answered by tun2proxy and
# the connection is then closed by the relay) and that name lookups no longer reach the resolvers
# of the network.
#
# Usage: sudo bash scripts/tun-smoke-macos.sh path/to/kite path/to/relay-dev
# It changes this machine's routes and DNS settings while it runs, and puts them back.
set -euo pipefail

[ "$(id -u)" -eq 0 ] || { echo "run this as root" >&2; exit 1; }
KITE=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
RELAY_DEV=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
RELAY=127.0.0.1:7000
WORK=$(mktemp -d)
SOCK=$WORK/kite.sock
pids=""

fail() {
  echo "FAIL: $*" >&2
  for log in "$WORK"/*.log; do
    echo "--- $log" >&2
    cat "$log" >&2 || true
  done
  exit 1
}

cleanup() {
  set +e
  for pid in $pids; do kill -9 "$pid" 2>/dev/null; done
  "$KITE" restore 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

kite() { "$KITE" "$@" --socket "$SOCK"; }

wait_for() { # description, command...
  local what=$1
  shift
  for _ in $(seq 1 120); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 0.5
  done
  fail "timed out waiting for $what"
}

# What TUN mode changes, as the system reports it.
tunnel_device() { ifconfig | awk '/^utun/ { dev = $1 } /inet 198\.18\.0\.1 / { sub(":", "", dev); print dev }'; }
route_device() { route -n get "$1" | awk '/interface:/ { print $2 }'; }
dns_key() { scutil <<<'show State:/Network/Service/kite/DNS' | grep -q 'ServerAddresses'; }
resolver_listed() { scutil --dns | grep -q 'nameserver\[[0-9]*\] : 1\.1\.1\.1'; }
resolves() { [ -n "$(dscacheutil -q host -a name example.com)" ]; }

is_up() {
  local dev
  dev=$(tunnel_device)
  [ -n "$dev" ] && [ "$(route_device 1.1.1.1)" = "$dev" ] && [ "$(route_device 200.1.1.1)" = "$dev" ] \
    && dns_key && resolver_listed
}
is_clean() {
  [ -z "$(tunnel_device)" ] && ! dns_key && ! resolver_listed \
    && ! route_device 1.1.1.1 | grep -q '^utun' \
    && [ ! -s /var/db/kite/tun.state ]
}

assert_up() {
  wait_for "the tunnel" is_up
  echo "tunnel is up: $(tunnel_device), routes, DNS resolver as expected"
  # The handshake is answered inside the tunnel; the relay then refuses the target and closes it.
  # Outside the tunnel this address would answer for real, or time out (28).
  local code=0
  curl -sS -o /dev/null --connect-timeout 5 -m 10 http://1.1.1.1/ 2>/dev/null || code=$?
  case $code in
    52 | 56) echo "packets enter the tunnel (curl exit $code)" ;;
    *) fail "a connection to 1.1.1.1 ended with curl exit $code, not through the tunnel" ;;
  esac
  dscacheutil -flushcache
  if resolves; then fail "names still resolve through the network's own resolvers"; fi
  echo "name lookups no longer reach the network's resolvers"
}

assert_clean() {
  wait_for "the tunnel to be removed ($1)" is_clean
  wait_for "names to resolve again ($1)" resolves
  echo "clean $1"
}

resolves || fail "this machine cannot resolve names before the test"
assert_clean "before the test"

"$RELAY_DEV" "$RELAY" --refuse-external >"$WORK/relay.log" 2>&1 &
pids="$pids $!"
wait_for "relay-dev" grep -q listening "$WORK/relay.log"
"$KITE" daemon --socket "$SOCK" --state-dir "$WORK/state" >"$WORK/daemon.log" 2>&1 &
daemon=$!
pids="$pids $daemon"
wait_for "the daemon" grep -q 'daemon ready' "$WORK/daemon.log"
[ "$(stat -f '%Lp' "$SOCK")" = 660 ] || fail "the socket is not 0660"

# Pairing: kite pair asks for the code the phone would show.
{
  for _ in $(seq 1 100); do
    code=$(grep -o 'pairing code [0-9]*' "$WORK/relay.log" | tail -1 | grep -o '[0-9]*$' || true)
    if [ -n "$code" ]; then
      echo "$code"
      break
    fi
    sleep 0.3
  done
} | kite pair --tcp "$RELAY" || fail "pairing failed"

# A clean disconnect takes everything away.
kite connect --tcp "$RELAY" --tun
assert_up
kite status
kite disconnect
assert_clean "after disconnect"

# A daemon that is killed leaves the resolver behind; restore takes it away.
kite connect --tcp "$RELAY" --tun
assert_up
kill -9 "$daemon"
wait "$daemon" 2>/dev/null || true
sleep 2
echo "after the kill: device=$(tunnel_device) resolver=$(dns_key && echo yes || echo no)"
"$KITE" restore || fail "kite restore failed"
assert_clean "after kill and restore"
echo "ok"
