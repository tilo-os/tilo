//! What an active member costs a server: the server-cost load bench.
//!
//! One server runs in its own process, so its CPU time and memory are its
//! own: this binary re-executes itself with `--serve`, which starts the
//! production [`tilo_runtime::device::keep::server::KeptServer`] with authentication and the co-edit path
//! as a daemon runs them. The parent then connects N members in workspaces
//! of four, each its own device certificate, iroh endpoint and device link
//! (`tilo_server_client::link` and `DirectSync`), and plays a fixed,
//! deterministic minute of work for each of them, over and over:
//!
//! - **typing**: 30 one-character edits, one every 200 ms, through the
//!   production publish path (`PeerPublicationMut::publish_incremental` on the
//!   member's link), each followed by the toolkit's `Typing` and `Caret`
//!   presence;
//! - **pointing**: 4 seconds of pointer motion at the shell's 30 Hz publish
//!   rate (`peer_presence::MIN_PUBLISH_GAP`), which the server forwards in the
//!   order it was posted, one frame per move;
//! - **idle** for the rest of the minute.
//!
//! That is 1,800 edits and 7,200 pointer moves per active hour, the cost
//! model's assumptions. Members start their minute at
//! staggered offsets, so the load is spread as people's would be.
//!
//! The first minute warms up; then each further minute is one sample window.
//! Per window the parent asks the relay process for its CPU time, resident
//! memory, egress counters and the bytes its iroh endpoint put on UDP, and
//! divides by members and hours. Fan-out latency is publish to receipt
//! by each other member of the workspace; pointer and caret latency are the same
//! for each pointer move and caret the server forwarded (one it coalesced
//! away under a later one is never received, and not counted).
//!
//! Packets are counted where QUIC already counts them, on each
//! member's connection ([`Packets`]): the datagrams the members
//! sent (and of those, the ones carrying data) and the datagrams the server
//! sent them (and of those, the ones carrying only acknowledgements), per
//! member-hour. Bytes cannot see a change that sends the same bytes in
//! more packets; these can.
//!
//! The samples go to `--out DIR/server-load.json` for
//! `tools/perf/adapters.py server-load`, which turns them into records and
//! prices them with `tools/perf/prices.toml`; the summary is printed as one
//! `SERVER_LOAD key=value …` line.
//!
//! The server process ends when this process ends, however it ends: it reads
//! its commands from a pipe this process holds, and is killed on drop.

mod server_process;
mod workload;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;

use server_process::{Sample, ServerProcess};
use workload::Fleet;

/// Length of one workload cycle and of one sample window.
const WINDOW: Duration = Duration::from_secs(60);
const SECONDS_PER_HOUR: f64 = 3_600.0;
const MIB: f64 = 1_048_576.0;

/// Measure what N members cost one server.
#[derive(Parser, Debug)]
#[command(name = "server_load")]
struct Args {
    /// Simulated members, each its own device and link.
    #[arg(long, default_value_t = 100)]
    members: usize,
    /// Members per workspace; the last workspace takes the remainder.
    #[arg(long, default_value_t = 4)]
    workspace_size: usize,
    /// Measured minutes after the one-minute warm-up; each is one sample.
    #[arg(long, default_value_t = 10)]
    minutes: u32,
    /// Directory for `server-load.json`.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Confine the server process to this many CPUs, as on a VM with that many
    /// vCPUs: its daemon runtime then starts that many workers (at most
    /// four), and its blocking pool shares the same CPUs. The members
    /// run on the CPUs left over. Linux only; by default the server may use
    /// every CPU.
    #[arg(long)]
    server_cpus: Option<usize>,
    /// Run as the server process (internal: the bench starts it itself).
    #[arg(long, hide = true)]
    serve: bool,
    /// The CPUs the server process runs on (internal: `--server-cpus` picks them).
    #[arg(long, hide = true, value_delimiter = ',')]
    cpus: Vec<usize>,
}

fn main() -> ExitCode {
    // Both processes run Automerge in isolated workers (the members'
    // `StatePeer`s, the server's store), which re-execute this binary.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    let args = Args::parse();
    let result = if args.serve {
        server_process::serve(&args.cpus)
    } else {
        bench(&args)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("server_load: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn bench(args: &Args) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.workspace_size >= 2,
        "--workspace-size must be at least 2"
    );
    anyhow::ensure!(
        args.members >= 2 && args.members.checked_rem(args.workspace_size) != Some(1),
        "--members must fill workspaces of at least two"
    );
    anyhow::ensure!(args.minutes >= 1, "--minutes must be at least 1");
    // Split the CPUs before this process starts its runtime, so its workers
    // count only the members' share.
    let server_cpus = match args.server_cpus {
        Some(count) => Some(server_process::split_cpus(count)?),
        None => None,
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args, server_cpus.as_deref()))
}

