//! Finding the phone on the local network: one mDNS question for `_kite._tcp.local`, and the SRV
//! records of the answers.
//!
//! The question leaves from an ephemeral port, so responders answer that port directly (RFC 6762
//! section 6.7) and nothing here joins a multicast group or binds port 5353. An answer gives the
//! port; the address is the one the datagram came from. Nothing in an answer is trusted beyond that:
//! a wrong or forged one leads to a connection whose Noise handshake fails, because the computer
//! only talks to a phone whose key it has paired.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::Instant;

const GROUP: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)), 5353);
/// Owner names of the phone's service records end like this.
const SERVICE: &str = "._kite._tcp.local";
/// How long answers are collected, and how many phones are remembered.
const WAIT: Duration = Duration::from_secs(3);
const MAX_FOUND: usize = 8;
/// A datagram with more records than this is not one of ours.
const MAX_RECORDS: usize = 64;
const SRV: u16 = 33;

/// Phones that answered within a few seconds, as `address:port`, in the order they answered.
/// Only private IPv4 senders count: that is where the phone listens.
///
/// The question goes out on every up interface that has a private IPv4 address. Without choosing
/// one, Windows sends it out of whichever interface has the lowest multicast metric, which is often
/// a virtual adapter (WSL, Hyper-V, Docker, a VPN) that the phone is not on.
pub async fn discover() -> io::Result<Vec<SocketAddr>> {
    discover_at(GROUP, WAIT, &interfaces()).await
}

/// Most interfaces asked, so a host with many virtual adapters does not open dozens of sockets.
const MAX_INTERFACES: usize = 16;

/// Private IPv4 addresses of the up interfaces; the unspecified address (the system's choice) if
/// there is none or the list cannot be read.
fn interfaces() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let IpAddr::V4(ip) = interface.ip() {
            if interface.is_oper_up()
                && ip.is_private()
                && !found.contains(&ip)
                && found.len() < MAX_INTERFACES
            {
                found.push(ip);
            }
        }
    }
    if found.is_empty() {
        found.push(Ipv4Addr::UNSPECIFIED);
    }
    found
}

/// A socket on the interface that has address `from`, so its multicast leaves there.
fn socket_on(from: Ipv4Addr) -> io::Result<UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if !from.is_unspecified() {
        sock.set_multicast_if_v4(&from)?;
    }
    sock.bind(&SocketAddr::new(from.into(), 0).into())?;
    sock.set_nonblocking(true)?;
    UdpSocket::from_std(sock.into())
}

async fn discover_at(
    group: SocketAddr,
    wait: Duration,
    from: &[Ipv4Addr],
) -> io::Result<Vec<SocketAddr>> {
    let mut asking = tokio::task::JoinSet::new();
    let mut last_error = None;
    for &ip in from {
        // An interface that refuses is skipped; the others may still reach the phone.
        match socket_on(ip) {
            Ok(sock) => {
                asking.spawn(ask(sock, group, wait));
            }
            Err(e) => last_error = Some(e),
        }
    }
    if asking.is_empty() {
        return Err(last_error.unwrap_or_else(|| io::Error::other("no interface to ask on")));
    }
    let mut found: Vec<SocketAddr> = Vec::new();
    while let Some(done) = asking.join_next().await {
        for at in done.map_err(io::Error::other)?.unwrap_or_default() {
            if found.len() < MAX_FOUND && !found.iter().any(|f| f.ip() == at.ip()) {
                found.push(at);
            }
        }
    }
    Ok(found)
}

