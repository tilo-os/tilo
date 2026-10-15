//! `tilo-collab-link` is a configurable-latency and throughput proxy in front
//! of the Tilo collaboration broker socket.
//!
//! ```text
//!     notes (alice) ─────►┐
//!                          │   /tmp/tilo-link-alice.sock
//!     tilo-collab-link ─────┤  (latency=80ms, throughput=2_000_000 bps)
//!                          │   ↕ delayed/rate-limited
//!                          ▼
//!     shell (broker)       /tmp/tilo-collab.sock
//!                          ▲
//!                          │   ↕ delayed/rate-limited
//!     tilo-collab-link ─────┤  (latency=80ms, throughput=2_000_000 bps)
//!                          │   /tmp/tilo-link-bob.sock
//!     notes (bob)   ──────►┘
//! ```
//!
//! Each app talks to its own link socket, forwarding to the same upstream
//! broker; the proxy is bidirectional and symmetric.
//!
//! ```bash
//! tilo-collab-link \
//!     --listen /tmp/tilo-link-alice.sock \
//!     --upstream /tmp/tilo-collab.sock \
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

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

/// A byte chunk plus the moment it was pulled off the reader side, so the
/// writer side can honor `latency` by sleeping until `arrived_at + latency`.
type Chunk = (Instant, Vec<u8>);

struct Config {
    listen: PathBuf,
    upstream: PathBuf,
    latency: Duration,
    throughput_bps: u64,
}

fn print_usage() {
    eprintln!(
        "usage: tilo-collab-link --listen <sock> --upstream <sock> \
         [--latency-ms <N>] [--throughput-bps <N>]"
    );
}

/// What `parse_args` decided: a usable config, an explicit `-h`/`--help`
/// (exit 0), or a parse failure (exit non-zero) — kept separate from `Help`
/// so `main` picks the right exit code without `parse_args` exiting itself.
enum ParseOutcome {
    Config(Config),
    Help,
    Error,
}

fn parse_args() -> ParseOutcome {
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
            "-h" | "--help" => return ParseOutcome::Help,
            other => {
                eprintln!("tilo-collab-link: unknown arg {other:?}");
                return ParseOutcome::Error;
            }
        }
    }
    match (listen, upstream) {
        (Some(listen), Some(upstream)) => ParseOutcome::Config(Config {
            listen,
            upstream,
            latency: Duration::from_millis(latency_ms),
            throughput_bps,
        }),
        _ => ParseOutcome::Error,
    }
}

fn main() {
    let cfg = match parse_args() {
        ParseOutcome::Config(c) => c,
        ParseOutcome::Help => {
            print_usage();
            std::process::exit(0);
        }
        ParseOutcome::Error => {
            print_usage();
            std::process::exit(2);
        }
    };
    // Best-effort unlink: prior process may have left a stale socket.
    let _ = std::fs::remove_file(&cfg.listen);
    let listener = match UnixListener::bind(&cfg.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "tilo-collab-link: bind {} failed: {e}",
                cfg.listen.display()
            );
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
        "tilo-collab-link: listening on {} → forwarding to {} \
         (latency={lat_ms} ms one-way, throughput={bps_display} bps each direction)",
        cfg.listen.display(),
        cfg.upstream.display(),
    );
    let cfg = Arc::new(cfg);
    let mut conn_id: u64 = 0;
    for incoming in listener.incoming() {
        match incoming {
            Ok(client) => {
                conn_id = conn_id.saturating_add(1);
                let cfg = Arc::clone(&cfg);
                let id = conn_id;
                thread::Builder::new()
                    .name(format!("tilo-collab-link-conn-{id}"))
                    .spawn(move || handle_connection(id, &client, &cfg))
                    .ok();
            }
            Err(e) => {
                eprintln!("tilo-collab-link: accept failed: {e}");
                // brief backoff so a flapping listener doesn't peg CPU
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// One client connection becomes one upstream connection. Two forwarder
/// threads (client→upstream, upstream→client) share an `alive` flag that
/// tears the pair down when either side closes.
fn handle_connection(id: u64, client: &UnixStream, cfg: &Config) {
    let upstream = match UnixStream::connect(&cfg.upstream) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "tilo-collab-link[{id}]: connect upstream {} failed: {e}",
                cfg.upstream.display()
            );
            return;
        }
    };
    let alive = Arc::new(AtomicBool::new(true));

    let client_to_upstream = client
        .try_clone()
        .and_then(|c2| upstream.try_clone().map(|u2| (c2, u2)));
    let upstream_to_client = upstream
        .try_clone()
        .and_then(|u2| client.try_clone().map(|c2| (u2, c2)));
    let (Ok(a), Ok(b)) = (client_to_upstream, upstream_to_client) else {
        eprintln!("tilo-collab-link[{id}]: try_clone failed; tearing down");
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
    if let Some(h1) = h1 {
        let _ = h1.join();
    }
    if let Some(h2) = h2 {
        let _ = h2.join();
    }
    eprintln!("tilo-collab-link[{id}]: closed");
}

/// Forwarder: reads from `src`, queues to a channel with arrival
/// timestamps, writes to `dst` honoring latency + throttle (one per
/// direction). `None` if the OS refuses to spawn either of the two threads
/// this needs (reader + writer) — logged, not fatal to the whole proxy.
fn spawn_forwarder(
    label: &'static str,
    id: u64,
    src: UnixStream,
    mut dst: UnixStream,
    latency: Duration,
    throughput_bps: u64,
    alive: Arc<AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let spawned = thread::Builder::new()
        .name(format!("tilo-collab-link-{label}-{id}"))
        .spawn(move || {
            // Reader: pulls bytes off `src` ASAP, queuing them with the
            // moment they were observed.
            let (tx, rx): (SyncSender<Chunk>, Receiver<Chunk>) = sync_channel(64);
            let reader = spawn_reader(id, label, src, tx, Arc::clone(&alive));
            let Some(reader) = reader else {
                alive.store(false, Ordering::Relaxed);
                return;
            };

            run_writer(id, label, &rx, &mut dst, latency, throughput_bps, &alive);

            let _ = reader.join();
            // Best-effort half-close to wake the peer's reader.
            let _ = dst.shutdown(std::net::Shutdown::Write);
        });
    match spawned {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("tilo-collab-link[{id}]/{label}: spawn forwarder failed: {e}");
            None
        }
    }
}

