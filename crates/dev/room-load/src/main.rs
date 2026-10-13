//! `tilo-room-load`: how many browser guests a public room host holds.
//!
//! `bench` starts the room host as a child process (`host`), then ramps up
//! `--guests` headless browser guests against it over the relay's real
//! browser routes (plain loopback HTTP, the same bytes an HTTPS ingress
//! forwards): each one is pre-admitted through `/join`, redeems its
//! invitation, holds a certified session, publishes cursor presence over
//! the live plane's HTTPS fallback and posts to the chat. It prints the
//! join times, delivery counts and latencies, failures, and the host
//! process's CPU and memory, sampled from `/proc`.
//!
//! With `--rooms N --shard S --chat-room`, the host spreads guests over N
//! desktop rooms (each filled to S before the next opens) and every guest
//! also joins one shared chat room. With `--max-guests N`, `/join` answers
//! past N guests as the room service does when its room is full, and the
//! run reports how fast those guests heard it.
//!
//! Run it on a machine with cores to spare, not a laptop:
//! `cargo run --release -p tilo-room-load -- bench --guests 300` (or any
//! cargo drop-in that builds and runs it on such a machine).

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a measuring tool: counters and rates are approximate by nature"
)]

mod guest;
mod host;
mod http;
mod live;
mod stats;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt as _;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartedRoom {
    pub room: String,
    pub workspace_id: u64,
}

/// What the host announces on its first stdout line.
#[derive(Debug, Serialize, Deserialize)]
pub struct Started {
    pub origin: String,
    pub join: String,
    pub desktops: Vec<StartedRoom>,
    pub chat: Option<StartedRoom>,
}

#[derive(Parser)]
#[command(about = "Headless load for a public room host")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the room host (used by `bench`).
    Host(HostArgs),
    /// Start a host and drive guests against it.
    Bench(BenchArgs),
}

#[derive(Args, Clone, Debug)]
pub struct HostArgs {
    /// Desktop rooms.
    #[arg(long, default_value_t = 1)]
    pub rooms: usize,
    /// Guests per desktop room before the next one fills.
    #[arg(long, default_value_t = 10_000)]
    pub shard: usize,
    /// Also one chat room every guest joins.
    #[arg(long)]
    pub chat_room: bool,
    /// Guests `/join` admits before it answers that the room is full.
    #[arg(long)]
    pub max_guests: Option<usize>,
}

#[derive(Args, Clone, Debug)]
pub struct DriveArgs {
    /// Guests to start.
    #[arg(long, default_value_t = 50)]
    pub guests: usize,
    /// New guests per second.
    #[arg(long, default_value_t = 20.0)]
    pub ramp: f64,
    /// Seconds to keep everyone connected after the last one joined.
    #[arg(long, default_value_t = 30)]
    pub hold: u64,
    /// Seconds a guest may take to be ready.
    #[arg(long, default_value_t = 60)]
    pub join_timeout: u64,
    /// Presence frames per second while a cursor moves (0: no live plane).
    #[arg(long, default_value_t = 30.0)]
    pub cursor_hz: f64,
    /// Share of time a cursor moves.
    #[arg(long, default_value_t = 0.3)]
    pub active_fraction: f64,
    /// Mean seconds between one guest's chat posts (0: none).
    #[arg(long, default_value_t = 60.0)]
    pub chat_every: f64,
    /// Payload bytes of a chat change.
    #[arg(long, default_value_t = 300)]
    pub chat_bytes: usize,
}

#[derive(Args, Clone, Debug)]
struct BenchArgs {
    /// Run the host with glibc's `MALLOC_ARENA_MAX` set.
    #[arg(long)]
    malloc_arenas: Option<u32>,
    /// Run the host with this many runtime worker threads (default: one per
    /// core), to see a smaller machine on a large builder.
    #[arg(long)]
    host_threads: Option<usize>,
    #[command(flatten)]
    host: HostArgs,
    #[command(flatten)]
    drive: DriveArgs,
}

/// Where `bench` keeps the host's log while it runs.
fn host_log() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("room-load-host-{}.log", std::process::id()))
}

