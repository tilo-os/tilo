//! What an active collaborator costs a relay: the hosting-cost load bench
//! (TIL-592).
//!
//! One relay runs in its own process, so its CPU time and memory are its
//! own: this binary re-executes itself with `--serve`, which starts the
//! production [`tilo_relay::Relay`] with authentication and the co-edit path
//! as a daemon runs them. The parent then connects N collaborators in rooms
//! of four, each its own device certificate, iroh endpoint and
//! `tilo-relay/5` session, and plays a fixed, deterministic minute of work
//! for each of them, over and over:
//!
//! - **typing**: 30 one-character edits, one every 200 ms, through the
//!   production publish path (`StatePeer::publish_incremental`, retention,
//!   `RelayCoeditSession::publish_retained_delta`), each followed by the
//!   toolkit's `Typing` and `Caret` presence (`NeighbourMsg::AppPresenceV2`);
//! - **pointing**: 4 seconds of pointer motion at the shell's 30 Hz publish
//!   rate (`NeighbourMsg::PresenceMoved`, `peer_presence::MIN_PUBLISH_GAP`);
//! - **idle** for the rest of the minute.
//!
//! That is 1,800 edits and 7,200 pointer moves per active hour, the cost
//! model's assumptions (TIL-625). Collaborators start their minute at
//! staggered offsets, so the load is spread as people's would be.
//!
//! The first minute warms up; then each further minute is one sample window.
//! Per window the parent asks the relay process for its CPU time, resident
//! memory, egress counters and the bytes its iroh endpoint put on UDP, and
//! divides by collaborators and hours. Fan-out latency is publish to receipt
//! by each other member of the room. Store fsyncs per edit divides the file
//! and directory barriers the relay's workspace stores issued by the deltas
//! it committed.
//!
//! The samples go to `--out DIR/hosting-load.json` for
//! `tools/perf/adapters.py hosting-load`, which turns them into records and
//! prices them with `tools/perf/prices.toml`; the summary is printed as one
//! `HOSTING_LOAD key=value …` line.
//!
//! The relay process ends when this process ends, however it ends: it reads
//! its commands from a pipe this process holds, and is killed on drop.

mod relay_process;
mod workload;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;

use relay_process::{RelayProcess, Sample};
use workload::Fleet;

/// Length of one workload cycle and of one sample window.
const WINDOW: Duration = Duration::from_secs(60);
const SECONDS_PER_HOUR: f64 = 3_600.0;
const MIB: f64 = 1_048_576.0;

/// Measure what N collaborators cost one relay.
#[derive(Parser, Debug)]
#[command(name = "relay_load")]
struct Args {
    /// Simulated collaborators, each its own device and session.
    #[arg(long, default_value_t = 100)]
    collaborators: usize,
    /// Collaborators per room; the last room takes the remainder.
    #[arg(long, default_value_t = 4)]
    room_size: usize,
    /// Measured minutes after the one-minute warm-up; each is one sample.
    #[arg(long, default_value_t = 10)]
    minutes: u32,
    /// Directory for `hosting-load.json`.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Run as the relay process (internal: the bench starts it itself).
    #[arg(long, hide = true)]
    serve: bool,
}

fn main() -> ExitCode {
    // Both processes run Automerge in isolated workers (the collaborators'
    // `StatePeer`s, the relay's store), which re-execute this binary.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    let args = Args::parse();
    let result = if args.serve {
        relay_process::serve()
    } else {
        bench(&args)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("relay_load: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn bench(args: &Args) -> anyhow::Result<()> {
    anyhow::ensure!(args.room_size >= 2, "--room-size must be at least 2");
    anyhow::ensure!(
        args.collaborators >= 2 && args.collaborators.checked_rem(args.room_size) != Some(1),
        "--collaborators must fill rooms of at least two"
    );
    anyhow::ensure!(args.minutes >= 1, "--minutes must be at least 1");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args))
}

