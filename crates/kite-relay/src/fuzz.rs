//! Entry points for the cargo-fuzz targets in `fuzz/`. Not part of the API.

use crate::{socks5, udpgw};
use std::future::Future;

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

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
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
        }
        super::udpgw_frames(&[0, 4, 2, 0, 1, 3]);
    }
}
