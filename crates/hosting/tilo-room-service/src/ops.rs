//! What an operator needs on launch day: Prometheus metrics, browser error
//! reports and a switch that stops new visitors without a redeploy.
//!
//! The counters live in one process-wide [`Ops`] so the join routes, the
//! persistent room's supervisor and the error endpoint all feed the same
//! `/metrics` page. Whatever other processes of the pod measure arrives as
//! Prometheus text files in `<state>/metrics.d/*.prom` (the bridge writes
//! `bridge.prom`), which `/metrics` appends under a freshness gauge; the
//! relay's own counters are read from the snapshot it persists every second.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use tilo_relay_protocol::metrics::{METRICS_FILE, MetricsSnapshot};

/// The file the `mode` command writes inside the state directory.
const MODE_FILE: &str = "mode";
/// Where other processes of the pod drop Prometheus text.
const TEXTFILE_DIR: &str = "metrics.d";
/// Larger text files are ignored: a runaway writer must not bloat the page.
const TEXTFILE_MAX: u64 = 64 * 1024;
/// How often the mode file is looked at.
const MODE_POLL: Duration = Duration::from_secs(1);
/// A relay snapshot older than this counts as the relay being down; the
/// relay writes one every second.
const RELAY_FRESH: Duration = Duration::from_secs(10);
/// The largest browser error report taken.
pub const REPORT_MAX_BYTES: usize = 4096;
/// Longest kept string in a report.
const FIELD_MAX: usize = 1024;
/// Reports per second across all visitors, and the burst allowed.
const REPORT_RATE: f64 = 20.0;
const REPORT_BURST: f64 = 50.0;

/// How much of the persistent room is open to visitors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Everything works.
    Normal,
    /// Nobody new becomes a member; visitors only watch, so a flood of
    /// vandals or a moderation emergency can be stopped at once.
    SpectatorOnly,
    /// The room is paused entirely: no joins and no spectating.
    BackSoon,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Normal, Self::SpectatorOnly, Self::BackSoon];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::SpectatorOnly => "spectator-only",
            Self::BackSoon => "back-soon",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == text.trim())
    }

    /// Whether a visitor may become a guest or create a room.
    pub fn allows_joins(self) -> bool {
        self == Self::Normal
    }

    /// Whether a visitor may watch read-only.
    pub fn allows_spectators(self) -> bool {
        self != Self::BackSoon
    }
}

/// Why a join did not happen; the label on `join_failures_total`.
#[derive(Clone, Copy, Debug)]
pub enum JoinFailure {
    /// The visitor's address used up its join window.
    RateLimited,
    /// The online or room cap.
    Full,
    /// The address was removed within the cooldown.
    CoolingDown,
    /// The kill switch is on.
    Paused,
    /// The relay did not answer or ended.
    RelayUnavailable,
    /// A malformed or unknown request.
    Invalid,
    /// The room refused the admission.
    Refused,
}

impl JoinFailure {
    const ALL: [Self; 7] = [
        Self::RateLimited,
        Self::Full,
        Self::CoolingDown,
        Self::Paused,
        Self::RelayUnavailable,
        Self::Invalid,
        Self::Refused,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Full => "full",
            Self::CoolingDown => "cooling_down",
            Self::Paused => "paused",
            Self::RelayUnavailable => "relay_unavailable",
            Self::Invalid => "invalid",
            Self::Refused => "refused",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|other| other.label() == self.label())
            .unwrap_or(0)
    }
}

/// A token bucket for browser error reports.
struct Bucket {
    tokens: f64,
    at: Instant,
}

pub struct Ops {
    state: PathBuf,
    initial_mode: Mode,
    mode: Mutex<(Instant, Mode)>,
    joins: AtomicU64,
    failures: [AtomicU64; 7],
    guests: AtomicU64,
    spectators: AtomicU64,
    reports: AtomicU64,
    reports_dropped: AtomicU64,
    report_bucket: Mutex<Bucket>,
}

static OPS: OnceLock<Ops> = OnceLock::new();

/// The process-wide instance; `install` runs first in `main`.
pub fn get() -> Option<&'static Ops> {
    OPS.get()
}

pub fn install(state: PathBuf, initial_mode: Mode) -> &'static Ops {
    OPS.get_or_init(|| Ops::new(state, initial_mode))
}

impl Ops {
    pub fn new(state: PathBuf, initial_mode: Mode) -> Self {
        let now = Instant::now();
        Self {
            state,
            initial_mode,
            // Stale from the start so the first read looks at the file.
            mode: Mutex::new((now.checked_sub(MODE_POLL).unwrap_or(now), initial_mode)),
            joins: AtomicU64::new(0),
            failures: Default::default(),
            guests: AtomicU64::new(0),
            spectators: AtomicU64::new(0),
            reports: AtomicU64::new(0),
            reports_dropped: AtomicU64::new(0),
            report_bucket: Mutex::new(Bucket {
                tokens: REPORT_BURST,
                at: now,
            }),
        }
    }

