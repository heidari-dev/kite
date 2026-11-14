//! Serves the relay on a TCP port, so the desktop side can be tested without a phone.
//!
//! Usage: `cargo run --release -p kite-relay --example relay-dev -- [ADDR] [--allow-loopback] [--refuse-external]`
//! ADDR defaults to 127.0.0.1:7000. `--allow-loopback` lets tests reach servers on this machine;
//! `--refuse-external` refuses every other target, for a test whose computer is also the relay's.
//! The relay key and the paired computers live only as long as the process: pairing stays open
//! until the first computer runs `kite pair`, and the code is printed where a phone would show it.

use kite_relay::{noise, pair, Config};
use std::sync::Arc;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let mut addr = "127.0.0.1:7000".to_string();
    let mut cfg = Config::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-loopback" => cfg.allow_loopback = true,
            "--refuse-external" => cfg.refuse_external = true,
            _ => addr = arg,
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        // Name lookups run on blocking threads; the default allows 512 of them.
        .max_blocking_threads(8)
        .build()?;
    let key = Arc::new(noise::StaticKey::generate()?);
    let pairing = Arc::new(pair::Pairing::new(Vec::new()));
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        println!("relay-dev listening on {}", listener.local_addr()?);
        let shown = pairing.clone();
        tokio::spawn(async move {
            let mut last = None;
            loop {
                // A phone opens pairing when the user asks; relay-dev keeps it open until the
                // first computer pairs.
                if shown.peers().is_empty() {
                    shown.open();
                }
                // No user to ask here: allow whoever pairs.
                if let Some(key) = shown.request() {
                    shown.answer(&key, true);
                }
                let code = shown.pairing_code();
                if code.is_some() && code != last {
                    println!("pairing code {}", code.as_deref().unwrap_or_default());
                }
                last = code;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        loop {
            let (tunnel, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) => {
                    eprintln!("accept: {e}");
                    continue;
                }
            };
            let cfg = cfg.clone();
            let key = key.clone();
            let pairing = pairing.clone();
            tokio::spawn(async move {
                let served = async {
                    let session = pair::accept(tunnel, &key, &pairing).await?;
                    kite_relay::serve(session, cfg).await
                };
                if let Err(e) = served.await {
                    eprintln!("{peer}: {e}");
                }
            });
        }
    })
}