/// Reader half of one forwarder direction, on its own thread. `None` if the
/// OS refuses to spawn it.
fn spawn_reader(
    id: u64,
    label: &'static str,
    mut src: UnixStream,
    tx: SyncSender<Chunk>,
    alive: Arc<AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let spawned = thread::Builder::new()
        .name(format!("tilo-collab-link-{label}-rd-{id}"))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                if !alive.load(Ordering::Relaxed) {
                    break;
                }
                match src.read(&mut buf) {
                    Ok(0) => {
                        alive.store(false, Ordering::Relaxed);
                        break;
                    }
                    Ok(n) => {
                        // `Read::read` never returns more bytes than the
                        // buffer it was given, so `n` is always in-bounds;
                        // `.get()` avoids indexing regardless.
                        let Some(chunk) = buf.get(..n) else {
                            alive.store(false, Ordering::Relaxed);
                            break;
                        };
                        if tx.send((Instant::now(), chunk.to_vec())).is_err() {
                            alive.store(false, Ordering::Relaxed);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("tilo-collab-link[{id}]/{label}: read: {e}");
                        alive.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            }
        });
    match spawned {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("tilo-collab-link[{id}]/{label}: spawn reader failed: {e}");
            None
        }
    }
}

/// Drains `rx`, sleeping until each chunk's arrival deadline + `latency`,
/// then refilling the token bucket and writing (sleeping more if dry).
/// Token bucket is refilled at `throughput_bps/8` bytes/sec, capped at
/// `BUCKET_BYTES` (converting bits/sec to bytes/sec once, up front).
fn run_writer(
    id: u64,
    label: &'static str,
    rx: &Receiver<Chunk>,
    dst: &mut UnixStream,
    latency: Duration,
    throughput_bps: u64,
    alive: &Arc<AtomicBool>,
) {
    const BUCKET_BYTES: u64 = 64 * 1024;
    let bytes_per_sec = if throughput_bps == u64::MAX {
        u64::MAX
    } else {
        throughput_bps / 8
    };
    let mut tokens: u64 = BUCKET_BYTES;
    let mut last_refill = Instant::now();

    while let Ok((arrived_at, chunk)) = rx.recv() {
        if !alive.load(Ordering::Relaxed) {
            break;
        }
        // Overflow (essentially unreachable for any real latency config)
        // just means "no extra wait" rather than a panic.
        let deadline = arrived_at.checked_add(latency).unwrap_or(arrived_at);
        thread::sleep(deadline.saturating_duration_since(Instant::now()));

        let mut written: usize = 0;
        while written < chunk.len() {
            let now = Instant::now();
            let elapsed = now.duration_since(last_refill);
            if elapsed > Duration::ZERO {
                let earned = if bytes_per_sec == u64::MAX {
                    BUCKET_BYTES
                } else {
                    u128::from(bytes_per_sec)
                        .checked_mul(elapsed.as_nanos())
                        .and_then(|ns_bytes| ns_bytes.checked_div(1_000_000_000))
                        .and_then(|v| u64::try_from(v).ok())
                        // Already clamped to BUCKET_BYTES below, so an
                        // overflow-to-huge and a clamp-to-huge look the same.
                        .unwrap_or(BUCKET_BYTES)
                };
                tokens = tokens.saturating_add(earned).min(BUCKET_BYTES);
                last_refill = now;
            }
            if tokens == 0 {
                // Sleep just long enough for one byte; dominates at very low throughput.
                let wait_ns = if bytes_per_sec == 0 {
                    Duration::from_millis(50)
                } else {
                    Duration::from_nanos(
                        1_000_000_000u64
                            .checked_div(bytes_per_sec)
                            .unwrap_or(50_000_000),
                    )
                };
                thread::sleep(wait_ns);
                continue;
            }
            let to_write = chunk
                .len()
                .saturating_sub(written)
                .min(usize::try_from(tokens).unwrap_or(usize::MAX));
            let end = written.saturating_add(to_write);
            let Some(slice) = chunk.get(written..end) else {
                eprintln!("tilo-collab-link[{id}]/{label}: internal write-range error");
                alive.store(false, Ordering::Relaxed);
                return;
            };
            if let Err(e) = dst.write_all(slice) {
                eprintln!("tilo-collab-link[{id}]/{label}: write: {e}");
                alive.store(false, Ordering::Relaxed);
                return;
            }
            tokens = tokens.saturating_sub(u64::try_from(to_write).unwrap_or(u64::MAX));
            written = written.saturating_add(to_write);
        }
    }
}
