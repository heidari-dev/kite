//! Stands in for a second VPN in the Windows TUN test (scripts/tun-smoke.ps1): a Wintun adapter
//! `vpn2` set up the way WireGuard sets up a full tunnel, with a default route through it, interface
//! metric 0 and a DNS server of its own. It carries no traffic; it only shows what Kite's routes and
//! DNS settings do while such a VPN is up. It stays until it is killed.
//!
//!     fake-vpn path\to\wintun.dll

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::net::{IpAddr, Ipv4Addr};
    let dll = std::env::args_os()
        .nth(1)
        .ok_or_else(|| std::io::Error::other("usage: fake-vpn path\\to\\wintun.dll"))?;
    let mut cfg = tun::Configuration::default();
    cfg.tun_name("vpn2")
        .address(Ipv4Addr::new(10, 99, 0, 2))
        .netmask(Ipv4Addr::new(255, 255, 255, 0))
        // The tun crate adds 0.0.0.0/0 through the destination.
        .destination(Ipv4Addr::new(10, 99, 0, 1))
        .metric(0)
        .up();
    cfg.platform_config(|p| {
        p.wintun_file(&dll);
        p.dns_servers(&[IpAddr::V4(Ipv4Addr::new(10, 99, 0, 1))]);
    });
    let _device = tun::create(&cfg).map_err(std::io::Error::other)?;
    println!("vpn2 is up");
    loop {
        std::thread::park();
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("fake-vpn is for the Windows test only");
}
