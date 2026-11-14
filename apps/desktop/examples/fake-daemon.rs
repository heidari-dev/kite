//! A stand-in for the daemon, to look at the window in every state without a phone.
//!
//!     cargo run -p kite-desktop --example fake-daemon -- /tmp/fake.sock /tmp/fake.json
//!     KITE_SOCKET=/tmp/fake.sock cargo run -p kite-desktop     (a debug build)
//!
//! The JSON file is read again for each request, so editing it changes what the window shows. Its
//! keys are the fields of the daemon's answers: `status`, `stats`, `devices`, `peers` and
//! `fingerprint`; a missing key gives an empty answer.

#[cfg(unix)]
use kite_ipc::{read_line, Request, Response, MAX_REQUEST};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use tokio::io::{AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixListener;

#[cfg(unix)]
/// `totals` are the byte counters: the `stats` of the file give how much they grow with each
/// `stats` request, so a steady poll shows a steady speed.
fn answer(state: &serde_json::Value, req: &Request, totals: &mut [u64; 2]) -> Response {
    let mut r = serde_json::json!({ "ok": true });
    let key = match req {
        Request::Status {} => "status",
        Request::Stats {} => "stats",
        Request::Devices {} => "devices",
        Request::Peers {} | Request::Pair { .. } => "peers",
        Request::PairStart { .. } => "fingerprint",
        _ => "",
    };
    if let (Request::Stats {}, Some(stats)) = (req, state.get("stats")) {
        for (total, field) in totals.iter_mut().zip(["bytes_up", "bytes_down"]) {
            *total += stats
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
        }
        let mut stats = stats.clone();
        stats["bytes_up"] = totals[0].into();
        stats["bytes_down"] = totals[1].into();
        r["stats"] = stats;
    } else if let Some(part) = state.get(key) {
        r[key] = part.clone();
    }
    serde_json::from_value(r).unwrap_or_else(|e| Response::err(e.to_string()))
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the fake daemon needs a Unix socket");
}

#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(socket), Some(file)) = (args.next().map(PathBuf::from), args.next()) else {
        eprintln!("usage: fake-daemon SOCKET STATE.json");
        return Ok(());
    };
    // Only a socket left by an earlier run is replaced, never another kind of file.
    if std::fs::symlink_metadata(&socket).is_ok_and(|m| m.file_type().is_socket()) {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    let mut totals = [0u64; 2];
    loop {
        let (stream, _) = listener.accept().await?;
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        while let Ok(Some(line)) = read_line(&mut r, MAX_REQUEST).await {
            let state = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            let reply = match Request::parse(&line) {
                Ok(req) => answer(&state, &req, &mut totals),
                Err(e) => Response::err(e),
            };
            if w.write_all(&reply.line()).await.is_err() {
                break;
            }
        }
    }
}
