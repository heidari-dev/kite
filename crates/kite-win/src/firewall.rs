//! The kill switch: Windows Filtering Platform filters that block everything the tunnel does not
//! carry.
//!
//! One sublayer (`Kite kill switch`, the highest weight) holds, on the ALE connect and receive
//! layers for IPv4 and IPv6, a block filter for everything and permit filters on top of it for:
//!
//! - loopback, where the proxy that feeds the tunnel listens;
//! - the tunnel adapter itself;
//! - this program's own connection to the relay, by application and address;
//! - DHCP and, for IPv6, neighbour discovery, so the link to the router or the phone stays up.
//!
//! The filters belong to no session and are not persistent: they stay when this process ends, a
//! crash included, so the block holds until [`lift`] deletes it (`kite disconnect`, `kite restore`,
//! the next connect), and the system drops them when the filtering engine stops, so a reboot never
//! leaves the computer offline.
//!
//! Connections that were already open when the block went up are not affected: WFP decides on the
//! first packet of a flow. They keep working until they close, or until the routes of the tunnel
//! catch them.

use crate::{check, wide};
use std::ffi::c_void;
use std::io;
use std::net::IpAddr;
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{FWP_E_FILTER_NOT_FOUND, FWP_E_SUBLAYER_NOT_FOUND, HANDLE};
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FwpmEngineClose0, FwpmEngineOpen0, FwpmFilterAdd0, FwpmFilterDeleteByKey0, FwpmFreeMemory0,
    FwpmGetAppIdFromFileName0, FwpmSubLayerAdd0, FwpmSubLayerDeleteByKey0, FwpmTransactionAbort0,
    FwpmTransactionBegin0, FwpmTransactionCommit0, FWPM_CONDITION_ALE_APP_ID, FWPM_CONDITION_FLAGS,
    FWPM_CONDITION_IP_LOCAL_INTERFACE, FWPM_CONDITION_IP_LOCAL_PORT, FWPM_CONDITION_IP_PROTOCOL,
    FWPM_CONDITION_IP_REMOTE_ADDRESS, FWPM_CONDITION_IP_REMOTE_PORT, FWPM_FILTER0,
    FWPM_FILTER_CONDITION0, FWPM_LAYER_ALE_AUTH_CONNECT_V4, FWPM_LAYER_ALE_AUTH_CONNECT_V6,
    FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4, FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6, FWPM_SESSION0,
    FWPM_SUBLAYER0, FWP_ACTION_BLOCK, FWP_ACTION_PERMIT, FWP_BYTE_ARRAY16, FWP_BYTE_ARRAY16_TYPE,
    FWP_BYTE_BLOB, FWP_BYTE_BLOB_TYPE, FWP_CONDITION_FLAG_IS_LOOPBACK, FWP_MATCH_EQUAL,
    FWP_MATCH_FLAGS_ALL_SET, FWP_MATCH_RANGE, FWP_RANGE0, FWP_RANGE_TYPE, FWP_UINT16, FWP_UINT32,
    FWP_UINT64, FWP_UINT8, FWP_VALUE0,
};
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;

/// `RPC_S_SERVER_UNAVAILABLE`, `EPT_S_NOT_REGISTERED`: the filtering engine is not running.
const ENGINE_DOWN: [i32; 2] = [1722, 1753];

/// The sublayer and, with the index in the low bytes, the filters. Fixed so that [`lift`] finds
/// what a run left.
const FILTER_BASE: u128 = 0x6b69_7465_0000_4000_8000_6b69_6c6c_0000;
const SUBLAYER: GUID = GUID::from_u128(FILTER_BASE | 0xffff);
/// More than [`specs`] ever returns; [`lift`] tries this many keys.
const MAX_FILTERS: u32 = 64;
const NAME: &str = "Kite kill switch";

const UDP: u8 = 17;
const MDNS_PORT: u16 = 5353;
const ICMPV6: u8 = 58;
/// ICMPv6 that keeps an IPv6 link up: multicast listener discovery (130 to 132, 143) and neighbour
/// discovery (133 to 137: router and neighbour solicitation and advertisement, redirect).
const LINK_TYPES: [(u16, u16); 2] = [(130, 137), (143, 143)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Connect4,
    Connect6,
    Recv4,
    Recv6,
}