/// Asks on `sock` for `wait` and returns who answered.
async fn ask(sock: UdpSocket, group: SocketAddr, wait: Duration) -> io::Result<Vec<SocketAddr>> {
    let query = query();
    let end = Instant::now() + wait;
    let mut found = Vec::new();
    let mut buf = [0u8; 1500];
    // Asked again after a second: a datagram can be lost, and a phone may join in the meantime.
    let mut ask = Instant::now();
    while Instant::now() < end && found.len() < MAX_FOUND {
        if Instant::now() >= ask {
            // One interface failing to send (it just went down) must not end the others.
            let _ = sock.send_to(&query, group).await;
            ask = Instant::now() + Duration::from_secs(1);
        }
        let Ok(got) = tokio::time::timeout_at(ask.min(end), sock.recv_from(&mut buf)).await else {
            continue;
        };
        let (n, from) = got?;
        if let (IpAddr::V4(ip), Some(port)) = (from.ip(), response_port(&buf[..n])) {
            let at = SocketAddr::new(ip.into(), port);
            // Loopback only so the tests can answer from there.
            // One entry per address: a device that answers with many ports must not fill the list.
            if (ip.is_private() || cfg!(test) && ip.is_loopback())
                && !found.iter().any(|f: &SocketAddr| f.ip() == at.ip())
            {
                found.push(at);
            }
        }
    }
    Ok(found)
}

/// The question: PTR for `_kite._tcp.local`, asking for a direct answer.
fn query() -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["_kite", "_tcp", "local"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 12, 0x80, 1]);
    q
}

/// The port of the first SRV record named `<instance>._kite._tcp.local` in an mDNS response, or
/// None if `msg` is not one. Never panics, whatever `msg` holds.
pub fn response_port(msg: &[u8]) -> Option<u16> {
    let word = |at: usize| Some(u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?]));
    // A response: the QR bit is set.
    if msg.get(2)? & 0x80 == 0 {
        return None;
    }
    let questions = word(4)? as usize;
    let records = [word(6)?, word(8)?, word(10)?]
        .iter()
        .map(|&n| n as usize)
        .sum::<usize>();
    let mut at = 12;
    for _ in 0..questions.min(MAX_RECORDS) {
        at = name(msg, at)?.1 + 4;
    }
    for _ in 0..records.min(MAX_RECORDS) {
        let (owner, next) = name(msg, at)?;
        let kind = word(next)?;
        let len = word(next + 8)? as usize;
        let data = msg.get(next + 10..next + 10 + len)?;
        if kind == SRV && len >= 6 && owner.ends_with(SERVICE) {
            let port = u16::from_be_bytes([data[4], data[5]]);
            if port != 0 {
                return Some(port);
            }
        }
        at = next + 10 + len;
    }
    None
}