async fn run(args: &Args, server_cpus: Option<&[usize]>) -> anyhow::Result<()> {
    let fleet = Fleet::enrol(args.members, args.workspace_size)?;
    let state = tempfile::Builder::new()
        .prefix("tilo-server-load-")
        .tempdir()
        .context("the server's state directory")?;
    let mut server = ServerProcess::spawn(&fleet, state.path(), server_cpus).await?;
    let result = measure(args, &fleet, &mut server).await;
    server.stop().await;
    drop(state);
    let report = result?;
    println!("{}", report.summary_line());
    if let Some(out) = &args.out {
        std::fs::create_dir_all(out)?;
        let path = out.join("server-load.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report.json())?)
            .with_context(|| format!("write {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

async fn measure(
    args: &Args,
    fleet: &Arc<Fleet>,
    server: &mut ServerProcess,
) -> anyhow::Result<Report> {
    let idle = server.sample().await?;
    let members = fleet.connect(server.addr(), server.device()).await?;
    anyhow::ensure!(
        members.len() == args.members,
        "{} members connected, not {}",
        members.len(),
        args.members
    );
    let connected = server.sample().await?;
    eprintln!(
        "{} members in {} workspaces connected; server RSS {:.1} MiB idle, {:.1} MiB connected",
        members.len(),
        fleet.workspaces(),
        mib(idle.rss_bytes),
        mib(connected.rss_bytes),
    );
    let start = Instant::now()
        .checked_add(Duration::from_secs(1))
        .context("clock")?;
    let windows = usize::try_from(args.minutes)?;
    let end = at(start, WINDOW, windows.saturating_add(1))?;
    let links: Vec<_> = members.iter().map(|member| member.link.clone()).collect();
    let tasks = workload::play(members, start, end);

    tokio::time::sleep_until(at(start, WINDOW, 1)?.into()).await;
    fleet.measuring.store(true, Ordering::SeqCst);
    let mut previous = server.sample().await?;
    let mut previous_packets = Packets::of(&links)?;
    let mut previous_at = Instant::now();
    let mut samples = Vec::with_capacity(windows);
    for window in 0..windows {
        tokio::time::sleep_until(at(start, WINDOW, window.saturating_add(2))?.into()).await;
        let sample = server.sample().await?;
        let packets = Packets::of(&links)?;
        let now = Instant::now();
        samples.push(Window::between(
            &previous,
            &sample,
            packets.since(previous_packets),
            now.duration_since(previous_at),
        ));
        previous = sample;
        previous_packets = packets;
        previous_at = now;
    }
    fleet.measuring.store(false, Ordering::SeqCst);
    for task in tasks {
        task.await??;
    }
    drop(links);
    anyhow::ensure!(
        previous.subscriber_dropped == idle.subscriber_dropped,
        "the server dropped {} member link(s) under load; the run measured a degraded server",
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

/// The members' QUIC connections to the server, summed: the packets
/// they sent it and it sent them. On the bench's loopback path nothing is
/// lost, so what they received is what the server sent. Read from each
/// connection's own counters (`iroh::endpoint::Connection::stats`), which
/// QUIC keeps anyway; nothing is logged or hooked per packet.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Packets {
    /// UDP datagrams the members sent the relay.
    client: u64,
    /// Of those, the ones carrying stream data: one STREAM frame each, as a
    /// link has one stream and QUIC writes a stream once per packet.
    client_data: u64,
    /// UDP datagrams the relay sent the members.
    server: u64,
    /// Of those, the ones carrying stream data or a PING (keep-alives, path
    /// MTU probes): the rest carry only acknowledgements.
    server_eliciting: u64,
}

impl Packets {
    /// Every member link's counters now. A link that is not an iroh connection
    /// fails the run: a packet KPI that cannot see its packets must not
    /// report zero.
    fn of(links: &[Arc<tilo_server_client::link::Link>]) -> anyhow::Result<Self> {
        links.iter().try_fold(Self::default(), |sum, link| {
            let stats = link
                .connection()
                .context("a member's link is not an iroh connection")?
                .stats();
            Ok(Self {
                client: sum.client.saturating_add(stats.udp_tx.datagrams),
                client_data: sum.client_data.saturating_add(stats.frame_tx.stream),
                server: sum.server.saturating_add(stats.udp_rx.datagrams),
                server_eliciting: sum
                    .server_eliciting
                    .saturating_add(stats.frame_rx.stream)
                    .saturating_add(stats.frame_rx.ping),
            })
        })
    }

    /// What was counted between `before` and these counters.
    const fn since(self, before: Self) -> Self {
        Self {
            client: self.client.saturating_sub(before.client),
            client_data: self.client_data.saturating_sub(before.client_data),
            server: self.server.saturating_sub(before.server),
            server_eliciting: self
                .server_eliciting
                .saturating_sub(before.server_eliciting),
        }
    }

    /// Datagrams the server sent that carried only acknowledgements.
    const fn server_ack_only(self) -> u64 {
        self.server.saturating_sub(self.server_eliciting)
    }
}

/// What the server spent during one measured window, and how long it was.
struct Window {
    cpu_s: f64,
    rss_bytes: u64,
    transport_bytes: u64,
    app_bytes: u64,
    live_bytes: u64,
    packets: Packets,
    hours: f64,
}

impl Window {
    fn between(before: &Sample, after: &Sample, packets: Packets, elapsed: Duration) -> Self {
        Self {
            cpu_s: float(after.cpu_ns.saturating_sub(before.cpu_ns)) / 1e9,
            rss_bytes: after.rss_bytes,
            transport_bytes: after
                .transport_bytes_sent
                .saturating_sub(before.transport_bytes_sent),
            app_bytes: after.bytes_sent.saturating_sub(before.bytes_sent),
            live_bytes: after.bytes_sent_live.saturating_sub(before.bytes_sent_live),
            packets,
            hours: elapsed.as_secs_f64() / SECONDS_PER_HOUR,
        }
    }
}

/// Everything one run measured.
struct Report {
    members: usize,
    workspaces: usize,
    workspace_size: usize,
    /// The CPUs the server was confined to (`--relay-cpus`); none is all.
    server_cpus: Option<usize>,
    idle_rss_bytes: u64,
    windows: Vec<Window>,
    fanout_ms: Vec<f64>,
    /// Pointer moves and carets, publish to receipt by each other member.
    pointer_ms: Vec<f64>,
    caret_ms: Vec<f64>,
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
            "no member received another's edit while measuring; nothing was fanned out"
        );
        fanout_ms.sort_by(f64::total_cmp);
        let (mut pointer_ms, mut caret_ms) = fleet.take_presence_latencies();
        anyhow::ensure!(
            !pointer_ms.is_empty() && !caret_ms.is_empty(),
            "no member received another's pointer move and caret while measuring"
        );
        pointer_ms.sort_by(f64::total_cmp);
        caret_ms.sort_by(f64::total_cmp);
        Ok(Self {
            members: args.members,
            workspaces: fleet.workspaces(),
            workspace_size: args.workspace_size,
            server_cpus: args.server_cpus,
            idle_rss_bytes: idle.rss_bytes,
            windows,
            fanout_ms,
            pointer_ms,
            caret_ms,
            edits: fleet.edits.load(Ordering::SeqCst),
            presence: fleet.presence.load(Ordering::SeqCst),
            undelivered: fleet.undelivered(),
        })
    }

    fn per_member_hour(&self, value: impl Fn(&Window) -> f64) -> Vec<f64> {
        let members = float(u64::try_from(self.members).unwrap_or(u64::MAX));
        self.windows
            .iter()
            .map(|window| value(window) / members / window.hours)
            .collect()
    }

    fn cpu_s(&self) -> Vec<f64> {
        self.per_member_hour(|window| window.cpu_s)
    }

    fn transport_bytes(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.transport_bytes))
    }

    fn app_bytes(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.app_bytes))
    }

    fn live_bytes(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.live_bytes))
    }

    /// UDP datagrams the members sent the relay.
    fn client_packets(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.packets.client))
    }

    /// Client datagrams that carried stream data.
    fn client_data_packets(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.packets.client_data))
    }

    /// UDP datagrams the relay sent the members.
    fn relay_datagrams(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.packets.server))
    }

    /// Server datagrams that carried only acknowledgements.
    fn ack_only_datagrams(&self) -> Vec<f64> {
        self.per_member_hour(|window| float(window.packets.server_ack_only()))
    }

    /// Resident memory each connected, active member adds to the server.
    fn rss_mib(&self) -> Vec<f64> {
        let members = float(u64::try_from(self.members).unwrap_or(u64::MAX));
        self.windows
            .iter()
            .map(|window| mib(window.rss_bytes.saturating_sub(self.idle_rss_bytes)) / members)
            .collect()
    }

    fn json(&self) -> serde_json::Value {
        let kpi =
            |unit: &str, samples: Vec<f64>| serde_json::json!({"unit": unit, "samples": samples});
        serde_json::json!({
            "members": self.members,
            "workspaces": self.workspaces,
            "workspace_size": self.workspace_size,
            "server_cpus": self.server_cpus,
            "window_s": WINDOW.as_secs(),
            "windows": self.windows.len(),
            "server_idle_rss_mib": mib(self.idle_rss_bytes),
            "edits": self.edits,
            "presence": self.presence,
            "undelivered_edits": self.undelivered,
            "kpis": {
                "server.cpu_s_per_collab_hour": kpi("s", self.cpu_s()),
                "server.rss_mib_per_collab": kpi("MiB", self.rss_mib()),
                "server.egress_bytes_per_collab_hour": kpi("bytes", self.transport_bytes()),
                "server.frame_bytes_per_collab_hour": kpi("bytes", self.app_bytes()),
                "server.client_packets_per_collab_hour": kpi("count", self.client_packets()),
                "server.client_data_packets_per_collab_hour": kpi("count", self.client_data_packets()),
                "server.datagrams_per_collab_hour": kpi("count", self.relay_datagrams()),
                "server.ack_only_datagrams_per_collab_hour": kpi("count", self.ack_only_datagrams()),
                "server.fanout_ms": kpi("ms", self.fanout_ms.clone()),
                "server.relay.presence_pointer_ms": kpi("ms", self.pointer_ms.clone()),
                "server.relay.presence_caret_ms": kpi("ms", self.caret_ms.clone()),
            },
        })
    }

    fn summary_line(&self) -> String {
        format!(
            "SERVER_LOAD members={} workspaces={} windows={} edits={} presence={} \
             undelivered_edits={} cpu_s_per_collab_hour_p50={:.3} rss_mib_per_collab_p50={:.3} \
             egress_mb_per_collab_hour_p50={:.3} frame_mb_per_collab_hour_p50={:.3} \
             live_frame_mb_per_collab_hour_p50={:.3} client_packets_per_collab_hour_p50={:.0} \
             client_data_packets_per_collab_hour_p50={:.0} datagrams_per_collab_hour_p50={:.0} \
             ack_only_datagrams_per_collab_hour_p50={:.0} fanout_ms_p50={:.1} fanout_ms_p99={:.1} \
             presence_pointer_ms_p50={:.1} presence_pointer_ms_p99={:.1} \
             presence_caret_ms_p50={:.1} presence_caret_ms_p99={:.1}",
            self.members,
            self.workspaces,
            self.windows.len(),
            self.edits,
            self.presence,
            self.undelivered,
            median(self.cpu_s()),
            median(self.rss_mib()),
            median(self.transport_bytes()) / 1e6,
            median(self.app_bytes()) / 1e6,
            median(self.live_bytes()) / 1e6,
            median(self.client_packets()),
            median(self.client_data_packets()),
            median(self.relay_datagrams()),
            median(self.ack_only_datagrams()),
            nearest_rank(&self.fanout_ms, 50),
            nearest_rank(&self.fanout_ms, 99),
            nearest_rank(&self.pointer_ms, 50),
            nearest_rank(&self.pointer_ms, 99),
            nearest_rank(&self.caret_ms, 50),
            nearest_rank(&self.caret_ms, 99),
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

    tilo_test_env::scenario! {
        fn the_load_report_computes_its_figures_per_window;

        phase nearest_rank_matches_record_py;
        {
            let values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
            assert!((nearest_rank(&values, 50) - 5.0).abs() < f64::EPSILON);
            assert!((nearest_rank(&values, 99) - 10.0).abs() < f64::EPSILON);
            assert!(nearest_rank(&[], 50).is_nan());
        }

        phase a_window_is_divided_by_its_own_length;
        {
            let before = Sample::default();
            let after = Sample {
                cpu_ns: 3_000_000_000,
                bytes_sent: 1_000,
                transport_bytes_sent: 1_500,
                ..Sample::default()
            };
            let window = Window::between(
                &before,
                &after,
                Packets::default(),
                Duration::from_secs(1_800),
            );
            assert!((window.cpu_s - 3.0).abs() < 1e-9);
            assert!((window.hours - 0.5).abs() < 1e-9);
            assert_eq!(window.transport_bytes, 1_500);
        }

        phase packets_are_counted_per_window_and_acks_are_what_carried_nothing_else;
        {
            let before = Packets {
                client: 1_000,
                client_data: 400,
                server: 900,
                server_eliciting: 600,
            };
            let after = Packets {
                client: 1_250,
                client_data: 500,
                server: 1_200,
                server_eliciting: 780,
            };
            let window = after.since(before);
            assert_eq!(
                window,
                Packets {
                    client: 250,
                    client_data: 100,
                    server: 300,
                    server_eliciting: 180,
                }
            );
            assert_eq!(window.server_ack_only(), 120);
            // Counters never run backwards; if a reading did, nothing goes negative.
            assert_eq!(before.since(after), Packets::default());
            assert_eq!(Packets::default().server_ack_only(), 0);
        }
    }
}