/// Ad-hoc probes: host log lines `TIMING <step> <microseconds> <size>`,
/// printed by a relay patched while hunting a hot spot, summarized per step
/// in arrival order, so a cost that grows with the room shows as rising
/// tenths.
fn timings(path: &std::path::Path) -> serde_json::Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return serde_json::Value::Null;
    };
    let mut steps = std::collections::BTreeMap::<String, Vec<(f64, f64)>>::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("TIMING ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let (Some(step), Some(us), Some(size)) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        steps.entry(step.to_owned()).or_default().push((
            us.parse::<f64>().unwrap_or_default() / 1000.0,
            size.parse::<f64>().unwrap_or_default(),
        ));
    }
    let mut out = serde_json::Map::new();
    for (step, values) in steps {
        let tenth = values.len().div_ceil(10).max(1);
        let by_tenth: Vec<String> = values
            .chunks(tenth)
            .map(|chunk| {
                format!(
                    "{:.2}",
                    chunk.iter().map(|v| v.0).sum::<f64>() / chunk.len() as f64
                )
            })
            .collect();
        let total: f64 = values.iter().map(|v| v.0).sum();
        let size = values.last().map_or(0.0, |v| v.1);
        out.insert(
            step,
            serde_json::json!(format!(
                "n={} total_ms={total:.0} last_size={size} ms_by_tenth={}",
                values.len(),
                by_tenth.join(",")
            )),
        );
    }
    serde_json::Value::Object(out)
}

/// The host log's most frequent warnings and errors, without their
/// `key=value` fields and with digits blanked, so one kind of event counts
/// as one line.
fn log_digest(path: &std::path::Path) -> serde_json::Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return serde_json::Value::Null;
    };
    let mut counts = std::collections::HashMap::<String, u64>::new();
    for line in text.lines() {
        if !(line.contains(" WARN ") || line.contains(" ERROR ")) {
            continue;
        }
        let message = line.split_once("Z ").map_or(line, |(_, rest)| rest);
        let mut shape: String = message
            .split_whitespace()
            .filter(|word| !word.contains('='))
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect();
        shape.truncate(200);
        *counts.entry(shape).or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    counts.truncate(15);
    serde_json::json!(counts)
}

/// CPU seconds (user + system) and resident bytes of `pid`.
fn sample(pid: u32) -> Option<(f64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // Fields after the command: state is index 0, utime 11, stime 12.
    let ticks: f64 = fields.get(11)?.parse::<f64>().ok()? + fields.get(12)?.parse::<f64>().ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kib: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // USER_HZ is 100 on every Linux the builder runs.
    Some((ticks / 100.0, rss_kib * 1024))
}

fn origin_addr(origin: &str) -> anyhow::Result<SocketAddr> {
    let port: u16 = origin
        .rsplit(':')
        .next()
        .context("origin port")?
        .parse()
        .context("origin port")?;
    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
}

