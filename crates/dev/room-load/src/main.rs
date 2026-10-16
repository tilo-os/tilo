//! `tilo-room-load`: how many browser guests a public room host holds.
//!
//! `bench` starts the room host as a child process (`host`), then ramps up
//! `--guests` headless browser guests against it over the relay's real
//! browser routes (plain loopback HTTP, the same bytes an HTTPS ingress
//! forwards): each one is pre-admitted through `/join`, redeems its
//! invitation, holds a certified session, publishes cursor presence over
//! the live plane's HTTPS fallback and posts to the chat. It prints the
//! join times, delivery counts and latencies, failures, and the host's CPU
//! and memory, sampled from `/proc` over its whole process tree (the relay
//! and the Automerge workers it starts), with the busiest single thread's
//! share during the hold.
//!
//! With `--rooms N --shard S --chat-room`, the host spreads guests over N
//! desktop rooms (each filled to S before the next opens) and every guest
//! also joins one shared chat room. With `--max-guests N`, `/join` answers
//! past N guests as the room service does when its room is full, and the
//! run reports how fast those guests heard it.
//!
//! With `--spectators N`, N watching pages also certify themselves through
//! `/spectate` (as the room service's homepage embed does) and read the
//! first desktop room and its live plane without sending; `--max-spectators`
//! caps them as `TILO_ROOMS_MAX_SPECTATORS` does; `--spectator-members`
//! admits each as a Viewer of its own instead. They are reported apart,
//! under `spectators`.
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
mod procfs;
mod spectator;
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
    /// Spectators `/spectate` certifies before it answers that too many
    /// people are watching.
    #[arg(long)]
    pub max_spectators: Option<usize>,
    /// The room's governor holds the chat record guests write from the
    /// start, as a persistent room seeds its chat: the guests' sealed
    /// writes then reach the governor's copy, which follows, reads and
    /// compacts them with the channel keys it stewards.
    #[arg(long)]
    pub seeded_chat: bool,
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
    /// Watching pages to start, beside the guests.
    #[arg(long, default_value_t = 0)]
    pub spectators: usize,
    /// New spectators per second.
    #[arg(long, default_value_t = 50.0)]
    pub spectator_ramp: f64,
    /// Admit each spectator as a Viewer of its own instead of certifying it
    /// as the room's one spectator person.
    #[arg(long)]
    pub spectator_members: bool,
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