async fn run(args: &Args) -> anyhow::Result<()> {
    let fleet = Fleet::enrol(args.collaborators, args.room_size)?;
    let state = tempfile::Builder::new()
        .prefix("tilo-relay-load-")
        .tempdir()
        .context("the relay's state directory")?;
    let mut relay = RelayProcess::spawn(&fleet, state.path()).await?;
    let result = measure(args, &fleet, &mut relay).await;
    relay.stop().await;
    drop(state);
    let report = result?;
    println!("{}", report.summary_line());
    if let Some(out) = &args.out {
        std::fs::create_dir_all(out)?;
        let path = out.join("hosting-load.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report.json())?)
            .with_context(|| format!("write {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

async fn measure(
    args: &Args,
    fleet: &Arc<Fleet>,
    relay: &mut RelayProcess,
) -> anyhow::Result<Report> {
    let idle = relay.sample().await?;
    let collaborators = fleet.connect(relay.addr(), relay.device()).await?;
    anyhow::ensure!(
        collaborators.len() == args.collaborators,
        "{} collaborators connected, not {}",
        collaborators.len(),
        args.collaborators
    );
    let connected = relay.sample().await?;
    eprintln!(
        "{} collaborators in {} rooms connected; relay RSS {:.1} MiB idle, {:.1} MiB connected",
        collaborators.len(),
        fleet.rooms(),
        mib(idle.rss_bytes),
        mib(connected.rss_bytes),
    );
    let start = Instant::now()
        .checked_add(Duration::from_secs(1))
        .context("clock")?;
    let windows = usize::try_from(args.minutes)?;
    let end = at(start, WINDOW, windows.saturating_add(1))?;
    let sessions: Vec<_> = collaborators
        .iter()
        .map(|collaborator| collaborator.session.clone())
        .collect();
    let tasks = workload::play(collaborators, start, end);

    tokio::time::sleep_until(at(start, WINDOW, 1)?.into()).await;
    fleet.measuring.store(true, Ordering::SeqCst);
    let mut previous = relay.sample().await?;
    let mut previous_at = Instant::now();
    let mut samples = Vec::with_capacity(windows);
    for window in 0..windows {
        tokio::time::sleep_until(at(start, WINDOW, window.saturating_add(2))?.into()).await;
        let sample = relay.sample().await?;
        let now = Instant::now();
        samples.push(Window::between(
            &previous,
            &sample,
            now.duration_since(previous_at),
        ));
        previous = sample;
        previous_at = now;
    }
    fleet.measuring.store(false, Ordering::SeqCst);
    for task in tasks {
        task.await??;
    }
    for session in &sessions {
        session.close().await;
    }
    anyhow::ensure!(
        previous.subscriber_dropped == idle.subscriber_dropped,
        "the relay dropped {} subscriber session(s) under load; the run measured a degraded relay",
        previous
            .subscriber_dropped
            .saturating_sub(idle.subscriber_dropped),
    );
    Report::new(args, fleet, &idle, samples)
}

/// `base + step * n`.
fn at(base: Instant, step: Duration, n: usize) -> anyhow::Result<Instant> {
    base.checked_add(step.checked_mul(u32::try_from(n)?).context("duration")?)
        .context("clock")
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte, nanosecond and event counts of a bench run stay far below 2^52"
)]
const fn float(value: u64) -> f64 {
    value as f64
}

fn mib(bytes: u64) -> f64 {
    float(bytes) / MIB
}

/// What the relay spent during one measured window, and how long it was.
struct Window {
    cpu_s: f64,
    rss_bytes: u64,
    transport_bytes: u64,
    app_bytes: u64,
    live_bytes: u64,
    /// Deltas the relay committed and fanned out.
    deltas: u64,
    store_fsyncs: u64,
    hours: f64,
}

impl Window {
    fn between(before: &Sample, after: &Sample, elapsed: Duration) -> Self {
        Self {
            cpu_s: float(after.cpu_ns.saturating_sub(before.cpu_ns)) / 1e9,
            rss_bytes: after.rss_bytes,
            transport_bytes: after
                .transport_bytes_sent
                .saturating_sub(before.transport_bytes_sent),
            app_bytes: after.bytes_sent.saturating_sub(before.bytes_sent),
            live_bytes: after.bytes_sent_live.saturating_sub(before.bytes_sent_live),
            deltas: after.deltas_received.saturating_sub(before.deltas_received),
            store_fsyncs: after.store_fsyncs.saturating_sub(before.store_fsyncs),
            hours: elapsed.as_secs_f64() / SECONDS_PER_HOUR,
        }
    }
}

/// Everything one run measured.
struct Report {
    collaborators: usize,
    rooms: usize,
    room_size: usize,
    idle_rss_bytes: u64,
    windows: Vec<Window>,
    fanout_ms: Vec<f64>,
    edits: u64,
    presence: u64,
    undelivered: usize,
}

impl Report {
    fn new(
        args: &Args,
        fleet: &Fleet,
        idle: &Sample,
        windows: Vec<Window>,
    ) -> anyhow::Result<Self> {
        let mut fanout_ms = fleet.take_latencies();
        anyhow::ensure!(
            !fanout_ms.is_empty(),
            "no collaborator received another's edit while measuring; nothing was fanned out"
        );
        fanout_ms.sort_by(f64::total_cmp);
        Ok(Self {
            collaborators: args.collaborators,
            rooms: fleet.rooms(),
            room_size: args.room_size,
            idle_rss_bytes: idle.rss_bytes,
            windows,
            fanout_ms,
            edits: fleet.edits.load(Ordering::SeqCst),
            presence: fleet.presence.load(Ordering::SeqCst),
            undelivered: fleet.undelivered(),
        })
    }

    fn per_collaborator_hour(&self, value: impl Fn(&Window) -> f64) -> Vec<f64> {
        let collaborators = float(u64::try_from(self.collaborators).unwrap_or(u64::MAX));
        self.windows
            .iter()
            .map(|window| value(window) / collaborators / window.hours)
            .collect()
    }

    fn cpu_s(&self) -> Vec<f64> {
        self.per_collaborator_hour(|window| window.cpu_s)
    }

    fn transport_bytes(&self) -> Vec<f64> {
        self.per_collaborator_hour(|window| float(window.transport_bytes))
    }

    fn app_bytes(&self) -> Vec<f64> {
        self.per_collaborator_hour(|window| float(window.app_bytes))
    }

    fn live_bytes(&self) -> Vec<f64> {
        self.per_collaborator_hour(|window| float(window.live_bytes))
    }

    /// Store fsyncs per committed delta: what durability costs an edit.
    fn store_fsyncs_per_edit(&self) -> Vec<f64> {
        self.windows
            .iter()
            .filter(|window| window.deltas > 0)
            .map(|window| float(window.store_fsyncs) / float(window.deltas))
            .collect()
    }

    /// Resident memory each connected, active collaborator adds to the relay.
    fn rss_mib(&self) -> Vec<f64> {
        let collaborators = float(u64::try_from(self.collaborators).unwrap_or(u64::MAX));
        self.windows
            .iter()
            .map(|window| mib(window.rss_bytes.saturating_sub(self.idle_rss_bytes)) / collaborators)
            .collect()
    }

    fn json(&self) -> serde_json::Value {
        let kpi =
            |unit: &str, samples: Vec<f64>| serde_json::json!({"unit": unit, "samples": samples});
        serde_json::json!({
            "collaborators": self.collaborators,
            "rooms": self.rooms,
            "room_size": self.room_size,
            "window_s": WINDOW.as_secs(),
            "windows": self.windows.len(),
            "relay_idle_rss_mib": mib(self.idle_rss_bytes),
            "edits": self.edits,
            "presence": self.presence,
            "undelivered_edits": self.undelivered,
            "kpis": {
                "hosting.relay.cpu_s_per_collab_hour": kpi("s", self.cpu_s()),
                "hosting.relay.rss_mib_per_collab": kpi("MiB", self.rss_mib()),
                "hosting.relay.egress_bytes_per_collab_hour": kpi("bytes", self.transport_bytes()),
                "hosting.relay.frame_bytes_per_collab_hour": kpi("bytes", self.app_bytes()),
                "hosting.relay.fanout_ms": kpi("ms", self.fanout_ms.clone()),
                "hosting.relay.store_fsyncs_per_edit": kpi("count", self.store_fsyncs_per_edit()),
            },
        })
    }

    fn summary_line(&self) -> String {
        format!(
            "HOSTING_LOAD collaborators={} rooms={} windows={} edits={} presence={} \
             undelivered_edits={} cpu_s_per_collab_hour_p50={:.3} rss_mib_per_collab_p50={:.3} \
             egress_mb_per_collab_hour_p50={:.3} frame_mb_per_collab_hour_p50={:.3} \
             live_frame_mb_per_collab_hour_p50={:.3} fanout_ms_p50={:.1} fanout_ms_p99={:.1} \
             store_fsyncs_per_edit_p50={:.3}",
            self.collaborators,
            self.rooms,
            self.windows.len(),
            self.edits,
            self.presence,
            self.undelivered,
            median(self.cpu_s()),
            median(self.rss_mib()),
            median(self.transport_bytes()) / 1e6,
            median(self.app_bytes()) / 1e6,
            median(self.live_bytes()) / 1e6,
            nearest_rank(&self.fanout_ms, 50),
            nearest_rank(&self.fanout_ms, 99),
            median(self.store_fsyncs_per_edit()),
        )
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    nearest_rank(&values, 50)
}

/// The nearest-rank percentile of sorted values (`tools/perf/record.py`'s rule).
fn nearest_rank(sorted: &[f64], percentile: usize) -> f64 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100).max(1);
    sorted
        .get(rank.saturating_sub(1))
        .copied()
        .unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_matches_record_py() {
        let values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert!((nearest_rank(&values, 50) - 5.0).abs() < f64::EPSILON);
        assert!((nearest_rank(&values, 99) - 10.0).abs() < f64::EPSILON);
        assert!(nearest_rank(&[], 50).is_nan());
    }

    #[test]
    fn a_window_is_divided_by_its_own_length() {
        let before = Sample::default();
        let after = Sample {
            cpu_ns: 3_000_000_000,
            bytes_sent: 1_000,
            transport_bytes_sent: 1_500,
            ..Sample::default()
        };
        let window = Window::between(&before, &after, Duration::from_secs(1_800));
        assert!((window.cpu_s - 3.0).abs() < 1e-9);
        assert!((window.hours - 0.5).abs() < 1e-9);
        assert_eq!(window.transport_bytes, 1_500);
    }
}
