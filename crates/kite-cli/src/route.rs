//! Policy routing for TUN mode, in the style of wg-quick:
//!
//! ```text
//! pref 5300  ipproto udp dport 53 -> table 5300   every DNS query, even to a LAN resolver
//! pref 5301  ipproto tcp dport 53 -> table 5300
//! pref 5302  table main, suppress_prefixlength 0  LAN and other specific routes stay as they are
//! pref 5303  not fwmark 5300 -> table 5300         everything else except the relay transport
//! ```
//!
//! Table 5300 only holds default routes through the TUN device, and the kernel deletes those with
//! the device when kite exits in any way, a crash or SIGKILL included. The rules left behind then
//! point at an empty table and fall through to the main table, so the network keeps working until
//! `restore` deletes them.
//!
//! With the kill switch the table also holds a blackhole default route with a worse metric. It
//! outlives the device, so after a crash everything but the relay transport and the local network
//! stays blocked until `restore` (or the next run) deletes it.

use netlink_packet_route::route::{RouteMessage, RouteType};
use netlink_packet_route::rule::{
    RuleAction, RuleAttribute, RuleFlags, RuleMessage, RulePortRange,
};
use netlink_packet_route::{AddressFamily, IpProtocol};
use rtnetlink::{Handle, RouteMessageBuilder};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Routing table of the tunnel, also the firewall mark of the relay transport socket. The rules use
/// priorities 5300 to 5303, after Tailscale's (5210 to 5270).
pub const TABLE: u32 = 5300;
const MAIN: u32 = 254;
const FAMILIES: [AddressFamily; 2] = [AddressFamily::Inet, AddressFamily::Inet6];

fn handle() -> io::Result<Handle> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::spawn(conn);
    Ok(handle)
}

fn nl(e: rtnetlink::Error) -> io::Error {
    match e {
        rtnetlink::Error::NetlinkError(msg) => msg.to_io(),
        e => io::Error::other(e),
    }
}

/// A kernel without IPv6 refuses IPv6 routes and rules; there is no IPv6 traffic to catch then.
fn no_ipv6(family: AddressFamily, e: &io::Error) -> bool {
    family == AddressFamily::Inet6 && matches!(e.raw_os_error(), Some(97 | 95)) // EAFNOSUPPORT, EOPNOTSUPP
}

/// IPv6 switched off with sysctl on the device (not on the whole kernel): the kernel refuses a route
/// through it with EACCES, though other interfaces may still carry IPv6.
fn ipv6_off_on_device(family: AddressFamily, e: &io::Error) -> bool {
    family == AddressFamily::Inet6 && e.raw_os_error() == Some(13) // EACCES
}

/// Metric of the blackhole route: the device's routes (metric 0) win while the device exists.
// Above 1024: the kernel stores an IPv6 route's metric 0 as 1024.
const BLACKHOLE_METRIC: u32 = 4096;

fn blackhole(family: AddressFamily) -> RouteMessage {
    match family {
        AddressFamily::Inet => RouteMessageBuilder::<Ipv4Addr>::new()
            .table_id(TABLE)
            .priority(BLACKHOLE_METRIC)
            .kind(RouteType::BlackHole)
            .build(),
        _ => RouteMessageBuilder::<Ipv6Addr>::new()
            .table_id(TABLE)
            .priority(BLACKHOLE_METRIC)
            .kind(RouteType::BlackHole)
            .build(),
    }
}

/// Sends all traffic except the relay transport into the device with index `link`. With
/// `kill_switch` it also blocks that traffic once the device is gone.
pub async fn setup(link: u32, kill_switch: bool) -> io::Result<()> {
    let h = handle()?;
    for family in FAMILIES {
        let route = match family {
            AddressFamily::Inet => RouteMessageBuilder::<Ipv4Addr>::new()
                .output_interface(link)
                .table_id(TABLE)
                .build(),
            _ => RouteMessageBuilder::<Ipv6Addr>::new()
                .output_interface(link)
                .table_id(TABLE)
                .build(),
        };
        match h.route().add(route).execute().await.map_err(nl) {
            Err(e) if no_ipv6(family, &e) => continue,
            // No IPv6 through the tunnel device. Without the kill switch there is nothing to
            // catch; with it, the blackhole below still stops IPv6 from leaving by another interface.
            Err(e) if ipv6_off_on_device(family, &e) && !kill_switch => continue,
            Err(e) if ipv6_off_on_device(family, &e) => {}
            r => r?,
        }
        if kill_switch {
            h.route()
                .add(blackhole(family))
                .execute()
                .await
                .map_err(nl)?;
        }
        for rule in rules(family) {
            let mut req = h.rule().add();
            *req.message_mut() = rule;
            req.execute().await.map_err(nl)?;
        }
    }
    Ok(())
}

