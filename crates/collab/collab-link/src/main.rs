//! `qos-collab-link` — a configurable-latency / throughput proxy in front
//! of the qOS collab broker socket, for testing collaboration under
//! realistic network conditions on a single machine.
//!
//! ```text
//!     notes (alice) ─────►┐
//!                          │   /tmp/qos-link-alice.sock
//!     qos-collab-link ─────┤  (latency=80ms, throughput=2_000_000 bps)
//!                          │   ↕ delayed/rate-limited
//!                          ▼
//!     shell (broker)       /tmp/qos-collab.sock
//!                          ▲
//!                          │   ↕ delayed/rate-limited
//!     qos-collab-link ─────┤  (latency=80ms, throughput=2_000_000 bps)
//!                          │   /tmp/qos-link-bob.sock
//!     notes (bob)   ──────►┘
//! ```
//!
//! Each app talks to its own link socket, forwarding to the same upstream
//! broker; the proxy is bidirectional and symmetric.
//!
//! ```bash
//! qos-collab-link \
//!     --listen /tmp/qos-link-alice.sock \
//!     --upstream /tmp/qos-collab.sock \
//!     --latency-ms 80 \
//!     --throughput-bps 2000000
//! ```
//!
//! Latency is one-way, not round-trip: a byte chunk read on side A is
//! held for `latency_ms` before delivery to side B. To emulate 80 ms RTT,
//! set `--latency-ms 40`.
//!
//! Throughput is a token bucket: at most `throughput_bps / 8` bytes per
//! second per direction, with bursts up to the 64 KiB bucket size.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

struct Config {
    listen: PathBuf,
    upstream: PathBuf,
    latency: Duration,
    throughput_bps: u64,
}

fn print_usage() {
    eprintln!(
        "usage: qos-collab-link --listen <sock> --upstream <sock> \
         [--latency-ms <N>] [--throughput-bps <N>]"
    );
}

fn parse_args() -> Option<Config> {
    let mut listen = None;
    let mut upstream = None;
    let mut latency_ms: u64 = 0;
    let mut throughput_bps: u64 = u64::MAX; // unconstrained by default
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().map(PathBuf::from),
            "--upstream" => upstream = args.next().map(PathBuf::from),
            "--latency-ms" => latency_ms = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--throughput-bps" => {
                throughput_bps = args.next().and_then(|v| v.parse().ok()).unwrap_or(u64::MAX);
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                eprintln!("qos-collab-link: unknown arg {other:?}");
                return None;
            }
        }
    }
    Some(Config {
        listen: listen?,
        upstream: upstream?,
        latency: Duration::from_millis(latency_ms),
        throughput_bps,
    })
}

