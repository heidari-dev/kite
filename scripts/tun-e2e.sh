#!/usr/bin/env bash
# End-to-end test of TUN mode with two network namespaces standing in for two hosts:
#
#   pc (kite --tun) --- phone (relay-dev) --- host --- Internet
#   10.200.0.2          10.200.0.1 / 10.201.0.2        10.201.0.1
#
# The phone also forwards and NATs the pc's packets, so anything that bypasses the tunnel would
# work and is caught by packet counters instead of failing silently. Traffic has left through the
# relay when the pc sent nothing but the relay connection, the phone forwarded nothing, and the
# phone's own sockets carried the connections.
#
# Usage: sudo bash scripts/tun-e2e.sh path/to/kite path/to/relay-dev
# Needs: iproute2, iptables, capsh, curl, dig, python3.
set -euo pipefail

PC=kite-pc
PHONE=kite-phone
RELAY=10.200.0.1:7000
HOST_IP=10.201.0.1
UDP_ECHO=$HOST_IP:9999
BIN=$(mktemp -d)
ROOT=$(pwd)
chmod 755 "$BIN"
install -m 755 "$1" "$BIN/kite"
install -m 755 "$2" "$BIN/relay-dev"
# nobody has no home; its key and paired phones go here.
CONF=$BIN/conf
install -d -m 700 -o nobody "$CONF"
pids=()
kite_pid=
forward=$(sysctl -n net.ipv4.ip_forward)

cleanup() {
  set +e
  [ -n "$kite_pid" ] && kill -9 "$kite_pid" 2>/dev/null
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null; done
  ip netns del "$PC" 2>/dev/null
  ip netns del "$PHONE" 2>/dev/null
  iptables -t nat -D POSTROUTING -s 10.201.0.0/24 -j MASQUERADE 2>/dev/null
  iptables -D FORWARD -s 10.201.0.0/24 -j ACCEPT 2>/dev/null
  iptables -D FORWARD -d 10.201.0.0/24 -j ACCEPT 2>/dev/null
  sysctl -qw net.ipv4.ip_forward="$forward"
  rm -rf "/etc/netns/$PC" "$BIN"
  true
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  if [ -f "$BIN/kite.log" ]; then cat "$BIN/kite.log" >&2; fi
  dmesg | grep kite-leak | tail -20 >&2
  exit 1
}
pc() { ip netns exec "$PC" "$@"; }
phone() { ip netns exec "$PHONE" "$@"; }
rss() { awk '/VmRSS/ {print $2 " KiB"}' "/proc/$1/status"; }
# kite runs as nobody with only CAP_NET_ADMIN, as an ambient capability, the way a systemd unit
# would start it. Every step execs, so $! is kite itself.
as_nobody=(ip netns exec "$PC" capsh --caps="cap_net_admin+eip cap_setpcap,cap_setuid,cap_setgid+ep"
  --keep=1 --user=nobody --addamb=cap_net_admin --)

# host <-> phone, with NAT to the Internet.
ip netns add "$PHONE"
ip link add veth-host type veth peer name veth-up netns "$PHONE"
ip addr add "$HOST_IP/24" dev veth-host
ip link set veth-host up
phone ip link set lo up
phone ip addr add 10.201.0.2/24 dev veth-up
phone ip link set veth-up up
phone ip route add default via "$HOST_IP"
sysctl -qw net.ipv4.ip_forward=1
iptables -t nat -A POSTROUTING -s 10.201.0.0/24 -j MASQUERADE
iptables -I FORWARD -s 10.201.0.0/24 -j ACCEPT
iptables -I FORWARD -d 10.201.0.0/24 -j ACCEPT

# phone <-> pc. The phone routes and NATs for the pc like a tethering phone would.
ip netns add "$PC"
phone ip link add veth-down type veth peer name veth-pc netns "$PC"
phone ip addr add 10.200.0.1/24 dev veth-down
phone ip link set veth-down up
phone sysctl -qw net.ipv4.ip_forward=1
phone iptables -t nat -A POSTROUTING -s 10.200.0.0/24 -o veth-up -j MASQUERADE
pc ip link set lo up
pc ip addr add 10.200.0.2/24 dev veth-pc
pc ip link set veth-pc up
pc ip route add default via 10.200.0.1
# The pc's resolver is a LAN address, as with a home router.
mkdir -p "/etc/netns/$PC"
echo "nameserver 10.200.0.1" > "/etc/netns/$PC/resolv.conf"

pc curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "no direct path out of the pc"

