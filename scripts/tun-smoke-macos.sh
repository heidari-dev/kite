#!/usr/bin/env bash
# Smoke test of TUN mode on macOS, with relay-dev standing in for the phone on the loopback
# address. It checks what kite changes on the system and that all of it is gone again, after a
# clean disconnect and after the daemon was killed. The relay runs on this machine, so it cannot
# carry real traffic: relay-dev refuses every target but the loopback. What it can show is that
# packets enter the tunnel (the TCP handshake with an outside address is answered by tun2proxy and
# the connection is then closed by the relay) and that name lookups no longer reach the resolvers
# of the network.
#
# It also tests the kill switch: with the tunnel's routes taken away, as if the device were gone, a
# connection to 1.1.1.1 and a DNS query to 8.8.8.8 must fail while the kill switch is on and the
# relay is dead, and work again after disconnect. A daemon that is killed leaves the block up until
# `kite restore`. It needs a working Internet connection for that, and pf must be as it was (off
# or on) before and after.
#
# The tunnel must be the primary network service with the default route through 198.18.0.2, which
# is what a second VPN looks at to find its way out, and the kill switch must hold no pass rule: a
# block in another anchor (a second VPN's kill switch, stood in for by an anchor of the test) must
# still stop traffic into the tunnel while the kill switch is on.
#
# Usage: sudo bash scripts/tun-smoke-macos.sh path/to/kite path/to/relay-dev
# It changes this machine's routes, DNS settings and pf rules while it runs, and puts them back.
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
  pfctl -a com.apple/zzz-kite-test -F rules 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

kite() { "$KITE" "$@" --socket "$SOCK"; }
# Read the whole status first: grep -q stops at the first match, and under pipefail the
# broken pipe that leaves kite with would fail the check.
kite_has() { grep -q "$1" <<<"$(kite status)"; }

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
anchor_rules() { pfctl -a com.apple/kite -s rules 2>/dev/null; }
block_rules() { pfctl -a com.apple/kite/block -s rules 2>/dev/null; }
service_key() { scutil <<<'show State:/Network/Service/kite/IPv4' | grep -q 'Router'; }
default_gateway() { route -n get default 2>/dev/null | awk '/gateway:/ { print $2 }'; }
# The first interface of `scutil --nwi` for IPv4 is the primary one.
primary_interface() { scutil --nwi | awk '/^IPv4 network interface information/ { on = 1; next } on && / : flags/ { print $1; exit }'; }
pf_status() { pfctl -s info 2>/dev/null | awk '/^Status:/ { print $2 }'; }
PF_BEFORE=$(pf_status)

# The rules kite loads, for a sample IPv4 and IPv6 relay, must parse under this pfctl (the loopback
# relay of this test never loads the relay lines). Keep in step with blocks() in tun_mac.rs.
pf_parses() { # rules on stdin
  pfctl -n -f - >/dev/null 2>"$WORK/pf-syntax.log" || { cat "$WORK/pf-syntax.log" >&2; echo "FAIL: pfctl does not parse the kill switch's rules" >&2; exit 1; }
}
pf_parses <<'RULES'
anchor "block" out on ! lo0 all
RULES
pf_parses <<'RULES'
block return out quick on ! utun9 inet proto tcp from any to ! 203.0.113.9
block return out quick on ! utun9 inet proto tcp from any to 203.0.113.9 port != 47810
block return out quick on ! utun9 inet6 proto tcp all
block return out quick on ! utun9 inet6 proto tcp from any to ! 2001:db8::9
block return out quick on ! utun9 inet6 proto tcp from any to 2001:db8::9 port != 47810
block return out quick on ! utun9 inet proto tcp all
block return out quick on ! utun9 proto tcp all
block return out quick on ! utun9 inet proto udp from any port != 68
block return out quick on ! utun9 inet proto udp from any port 68 to any port != 67
block return out quick on ! utun9 inet6 proto udp from any port != 546
block return out quick on ! utun9 inet6 proto udp from any port 546 to any port != 547
block return out quick on ! utun9 inet proto icmp all
block return out quick on ! utun9 inet6 proto icmp6 all icmp6-type { 128, 129, 139, 140 }
block return out quick on ! utun9 proto { 4, 41, 47, 50, 51, 103, 112, 115, 132 } all
RULES

