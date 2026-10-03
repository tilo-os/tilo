//! Protocol-free WebTransport server that reproduces the Chromium renderer
//! crash seen by the voice lane: a peer `STOP_SENDING` / `RESET_STREAM` carrying a
//! WebTransport application error code for a stream the page is using.
//!
//! No `MoQ`, no product code. The page picks the server behaviour with the URL
//! path (`/stop`, `/reset`, `/drain`) and the error code with `?code=N`.
//!
//! ```text
//! cargo run -p tilo-moq --example wt_stream_signal_repro -- 127.0.0.1:19950 cert.pem key.pem
//! ```
//!
//! * `/stop`:  for every page-opened unidirectional stream, read the first
//!   chunk, then send `STOP_SENDING(code)` (the page decides whether it already
//!   closed its writer).
//! * `/reset`: every 20 ms open a unidirectional stream, write 100 bytes, and
//!   `RESET_STREAM(code)` after `?after=MS` (default 30 ms), whatever the page's
//!   reader is doing.
//! * `/mix`:   `/stop` for page streams plus `/drain`-style server streams.
//! * `/mixreset`: `/stop` for page streams plus `/reset` server streams.
//! * `?close=MS`: additionally close the session (code 3) after MS.
//! * `/drain`: control. Read page streams to the end; open, write and FIN
//!   server streams; never send a stream signal.
#![allow(
    clippy::expect_used,
    reason = "a manual repro tool: bad arguments or a failed bind abort with a message"
)]

use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let addr: std::net::SocketAddr = args.next().expect("addr").parse().expect("addr");
    let chain = CertificateDer::pem_file_iter(args.next().expect("cert"))
        .expect("cert")
        .collect::<Result<Vec<_>, _>>()
        .expect("cert");
    let key = PrivateKeyDer::from_pem_file(args.next().expect("key")).expect("key");
    let mut server = web_transport_quinn::ServerBuilder::new()
        .with_addr(addr)
        .with_certificate(chain, key)
        .expect("server");
    println!("READY {addr}");
    while let Some(request) = server.accept().await {
        tokio::spawn(async move {
            let url = request.url.clone();
            let code = url
                .query_pairs()
                .find(|(k, _)| k == "code")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0u32);
            let after = url
                .query_pairs()
                .find(|(k, _)| k == "after")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(30u64);
            let close_after = url
                .query_pairs()
                .find(|(k, _)| k == "close")
                .and_then(|(_, v)| v.parse().ok());
            let Ok(session) = request.ok().await else {
                return;
            };
            if let Some(ms) = close_after {
                // Server-initiated CLOSE_WEBTRANSPORT_SESSION while streams are in flight.
                let session = session.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    session.close(3, b"invalid media stream");
                });
            }
            println!("SESSION {url}");
            let mode = url.path().to_owned();
            let reader = {
                let session = session.clone();
                let mode = mode.clone();
                tokio::spawn(async move {
                    let mut n = 0u64;
                    while let Ok(mut recv) = session.accept_uni().await {
                        n = n.saturating_add(1);
                        let mode = mode.clone();
                        tokio::spawn(async move {
                            let mut buf = [0u8; 2048];
                            if mode == "/stop" || mode == "/mix" || mode == "/mixreset" {
                                let _ = recv.read(&mut buf).await;
                                let _ = recv.stop(code);
                            } else {
                                while let Ok(Some(_)) = recv.read(&mut buf).await {}
                            }
                        });
                    }
                    println!("ACCEPTED {n}");
                })
            };
            if mode == "/stop" {
                let _ = session.closed().await;
            } else {
                let mut tick = tokio::time::interval(Duration::from_millis(20));
                loop {
                    tick.tick().await;
                    let Ok(mut send) = session.open_uni().await else {
                        break;
                    };
                    if send.write_all(&[7u8; 100]).await.is_err() {
                        continue;
                    }
                    if mode == "/drain" || mode == "/mix" {
                        let _ = send.finish();
                    } else {
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(after)).await;
                            let _ = send.reset(code);
                        });
                    }
                }
            }
            reader.abort();
            println!("CLOSED {url}");
        });
    }
}