# A UDP echo server on the Internet side.
python3 - "$UDP_ECHO" <<'PY' &
import socket, sys
host, port = sys.argv[1].rsplit(":", 1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind((host, int(port)))
while True:
    data, peer = s.recvfrom(2048)
    s.sendto(data, peer)
PY
pids+=($!)
udp_echo() {
  pc python3 - "$UDP_ECHO" <<'PY'
import socket, sys
host, port = sys.argv[1].rsplit(":", 1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(5)
for _ in range(3):
    s.sendto(b"kite udp", (host, int(port)))
    try:
        if s.recvfrom(64)[0] == b"kite udp":
            sys.exit(0)
    except socket.timeout:
        pass
sys.exit(1)
PY
}
sleep 0.5
udp_echo || fail "no direct UDP path out of the pc"

# Leak counters. pc: everything that leaves except the relay connection, logged to the kernel log.
# phone: everything it forwards for the pc, and what its own sockets send.
sysctl -qw net.netfilter.nf_log_all_netns=1
pc iptables -A OUTPUT -o veth-pc -p tcp -d 10.200.0.1 --dport 7000:7002 -j ACCEPT
pc iptables -A OUTPUT -o veth-pc -j LOG --log-prefix "kite-leak: "
pc ip6tables -A OUTPUT -o veth-pc -p ipv6-icmp -j ACCEPT
pc ip6tables -A OUTPUT -o veth-pc -j LOG --log-prefix "kite-leak: "
phone iptables -A FORWARD -i veth-down -j LOG --log-prefix "kite-leak forwarded: "
phone iptables -A OUTPUT -o veth-up -p tcp --dport 443
phone iptables -A OUTPUT -o veth-up -p tcp --dport 53
phone iptables -A OUTPUT -o veth-up -p udp -d "$HOST_IP" --dport 9999
# Packet count of rule number $4 in chain $3 of table command $2, run through $1.
counter() { "$1" "$2" -L "$3" -v -n -x | awk -v n="$4" 'NR == n + 2 {print $1}'; }
leaks() {
  echo $(($(counter pc iptables OUTPUT 2) + $(counter pc ip6tables OUTPUT 2) + $(counter phone iptables FORWARD 1)))
}
check_leaks() { [ "$(leaks)" = 0 ] || fail "$(leaks) packets left outside the tunnel ($1)"; }

# Another program's rule at kite's priority (Tailscale-style) must survive every kite exit path.
pc ip rule add pref 5300 lookup 52
foreign_rule() { pc ip rule | grep -q "^5300:.*lookup 52"; }

phone "$BIN/relay-dev" "$RELAY" > "$BIN/relay.log" 2>&1 &
pids+=($!)

# Pair once, typing the code relay-dev prints where the phone would show it.
shown_code() { sed -n 's/^pairing code \([0-9]\{6\}\)$/\1/p' "$BIN/relay.log" | tail -1; }
for _ in $(seq 50); do
  c=$(shown_code)
  if [ -n "$c" ]; then echo "$c"; break; fi
  sleep 0.2
done | "${as_nobody[@]}" -c "XDG_CONFIG_HOME=$CONF exec $BIN/kite pair --foreground --tcp $RELAY" \
  > "$BIN/pair.log" 2>&1 || { cat "$BIN/pair.log" >&2; fail "kite pair"; }
[ "$(stat -c %a "$CONF/kite/key")" = 600 ] || fail "key file is not owner-only"

start_kite() {
  "${as_nobody[@]}" -c "XDG_CONFIG_HOME=$CONF exec $BIN/kite connect --foreground --tcp $RELAY --tun $*" > "$BIN/kite.log" 2>&1 &
  kite_pid=$!
  for _ in $(seq 50); do
    # IPv6 rules are added last.
    if pc ip -6 rule | grep -q "lookup 5300"; then
      sleep 1
      return
    fi
    kill -0 "$kite_pid" 2>/dev/null || fail "kite exited"
    sleep 0.2
  done
  fail "kite did not set up routing"
}

start_kite
# Counting starts with the tunnel up; late replies to the direct checks above are not leaks.
pc iptables -Z OUTPUT
pc ip6tables -Z OUTPUT
phone iptables -Z
[ "$(stat -c %u "/proc/$kite_pid")" = "$(id -u nobody)" ] || fail "kite is not running as nobody"
for set in CapPrm CapEff CapAmb; do
  grep -q "^$set:\s*0000000000001000$" "/proc/$kite_pid/status" || fail "$set is not CAP_NET_ADMIN only"
done
"${as_nobody[@]}" -c "exec $BIN/kite restore" 2>/dev/null && fail "kite restore ran while kite was up"
pc ip rule | grep -q "not from all fwmark" || fail "kite restore removed live rules"
echo "kite idle RSS: $(rss "$kite_pid")"

echo "== TCP through the tunnel"
pc curl -sS --max-time 20 https://1.1.1.1/cdn-cgi/trace | grep -q '^ip=' || fail "curl by address"
pc curl -sS --max-time 20 -o /dev/null https://example.com/ || fail "curl by name"
check_leaks TCP

echo "== DNS through the tunnel"
for q in "example.com" "@9.9.9.9 example.com" "@9.9.9.9 +tcp example.com"; do
  # shellcheck disable=SC2086
  out=$(pc dig +short +time=10 +tries=1 $q A) || fail "dig $q"
  [ -n "$out" ] || fail "dig $q returned nothing"
done
# DNS over TCP to a LAN resolver enters the tunnel but the phone does not serve DNS there. It may
# fail; it must not leak.
pc dig +short +time=5 +tries=1 +tcp example.com A > /dev/null || true
check_leaks DNS

echo "== UDP through the tunnel"
udp_echo || fail "UDP echo through the tunnel"
check_leaks UDP

echo "== the relay carried it"
[ "$(counter phone iptables OUTPUT 1)" -gt 0 ] || fail "the relay opened no HTTPS connection"
[ "$(counter phone iptables OUTPUT 2)" -gt 0 ] || fail "the relay sent no DNS"
[ "$(counter phone iptables OUTPUT 3)" -gt 0 ] || fail "the relay sent no UDP"

echo "== load"
loads=()
for _ in $(seq 20); do
  pc curl -sS --max-time 60 -o /dev/null "https://speed.cloudflare.com/__down?bytes=2000000" &
  loads+=($!)
done
sleep 2
echo "kite RSS with 20 downloads: $(rss "$kite_pid")"
for pid in "${loads[@]}"; do wait "$pid" || fail "parallel downloads"; done
check_leaks load
echo "kite RSS after the downloads: $(rss "$kite_pid")"

echo "== SIGTERM restores routing"
kill -TERM "$kite_pid"
wait "$kite_pid" || fail "kite exited with an error on SIGTERM"
kite_pid=
if pc ip link show kite0 2>/dev/null; then fail "kite0 still exists"; fi
if pc ip rule | grep -q "lookup 5300"; then fail "rules left after SIGTERM"; fi
if pc ip -6 rule | grep -q "lookup 5300"; then fail "IPv6 rules left after SIGTERM"; fi
pc curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "direct path broken after SIGTERM"
foreign_rule || fail "SIGTERM removed another program's rule"

echo "== SIGKILL leaves the network working, kite restore cleans up"
start_kite
kill -9 "$kite_pid"
wait "$kite_pid" 2>/dev/null || true
kite_pid=
if pc ip link show kite0 2>/dev/null; then fail "kite0 survived SIGKILL"; fi
pc ip rule | grep -q "lookup 5300" || fail "expected leftover rules after SIGKILL"
pc curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "direct path broken after SIGKILL"
pc dig +short +time=5 +tries=1 @1.1.1.1 example.com A > /dev/null || fail "DNS broken after SIGKILL"
"${as_nobody[@]}" -c "exec $BIN/kite restore" || fail "kite restore"
if pc ip rule | grep -q "lookup 5300"; then fail "rules left after kite restore"; fi
foreign_rule || fail "kite restore removed another program's rule"

echo "== kill switch: the link drops mid-download"
start_kite --kill-switch
pc iptables -Z OUTPUT
pc ip6tables -Z OUTPUT
phone iptables -Z
pc curl -sS --max-time 60 -o /dev/null "https://speed.cloudflare.com/__down?bytes=200000000" 2>/dev/null &
download=$!
sleep 2
kill -0 "$download" 2>/dev/null || fail "the download ended before the link was cut"
# The phone goes away: every packet of the tunnel is answered with a reset, and so is every redial.
cut=(INPUT -p tcp --dport 7000 -j REJECT --reject-with tcp-reset)
phone iptables -I "${cut[@]}"
wait "$download" && fail "the download survived a dead link"
kill -0 "$kite_pid" 2>/dev/null || fail "kite exited when the link dropped"
pc ip link show kite0 > /dev/null || fail "kite0 went away with the link"
pc curl -sS --max-time 5 -o /dev/null https://1.1.1.1/ 2>/dev/null && fail "TCP got out while the link was down"
pc dig +short +time=3 +tries=1 @9.9.9.9 example.com A > /dev/null && fail "DNS got out while the link was down"
udp_echo && fail "UDP got out while the link was down"
sleep 5
check_leaks "kill switch, link down"
phone iptables -D "${cut[@]}"
for _ in $(seq 40); do
  pc curl -sS --max-time 5 https://1.1.1.1/cdn-cgi/trace 2>/dev/null | grep -q '^ip=' && break
  sleep 1
done
pc curl -sS --max-time 20 https://1.1.1.1/cdn-cgi/trace | grep -q '^ip=' || fail "traffic did not come back with the link"
pc curl -sS --max-time 30 -o /dev/null https://example.com/ || fail "curl by name after reconnecting"
check_leaks "kill switch, link back"

echo "== kill switch: kite killed, traffic stays blocked until kite restore"
kill -9 "$kite_pid"
wait "$kite_pid" 2>/dev/null || true
kite_pid=
if pc ip link show kite0 2>/dev/null; then fail "kite0 survived SIGKILL"; fi
pc curl -sS --max-time 5 -o /dev/null https://1.1.1.1/ 2>/dev/null && fail "traffic left after kite was killed with the kill switch on"
check_leaks "kill switch, kite killed"
"${as_nobody[@]}" -c "exec $BIN/kite restore" || fail "kite restore"
pc curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "direct path broken after kite restore"
foreign_rule || fail "kite restore removed another program's rule"

echo "== the daemon"
# A second phone: relay-dev pairs the first computer it meets, and the first one was kite above.
RELAY2=10.200.0.1:7001
phone "$BIN/relay-dev" "$RELAY2" > "$BIN/relay2.log" 2>&1 &
pids+=($!)
RUN=$BIN/run
install -d -m 750 -o nobody "$RUN"
install -d -m 700 -o nobody "$BIN/state"
sock=(--socket "$RUN/kite.sock")
kite_cli() { "${as_nobody[@]}" -c "exec $BIN/kite $*"; }
"${as_nobody[@]}" -c "exec $BIN/kite daemon ${sock[*]} --state-dir $BIN/state" > "$BIN/daemon.log" 2>&1 &
daemon_pid=$!
pids+=("$daemon_pid")
for _ in $(seq 50); do
  [ -S "$RUN/kite.sock" ] && break
  kill -0 "$daemon_pid" 2>/dev/null || { cat "$BIN/daemon.log" >&2; fail "daemon exited"; }
  sleep 0.2
done
[ "$(stat -c %a "$RUN/kite.sock")" = 660 ] || fail "daemon socket is not 0660"
for set in CapPrm CapEff CapAmb; do
  grep -q "^$set:\s*0000000000001000$" "/proc/$daemon_pid/status" || fail "daemon $set is not CAP_NET_ADMIN only"
done
echo "daemon idle RSS: $(rss "$daemon_pid")"

shown_code2() { sed -n 's/^pairing code \([0-9]\{6\}\)$/\1/p' "$BIN/relay2.log" | tail -1; }
for _ in $(seq 50); do
  c=$(shown_code2)
  if [ -n "$c" ]; then echo "$c"; break; fi
  sleep 0.2
done | kite_cli pair --tcp "$RELAY2" "${sock[@]}" > "$BIN/pair2.log" 2>&1 || { cat "$BIN/pair2.log" "$BIN/daemon.log" >&2; fail "kite pair through the daemon"; }
[ "$(stat -c %a "$BIN/state/key")" = 600 ] || fail "daemon key file is not owner-only"
[ "$(kite_cli peers "${sock[@]}" | wc -l)" = 1 ] || fail "the daemon did not keep the phone"

pc iptables -Z OUTPUT
pc ip6tables -Z OUTPUT
phone iptables -Z
kite_cli connect --tcp "$RELAY2" --tun "${sock[@]}" || { cat "$BIN/daemon.log" >&2; fail "kite connect through the daemon"; }
kite_cli status "${sock[@]}" | grep -q '^state: connected' || fail "daemon status is not connected"
pc curl -sS --max-time 20 https://1.1.1.1/cdn-cgi/trace | grep -q '^ip=' || fail "curl through the daemon"
pc dig +short +time=10 +tries=1 example.com A | grep -q . || fail "DNS through the daemon"
check_leaks daemon
echo "daemon RSS with the tunnel up: $(rss "$daemon_pid")"
kite_cli connect --tcp "$RELAY2" --tun "${sock[@]}" 2>/dev/null && fail "a second connect was accepted"
kite_cli disconnect "${sock[@]}" || fail "kite disconnect"
if pc ip link show kite0 2>/dev/null; then fail "kite0 still exists after disconnect"; fi
if pc ip rule | grep -q "lookup 5300"; then fail "rules left after disconnect"; fi
pc curl -sS --max-time 15 -o /dev/null https://1.1.1.1/ || fail "direct path broken after disconnect"
kill -TERM "$daemon_pid"
wait "$daemon_pid" || fail "the daemon exited with an error on SIGTERM"
[ ! -e "$RUN/kite.sock" ] || fail "daemon socket left behind"

echo "== the systemd unit"
# The real unit, with its sandbox, in the pc namespace: only the unit file's own settings count.
# The resolver is the pc's, so the daemon does not ask the host's systemd-resolved for a link
# that lives in another namespace.
RELAY3=10.200.0.1:7002
phone "$BIN/relay-dev" "$RELAY3" > "$BIN/relay3.log" 2>&1 &
pids+=($!)
install -m 755 "$BIN/kite" /usr/bin/kite
systemd-sysusers "$ROOT/packaging/linux/kite.sysusers"
install -m 644 "$ROOT/packaging/linux/kite.service" /etc/systemd/system/kite.service
install -d /etc/systemd/system/kite.service.d
cat > /etc/systemd/system/kite.service.d/e2e.conf <<EOF
[Service]
NetworkNamespacePath=/run/netns/$PC
BindReadOnlyPaths=/etc/netns/$PC/resolv.conf:/etc/resolv.conf
EOF
unit_cleanup() { systemctl stop kite 2>/dev/null; rm -rf /etc/systemd/system/kite.service /etc/systemd/system/kite.service.d /usr/bin/kite; systemctl daemon-reload; }
trap 'unit_cleanup; cleanup' EXIT
systemctl daemon-reload
systemctl start kite || { journalctl -u kite --no-pager | tail -30 >&2; fail "the unit did not start"; }
for _ in $(seq 50); do
  [ -S /run/kite/kite.sock ] && break
  sleep 0.2
done
unit_fail() { journalctl -u kite --no-pager | tail -30 >&2; fail "$1"; }
[ -S /run/kite/kite.sock ] || unit_fail "no socket from the unit"
[ "$(stat -c '%a %U:%G' /run/kite/kite.sock)" = "660 kite:kite" ] || unit_fail "socket is $(stat -c '%a %U:%G' /run/kite/kite.sock)"
[ "$(stat -c %a /var/lib/kite)" = 700 ] || unit_fail "state directory is not 0700"
unit_pid=$(systemctl show -p MainPID --value kite)
[ "$(stat -c %u "/proc/$unit_pid")" = "$(id -u kite)" ] || unit_fail "the unit does not run as kite"
for set in CapPrm CapEff CapAmb CapBnd; do
  grep -q "^$set:\s*0000000000001000$" "/proc/$unit_pid/status" || unit_fail "$set is not CAP_NET_ADMIN only"
done
for _ in $(seq 50); do
  c=$(sed -n 's/^pairing code \([0-9]\{6\}\)$/\1/p' "$BIN/relay3.log" | tail -1)
  if [ -n "$c" ]; then echo "$c"; break; fi
  sleep 0.2
done | /usr/bin/kite pair --tcp "$RELAY3" > "$BIN/pair3.log" 2>&1 || { cat "$BIN/pair3.log" >&2; unit_fail "kite pair through the unit"; }
pc iptables -Z OUTPUT
pc ip6tables -Z OUTPUT
phone iptables -Z
/usr/bin/kite connect --tcp "$RELAY3" --tun || unit_fail "kite connect through the unit"
pc curl -sS --max-time 20 https://1.1.1.1/cdn-cgi/trace | grep -q '^ip=' || unit_fail "curl through the unit"
pc dig +short +time=10 +tries=1 example.com A | grep -q . || unit_fail "DNS through the unit"
check_leaks unit
echo "unit RSS with the tunnel up: $(rss "$unit_pid")"
/usr/bin/kite disconnect || unit_fail "kite disconnect through the unit"
systemctl stop kite || unit_fail "the unit did not stop cleanly"
if pc ip link show kite0 2>/dev/null; then fail "kite0 still exists after the unit stopped"; fi
if pc ip rule | grep -q "lookup 5300"; then fail "rules left after the unit stopped"; fi
unit_cleanup
trap cleanup EXIT

echo "all TUN mode checks passed"