/// The host's phase timings (`tilo_relay::timing`, which the host enables):
///
/// - `TIMING <step> <microseconds> <size>`, one per occurrence of a phase
///   that happens about once per newcomer or commit (`join.*`, `commit.*`),
///   summarized per step in arrival order, so a cost that grows with the room
///   shows as rising tenths;
/// - `TIMING_WINDOW <unix second> <step> <count> <total µs> <max µs>`, one per
///   step and second for frequent phases (`owner.*`, `room.lock`, …),
///   summarized as totals and as milliseconds spent per second of the run
///   (`busy_ms_by_s`, from `begun_unix_s`): an owner whose turns add up to
///   1000 ms a second is saturated.
fn timings(path: &std::path::Path, begun_unix_s: u64) -> serde_json::Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return serde_json::Value::Null;
    };
    let mut steps = std::collections::BTreeMap::<String, Vec<(f64, f64)>>::new();
    let mut windows = std::collections::BTreeMap::<String, Vec<(u64, u64, u64, u64)>>::new();
    for line in text.lines() {
        if let Some((_, rest)) = line.split_once("TIMING_WINDOW ") {
            let mut words = rest.split_whitespace();
            let (Some(second), Some(step), Some(count), Some(total), Some(max)) = (
                words.next().and_then(|w| w.parse::<u64>().ok()),
                words.next(),
                words.next().and_then(|w| w.parse::<u64>().ok()),
                words.next().and_then(|w| w.parse::<u64>().ok()),
                words.next().and_then(|w| w.parse::<u64>().ok()),
            ) else {
                continue;
            };
            windows
                .entry(step.to_owned())
                .or_default()
                .push((second, count, total, max));
            continue;
        }
        let Some((_, rest)) = line.split_once("TIMING ") else {
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
        let mut ms: Vec<f64> = values.iter().map(|v| v.0).collect();
        let p99 = stats::quantile(&mut ms, 0.99);
        out.insert(
            step,
            serde_json::json!(format!(
                "n={} total_ms={total:.0} p99_ms={p99:.1} last_size={size} ms_by_tenth={}",
                values.len(),
                by_tenth.join(",")
            )),
        );
    }
    for (step, seconds) in windows {
        let count: u64 = seconds.iter().map(|w| w.1).sum();
        let total_us: u64 = seconds.iter().map(|w| w.2).sum();
        let max_us = seconds.iter().map(|w| w.3).max().unwrap_or_default();
        let last = seconds
            .iter()
            .map(|w| w.0.saturating_sub(begun_unix_s))
            .max()
            .unwrap_or_default();
        let mut busy = vec![0u64; usize::try_from(last).unwrap_or_default() + 1];
        for (second, _, total, _) in &seconds {
            if let Some(slot) = usize::try_from(second.saturating_sub(begun_unix_s))
                .ok()
                .and_then(|at| busy.get_mut(at))
            {
                *slot += total / 1000;
            }
        }
        let busy: Vec<String> = busy.iter().map(u64::to_string).collect();
        out.insert(
            step,
            serde_json::json!(format!(
                "n={count} total_ms={} max_ms={:.1} busy_ms_by_s={}",
                total_us / 1000,
                max_us as f64 / 1000.0,
                busy.join(",")
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
    if let Some(max) = args.host.max_spectators {
        command.arg("--max-spectators").arg(max.to_string());
    }
    if args.host.seeded_chat {
        command.arg("--seeded-chat");
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
    let spectator_stats = Arc::new(stats::Stats::default());
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    let (quiet_tx, quiet) = tokio::sync::watch::channel(false);
    let shared = Arc::new(guest::Shared {
        args: args.drive.clone(),
        relay,
        service: http::Plain::new(started.join.parse()?),
        host_cert: host_info.host_cert,
        stats: stats.clone(),
        spectator_stats: spectator_stats.clone(),
        started: Instant::now(),
        stop,
        quiet,
        sent: std::sync::Mutex::default(),
    });

    let cpu0 = procfs::Sample::of(pid).unwrap_or_default().cpu_s;
    let begun = Instant::now();
    let begun_unix_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut peak_rss = 0u64;
    let ramp = Duration::from_secs_f64(1.0 / args.drive.ramp.max(0.01));
    let mut guests = tokio::task::JoinSet::new();
    let mut next = Instant::now();
    let mut started_guests = 0;
    let spectator_ramp = Duration::from_secs_f64(1.0 / args.drive.spectator_ramp.max(0.01));
    let mut next_spectator = Instant::now();
    let mut started_spectators = 0;
    let mut last = (begun, cpu0);
    let mut report = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(next.into()), if started_guests < args.drive.guests => {
                guests.spawn(guest::run(shared.clone(), started_guests));
                started_guests += 1;
                next += ramp;
            }
            () = tokio::time::sleep_until(next_spectator.into()), if started_spectators < args.drive.spectators => {
                guests.spawn(spectator::run(shared.clone()));
                started_spectators += 1;
                next_spectator += spectator_ramp;
            }
            _ = report.tick() => {
                if let Some(procfs::Sample { cpu_s: cpu, bytes: rss, .. }) = procfs::Sample::of(pid) {
                    let now = Instant::now();
                    let cores = (cpu - last.1) / now.duration_since(last.0).as_secs_f64().max(1e-3);
                    last = (now, cpu);
                    peak_rss = peak_rss.max(rss);
                    let joined = stats.joined.load(std::sync::atomic::Ordering::Relaxed);
                    let watching = spectator_stats.joined.load(std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "t={:>5.1}s started={started_guests} joined={joined} spectators={started_spectators}/{watching} host cpu={cores:.2} cores rss={} MiB presence rx={} chat rx={}",
                        begun.elapsed().as_secs_f64(),
                        rss / (1024 * 1024),
                        stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed),
                        stats.chat_rx.load(std::sync::atomic::Ordering::Relaxed),
                    );
                }
                if started_guests >= args.drive.guests && started_spectators >= args.drive.spectators {
                    break;
                }
            }
        }
    }
    let ramp_done_at = begun.elapsed().as_secs_f64();
    // Hold: everyone connected, measuring the steady state.
    let hold_begun = Instant::now();
    let hold0 = procfs::Sample::of(pid).unwrap_or_default();
    let presence0 = stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed);
    let presence_tx0 = stats.presence_tx.load(std::sync::atomic::Ordering::Relaxed);
    let received0 = shared
        .relay
        .received
        .load(std::sync::atomic::Ordering::Relaxed);
    while hold_begun.elapsed() < Duration::from_secs(args.drive.hold) {
        report.tick().await;
        if let Some(procfs::Sample {
            cpu_s: cpu,
            bytes: rss,
            ..
        }) = procfs::Sample::of(pid)
        {
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
    let hold1 = procfs::Sample::of(pid).unwrap_or_default();
    let presence_rate =
        (stats.presence_rx.load(std::sync::atomic::Ordering::Relaxed) - presence0) as f64 / held;
    let presence_tx_rate =
        (stats.presence_tx.load(std::sync::atomic::Ordering::Relaxed) - presence_tx0) as f64 / held;
    // What the host sent every page during the hold: the egress an ingress
    // in front of it forwards.
    let egress_rate = (shared
        .relay
        .received
        .load(std::sync::atomic::Ordering::Relaxed)
        - received0) as f64
        / held;
    let _ = quiet_tx.send(true);
    let settled = settle(&shared, args.host.seeded_chat).await;
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
        "max_guests": args.host.max_guests,
        "spectators": args.drive.spectators,
        "spectator_members": args.drive.spectator_members,
        "max_spectators": args.host.max_spectators,
        "seeded_chat": args.host.seeded_chat,
        "host_threads": args.host_threads,
    });
    let host = serde_json::json!({
        "ramp_s": ramp_done_at,
        "hold_s": held,
        "hold_cpu_cores": (hold1.cpu_s - hold0.cpu_s) / held.max(1e-3),
        "hold_busiest_thread_cores": hold1.busiest_thread_cores(&hold0, held),
        "processes": hold1.processes,
        "rss_mib_end": hold1.bytes / (1024 * 1024),
        "rss_mib_peak": peak_rss / (1024 * 1024),
        "presence_rx_per_s": presence_rate,
        "presence_tx_per_s": presence_tx_rate,
        "egress_bytes_per_s": egress_rate,
        // The driver shares the machine: when it runs out of cores, the
        // host's numbers are the driver's limit, not the host's.
        "session_mib_received": shared.relay.received.load(std::sync::atomic::Ordering::Relaxed) / (1024 * 1024),
        "driver_cpu_cores_avg": procfs::process_cpu(std::process::id()).map_or(0.0, |cpu| cpu / begun.elapsed().as_secs_f64()),
        "driver_mib": procfs::proportional_bytes(std::process::id()).unwrap_or_default() / (1024 * 1024),
        "machine_cores": std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
    });
    if let Some(fields) = summary.as_object_mut() {
        fields.insert("config".into(), config);
        fields.insert("host".into(), host);
        if args.drive.spectators > 0 {
            fields.insert("spectators".into(), spectator_stats.summary());
        }
        fields.insert("host_log".into(), log_digest(&host_log()));
        fields.insert("timings".into(), timings(&host_log(), begun_unix_s));
        fields.insert("settled".into(), settled.clone());
    }
    let _ = std::fs::remove_file(host_log());
    println!("{}", serde_json::to_string_pretty(&summary)?);
    drop(child.stdin.take());
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    let missing = settled
        .get("missing_in_governor")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    if missing > 0 {
        anyhow::bail!("the room's governor never held {missing} of the guests' chat posts");
    }
    Ok(())
}

/// Once guests post nothing more: wait until each post is committed and,
/// with a seeded chat, until the governor's copy of the chat holds every
/// one, as a CRDT replica that took in the same changes must.
async fn settle(shared: &guest::Shared, seeded: bool) -> serde_json::Value {
    use std::sync::atomic::Ordering;
    const SETTLE: Duration = Duration::from_secs(60);
    let begun = Instant::now();
    let stats = &shared.stats;
    let unsettled = || {
        stats
            .chat_tx
            .load(Ordering::Relaxed)
            .saturating_sub(stats.committed.load(Ordering::Relaxed))
    };
    while unsettled() > 0 && begun.elapsed() < SETTLE {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let committed_s = begun.elapsed().as_secs_f64();
    let posted: std::collections::HashSet<String> = shared
        .sent
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .cloned()
        .collect();
    let mut settled = serde_json::json!({
        "posted": posted.len(),
        "uncommitted": unsettled(),
        "committed_s": committed_s,
    });
    if !seeded {
        return settled;
    }
    let mut missing = posted.len();
    loop {
        if let Ok((200, body)) = shared.service.post_with("/record", &[], Vec::new()).await
            && let Ok(reply) = serde_json::from_slice::<serde_json::Value>(&body)
        {
            let held: std::collections::HashSet<&str> = reply
                .get("changes")
                .and_then(serde_json::Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect()
                })
                .unwrap_or_default();
            missing = posted
                .iter()
                .filter(|hash| !held.contains(hash.as_str()))
                .count();
        }
        if missing == 0 || begun.elapsed() >= SETTLE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if let Some(fields) = settled.as_object_mut() {
        fields.insert("missing_in_governor".into(), missing.into());
        fields.insert("governor_s".into(), begun.elapsed().as_secs_f64().into());
    }
    settled
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
                    .unwrap_or_else(|_| {
                        tracing_subscriber::EnvFilter::new(
                            "tilo_relay=info,tilo_relay::timing=debug,warn",
                        )
                    });
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
