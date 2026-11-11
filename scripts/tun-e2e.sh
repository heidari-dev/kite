#!/usr/bin/env bash
# End-to-end test of TUN mode. kite runs in a network namespace whose only link leads to the host,
# where relay-dev listens. The namespace also has a direct NAT path to the Internet, so anything that
# bypasses the tunnel would work and is caught by packet counters instead of failing silently.
#
# Usage: sudo scripts/tun-e2e.sh path/to/kite path/to/relay-dev
# Needs: iproute2, iptables, capsh, curl, dig.
set -euo pipefail

NS=kite-e2e
HOST_IP=10.200.0.1
RELAY=$HOST_IP:7000
BIN=$(mktemp -d)
chmod 755 "$BIN"
install -m 755 "$1" "$BIN/kite"
install -m 755 "$2" "$BIN/relay-dev"
relay_pid=
kite_pid=
forward=$(sysctl -n net.ipv4.ip_forward)

cleanup() {
  set +e
  [ -n "$kite_pid" ] && kill -9 "$kite_pid" 2>/dev/null
  [ -n "$relay_pid" ] && kill "$relay_pid" 2>/dev/null
  ip netns del "$NS" 2>/dev/null
  iptables -t nat -D POSTROUTING -s 10.200.0.0/24 -j MASQUERADE 2>/dev/null
  iptables -D FORWARD -s 10.200.0.0/24 -j ACCEPT 2>/dev/null
  iptables -D FORWARD -d 10.200.0.0/24 -j ACCEPT 2>/dev/null
  sysctl -qw net.ipv4.ip_forward="$forward"
  rm -rf "/etc/netns/$NS" "$BIN"
  true
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  if [ -f "$BIN/kite.log" ]; then cat "$BIN/kite.log" >&2; fi
  exit 1
}
in_ns() { ip netns exec "$NS" "$@"; }
rss() { awk '/VmRSS/ {print $2 " KiB"}' "/proc/$1/status"; }
# kite runs as nobody with only CAP_NET_ADMIN, as an ambient capability, the way a systemd unit
# would start it. Every step execs, so $! is kite itself.
as_nobody=(ip netns exec "$NS" capsh --caps="cap_net_admin+eip cap_setpcap,cap_setuid,cap_setgid+ep"
  --keep=1 --user=nobody --addamb=cap_net_admin --)

# Namespace with a veth link to the host and a direct NAT path out.
ip netns add "$NS"
ip link add veth-host type veth peer name veth-kite netns "$NS"
ip addr add "$HOST_IP/24" dev veth-host
ip link set veth-host up
in_ns ip link set lo up
in_ns ip addr add 10.200.0.2/24 dev veth-kite
in_ns ip link set veth-kite up
in_ns ip route add default via "$HOST_IP"
sysctl -qw net.ipv4.ip_forward=1
iptables -t nat -A POSTROUTING -s 10.200.0.0/24 -j MASQUERADE
iptables -I FORWARD -s 10.200.0.0/24 -j ACCEPT
iptables -I FORWARD -d 10.200.0.0/24 -j ACCEPT
# The namespace's resolver is a LAN address, as with a home router.
mkdir -p "/etc/netns/$NS"
echo "nameserver $HOST_IP" > "/etc/netns/$NS/resolv.conf"

in_ns curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "no direct path out of the namespace"

# Everything that leaves the namespace other than the relay connection counts as a leak.
in_ns iptables -A OUTPUT -o veth-kite -p tcp -d "$HOST_IP" --dport 7000 -j ACCEPT
in_ns iptables -A OUTPUT -o veth-kite -j ACCEPT
in_ns ip6tables -A OUTPUT -o veth-kite -p ipv6-icmp -j ACCEPT
in_ns ip6tables -A OUTPUT -o veth-kite -j ACCEPT
leaks() {
  local v4 v6
  v4=$(in_ns iptables -L OUTPUT -v -n -x | awk 'NR == 4 {print $1}')
  v6=$(in_ns ip6tables -L OUTPUT -v -n -x | awk 'NR == 4 {print $1}')
  echo $((v4 + v6))
}

# Another program's rule at kite's priority (Tailscale-style) must survive every kite exit path.
in_ns ip rule add pref 5300 lookup 52
foreign_rule() { in_ns ip rule | grep -q "^5300:.*lookup 52"; }