fn main() {
    let cfg = if let Some(c) = parse_args() {
        c
    } else {
        print_usage();
        std::process::exit(2);
    };
    // Best-effort unlink: prior process may have left a stale socket.
    let _ = std::fs::remove_file(&cfg.listen);
    let listener = match UnixListener::bind(&cfg.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("qos-collab-link: bind {} failed: {e}", cfg.listen.display());
            std::process::exit(1);
        }
    };
    let lat_ms = cfg.latency.as_millis();
    let bps = cfg.throughput_bps;
    let bps_display = if bps == u64::MAX {
        "unlimited".to_string()
    } else {
        format!("{bps}")
    };
    eprintln!(
        "qos-collab-link: listening on {} → forwarding to {} \
         (latency={lat_ms} ms one-way, throughput={bps_display} bps each direction)",
        cfg.listen.display(),
        cfg.upstream.display(),
    );
    let cfg = Arc::new(cfg);
    let mut conn_id: u64 = 0;
    for incoming in listener.incoming() {
        match incoming {
            Ok(client) => {
                conn_id += 1;
                let cfg = Arc::clone(&cfg);
                let id = conn_id;
                thread::Builder::new()
                    .name(format!("qos-collab-link-conn-{id}"))
                    .spawn(move || handle_connection(id, client, cfg))
                    .ok();
            }
            Err(e) => {
                eprintln!("qos-collab-link: accept failed: {e}");
                // brief backoff so a flapping listener doesn't peg CPU
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// One client connection becomes one upstream connection. Two forwarder
/// threads (client→upstream, upstream→client) share an `alive` flag that
/// tears the pair down when either side closes.
fn handle_connection(id: u64, client: UnixStream, cfg: Arc<Config>) {
    let upstream = match UnixStream::connect(&cfg.upstream) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "qos-collab-link[{id}]: connect upstream {} failed: {e}",
                cfg.upstream.display()
            );
            return;
        }
    };
    let alive = Arc::new(AtomicBool::new(true));

    // Forwarder: reads from `src`, queues to a channel with arrival
    // timestamps, writes to `dst` honoring latency + throttle (one per direction).
    fn spawn_forwarder(
        label: &'static str,
        id: u64,
        mut src: UnixStream,
        mut dst: UnixStream,
        latency: Duration,
        throughput_bps: u64,
        alive: Arc<AtomicBool>,
    ) -> thread::JoinHandle<()> {
        thread::Builder::new()
            .name(format!("qos-collab-link-{label}-{id}"))
            .spawn(move || {
                // Token bucket refilled at `throughput_bps/8` bytes/sec, capped at
                // `BUCKET_BYTES` (converting bits/sec to bytes/sec once, up front).
                const BUCKET_BYTES: u64 = 64 * 1024;
                let bytes_per_sec = if throughput_bps == u64::MAX {
                    u64::MAX
                } else {
                    throughput_bps / 8
                };
                let mut tokens: u64 = BUCKET_BYTES;
                let mut last_refill = Instant::now();
                // Buffers chunks with their arrival time so the writer can sleep until `arrival + latency` before delivering each.
                let (tx, rx): (SyncSender<(Instant, Vec<u8>)>, Receiver<(Instant, Vec<u8>)>) =
                    sync_channel(64);
                // Reader: pulls bytes off `src` ASAP, queuing them with the moment they were observed.
                let reader_alive = Arc::clone(&alive);
                let reader_label = label;
                let reader = thread::Builder::new()
                    .name(format!("qos-collab-link-{label}-rd-{id}"))
                    .spawn(move || {
                        let mut buf = [0u8; 8192];
                        loop {
                            if !reader_alive.load(Ordering::Relaxed) {
                                break;
                            }
                            match src.read(&mut buf) {
                                Ok(0) => {
                                    reader_alive.store(false, Ordering::Relaxed);
                                    break;
                                }
                                Ok(n) => {
                                    if tx.send((Instant::now(), buf[..n].to_vec())).is_err() {
                                        reader_alive.store(false, Ordering::Relaxed);
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("qos-collab-link[{id}]/{reader_label}: read: {e}");
                                    reader_alive.store(false, Ordering::Relaxed);
                                    break;
                                }
                            }
                        }
                    })
                    .expect("spawn reader");

                // Writer: drains the channel, sleeps until each chunk's arrival deadline
                // + latency, then refills tokens and writes (sleeping more if dry).
                while let Ok((arrived_at, chunk)) = rx.recv() {
                    if !alive.load(Ordering::Relaxed) {
                        break;
                    }
                    let deadline = arrived_at + latency;
                    let now = Instant::now();
                    if deadline > now {
                        thread::sleep(deadline - now);
                    }
                    let mut written = 0;
                    while written < chunk.len() {
                        let now = Instant::now();
                        let elapsed = now.duration_since(last_refill);
                        if elapsed > Duration::ZERO {
                            let earned = if bytes_per_sec == u64::MAX {
                                BUCKET_BYTES
                            } else {
                                ((u128::from(bytes_per_sec) * elapsed.as_nanos()) / 1_000_000_000)
                                    as u64
                            };
                            tokens = (tokens.saturating_add(earned)).min(BUCKET_BYTES);
                            last_refill = now;
                        }
                        if tokens == 0 {
                            // Sleep just long enough for one byte; dominates at very low throughput.
                            let wait_ns = if bytes_per_sec == 0 {
                                Duration::from_millis(50)
                            } else {
                                Duration::from_nanos(1_000_000_000 / bytes_per_sec.max(1))
                            };
                            thread::sleep(wait_ns);
                            continue;
                        }
                        let to_write = (chunk.len() - written).min(tokens as usize);
                        if let Err(e) = dst.write_all(&chunk[written..written + to_write]) {
                            eprintln!("qos-collab-link[{id}]/{label}: write: {e}");
                            alive.store(false, Ordering::Relaxed);
                            return;
                        }
                        tokens -= to_write as u64;
                        written += to_write;
                    }
                }
                let _ = reader.join();
                // Best-effort half-close to wake the peer's reader.
                let _ = dst.shutdown(std::net::Shutdown::Write);
            })
            .expect("spawn forwarder")
    }

    let client_to_upstream = client
        .try_clone()
        .and_then(|c2| upstream.try_clone().map(|u2| (c2, u2)));
    let upstream_to_client = upstream
        .try_clone()
        .and_then(|u2| client.try_clone().map(|c2| (u2, c2)));
    let (a, b) = if let (Ok(pair_a), Ok(pair_b)) = (client_to_upstream, upstream_to_client) {
        (pair_a, pair_b)
    } else {
        eprintln!("qos-collab-link[{id}]: try_clone failed; tearing down");
        return;
    };
    let h1 = spawn_forwarder(
        "c2u",
        id,
        a.0,
        a.1,
        cfg.latency,
        cfg.throughput_bps,
        Arc::clone(&alive),
    );
    let h2 = spawn_forwarder(
        "u2c",
        id,
        b.0,
        b.1,
        cfg.latency,
        cfg.throughput_bps,
        Arc::clone(&alive),
    );
    let _ = h1.join();
    let _ = h2.join();
    eprintln!("qos-collab-link[{id}]: closed");
}