    /// The current mode: the operator's `mode` file wins over the
    /// environment's initial one, and is looked at once a second.
    pub fn mode(&self) -> Mode {
        let mut cached = self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        if cached.0.elapsed() >= MODE_POLL {
            let from_file = std::fs::read_to_string(self.state.join(MODE_FILE))
                .ok()
                .and_then(|text| Mode::parse(&text));
            *cached = (Instant::now(), from_file.unwrap_or(self.initial_mode));
        }
        cached.1
    }

    pub fn joined(&self) {
        self.joins.fetch_add(1, Ordering::Relaxed);
    }

    pub fn join_failed(&self, why: JoinFailure) {
        if let Some(counter) = self.failures.get(why.index()) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The members online now, as the relay's `Online` event counts them.
    pub fn set_online(&self, guests: u64, spectators: u64) {
        self.guests.store(guests, Ordering::Relaxed);
        self.spectators.store(spectators, Ordering::Relaxed);
    }

    /// Takes one report token; false when reports arrive too fast
    /// across all visitors.
    pub fn admit_report(&self) -> bool {
        let mut bucket = self
            .report_bucket
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let refill = now.duration_since(bucket.at).as_secs_f64() * REPORT_RATE;
        bucket.tokens = (bucket.tokens + refill).min(REPORT_BURST);
        bucket.at = now;
        let taken = bucket.tokens >= 1.0;
        if taken {
            bucket.tokens -= 1.0;
        }
        drop(bucket);
        if taken {
            self.reports.fetch_add(1, Ordering::Relaxed);
        } else {
            self.reports_dropped.fetch_add(1, Ordering::Relaxed);
        }
        taken
    }

    /// The `/metrics` page. `rooms` is how many rooms run now.
    pub fn render(&self, rooms: usize) -> String {
        let mut out = String::new();
        let mode = self.mode();
        metric(&mut out, "gauge", "tilo_rooms_rooms", "Rooms running.");
        sample(&mut out, "tilo_rooms_rooms", "", rooms);
        metric(
            &mut out,
            "gauge",
            "tilo_rooms_mode",
            "1 for the current mode.",
        );
        for each in Mode::ALL {
            let label = format!("{{mode=\"{}\"}}", each.as_str());
            sample(&mut out, "tilo_rooms_mode", &label, u8::from(each == mode));
        }
        metric(
            &mut out,
            "gauge",
            "tilo_rooms_online_guests",
            "Guests online.",
        );
        sample(&mut out, "tilo_rooms_online_guests", "", load(&self.guests));
        metric(
            &mut out,
            "gauge",
            "tilo_rooms_online_spectators",
            "Spectators online.",
        );
        sample(
            &mut out,
            "tilo_rooms_online_spectators",
            "",
            load(&self.spectators),
        );
        metric(
            &mut out,
            "counter",
            "tilo_rooms_joins_total",
            "Joins that succeeded; rate() is joins per second.",
        );
        sample(&mut out, "tilo_rooms_joins_total", "", load(&self.joins));
        metric(
            &mut out,
            "counter",
            "tilo_rooms_join_failures_total",
            "Joins that failed, by reason.",
        );
        for (why, count) in JoinFailure::ALL.into_iter().zip(&self.failures) {
            let label = format!("{{reason=\"{}\"}}", why.label());
            sample(
                &mut out,
                "tilo_rooms_join_failures_total",
                &label,
                load(count),
            );
        }
        metric(
            &mut out,
            "counter",
            "tilo_rooms_client_errors_total",
            "Browser error reports logged.",
        );
        sample(
            &mut out,
            "tilo_rooms_client_errors_total",
            "",
            load(&self.reports),
        );
        metric(
            &mut out,
            "counter",
            "tilo_rooms_client_errors_dropped_total",
            "Browser error reports dropped by the rate limit.",
        );
        sample(
            &mut out,
            "tilo_rooms_client_errors_dropped_total",
            "",
            load(&self.reports_dropped),
        );
        relay(&mut out, &self.state);
        textfiles(&mut out, &self.state.join(TEXTFILE_DIR));
        out
    }
}

fn load(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

fn metric(out: &mut String, kind: &str, name: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

fn sample(out: &mut String, name: &str, labels: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{name}{labels} {value}");
}

fn age_of(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

/// The relay snapshots under the state directory: one per room
/// (`<state>/<room>/state/metrics.json`) or the persistent room's own.
fn relay_snapshots(state: &Path) -> Vec<(MetricsSnapshot, Duration)> {
    let mut candidates = vec![state.join(METRICS_FILE)];
    if let Ok(entries) = std::fs::read_dir(state) {
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            candidates.push(entry.path().join(METRICS_FILE));
            candidates.push(entry.path().join("state").join(METRICS_FILE));
        }
    }
    candidates
        .into_iter()
        .filter_map(|path| {
            let age = age_of(&path)?;
            let snapshot = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            Some((snapshot, age))
        })
        .collect()
}

fn relay(out: &mut String, state: &Path) {
    let snapshots = relay_snapshots(state);
    let youngest = snapshots.iter().map(|(_, age)| *age).min();
    let fresh: Vec<&MetricsSnapshot> = snapshots
        .iter()
        .filter(|(_, age)| *age < RELAY_FRESH)
        .map(|(snapshot, _)| snapshot)
        .collect();
    let sum = |field: fn(&MetricsSnapshot) -> u64| -> u64 {
        fresh.iter().map(|snapshot| field(snapshot)).sum()
    };
    let rows: [(&str, &str, &str, u64); 8] = [
        (
            "tilo_rooms_relay_up",
            "gauge",
            "Relays whose metrics were written in the last 10 s; 0 means the relay is down.",
            u64::try_from(fresh.len()).unwrap_or(u64::MAX),
        ),
        (
            "tilo_rooms_relay_sessions",
            "gauge",
            "Peer sessions connected.",
            sum(|s| s.sessions_open),
        ),
        (
            "tilo_rooms_relay_auth_rejected_total",
            "counter",
            "Connections refused at authentication.",
            sum(|s| s.auth_rejected),
        ),
        (
            "tilo_rooms_relay_live_drops_total",
            "counter",
            "Live-plane subscribers dropped because they fell behind.",
            sum(|s| s.subscriber_dropped_backpressure),
        ),
        (
            "tilo_rooms_relay_subscriber_drops_total",
            "counter",
            "Peer sessions that ended with an error.",
            sum(|s| s.subscriber_dropped),
        ),
        (
            "tilo_rooms_relay_records_total",
            "counter",
            "Records received; the ratio of the rates of record_bytes_total to this is the mean record size.",
            sum(|s| s.deltas_received),
        ),
        (
            "tilo_rooms_relay_record_bytes_total",
            "counter",
            "Framed record bytes retained.",
            sum(|s| s.bytes_queued),
        ),
        (
            "tilo_rooms_relay_bytes_sent_total",
            "counter",
            "Bytes written to peer sessions.",
            sum(|s| s.bytes_sent),
        ),
    ];
    for (name, kind, help, value) in rows {
        metric(out, kind, name, help);
        sample(out, name, "", value);
    }
    if let Some(age) = youngest {
        metric(
            out,
            "gauge",
            "tilo_rooms_relay_metrics_age_seconds",
            "Age of the newest relay snapshot.",
        );
        sample(
            out,
            "tilo_rooms_relay_metrics_age_seconds",
            "",
            age.as_secs(),
        );
    }
}

/// Appends every `*.prom` file of `dir`, with a per-file age gauge so a
/// writer that stopped is visible.
fn textfiles(out: &mut String, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|_| {
                path.extension()
                    .is_some_and(|extension| extension == "prom")
            })
            .map(str::to_owned)
        else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > TEXTFILE_MAX {
            continue;
        }
        let age = age_of(&path).map_or(0, |age| age.as_secs());
        if let Ok(text) = std::fs::read_to_string(&path) {
            files.insert(name, (text, age));
        }
    }
    if files.is_empty() {
        return;
    }
    metric(
        out,
        "gauge",
        "tilo_rooms_textfile_age_seconds",
        "Age of each dropped-in metrics file.",
    );
    for (name, (_, age)) in &files {
        sample(
            out,
            "tilo_rooms_textfile_age_seconds",
            &format!("{{file=\"{name}\"}}"),
            age,
        );
    }
    for (text, _) in files.values() {
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
}

/// One browser error report, as the page sends it. Unknown fields are
/// ignored; every field is cut to a safe length before it is logged.
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Report {
    kind: String,
    message: String,
    source: String,
    line: u64,
    column: u64,
    stack: String,
    page: String,
    release: String,
}

fn cut(text: &str) -> String {
    text.chars().take(FIELD_MAX).collect()
}

/// Turns a report body into the structured log line, or says why not.
pub fn report_line(body: &[u8]) -> Result<String> {
    if body.len() > REPORT_MAX_BYTES {
        bail!("report too large");
    }
    let report: Report = serde_json::from_slice(body).context("invalid report")?;
    let kind = match report.kind.as_str() {
        "error" | "unhandledrejection" | "console" => report.kind.as_str(),
        _ => "other",
    };
    Ok(serde_json::json!({
        "event": "browser_error",
        "kind": kind,
        "message": cut(&report.message),
        "source": cut(&report.source),
        "line": report.line,
        "column": report.column,
        "stack": cut(&report.stack),
        "page": cut(&report.page),
        "release": cut(&report.release),
    })
    .to_string())
}

/// The `mode` command: `tilo-room-service mode [normal|spectator-only|back-soon]`
/// prints or sets the mode of the service whose state directory is
/// `TILO_ROOMS_STATE`; the running service notices within a second.
pub fn mode_command(state: &Path, args: &[String]) -> Result<()> {
    match args {
        [] => {
            let text = std::fs::read_to_string(state.join(MODE_FILE)).unwrap_or_default();
            println!("{}", Mode::parse(&text).unwrap_or(Mode::Normal).as_str());
            Ok(())
        }
        [wanted] => {
            let Some(mode) = Mode::parse(wanted) else {
                bail!("mode is one of: normal, spectator-only, back-soon");
            };
            std::fs::create_dir_all(state).context("create the state directory")?;
            let temporary = state.join("mode.new");
            std::fs::write(&temporary, mode.as_str()).context("write the mode")?;
            std::fs::rename(&temporary, state.join(MODE_FILE)).context("set the mode")?;
            println!("{}", mode.as_str());
            Ok(())
        }
        _ => bail!("usage: tilo-room-service mode [normal|spectator-only|back-soon]"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;

    #[test]
    fn the_mode_file_overrides_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let ops = Ops::new(dir.path().to_owned(), Mode::Normal);
        assert_eq!(ops.mode(), Mode::Normal);
        mode_command(dir.path(), &["spectator-only".into()]).unwrap();
        // The poll interval caches the answer; a fresh instance reads it.
        let ops = Ops::new(dir.path().to_owned(), Mode::Normal);
        assert_eq!(ops.mode(), Mode::SpectatorOnly);
        assert!(!ops.mode().allows_joins() && ops.mode().allows_spectators());
        mode_command(dir.path(), &["back-soon".into()]).unwrap();
        let ops = Ops::new(dir.path().to_owned(), Mode::Normal);
        assert!(!ops.mode().allows_spectators());
        assert!(mode_command(dir.path(), &["nonsense".into()]).is_err());
    }

    #[test]
    fn metrics_render_counters_relay_snapshots_and_dropped_in_files() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        std::fs::create_dir_all(state.join("room/state")).unwrap();
        tilo_relay_protocol::metrics::save_metrics(
            &state.join("room/state"),
            &MetricsSnapshot {
                sessions_open: 7,
                subscriber_dropped_backpressure: 2,
                ..MetricsSnapshot::default()
            },
        )
        .unwrap();
        std::fs::create_dir_all(state.join(TEXTFILE_DIR)).unwrap();
        std::fs::write(
            state.join(TEXTFILE_DIR).join("bridge.prom"),
            "tilo_rooms_bridge_queue_depth 3",
        )
        .unwrap();
        let ops = Ops::new(state.to_owned(), Mode::Normal);
        ops.joined();
        ops.join_failed(JoinFailure::Full);
        ops.set_online(5, 9);
        let page = ops.render(1);
        for expected in [
            "tilo_rooms_rooms 1",
            "tilo_rooms_online_guests 5",
            "tilo_rooms_online_spectators 9",
            "tilo_rooms_joins_total 1",
            "tilo_rooms_join_failures_total{reason=\"full\"} 1",
            "tilo_rooms_relay_up 1",
            "tilo_rooms_relay_sessions 7",
            "tilo_rooms_relay_live_drops_total 2",
            "tilo_rooms_bridge_queue_depth 3",
            "tilo_rooms_textfile_age_seconds{file=\"bridge.prom\"}",
            "tilo_rooms_mode{mode=\"normal\"} 1",
        ] {
            assert!(page.contains(expected), "{expected} missing from\n{page}");
        }
    }

    #[test]
    fn reports_are_size_capped_cut_and_rate_limited() {
        assert!(report_line(&vec![b' '; REPORT_MAX_BYTES + 1]).is_err());
        assert!(report_line(b"not json").is_err());
        let line = report_line(
            serde_json::json!({"kind": "error", "message": "x".repeat(3000), "line": 3})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["event"], "browser_error");
        assert_eq!(value["message"].as_str().unwrap().len(), FIELD_MAX);
        let ops = Ops::new(std::env::temp_dir(), Mode::Normal);
        let taken = (0..200).filter(|_| ops.admit_report()).count();
        assert!((50..=60).contains(&taken), "{taken}");
    }
}
