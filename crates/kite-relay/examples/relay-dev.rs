//! Serves the relay on a TCP port, so the desktop side can be tested without a phone.
//!
//! Usage: `cargo run --release -p kite-relay --example relay-dev -- [ADDR] [--allow-loopback]`
//! ADDR defaults to 127.0.0.1:7000. `--allow-loopback` lets tests reach servers on this machine.

use kite_relay::Config;

fn main() -> std::io::Result<()> {
    let mut addr = "127.0.0.1:7000".to_string();
    let mut cfg = Config::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-loopback" => cfg.allow_loopback = true,
            _ => addr = arg,
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        // Name lookups run on blocking threads; the default allows 512 of them.
        .max_blocking_threads(8)
        .build()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        println!("relay-dev listening on {}", listener.local_addr()?);
        loop {
            let (tunnel, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) => {
                    eprintln!("accept: {e}");
                    continue;
                }
            };
            let cfg = cfg.clone();
            tokio::spawn(async move {
                if let Err(e) = kite_relay::serve(tunnel, cfg).await {
                    eprintln!("{peer}: {e}");
                }
            });
        }
    })
}