/// The name at `at`, lowercased and joined with dots, and the offset after it. Pointers must go
/// backwards and there are few of them, so a loop ends the name instead of the program.
fn name(msg: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut after = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(at)? as usize;
        match len {
            0 => return Some((out, after.unwrap_or(at + 1))),
            _ if len & 0xC0 == 0xC0 => {
                let to = (len & 0x3F) << 8 | *msg.get(at + 1)? as usize;
                after.get_or_insert(at + 2);
                jumps += 1;
                if jumps > 8 || to >= at {
                    return None;
                }
                at = to;
            }
            _ if len & 0xC0 != 0 => return None,
            _ => {
                let label = msg.get(at + 1..at + 1 + len)?;
                if out.len() + len + 1 > 255 {
                    return None;
                }
                if !out.is_empty() {
                    out.push('.');
                }
                out.extend(label.iter().map(|b| b.to_ascii_lowercase() as char));
                at += 1 + len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(s: &str) -> Vec<u8> {
        let mut v = vec![s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    }

    /// A response with a PTR record for the service and an SRV record for `instance`, the way a
    /// responder writes it: the instance name points back into the service name.
    fn response(instance: &str, port: u16) -> Vec<u8> {
        let mut m = vec![0, 0, 0x84, 0, 0, 0, 0, 2, 0, 0, 0, 0];
        let service = m.len();
        m.extend([label("_kite"), label("_tcp"), label("local"), vec![0]].concat());
        let instance_name = [label(instance), vec![0xC0, service as u8]].concat();
        m.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 120]);
        m.extend_from_slice(&(instance_name.len() as u16).to_be_bytes());
        m.extend_from_slice(&instance_name);
        m.extend_from_slice(&instance_name);
        m.extend_from_slice(&[0, 33, 0x80, 1, 0, 0, 0, 120]);
        let target = [label("phone"), vec![0]].concat();
        m.extend_from_slice(&(6 + target.len() as u16).to_be_bytes());
        m.extend_from_slice(&[0, 0, 0, 0]);
        m.extend_from_slice(&port.to_be_bytes());
        m.extend_from_slice(&target);
        m
    }

    #[test]
    fn reads_the_port_of_a_kite_service() {
        assert_eq!(response_port(&response("Kite", 47810)), Some(47810));
        assert_eq!(response_port(&response("My Phone", 9)), Some(9));
    }

    #[test]
    fn ignores_what_is_not_a_kite_answer() {
        assert_eq!(response_port(&response("Kite", 0)), None, "port 0");
        let mut question = response("Kite", 47810);
        question[2] = 0; // not a response
        assert_eq!(response_port(&question), None);
        // Another program's service name.
        let swapped = String::from_utf8_lossy(&response("Kite", 1)).replace("_kite", "_kote");
        assert_eq!(response_port(swapped.as_bytes()), None);
        assert_eq!(response_port(&query()), None);
    }

    #[test]
    fn survives_damaged_messages() {
        let good = response("Kite", 47810);
        for cut in 0..good.len() {
            assert_eq!(response_port(&good[..cut]), None, "cut at {cut}");
        }
        // Every single-byte change either still parses or gives None; it never panics.
        for at in 0..good.len() {
            for byte in 0..=255u8 {
                let mut bad = good.clone();
                bad[at] = byte;
                let _ = response_port(&bad);
            }
        }
        // A name pointing at itself, or forward, ends the name, not the program.
        let mut looped = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0xC0, 12, 0, 33, 0, 1];
        looped.extend_from_slice(&[0; 20]);
        assert_eq!(response_port(&looped), None);
        looped[13] = 0xFF;
        assert_eq!(response_port(&looped), None);
        // Counts far larger than the message.
        assert_eq!(
            response_port(&[0, 0, 0x84, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            None
        );
    }

    #[tokio::test]
    async fn collects_each_phone_once_and_skips_junk() {
        let phone = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = phone.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            // It answers every question, as a phone does when asked twice.
            while let Ok((n, from)) = phone.recv_from(&mut buf).await {
                assert_eq!(&buf[..n], &query()[..]);
                phone.send_to(b"junk", from).await.unwrap();
                phone.send_to(&response("Kite", 47810), from).await.unwrap();
            }
        });
        let found = discover_at(at, Duration::from_millis(1500), &[Ipv4Addr::LOCALHOST])
            .await
            .unwrap();
        assert_eq!(found, vec!["127.0.0.1:47810".parse().unwrap()]);
    }

    #[tokio::test]
    async fn merges_answers_from_every_interface_and_survives_a_bad_one() {
        let phone = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = phone.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((_, from)) = phone.recv_from(&mut buf).await {
                phone.send_to(&response("Kite", 47810), from).await.unwrap();
            }
        });
        // 192.0.2.1 (TEST-NET) is on no interface of this machine, so its socket cannot bind.
        let bad = Ipv4Addr::new(192, 0, 2, 1);
        let from = [bad, Ipv4Addr::LOCALHOST, Ipv4Addr::LOCALHOST];
        let found = discover_at(at, Duration::from_millis(1500), &from)
            .await
            .unwrap();
        assert_eq!(
            found,
            vec!["127.0.0.1:47810".parse().unwrap()],
            "one entry per phone"
        );
        assert!(discover_at(at, Duration::from_millis(100), &[bad])
            .await
            .is_err());
    }

    #[test]
    fn asks_on_private_interfaces_or_leaves_the_choice_to_the_system() {
        let from = interfaces();
        assert!(!from.is_empty() && from.len() <= MAX_INTERFACES);
        assert!(from.iter().all(|ip| ip.is_private() || ip.is_unspecified()));
    }

    #[test]
    fn the_question_is_a_well_formed_ptr_query() {
        let q = query();
        assert_eq!(&q[..12], &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(name(&q, 12), Some(("_kite._tcp.local".into(), q.len() - 4)));
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0x80, 1]);
    }
}