#[allow(
    clippy::too_many_lines,
    reason = "the bench reads as its timeline: start, ramp, hold, summarize"
)]
async fn bench(args: BenchArgs) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("host")
        .arg("--rooms")
        .arg(args.host.rooms.to_string())
        .arg("--shard")
        .arg(args.host.shard.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::fs::File::create(host_log()).context("the host's log")?)
        .kill_on_drop(true);
    if args.host.chat_room {
        command.arg("--chat-room");
    }
    if let Some(max) = args.host.max_guests {
        command.arg("--max-guests").arg(max.to_string());
    }
    if let Some(arenas) = args.malloc_arenas {
        command.env("MALLOC_ARENA_MAX", arenas.to_string());
    }
    if let Some(threads) = args.host_threads {
        command.env("TOKIO_WORKER_THREADS", threads.to_string());
    }
    let mut child = command.spawn().context("start the host")?;
    let pid = child.id().context("host pid")?;
    let stdout = child.stdout.take().context("host stdout")?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let line = lines
        .next_line()
        .await?
        .context("the host ended before it started")?;
    let started: Started = serde_json::from_str(&line).context("the host's announcement")?;
    eprintln!(
        "host {pid} at {} ({} desktop rooms, chat room: {})",
        started.origin,
        started.desktops.len(),
        started.chat.is_some()
    );

    let relay = Arc::new(http::Plain::new(origin_addr(&started.origin)?));
    let host_info = tilo_browser_peer::carrier::host_info(relay.as_ref())
        .await
        .map_err(|error| anyhow::anyhow!("host info: {error}"))?;
    let stats = Arc::new(stats::Stats::default());
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    let shared = Arc::new(guest::Shared {
        args: args.drive.clone(),
        relay,
        service: http::Plain::new(started.join.parse()?),
        host_cert: host_info.host_cert,
        stats: stats.clone(),
        started: Instant::now(),
        stop,
    });

    let (cpu0, _) = sample(pid).unwrap_or_default();
    let begun = Instant::now();
    let mut peak_rss = 0u64;
    let ramp = Duration::from_secs_f64(1.0 / args.drive.ramp.max(0.01));
    let mut guests = tokio::task::JoinSet::new();
    let mut next = Instant::now();
    let mut started_guests = 0;
    let mut last = (begun, cpu0);
    let mut report = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(next.into()), if started_guests < args.drive.guests => {
                guests.spawn(guest::run(shared.clone(), started_guests));
                started_guests += 1;
                next += ramp;
            }
            _ = report.tick() => {
                if let Some((cpu, rss)) = sample(pid) {
                    let now = Instant::now();
                    let cores = (cpu - last.1) / now.duration_since(last.0).as_secs_f64().max(1e-3);
                    last = (now, cpu);
                    peak_rss = peak_rss.max(rss);
                    let joined = stats.joined.load(std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "t={:>5.1}s started={started_guests} joined={joined} host cpu={cores:.2} cores rss={} MiB presence rx={} chat rx={}",
                        begun.elapsed().as_secs_f64(),
                        rss / (1024 * 1024),
                        stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed),
                        stats.chat_rx.load(std::sync::atomic::Ordering::Relaxed),
                    );
                }
                if started_guests >= args.drive.guests {
                    break;
                }
            }
        }
    }
    let ramp_done_at = begun.elapsed().as_secs_f64();
    // Hold: everyone connected, measuring the steady state.
    let hold_begun = Instant::now();
    let (hold_cpu0, _) = sample(pid).unwrap_or_default();
    let presence0 = stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed);
    let presence_tx0 = stats.presence_tx.load(std::sync::atomic::Ordering::Relaxed);
    while hold_begun.elapsed() < Duration::from_secs(args.drive.hold) {
        report.tick().await;
        if let Some((cpu, rss)) = sample(pid) {
            let now = Instant::now();
            let cores = (cpu - last.1) / now.duration_since(last.0).as_secs_f64().max(1e-3);
            last = (now, cpu);
            peak_rss = peak_rss.max(rss);
            let joined = stats.joined.load(std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "t={:>5.1}s hold joined={joined} host cpu={cores:.2} cores rss={} MiB presence rx={} chat rx={}",
                begun.elapsed().as_secs_f64(),
                rss / (1024 * 1024),
                stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed),
                stats.chat_rx.load(std::sync::atomic::Ordering::Relaxed),
            );
        }
    }
    let held = hold_begun.elapsed().as_secs_f64();
    let (hold_cpu1, rss_end) = sample(pid).unwrap_or_default();
    let presence_rate =
        (stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed) - presence0) as f64 / held;
    let presence_tx_rate =
        (stats.presence_tx.load(std::sync::atomic::Ordering::Relaxed) - presence_tx0) as f64 / held;
    let _ = stop_tx.send(true);
    while tokio::time::timeout(Duration::from_secs(10), guests.join_next())
        .await
        .ok()
        .flatten()
        .is_some()
    {}
    let mut summary = stats.summary();
    let config = serde_json::json!({
        "guests": args.drive.guests,
        "ramp_per_s": args.drive.ramp,
        "cursor_hz": args.drive.cursor_hz,
        "active_fraction": args.drive.active_fraction,
        "chat_every_s": args.drive.chat_every,
        "rooms": args.host.rooms,
        "shard": args.host.shard,
        "chat_room": args.host.chat_room,
    });
    let host = serde_json::json!({
        "ramp_s": ramp_done_at,
        "hold_s": held,
        "hold_cpu_cores": (hold_cpu1 - hold_cpu0) / held.max(1e-3),
        "rss_mib_end": rss_end / (1024 * 1024),
        "rss_mib_peak": peak_rss / (1024 * 1024),
        "presence_rx_per_s": presence_rate,
        "presence_tx_per_s": presence_tx_rate,
        // The driver shares the machine: when it runs out of cores, the
        // host's numbers are the driver's limit, not the host's.
        "session_mib_received": shared.relay.received.load(std::sync::atomic::Ordering::Relaxed) / (1024 * 1024),
        "driver_cpu_cores_avg": sample(std::process::id()).map_or(0.0, |(cpu, _)| cpu / begun.elapsed().as_secs_f64()),
        "machine_cores": std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
    });
    if let Some(fields) = summary.as_object_mut() {
        fields.insert("config".into(), config);
        fields.insert("host".into(), host);
        fields.insert("host_log".into(), log_digest(&host_log()));
        fields.insert("timings".into(), timings(&host_log()));
    }
    let _ = std::fs::remove_file(host_log());
    println!("{}", serde_json::to_string_pretty(&summary)?);
    drop(child.stdin.take());
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    Ok(())
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // The host's rooms run Automerge in a worker process re-executed from
    // this binary.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match cli.command {
            Command::Host(args) => {
                let filter = tracing_subscriber::EnvFilter::try_from_env("TILO_RELAY_LOG")
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("tilo_relay=info,warn"));
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(std::io::stderr)
                    .init();
                host::run(args).await
            }
            Command::Bench(args) => bench(args).await,
        }
    })?;
    Ok(std::process::ExitCode::SUCCESS)
}