# A connection and a DNS query that do not use the tunnel's routes, to servers outside this machine.
reach() { curl -sS -o /dev/null --connect-timeout 5 -m 8 http://1.1.1.1/ >/dev/null 2>&1; }
# An answer is an address: dig prints its errors ("no servers could be reached") on stdout too.
reach_dns() { dig +time=3 +tries=1 +short @8.8.8.8 example.com 2>/dev/null | grep -Eq '^[0-9]+[.][0-9]+[.][0-9]+[.][0-9]+$'; }
# What got out, and what pf did about it, for the log of a failure.
explain_leak() {
  echo "--- dig @8.8.8.8 example.com" >&2
  dig +time=3 +tries=1 @8.8.8.8 example.com >&2 2>&1 || true
  echo "--- pf rules of the kill switch, with counters" >&2
  pfctl -a com.apple/kite -s rules -v >&2 2>&1 || true
  pfctl -a com.apple/kite/block -s rules -v >&2 2>&1 || true
  echo "--- pf main ruleset" >&2
  pfctl -s rules >&2 2>&1 || true
  echo "--- routes to 8.8.8.8" >&2
  route -n get 8.8.8.8 >&2 2>&1 || true
}
# As if the device were gone: the service and the routes of the tunnel off, the rest of the tunnel
# as it was.
drop_routes() {
  scutil <<<'remove State:/Network/Service/kite/IPv4'
  sleep 1
  for half in 0.0.0.0/1 128.0.0.0/1; do route -n delete -inet "$half" >/dev/null 2>&1 || true; done
  for half in ::/1 8000::/1; do route -n delete -inet6 "$half" >/dev/null 2>&1 || true; done
  sleep 1
}
assert_open() {
  reach || fail "$1: a connection to 1.1.1.1 does not get out"
  reach_dns || fail "$1: a DNS query to 8.8.8.8 gets no answer"
}
assert_blocked() {
  if reach; then fail "$1: a connection to 1.1.1.1 got out of the computer"; fi
  if reach_dns; then
    explain_leak
    fail "$1: a DNS query to 8.8.8.8 got out of the computer"
  fi
}

is_up() {
  local dev
  dev=$(tunnel_device)
  [ -n "$dev" ] && [ "$(route_device 1.1.1.1)" = "$dev" ] && [ "$(route_device 200.1.1.1)" = "$dev" ] \
    && dns_key && resolver_listed
}
is_clean() {
  [ -z "$(tunnel_device)" ] && ! dns_key && ! service_key && ! resolver_listed \
    && ! route_device 1.1.1.1 | grep -q '^utun' \
    && [ -z "$(anchor_rules)" ] && [ -z "$(block_rules)" ] && [ "$(pf_status)" = "$PF_BEFORE" ] \
    && [ ! -s /var/db/kite/tun.state ]
}

assert_up() {
  wait_for "the tunnel" is_up
  # What a second VPN reads to find its way out (wg-quick and OpenVPN the default gateway, a Network
  # Extension VPN the primary interface).
  [ "$(default_gateway)" = 198.18.0.2 ] || { route -n get default >&2; fail "the default route goes through $(default_gateway), not 198.18.0.2"; }
  [ "$(primary_interface)" = "$(tunnel_device)" ] || { scutil --nwi >&2; fail "the primary interface is $(primary_interface), not the tunnel"; }
  echo "tunnel is up: $(tunnel_device), primary with the default route, DNS resolver as expected"
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

start_relay() { # log name
  "$RELAY_DEV" "$RELAY" --refuse-external >"$WORK/$1.log" 2>&1 &
  relay=$!
  pids="$pids $relay"
  wait_for "relay-dev" grep -q listening "$WORK/$1.log"
}

# Pairing: kite pair asks for the code the phone would show.
pair() { # log name
  {
    for _ in $(seq 1 100); do
      code=$(grep -o 'pairing code [0-9]*' "$WORK/$1.log" | tail -1 | grep -o '[0-9]*$' || true)
      if [ -n "$code" ]; then
        echo "$code"
        break
      fi
      sleep 0.3
    done
  } | kite pair --tcp "$RELAY" || fail "pairing failed"
}

start_relay relay
"$KITE" daemon --socket "$SOCK" --state-dir "$WORK/state" >"$WORK/daemon.log" 2>&1 &
daemon=$!
pids="$pids $daemon"
wait_for "the daemon" grep -q 'daemon ready' "$WORK/daemon.log"
[ "$(stat -f '%Lp' "$SOCK")" = 660 ] || fail "the socket is not 0660"

pair relay

# A clean disconnect takes everything away.
kite connect --tcp "$RELAY" --tun
assert_up
kite status
# Without the kill switch nothing stops a connection that does not use the tunnel's routes; this is
# what the next part must see blocked.
drop_routes
assert_open "without the kill switch, tunnel routes gone"
kite disconnect
assert_clean "after disconnect"

# The kill switch: the phone goes away, the tunnel's routes go too, and nothing may leave.
kite connect --tcp "$RELAY" --tun --kill-switch
assert_up
kite_has 'kill switch: on' || fail "status does not show the kill switch"
# Another program's block of traffic into the tunnel still stops it: the kill switch passes
# nothing. Without the block the handshake is answered inside the tunnel (curl 52 or 56).
dev=$(tunnel_device)
pfctl -a com.apple/zzz-kite-test -f - <<<"block return out quick on $dev proto tcp from any to 1.1.1.1 port 80"   || fail "loading the test anchor failed"
code=0
curl -sS -o /dev/null --connect-timeout 5 -m 10 http://1.1.1.1/ 2>/dev/null || code=$?
pfctl -a com.apple/zzz-kite-test -F rules || fail "flushing the test anchor failed"
[ "$code" = 7 ] || fail "another anchor's block did not hold with the kill switch on (curl exit $code)"
echo "kill switch: another program's block still holds"
# The link to the relay was dialled before the rules went up; it must survive them.
sleep 3
kite_has 'state: connected' || fail "the tunnel dropped when the kill switch went up"
kill -9 "$relay"
wait "$relay" 2>/dev/null || true
reconnecting() { kite_has 'state: reconnecting'; }
wait_for "the tunnel to notice the lost relay" reconnecting
anchor_rules | grep -q 'anchor "block"' || fail "the kill switch's rules are not in pf"
block_rules | grep -q 'block return out quick on ! utun' || fail "the kill switch's blocks are not in pf"
if { anchor_rules; block_rules; } | grep -q '^pass'; then fail "the kill switch has a pass rule"; fi
drop_routes
assert_blocked "kill switch on, relay gone"
echo "kill switch: nothing gets out while the relay is gone"
kite disconnect
assert_clean "after disconnect with the kill switch"
assert_open "after disconnect with the kill switch"
echo "kill switch: lifted by disconnect"

# A new relay, a new pairing.
start_relay relay2
pair relay2

# A firewall the user switched on stays on, and one that was off goes off again (checked above).
if [ "$PF_BEFORE" != Enabled ]; then pfctl -e >/dev/null 2>&1 || true; fi
kite connect --tcp "$RELAY" --tun --kill-switch
assert_up
kite disconnect
gone() { [ -z "$(tunnel_device)" ] && [ -z "$(anchor_rules)" ] && [ -z "$(block_rules)" ]; }
wait_for "the tunnel to be removed" gone
[ "$(pf_status)" = Enabled ] || fail "kite switched off a firewall that was on"
if [ "$PF_BEFORE" != Enabled ]; then pfctl -d >/dev/null 2>&1 || true; fi
wait_for "names to resolve again" resolves
echo "pf stays on when it was on"

# A daemon that is killed leaves the resolver and the block behind; restore takes them away.
kite connect --tcp "$RELAY" --tun --kill-switch
assert_up
kill -9 "$daemon"
wait "$daemon" 2>/dev/null || true
sleep 2
echo "after the kill: device=$(tunnel_device) resolver=$(dns_key && echo yes || echo no)"
assert_blocked "after the daemon was killed with the kill switch on"
"$KITE" restore || fail "kite restore failed"
assert_clean "after kill and restore"
assert_open "after kill and restore"
echo "ok"
