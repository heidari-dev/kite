//! The ADB packet format: a 24-byte header (six little-endian words: command, two arguments, the
//! length of the data, a checksum of it, and the command with every bit flipped) and the data.
//! Everything here comes from the device, so every length is checked before anything is allocated.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const CNXN: u32 = 0x4e58_4e43;
pub const AUTH: u32 = 0x4854_5541;
pub const OPEN: u32 = 0x4e45_504f;
pub const OKAY: u32 = 0x5941_4b4f;
pub const CLSE: u32 = 0x4553_4c43;
pub const WRTE: u32 = 0x4554_5257;

/// The protocol version we speak: the first one without data checksums.
pub const VERSION: u32 = 0x0100_0001;
/// The most data one packet may carry, announced in CNXN and enforced on what comes back.
pub const MAX_DATA: usize = 64 * 1024;
pub const HEADER_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub data: Vec<u8>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// The command, the two arguments and the length of the data, from a header the device sent.
pub fn parse_header(h: &[u8; HEADER_LEN]) -> io::Result<(u32, u32, u32, usize)> {
    let word = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let (command, arg0, arg1, len, magic) = (word(0), word(4), word(8), word(12), word(20));
    if magic != !command {
        return Err(bad("a damaged ADB packet"));
    }
    if ![CNXN, AUTH, OPEN, OKAY, CLSE, WRTE].contains(&command) {
        return Err(bad("an unknown ADB command"));
    }
    let len = usize::try_from(len).map_err(|_| bad("an ADB packet that is too long"))?;
    if len > MAX_DATA {
        return Err(bad("an ADB packet that is too long"));
    }
    Ok((command, arg0, arg1, len))
}

/// A packet as bytes: the header (the checksum is zero, as the version we announce allows) and
/// the data. The data must fit [`MAX_DATA`].
pub fn encode(command: u32, arg0: u32, arg1: u32, data: &[u8]) -> io::Result<Vec<u8>> {
    if data.len() > MAX_DATA {
        return Err(bad("an ADB packet that is too long"));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + data.len());
    for word in [command, arg0, arg1, data.len() as u32, 0, !command] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(data);
    Ok(out)
}

/// Reads one packet. The caller puts a timeout around it.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Message> {
    let mut header = [0u8; HEADER_LEN];
    r.read_exact(&mut header).await?;
    let (command, arg0, arg1, len) = parse_header(&header)?;
    let mut data = vec![0u8; len];
    r.read_exact(&mut data).await?;
    Ok(Message {
        command,
        arg0,
        arg1,
        data,
    })
}

/// For the fuzz target: the packets in `bytes`, as far as they parse. Never panics.
#[doc(hidden)]
pub fn parse_all(mut bytes: &[u8]) -> usize {
    let mut packets = 0;
    while let Some((header, rest)) = bytes.split_first_chunk::<HEADER_LEN>() {
        let Ok((_, _, _, len)) = parse_header(header) else {
            break;
        };
        let Some(rest) = rest.get(len..) else { break };
        bytes = rest;
        packets += 1;
    }
    packets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_survives_the_round_trip() {
        let bytes = encode(WRTE, 7, 9, b"hello").unwrap();
        assert_eq!(bytes.len(), HEADER_LEN + 5);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let m = rt.block_on(read_message(&mut &bytes[..])).unwrap();
        assert_eq!(
            m,
            Message {
                command: WRTE,
                arg0: 7,
                arg1: 9,
                data: b"hello".to_vec()
            }
        );
        assert_eq!(parse_all(&bytes), 1);
        assert_eq!(parse_all(&[bytes.clone(), bytes].concat()), 2);
    }

    #[test]
    fn refuses_what_the_device_must_not_send() {
        let good = encode(OKAY, 1, 2, b"").unwrap();
        let header = |f: &dyn Fn(&mut [u8; HEADER_LEN])| {
            let mut h: [u8; HEADER_LEN] = good[..HEADER_LEN].try_into().unwrap();
            f(&mut h);
            parse_header(&h)
        };
        assert!(header(&|_| {}).is_ok());
        // The magic word does not match the command.
        assert!(header(&|h| h[20] ^= 1).is_err());
        // A command we do not know, with a matching magic.
        assert!(header(&|h| {
            h[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            h[20..24].copy_from_slice(&(!0x1234_5678u32).to_le_bytes());
        })
        .is_err());
        // Too much data, and a length that does not fit a usize on 32-bit systems.
        for len in [MAX_DATA as u32 + 1, u32::MAX] {
            assert!(header(&|h| h[12..16].copy_from_slice(&len.to_le_bytes())).is_err());
        }
        assert!(encode(WRTE, 0, 0, &vec![0; MAX_DATA + 1]).is_err());
    }

    #[test]
    fn a_truncated_packet_is_an_error_not_a_hang_or_a_panic() {
        let bytes = encode(WRTE, 1, 2, b"data").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for cut in 0..bytes.len() {
            assert!(
                rt.block_on(read_message(&mut &bytes[..cut])).is_err(),
                "{cut}"
            );
        }
        assert_eq!(parse_all(&bytes[..bytes.len() - 1]), 0);
    }
}
