//! Entry points for the cargo-fuzz targets in `fuzz/`. Not part of the API.

use crate::{noise, socks5, udpgw};
use std::future::Future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Feeds `data` to the SOCKS5 server as a client's greeting and request.
pub fn socks5_request(data: &[u8]) {
    let mut client = tokio::io::join(data, tokio::io::sink());
    let _ = block_on(socks5::handshake(&mut client));
}

/// Splits `data` into udpgw frames and parses each one the way the gateway does.
pub fn udpgw_frames(mut data: &[u8]) {
    block_on(async {
        while let Ok(Some(len)) = udpgw::frame_len(data) {
            if let [udpgw::DATA, _, _, rest @ ..] = &data[2..len] {
                let _ = udpgw::parse_data(rest).await;
            }
            data = &data[len..];
        }
    });
}

/// Feeds `data` to a responder as handshake messages, to an established session as raw transport
/// frames, and through a session as plaintext that must come out unchanged.
pub fn noise_frames(data: &[u8]) {
    block_on(async {
        if let Ok(key) = noise::StaticKey::generate() {
            let _ = noise::accept(tokio::io::join(data, tokio::io::sink()), &key).await;
        }
        let Ok((mut client, mut server)) = noise::pair().await else {
            return;
        };
        let mut out = Vec::new();
        let write = async {
            let _ = client.raw().write_all(data).await;
            let _ = client.raw().shutdown().await;
        };
        let _ = tokio::join!(write, server.read_to_end(&mut out));

        let Ok((mut client, mut server)) = noise::pair().await else {
            return;
        };
        let mut out = Vec::new();
        let write = async {
            client.write_all(data).await.expect("write");
            client.shutdown().await.expect("shutdown");
        };
        let (_, read) = tokio::join!(write, server.read_to_end(&mut out));
        read.expect("read");
        assert!(out == data, "plaintext changed in transit");
    });
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}

#[cfg(test)]
mod tests {
    #[test]
    fn handles_garbage_and_truncated_input() {
        for data in [&[][..], &[5], &[5, 1, 0, 5, 1, 0, 3, 200], &[0xff; 64]] {
            super::socks5_request(data);
            super::udpgw_frames(data);
            super::noise_frames(data);
        }
        super::noise_frames(&[0, 32, 1, 2]);
        super::noise_frames(&[0x40, 0x10, 0xff, 0xff]);
        super::udpgw_frames(&[0, 4, 2, 0, 1, 3]);
    }
}
