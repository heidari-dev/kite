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
    family == AddressFamily::Inet6 && e.raw_os_error() == Some(97) // EAFNOSUPPORT
}

/// Sends all traffic except the relay transport into the device with index `link`.
pub async fn setup(link: u32) -> io::Result<()> {
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
            r => r?,
        }
        for rule in rules(family) {
            let mut req = h.rule().add();
            *req.message_mut() = rule;
            req.execute().await.map_err(nl)?;
        }
    }
    Ok(())
}

/// Deletes the rules `setup` added, including ones left by a run that did not exit cleanly. Each
/// is matched in full, so rules of other software at the same priorities stay.
pub async fn restore() -> io::Result<()> {
    let h = handle()?;
    for family in FAMILIES {
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