"$BIN/relay-dev" "$RELAY" > "$BIN/relay.log" 2>&1 &
relay_pid=$!

start_kite() {
  "${as_nobody[@]}" -c "exec $BIN/kite connect --tcp $RELAY --tun" > "$BIN/kite.log" 2>&1 &
  kite_pid=$!
  for _ in $(seq 50); do
    # IPv6 rules are added last.
    if in_ns ip -6 rule | grep -q "lookup 5300"; then
      sleep 1
      return
    fi
    kill -0 "$kite_pid" 2>/dev/null || fail "kite exited"
    sleep 0.2
  done
  fail "kite did not set up routing"
}

start_kite
[ "$(stat -c %u "/proc/$kite_pid")" = "$(id -u nobody)" ] || fail "kite is not running as nobody"
for set in CapPrm CapEff CapAmb; do
  grep -q "^$set:\s*0000000000001000$" "/proc/$kite_pid/status" || fail "$set is not CAP_NET_ADMIN only"
done
"${as_nobody[@]}" -c "exec $BIN/kite restore" 2>/dev/null && fail "kite restore ran while kite was up"
in_ns ip rule | grep -q "not from all fwmark" || fail "kite restore removed live rules"
echo "kite idle RSS: $(rss "$kite_pid")"

echo "== traffic goes through the tunnel"
in_ns curl -sS --max-time 20 https://1.1.1.1/cdn-cgi/trace | grep -q '^ip=' || fail "curl through the tunnel"
in_ns curl -sS --max-time 20 -o /dev/null https://example.com/ || fail "curl by name through the tunnel"

echo "== DNS goes through the tunnel"
for q in "example.com" "+tcp example.com" "@9.9.9.9 example.com" "@9.9.9.9 +tcp example.com"; do
  # shellcheck disable=SC2086
  out=$(in_ns dig +short +time=10 +tries=1 $q A) || fail "dig $q"
  [ -n "$out" ] || fail "dig $q returned nothing"
done
[ "$(leaks)" = 0 ] || fail "$(leaks) packets left outside the tunnel"

echo "== load"
loads=()
for _ in $(seq 20); do
  in_ns curl -sS --max-time 60 -o /dev/null "https://speed.cloudflare.com/__down?bytes=2000000" &
  loads+=($!)
done
sleep 2
echo "kite RSS with 20 downloads: $(rss "$kite_pid")"
for pid in "${loads[@]}"; do wait "$pid" || fail "parallel downloads"; done
[ "$(leaks)" = 0 ] || fail "$(leaks) packets left outside the tunnel under load"
echo "kite RSS after the downloads: $(rss "$kite_pid")"

echo "== SIGTERM restores routing"
kill -TERM "$kite_pid"
wait "$kite_pid" || fail "kite exited with an error on SIGTERM"
kite_pid=
if in_ns ip link show kite0 2>/dev/null; then fail "kite0 still exists"; fi
if in_ns ip rule | grep -q "lookup 5300"; then fail "rules left after SIGTERM"; fi
if in_ns ip -6 rule | grep -q "lookup 5300"; then fail "IPv6 rules left after SIGTERM"; fi
in_ns curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "direct path broken after SIGTERM"
foreign_rule || fail "SIGTERM removed another program's rule"

echo "== SIGKILL leaves the network working, kite restore cleans up"
start_kite
kill -9 "$kite_pid"
wait "$kite_pid" 2>/dev/null || true
kite_pid=
if in_ns ip link show kite0 2>/dev/null; then fail "kite0 survived SIGKILL"; fi
in_ns ip rule | grep -q "lookup 5300" || fail "expected leftover rules after SIGKILL"
in_ns curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "direct path broken after SIGKILL"
in_ns dig +short +time=5 +tries=1 @1.1.1.1 example.com A > /dev/null || fail "DNS broken after SIGKILL"
"${as_nobody[@]}" -c "exec $BIN/kite restore" || fail "kite restore"
if in_ns ip rule | grep -q "lookup 5300"; then fail "rules left after kite restore"; fi
foreign_rule || fail "kite restore removed another program's rule"

echo "all TUN mode checks passed"
