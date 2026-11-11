//! Policy routing for TUN mode, in the style of wg-quick:
//!
//! ```text
//! pref 5270  ipproto udp dport 53 -> table 5270   every DNS query, even to a LAN resolver
//! pref 5271  ipproto tcp dport 53 -> table 5270
//! pref 5272  table main, suppress_prefixlength 0  LAN and other specific routes stay as they are
//! pref 5273  not fwmark 5270 -> table 5270         everything else except the relay transport
//! ```
//!
//! Table 5270 only holds default routes through the TUN device, and the kernel deletes those with
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

/// Routing table of the tunnel, also the firewall mark of the relay transport socket.
pub const TABLE: u32 = 5270;
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

/// Sends all traffic except the relay transport into the device with index `link`.
pub async fn setup(link: u32) -> io::Result<()> {
    let h = handle()?;
    let v4 = RouteMessageBuilder::<Ipv4Addr>::new()
        .output_interface(link)
        .table_id(TABLE)
        .build();
    let v6 = RouteMessageBuilder::<Ipv6Addr>::new()
        .output_interface(link)
        .table_id(TABLE)
        .build();
    for route in [v4, v6] {
        h.route().add(route).execute().await.map_err(nl)?;
    }
    for family in FAMILIES {
        for rule in rules(family) {
            let mut req = h.rule().add();
            *req.message_mut() = rule;
            req.execute().await.map_err(nl)?;
        }
    }
    Ok(())
}

/// Deletes the rules `setup` added, including ones left by a run that did not exit cleanly.
pub async fn restore() -> io::Result<()> {
    let h = handle()?;
    for family in FAMILIES {
        for pref in TABLE..TABLE + 4 {
            // A rule is deleted by its priority alone. One copy is expected; the bound is a guard.
            for _ in 0..8 {
                let mut rule = RuleMessage::default();
                rule.header.family = family;
                rule.attributes.push(RuleAttribute::Priority(pref));
                match h.rule().del(rule).execute().await.map_err(nl) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => break,
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