/// Deletes the rules and blackhole routes `setup` added, including ones left by a run that did not
/// exit cleanly. Each is matched in full, so rules of other software at the same priorities stay.
pub async fn restore() -> io::Result<()> {
    let h = handle()?;
    for family in FAMILIES {
        match h.route().del(blackhole(family)).execute().await.map_err(nl) {
            // The kernel answers ESRCH for a route that is not there.
            Err(e) if e.raw_os_error() == Some(3) || no_ipv6(family, &e) => {}
            r => r?,
        }
        for rule in rules(family) {
            // One copy is expected; the bound is a guard.
            for _ in 0..8 {
                match h.rule().del(rule.clone()).execute().await.map_err(nl) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound || no_ipv6(family, &e) => break,
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(())
}

fn rules(family: AddressFamily) -> [RuleMessage; 4] {
    let rule = |pref: u32, table: u32, matches: &[RuleAttribute]| {
        let mut rule = RuleMessage::default();
        rule.header.family = family;
        rule.header.action = RuleAction::ToTable;
        rule.attributes = vec![RuleAttribute::Priority(pref), RuleAttribute::Table(table)];
        rule.attributes.extend_from_slice(matches);
        rule
    };
    let port_53 = RuleAttribute::DestinationPortRange(RulePortRange { start: 53, end: 53 });
    let mut rest = rule(TABLE + 3, TABLE, &[RuleAttribute::FwMark(TABLE)]);
    rest.header.flags = RuleFlags::Invert;
    [
        rule(
            TABLE,
            TABLE,
            &[RuleAttribute::IpProtocol(IpProtocol::Udp), port_53.clone()],
        ),
        rule(
            TABLE + 1,
            TABLE,
            &[RuleAttribute::IpProtocol(IpProtocol::Tcp), port_53],
        ),
        rule(TABLE + 2, MAIN, &[RuleAttribute::SuppressPrefixLen(0)]),
        rest,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_disabled_on_the_device_is_skipped_for_ipv6_only() {
        let eacces = io::Error::from_raw_os_error(13);
        assert!(ipv6_off_on_device(AddressFamily::Inet6, &eacces));
        assert!(!ipv6_off_on_device(AddressFamily::Inet, &eacces));
        assert!(!ipv6_off_on_device(
            AddressFamily::Inet6,
            &io::Error::from_raw_os_error(1)
        ));
    }

    #[test]
    fn blackhole_is_a_default_route_behind_the_tunnel_device() {
        for family in FAMILIES {
            let r = blackhole(family);
            assert_eq!(r.header.kind, RouteType::BlackHole);
            assert_eq!(r.header.destination_prefix_length, 0);
        }
    }

    #[test]
    fn rules_send_dns_and_unmarked_traffic_to_the_tunnel_table() {
        let [udp, tcp, main, rest] = rules(AddressFamily::Inet6);
        for r in [&udp, &tcp, &main, &rest] {
            assert_eq!(r.header.family, AddressFamily::Inet6);
            assert_eq!(r.header.action, RuleAction::ToTable);
        }
        assert!(udp
            .attributes
            .contains(&RuleAttribute::IpProtocol(IpProtocol::Udp)));
        assert!(tcp
            .attributes
            .contains(&RuleAttribute::DestinationPortRange(RulePortRange {
                start: 53,
                end: 53
            })));
        assert!(main.attributes.contains(&RuleAttribute::Table(MAIN)));
        assert!(main
            .attributes
            .contains(&RuleAttribute::SuppressPrefixLen(0)));
        assert_eq!(rest.header.flags, RuleFlags::Invert);
        assert!(rest.attributes.contains(&RuleAttribute::FwMark(TABLE)));
        let prefs: Vec<_> = [udp, tcp, main, rest]
            .iter()
            .flat_map(|r| r.attributes.iter())
            .filter_map(|a| match a {
                RuleAttribute::Priority(p) => Some(*p),
                _ => None,
            })
            .collect();
        assert_eq!(prefs, [TABLE, TABLE + 1, TABLE + 2, TABLE + 3]);
    }
}