impl Layer {
    fn key(self) -> GUID {
        match self {
            Self::Connect4 => FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            Self::Connect6 => FWPM_LAYER_ALE_AUTH_CONNECT_V6,
            Self::Recv4 => FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
            Self::Recv6 => FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6,
        }
    }
}

/// What a filter matches on; all of a filter's conditions must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cond {
    Loopback,
    /// Traffic on the adapter with this LUID.
    Interface(u64),
    /// This executable.
    Program,
    Remote(IpAddr),
    Protocol(u8),
    LocalPort(u16),
    RemotePort(u16),
    /// For ICMP the local port holds the message type.
    IcmpType(u16, u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Spec {
    layer: Layer,
    permit: bool,
    conds: Vec<Cond>,
}

/// Every filter, in the order their keys are numbered.
fn specs(tunnel: u64, relay: Option<IpAddr>, discovery: bool) -> Vec<Spec> {
    // WFP sees a connection to ::ffff:a.b.c.d on the IPv4 layers, as a.b.c.d.
    let relay = relay.map(|ip| ip.to_canonical());
    let mut out = Vec::new();
    let mut add = |layer, permit, conds: Vec<Cond>| {
        out.push(Spec {
            layer,
            permit,
            conds,
        });
    };
    for (connect, recv, v6) in [
        (Layer::Connect4, Layer::Recv4, false),
        (Layer::Connect6, Layer::Recv6, true),
    ] {
        for layer in [connect, recv] {
            add(layer, false, vec![]);
            add(layer, true, vec![Cond::Loopback]);
            add(layer, true, vec![Cond::Interface(tunnel)]);
        }
        if let Some(ip) = relay.filter(|ip| ip.is_ipv6() == v6 && !ip.is_loopback()) {
            add(connect, true, vec![Cond::Program, Cond::Remote(ip)]);
        }
        // Looking the phone up again: this program's mDNS question to the group and the direct
        // answers, which come from port 5353. Nothing else of this program passes on these.
        if discovery && !v6 {
            let group = IpAddr::from([224, 0, 0, 251]);
            add(
                connect,
                true,
                vec![
                    Cond::Program,
                    Cond::Protocol(UDP),
                    Cond::Remote(group),
                    Cond::RemotePort(MDNS_PORT),
                ],
            );
            add(
                recv,
                true,
                vec![
                    Cond::Program,
                    Cond::Protocol(UDP),
                    Cond::RemotePort(MDNS_PORT),
                ],
            );
        }
        // DHCP: the client sends from port 68 (546) to port 67 (547) and is answered the other way.
        let (client, server) = if v6 { (546, 547) } else { (68, 67) };
        add(
            connect,
            true,
            vec![
                Cond::Protocol(UDP),
                Cond::LocalPort(client),
                Cond::RemotePort(server),
            ],
        );
        add(
            recv,
            true,
            vec![
                Cond::Protocol(UDP),
                Cond::LocalPort(client),
                Cond::RemotePort(server),
            ],
        );
        if v6 {
            for layer in [connect, recv] {
                for (low, high) in LINK_TYPES {
                    add(
                        layer,
                        true,
                        vec![Cond::Protocol(ICMPV6), Cond::IcmpType(low, high)],
                    );
                }
            }
        }
    }
    out
}

fn filter_key(index: u32) -> GUID {
    GUID::from_u128(FILTER_BASE | u128::from(index))
}

/// An open connection to the filtering engine, closed on drop.
struct Engine(HANDLE);

impl Engine {
    fn open() -> io::Result<Self> {
        // SAFETY: plain data; all zeroes is the documented default (not dynamic).
        let mut session: FWPM_SESSION0 = unsafe { std::mem::zeroed() };
        // Another program's open transaction must not hold up this one (and the tunnel) for long.
        session.txnWaitTimeoutInMSec = 5000;
        let mut engine: HANDLE = null_mut();
        // SAFETY: the local engine (null server), the default authentication, a valid session and
        // a writable out value.
        check(unsafe {
            FwpmEngineOpen0(
                null(),
                RPC_C_AUTHN_DEFAULT as u32,
                null(),
                &session,
                &mut engine,
            )
        })?;
        Ok(Self(engine))
    }

    /// Runs `f` in a transaction: all of it or none.
    fn transaction(&self, f: impl FnOnce(HANDLE) -> io::Result<()>) -> io::Result<()> {
        // SAFETY: a valid engine handle.
        check(unsafe { FwpmTransactionBegin0(self.0, 0) })?;
        match f(self.0) {
            // SAFETY: a valid engine with an open transaction.
            Ok(()) => check(unsafe { FwpmTransactionCommit0(self.0) }),
            Err(e) => {
                // SAFETY: as above.
                unsafe { FwpmTransactionAbort0(self.0) };
                Err(e)
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: the handle came from FwpmEngineOpen0 and is closed once, here.
        unsafe { FwpmEngineClose0(self.0) };
    }
}

/// Blocks everything except what the tunnel needs, until [`lift`] runs. `tunnel` is the LUID of the
/// tunnel adapter, `relay` the address this program reaches the relay at (none for a phone on a
/// direct USB link). Any filters of an earlier run are replaced, all at once, so calling this again
/// with another relay address never opens a gap. With `discovery` the program may also ask the
/// network for the phone with mDNS.
pub fn engage(tunnel: u64, relay: Option<IpAddr>, discovery: bool) -> io::Result<()> {
    let program = AppId::of_current_exe()?;
    let name = wide(NAME);
    Engine::open()
        .map_err(|e| io::Error::new(e.kind(), format!("opening the Windows firewall: {e}")))?
        .transaction(|engine| {
            delete_all(engine)?;
            // SAFETY: plain data; all zeroes is the documented default.
            let mut sublayer: FWPM_SUBLAYER0 = unsafe { std::mem::zeroed() };
            sublayer.subLayerKey = SUBLAYER;
            sublayer.displayData.name = name.as_ptr().cast_mut();
            sublayer.weight = u16::MAX;
            // SAFETY: a valid engine and sublayer; `name` outlives the call.
            check(unsafe { FwpmSubLayerAdd0(engine, &sublayer, null_mut()) })?;
            for (i, spec) in specs(tunnel, relay, discovery).iter().enumerate() {
                add_filter(engine, &name, filter_key(i as u32), spec, &program)?;
            }
            Ok(())
        })
}

/// Removes the filters of this run or of one that ended; there is nothing to do when there are
/// none, or when the filtering engine is not running.
pub fn lift() -> io::Result<()> {
    match Engine::open() {
        Err(e) if e.raw_os_error().is_some_and(|c| ENGINE_DOWN.contains(&c)) => Ok(()),
        Err(e) => Err(e),
        Ok(engine) => engine.transaction(delete_all),
    }
}

/// Deletes the filters and then the sublayer; what is not there is skipped.
fn delete_all(engine: HANDLE) -> io::Result<()> {
    for i in 0..MAX_FILTERS {
        let key = filter_key(i);
        // SAFETY: a valid engine and key.
        match unsafe { FwpmFilterDeleteByKey0(engine, &key) } {
            0 => {}
            code if code == FWP_E_FILTER_NOT_FOUND as u32 => {}
            code => return check(code),
        }
    }
    // SAFETY: a valid engine and key.
    match unsafe { FwpmSubLayerDeleteByKey0(engine, &SUBLAYER) } {
        code if code == FWP_E_SUBLAYER_NOT_FOUND as u32 => Ok(()),
        code => check(code),
    }
}

/// The application identity WFP matches a program by, for this executable.
struct AppId(*mut FWP_BYTE_BLOB);

impl AppId {
    fn of_current_exe() -> io::Result<Self> {
        let path = wide(std::env::current_exe()?.as_os_str());
        let mut blob = null_mut();
        // SAFETY: `path` is zero-terminated; `blob` receives memory freed in `drop`.
        check(unsafe { FwpmGetAppIdFromFileName0(path.as_ptr(), &mut blob) })?;
        Ok(Self(blob))
    }
}

impl Drop for AppId {
    fn drop(&mut self) {
        // SAFETY: the blob came from FwpmGetAppIdFromFileName0 and is freed once, here.
        unsafe { FwpmFreeMemory0(std::ptr::from_mut(&mut self.0).cast::<*mut c_void>()) };
    }
}

/// Memory a condition's value points to; it must outlive the call that adds the filter. A filter
/// has at most one condition of each kind, so one slot each is enough.
#[derive(Default)]
struct Held {
    luid: u64,
    v6: FWP_BYTE_ARRAY16,
    range: FWP_RANGE0,
}

fn word(v: u16) -> FWP_VALUE0 {
    // SAFETY: plain data; the type and member are set right after.
    let mut w: FWP_VALUE0 = unsafe { std::mem::zeroed() };
    w.r#type = FWP_UINT16;
    w.Anonymous.uint16 = v;
    w
}

fn condition(cond: &Cond, held: &mut Held, program: &AppId) -> FWPM_FILTER_CONDITION0 {
    // SAFETY: plain data; all zeroes is a valid value and the fields are set below.
    let mut c: FWPM_FILTER_CONDITION0 = unsafe { std::mem::zeroed() };
    c.matchType = FWP_MATCH_EQUAL;
    // The pointers below stay valid as long as `held` is not moved.
    match cond {
        Cond::Loopback => {
            c.fieldKey = FWPM_CONDITION_FLAGS;
            c.matchType = FWP_MATCH_FLAGS_ALL_SET;
            c.conditionValue.r#type = FWP_UINT32;
            c.conditionValue.Anonymous.uint32 = FWP_CONDITION_FLAG_IS_LOOPBACK;
        }
        Cond::Interface(luid) => {
            c.fieldKey = FWPM_CONDITION_IP_LOCAL_INTERFACE;
            held.luid = *luid;
            c.conditionValue.r#type = FWP_UINT64;
            c.conditionValue.Anonymous.uint64 = &mut held.luid;
        }
        Cond::Program => {
            c.fieldKey = FWPM_CONDITION_ALE_APP_ID;
            c.conditionValue.r#type = FWP_BYTE_BLOB_TYPE;
            c.conditionValue.Anonymous.byteBlob = program.0;
        }
        Cond::Remote(IpAddr::V4(ip)) => {
            c.fieldKey = FWPM_CONDITION_IP_REMOTE_ADDRESS;
            c.conditionValue.r#type = FWP_UINT32;
            // WFP keeps IPv4 addresses in host byte order.
            c.conditionValue.Anonymous.uint32 = u32::from(*ip);
        }
        Cond::Remote(IpAddr::V6(ip)) => {
            c.fieldKey = FWPM_CONDITION_IP_REMOTE_ADDRESS;
            held.v6.byteArray16 = ip.octets();
            c.conditionValue.r#type = FWP_BYTE_ARRAY16_TYPE;
            c.conditionValue.Anonymous.byteArray16 = &mut held.v6;
        }
        Cond::Protocol(p) => {
            c.fieldKey = FWPM_CONDITION_IP_PROTOCOL;
            c.conditionValue.r#type = FWP_UINT8;
            c.conditionValue.Anonymous.uint8 = *p;
        }
        Cond::LocalPort(p) => {
            c.fieldKey = FWPM_CONDITION_IP_LOCAL_PORT;
            c.conditionValue.r#type = FWP_UINT16;
            c.conditionValue.Anonymous.uint16 = *p;
        }
        Cond::RemotePort(p) => {
            c.fieldKey = FWPM_CONDITION_IP_REMOTE_PORT;
            c.conditionValue.r#type = FWP_UINT16;
            c.conditionValue.Anonymous.uint16 = *p;
        }
        Cond::IcmpType(low, high) => {
            c.fieldKey = FWPM_CONDITION_IP_LOCAL_PORT;
            c.matchType = FWP_MATCH_RANGE;
            held.range = FWP_RANGE0 {
                valueLow: word(*low),
                valueHigh: word(*high),
            };
            c.conditionValue.r#type = FWP_RANGE_TYPE;
            c.conditionValue.Anonymous.rangeValue = &mut held.range;
        }
    }
    c
}

fn add_filter(
    engine: HANDLE,
    name: &[u16],
    key: GUID,
    spec: &Spec,
    program: &AppId,
) -> io::Result<()> {
    let mut held = Held::default();
    let mut conds: Vec<_> = spec
        .conds
        .iter()
        .map(|c| condition(c, &mut held, program))
        .collect();
    // SAFETY: plain data; all zeroes is the documented default.
    let mut filter: FWPM_FILTER0 = unsafe { std::mem::zeroed() };
    filter.filterKey = key;
    filter.displayData.name = name.as_ptr().cast_mut();
    filter.layerKey = spec.layer.key();
    filter.subLayerKey = SUBLAYER;
    // Weight ranges 0 to 15: a permit (12) is tried before the block (0) in the same sublayer.
    filter.weight.r#type = FWP_UINT8;
    filter.weight.Anonymous.uint8 = if spec.permit { 12 } else { 0 };
    filter.numFilterConditions = conds.len() as u32;
    filter.filterCondition = if conds.is_empty() {
        null_mut()
    } else {
        conds.as_mut_ptr()
    };
    filter.action.r#type = if spec.permit {
        FWP_ACTION_PERMIT
    } else {
        FWP_ACTION_BLOCK
    };
    // SAFETY: `filter` and everything it points to (`conds`, `held`, `program`, `name`) live until
    // the call returns; the system copies them.
    check(unsafe { FwpmFilterAdd0(engine, &filter, null_mut(), null_mut()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    const LAYERS: [Layer; 4] = [Layer::Connect4, Layer::Connect6, Layer::Recv4, Layer::Recv6];

    #[test]
    fn every_layer_gets_one_block_for_everything() {
        let all = specs(7, None, false);
        for layer in LAYERS {
            let blocks: Vec<_> = all
                .iter()
                .filter(|s| s.layer == layer && !s.permit)
                .collect();
            assert_eq!(blocks.len(), 1, "{layer:?}");
            assert!(blocks[0].conds.is_empty());
        }
    }

    #[test]
    fn a_permit_always_names_what_it_lets_through() {
        for spec in specs(7, Some("203.0.113.9".parse().unwrap()), false) {
            assert_eq!(spec.permit, !spec.conds.is_empty(), "{spec:?}");
        }
    }

    #[test]
    fn loopback_and_the_tunnel_pass_on_every_layer() {
        let all = specs(7, None, false);
        for layer in LAYERS {
            for want in [Cond::Loopback, Cond::Interface(7)] {
                assert!(
                    all.iter()
                        .any(|s| s.layer == layer && s.permit && s.conds == [want.clone()]),
                    "{layer:?} {want:?}"
                );
            }
        }
    }

    #[test]
    fn the_relay_is_reached_by_this_program_on_the_layer_of_its_family_only() {
        let ip4: IpAddr = "203.0.113.9".parse().unwrap();
        let ip6: IpAddr = "2001:db8::9".parse().unwrap();
        for (ip, layer) in [(ip4, Layer::Connect4), (ip6, Layer::Connect6)] {
            let want = vec![Cond::Program, Cond::Remote(ip)];
            let found: Vec<_> = specs(7, Some(ip), false)
                .into_iter()
                .filter(|s| s.conds == want)
                .collect();
            assert_eq!(found.len(), 1, "{ip}");
            assert_eq!(found[0].layer, layer);
        }
    }

    #[test]
    fn conditions_carry_the_values_wfp_expects() {
        use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::FWP_DATA_TYPE;
        // Only the program condition reads this, and it is not used here.
        let program = std::mem::ManuallyDrop::new(AppId(null_mut()));
        let mut held = Held::default();
        let mut one = |cond: Cond| condition(&cond, &mut held, &program);
        let v4 = one(Cond::Remote("203.0.113.9".parse().unwrap()));
        assert_eq!(v4.conditionValue.r#type, FWP_UINT32 as FWP_DATA_TYPE);
        // SAFETY: the type says which member was set.
        assert_eq!(unsafe { v4.conditionValue.Anonymous.uint32 }, 0xcb00_7109);
        let port = one(Cond::RemotePort(67));
        assert_eq!(port.conditionValue.r#type, FWP_UINT16);
        // SAFETY: as above.
        assert_eq!(unsafe { port.conditionValue.Anonymous.uint16 }, 67);
        let proto = one(Cond::Protocol(UDP));
        assert_eq!(proto.conditionValue.r#type, FWP_UINT8);
        let loopback = one(Cond::Loopback);
        assert_eq!(loopback.matchType, FWP_MATCH_FLAGS_ALL_SET);
        let icmp = one(Cond::IcmpType(133, 137));
        assert_eq!(icmp.matchType, FWP_MATCH_RANGE);
        // SAFETY: the type says which member was set; the range lives in `held`.
        let range = unsafe { &*icmp.conditionValue.Anonymous.rangeValue };
        assert_eq!(unsafe { range.valueLow.Anonymous.uint16 }, 133);
        assert_eq!(unsafe { range.valueHigh.Anonymous.uint16 }, 137);
    }

    #[test]
    fn a_mapped_relay_address_is_the_ipv4_one() {
        let mapped: IpAddr = "::ffff:203.0.113.9".parse().unwrap();
        let want = vec![Cond::Program, Cond::Remote("203.0.113.9".parse().unwrap())];
        let all = specs(7, Some(mapped), false);
        assert!(all
            .iter()
            .any(|s| s.layer == Layer::Connect4 && s.conds == want));
        let loopback: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(!specs(7, Some(loopback), false)
            .iter()
            .any(|s| s.conds.contains(&Cond::Program)));
    }

    #[test]
    fn there_is_no_relay_rule_without_a_relay_or_for_loopback() {
        for relay in [
            None,
            Some(IpAddr::from([127, 0, 0, 1])),
            Some(IpAddr::from(Ipv6Addr::LOCALHOST)),
        ] {
            assert!(!specs(7, relay, false)
                .iter()
                .any(|s| s.conds.contains(&Cond::Program)));
        }
    }

    #[test]
    fn dhcp_and_neighbour_discovery_keep_the_link_up() {
        let all = specs(7, None, false);
        let has = |layer, want: &[Cond]| all.iter().any(|s| s.layer == layer && s.conds == want);
        let dhcp = |client, server| {
            vec![
                Cond::Protocol(UDP),
                Cond::LocalPort(client),
                Cond::RemotePort(server),
            ]
        };
        assert!(has(Layer::Connect4, &dhcp(68, 67)));
        assert!(has(Layer::Recv4, &dhcp(68, 67)));
        assert!(has(Layer::Connect6, &dhcp(546, 547)));
        for layer in [Layer::Connect6, Layer::Recv6] {
            for (low, high) in [(130, 137), (143, 143)] {
                assert!(has(
                    layer,
                    &[Cond::Protocol(ICMPV6), Cond::IcmpType(low, high)]
                ));
            }
        }
        assert!(!all
            .iter()
            .any(|s| !matches!(s.layer, Layer::Connect6 | Layer::Recv6)
                && s.conds.contains(&Cond::Protocol(ICMPV6))));
    }

    #[test]
    fn the_keys_are_distinct_and_cover_every_filter() {
        let n = specs(7, Some("203.0.113.9".parse().unwrap()), false).len();
        assert!(
            n <= MAX_FILTERS as usize,
            "{n} filters, lift tries {MAX_FILTERS}"
        );
        let id = |g: GUID| (g.data1, g.data2, g.data3, g.data4);
        let mut keys: Vec<_> = (0..MAX_FILTERS).map(|i| id(filter_key(i))).collect();
        keys.push(id(SUBLAYER));
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), MAX_FILTERS as usize + 1);
    }

    #[test]
    fn discovery_lets_only_this_program_ask_for_the_phone() {
        let without = specs(7, None, false);
        let with = specs(7, None, true);
        assert_eq!(with.len(), without.len() + 2);
        let extra: Vec<_> = with.iter().filter(|s| !without.contains(s)).collect();
        for spec in extra {
            assert!(spec.permit);
            assert!(spec.conds.contains(&Cond::Program));
            assert!(spec.conds.contains(&Cond::Protocol(UDP)));
            assert!(spec.conds.contains(&Cond::RemotePort(MDNS_PORT)));
        }
    }
}
