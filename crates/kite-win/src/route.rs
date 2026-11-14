//! The IP Helper calls TUN mode needs: find the route a destination takes now, add and delete
//! routes on an interface, and turn an interface name or index into the LUID the calls use.
//!
//! A route here is the destination prefix, the interface (by LUID) and the next hop; an
//! unspecified next hop means on-link.

use crate::check;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ptr::null;
use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceAliasToLuid, ConvertInterfaceIndexToLuid, CreateIpForwardEntry2,
    DeleteIpForwardEntry2, GetBestRoute2, InitializeIpForwardEntry, MIB_IPFORWARD_ROW2,
};
use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, SOCKADDR_INET};

/// `MIB_IPPROTO_NETMGMT`: a route set by a program, as `route add` does.
const NETMGMT: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub luid: u64,
    pub dest: IpAddr,
    pub prefix: u8,
    pub next_hop: IpAddr,
}

/// The route the system uses to reach a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Best {
    pub luid: u64,
    pub next_hop: IpAddr,
    /// Length of the matching prefix: 0 for the default route, 24 for a typical LAN.
    pub prefix: u8,
}

fn inet(ip: IpAddr) -> SOCKADDR_INET {
    // SAFETY: SOCKADDR_INET is plain data and all zeroes is a valid value.
    let mut a: SOCKADDR_INET = unsafe { std::mem::zeroed() };
    match ip {
        IpAddr::V4(v4) => {
            a.Ipv4.sin_family = AF_INET;
            a.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(v4.octets());
        }
        IpAddr::V6(v6) => {
            a.Ipv6.sin6_family = AF_INET6;
            a.Ipv6.sin6_addr.u.Byte = v6.octets();
        }
    }
    a
}

fn ip_of(a: &SOCKADDR_INET) -> io::Result<IpAddr> {
    // SAFETY: `si_family` tells which member of the union the system filled.
    unsafe {
        match a.si_family {
            AF_INET => Ok(Ipv4Addr::from(a.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()).into()),
            AF_INET6 => Ok(Ipv6Addr::from(a.Ipv6.sin6_addr.u.Byte).into()),
            _ => Err(io::Error::other("an address of an unknown family")),
        }
    }
}

fn row(r: &Route) -> MIB_IPFORWARD_ROW2 {
    // SAFETY: plain data; the call below fills it with defaults.
    let mut row: MIB_IPFORWARD_ROW2 = unsafe { std::mem::zeroed() };
    // SAFETY: `row` is a valid, writable row.
    unsafe { InitializeIpForwardEntry(&mut row) };
    row.InterfaceLuid = NET_LUID_LH { Value: r.luid };
    row.DestinationPrefix.Prefix = inet(r.dest);
    row.DestinationPrefix.PrefixLength = r.prefix;
    row.NextHop = inet(r.next_hop);
    row.Protocol = NETMGMT;
    row
}

/// Adds the route with `metric` (added to the interface's own).
pub fn add(r: &Route, metric: u32) -> io::Result<()> {
    let mut row = row(r);
    row.Metric = metric;
    // SAFETY: `row` is a fully initialised row.
    check(unsafe { CreateIpForwardEntry2(&row) })
}

/// Deletes the route. One that is not there is not an error, so this can run again and again.
pub fn delete(r: &Route) -> io::Result<()> {
    let row = row(r);
    // SAFETY: `row` is a fully initialised row.
    match unsafe { DeleteIpForwardEntry2(&row) } {
        ERROR_NOT_FOUND => Ok(()),
        code => check(code),
    }
}

/// The route `dest` takes right now.
pub fn best_to(dest: IpAddr) -> io::Result<Best> {
    let dest = inet(dest);
    // SAFETY: plain data, filled by the call below.
    let mut best: MIB_IPFORWARD_ROW2 = unsafe { std::mem::zeroed() };
    // SAFETY: a zeroed SOCKADDR_INET for the source address is allowed (the system picks).
    let mut source: SOCKADDR_INET = unsafe { std::mem::zeroed() };
    // SAFETY: no interface or source is given; `dest` is valid; the outputs are writable.
    check(unsafe { GetBestRoute2(null(), 0, null(), &dest, 0, &mut best, &mut source) })?;
    Ok(Best {
        // SAFETY: a union of a u64 and a struct of the same size, filled either way.
        luid: unsafe { best.InterfaceLuid.Value },
        next_hop: ip_of(&best.NextHop)?,
        prefix: best.DestinationPrefix.PrefixLength,
    })
}

/// The LUID of the interface with index `index`.
pub fn luid_of_index(index: u32) -> io::Result<u64> {
    let mut luid = NET_LUID_LH { Value: 0 };
    // SAFETY: `luid` is a writable out value.
    check(unsafe { ConvertInterfaceIndexToLuid(index, &mut luid) })?;
    // SAFETY: see `best_to`.
    Ok(unsafe { luid.Value })
}

/// The LUID of the interface called `alias` (the name Windows shows), or None if there is none.
pub fn luid_of_alias(alias: &str) -> io::Result<Option<u64>> {
    let alias = crate::wide(alias);
    let mut luid = NET_LUID_LH { Value: 0 };
    // SAFETY: `alias` is zero-terminated; `luid` is a writable out value.
    match unsafe { ConvertInterfaceAliasToLuid(alias.as_ptr(), &mut luid) } {
        0 => Ok(Some(unsafe { luid.Value })), // SAFETY: see `best_to`
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_reached_through_a_specific_route() {
        let best = best_to(Ipv4Addr::LOCALHOST.into()).unwrap();
        // The system lists 127.0.0.1/32; a default route (0) is what must not show up here.
        assert!(best.prefix >= 8, "{best:?}");
        // The interface the route names is a real one.
        assert!(best.luid != 0);
    }

    #[test]
    fn addresses_survive_the_round_trip() {
        for ip in [
            IpAddr::from([192, 168, 43, 1]),
            IpAddr::from([0, 0, 0, 0]),
            IpAddr::from(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::from(Ipv6Addr::UNSPECIFIED),
        ] {
            assert_eq!(ip_of(&inet(ip)).unwrap(), ip);
        }
    }

    #[test]
    fn an_interface_that_is_not_there_is_none() {
        assert_eq!(luid_of_alias("kite-no-such-interface").unwrap(), None);
    }
}
