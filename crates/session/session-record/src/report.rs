//! The session report: summarises a login's private flight records (the
//! recorder's full and event records, and the compositor's frame trace)
//! without controlling the desktop.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::value::Value;

/// The report's schema name.
pub const SCHEMA: &str = "tilo-session-report-v1";

/// A report pattern. Every one compiles (a unit test proves it); one that did
/// not would match nothing rather than end the report.
type Pattern = LazyLock<Option<Regex>>;

macro_rules! pattern {
    ($name:ident, $pattern:expr) => {
        static $name: Pattern = LazyLock::new(|| Regex::new($pattern).ok());
    };
}

fn captures<'t>(pattern: &Pattern, text: &'t str) -> Option<Captures<'t>> {
    pattern.as_ref()?.captures(text)
}

/// Every `(name, number)` pair the two-group `pattern` finds in `text`, later
/// ones replacing earlier ones with the same name.
fn number_fields(pattern: &Pattern, text: &str) -> Vec<(String, i64)> {
    let mut fields: Vec<(String, i64)> = Vec::new();
    let Some(pattern) = pattern.as_ref() else {
        return fields;
    };
    for found in pattern.captures_iter(text) {
        let (Some(name), Some(number)) = (found.get(1), found.get(2)) else {
            continue;
        };
        let Ok(number) = number.as_str().parse::<i64>() else {
            continue;
        };
        match fields.iter_mut().find(|(key, _)| key == name.as_str()) {
            Some((_, existing)) => *existing = number,
            None => fields.push((name.as_str().to_owned(), number)),
        }
    }
    fields
}

fn without_ansi(text: &str) -> String {
    match ANSI.as_ref() {
        Some(ansi) => ansi.replace_all(text, "").into_owned(),
        None => text.to_owned(),
    }
}

pattern!(
    PAINT,
    r"paint-stats: (?P<painted>\d+)/(?P<ticks>\d+) painted .*?(?P<hz>[0-9.]+) Hz target \| present=(?P<present>\d+) backpressure=(?P<backpressure>\d+)"
);
pattern!(ANSI, r"\x1b\[[0-9;]*m");
pattern!(
    APP_FAILURE,
    r"package_id=(?P<id>[\w.-]+).*?display_name=(?P<name>.+?)\s+exit_code="
);
pattern!(
    RENDER_PHASE_FIELD,
    r"\b(frames|direct_frames|direct_dropped|flip_completions|unprofiled_frames|window_us|bridge_us|scanout_us|readback_bytes|output_bytes|avg_direct_us|max_direct_us|direct_failures)=(\d+)\b"
);
pattern!(
    DIRECT_STATUS,
    r#"\bdirect_scanout_status=(?P<status>[\w-]+)(?:\s+direct_scanout_reason=(?P<reason>"(?:[^"\\]|\\.)*"|\S+))?"#
);
pattern!(
    KMS_MODE,
    r"DRM scanout: modeset complete .*?connector=(?P<connector>\d+) .*?width=(?P<width>\d+) .*?height=(?P<height>\d+) .*?refresh_mhz=(?P<refresh_mhz>\d+)"
);
pattern!(
    KEY_LATENCY,
    r"KEY-LATENCY app=(?P<app>\S*) key_to_commit_us=(?P<key_to_commit_us>\d+) key_to_submit_us=(?P<key_to_submit_us>\d+) key_to_present_us=(?P<key_to_present_us>\d+)"
);
pattern!(KEY_UNANSWERED, r"KEY-LATENCY app=(?P<app>\S*) unanswered=1");
pattern!(
    LAUNCH_PACKAGE,
    r"\bpackage_id=(?:\x1b\[[0-9;]*m)*(?P<id>[\w.-]+)"
);
pattern!(
    LAUNCH_FIELD,
    r"\b(request_to_dispatch_us|request_to_spawn_us|request_to_map_us|spawn_to_map_us|request_to_first_present_us|spawn_to_first_present_us|map_to_first_present_us)=(?:Some\()?(\d+)"
);

/// The value at `percentile` of `values` (nearest rank), 0 for none.
#[must_use]
pub fn percentile(values: &[i64], percentile: usize) -> i64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = percentile
        .saturating_mul(sorted.len())
        .div_ceil(100)
        .clamp(1, sorted.len());
    sorted.get(rank.saturating_sub(1)).copied().unwrap_or(0)
}

fn stats(values: &[i64], percentiles: &[usize]) -> Value {
    let mut stats = Value::map();
    stats.set("count", values.len());
    for &p in percentiles {
        stats.set(&format!("p{p}"), percentile(values, p));
    }
    stats.set("max", values.iter().copied().max().unwrap_or(0));
    stats
}

/// Lines as Python's `str.splitlines` splits them.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut characters = text.char_indices().peekable();
    while let Some((at, character)) = characters.next() {
        let separator = matches!(
            character,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !separator {
            continue;
        }
        lines.push(text.get(start..at).unwrap_or_default());
        let mut end = at.saturating_add(character.len_utf8());
        if character == '\r' && characters.peek().map(|(_, next)| *next) == Some('\n') {
            characters.next();
            end = end.saturating_add(1);
        }
        start = end;
    }
    if let Some(last) = text.get(start..).filter(|last| !last.is_empty()) {
        lines.push(last);
    }
    lines
}

fn read_lossy(path: &Path) -> std::io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn unavailable(path: &Path, error: Option<String>) -> Value {
    let mut report = Value::map();
    report.set("available", false);
    report.set("path", path.display().to_string());
    if let Some(error) = error {
        report.set("error", error);
    }
    report
}

/// Rows of a CSV file as header-keyed maps; quoted fields are understood.
fn csv_rows(text: &str) -> Vec<BTreeMap<String, String>> {
    let records = csv_records(text);
    let Some((header, rows)) = records.split_first() else {
        return Vec::new();
    };
    rows.iter()
        .filter(|row| !row.is_empty())
        .map(|row| {
            header
                .iter()
                .zip(row.iter())
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .collect()
}

fn csv_records(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut characters = text.chars().peekable();
    let mut any = false;
    while let Some(character) = characters.next() {
        any = true;
        if quoted {
            match character {
                '"' if characters.peek() == Some(&'"') => {
                    characters.next();
                    field.push('"');
                }
                '"' => quoted = false,
                other => field.push(other),
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                any = false;
            }
            other => field.push(other),
        }
    }
    if any {
        record.push(field);
        records.push(record);
    }
    // A blank line is an empty record, which DictReader skips.
    records
        .into_iter()
        .map(|record| {
            if matches!(record.as_slice(), [only] if only.is_empty()) {
                Vec::new()
            } else {
                record
            }
        })
        .collect()
}

fn parse_int(value: &str) -> Option<i64> {
    let value = value.trim();
    value
        .strip_prefix('+')
        .unwrap_or(value)
        .parse::<i64>()
        .ok()
        .filter(|_| !value.starts_with("+-"))
}

/// Microseconds between consecutive painted ticks.
fn paint_periods(rows: &[BTreeMap<String, String>]) -> Vec<i64> {
    let painted_starts: Vec<i64> = rows
        .iter()
        .filter(|row| row.get("painted").is_some_and(|painted| painted == "1"))
        .filter_map(|row| {
            let start = row.get("start_us").cloned().unwrap_or_default();
            (!start.is_empty() && start.chars().all(|c| c.is_ascii_digit()))
                .then(|| start.parse().ok())
                .flatten()
        })
        .collect();
    let gaps: Vec<i64> = painted_starts
        .windows(2)
        .map(|pair| match pair {
            [previous, current] => current.saturating_sub(*previous),
            _ => 0,
        })
        .collect();
    gaps
}

/// The frame-trace part of the report: `frame-trace.csv`, one row per
/// compositor tick.
#[must_use]
pub fn trace_report(path: &Path) -> Value {
    if !path.is_file() {
        return unavailable(path, None);
    }
    let text = match read_lossy(path) {
        Ok(text) => text,
        Err(error) => return unavailable(path, Some(error.to_string())),
    };
    let rows = csv_rows(&text);
    let field = |row: &BTreeMap<String, String>, name: &str| -> String {
        row.get(name).cloned().unwrap_or_default()
    };
    let painted_row = |row: &BTreeMap<String, String>| field(row, "painted") == "1";
    let numbers = |name: &str, painted_only: bool| -> Vec<i64> {
        rows.iter()
            .filter(|row| !painted_only || painted_row(row))
            .filter_map(|row| {
                let value = field(row, name);
                if value.is_empty() {
                    None
                } else {
                    parse_int(&value)
                }
            })
            .collect()
    };
    let start = numbers("start_us", false);
    let painted = rows.iter().filter(|row| painted_row(row)).count();
    let span_us = start
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .saturating_sub(start.iter().copied().min().unwrap_or(0));
    let mut report = Value::map();
    report.set("available", true);
    report.set("path", path.display().to_string());
    report.set("rows", rows.len());
    report.set("painted", painted);
    report.set("span_us", span_us);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a rate: frame counts and microsecond spans are far below 2^52"
    )]
    let painted_fps = if span_us == 0 {
        0.0
    } else {
        painted as f64 * 1_000_000.0 / span_us as f64
    };
    report.set("painted_fps", painted_fps);
    for name in [
        "period_us",
        "input_us",
        "pump_us",
        "layout_us",
        "scene_us",
        "submit_us",
        "clients_us",
        "sleep_us",
        "input_latency_us",
        "input_retirement_latency_us",
        "deadline_late_us",
    ] {
        let painted_only = matches!(name, "layout_us" | "scene_us" | "submit_us");
        report.set(name, stats(&numbers(name, painted_only), &[50, 95, 99]));
    }
    // Main-thread work of a painted frame: layout, scene build and submit.
    let frame_work: Vec<i64> = rows
        .iter()
        .filter(|row| painted_row(row))
        .map(|row| {
            ["layout_us", "scene_us", "submit_us"]
                .iter()
                .map(|name| parse_int(&field(row, name)).unwrap_or(0))
                .sum()
        })
        .collect();
    report.set("frame_work_us", stats(&frame_work, &[50, 95]));
    report.set(
        "backpressured_ticks",
        rows.iter()
            .filter(|row| field(row, "backpressured") == "1")
            .count(),
    );
    report.set(
        "paint_period_us",
        stats(&paint_periods(&rows), &[50, 95, 99]),
    );
    report
}

fn tail(lines: &[String], count: usize) -> Value {
    Value::List(
        lines
            .get(lines.len().saturating_sub(count)..)
            .unwrap_or_default()
            .iter()
            .map(|line| Value::from(line.as_str()))
            .collect(),
    )
}

/// Samples per name, then per metric.
type Samples = BTreeMap<String, BTreeMap<String, Vec<i64>>>;

/// What the full record's lines say about presentation and typing.
#[derive(Default)]
struct Presentation {
    render_phases: Vec<Value>,
    direct_status: Vec<Value>,
    kms_modes: Vec<Value>,
    key_samples: Samples,
}

fn group_value(found: &Captures<'_>, name: &str) -> Value {
    found
        .name(name)
        .map(|value| value.as_str().to_owned())
        .into()
}

fn group_int(found: &Captures<'_>, name: &str) -> i64 {
    found
        .name(name)
        .and_then(|value| value.as_str().parse().ok())
        .unwrap_or(0)
}

fn app_name(found: &Captures<'_>) -> String {
    found
        .name("app")
        .map(|app| app.as_str())
        .filter(|app| !app.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

fn presentation(lines: &[&str]) -> Presentation {
    let mut seen = Presentation::default();
    for line in lines {
        let clean = without_ansi(line);
        if let Some(found) = captures(&KMS_MODE, &clean) {
            let mut mode = Value::map();
            for name in ["connector", "width", "height", "refresh_mhz"] {
                mode.set(name, group_int(&found, name));
            }
            seen.kms_modes.push(mode);
        }
        if let Some(found) = captures(&DIRECT_STATUS, &clean) {
            let mut status = Value::map();
            status.set("status", group_value(&found, "status"));
            status.set("reason", group_value(&found, "reason"));
            seen.direct_status.push(status);
        }
        if let Some(found) = captures(&KEY_LATENCY, &clean) {
            let metrics = seen.key_samples.entry(app_name(&found)).or_default();
            for name in ["key_to_commit_us", "key_to_submit_us", "key_to_present_us"] {
                metrics
                    .entry(name.to_owned())
                    .or_default()
                    .push(group_int(&found, name));
            }
        } else if let Some(found) = captures(&KEY_UNANSWERED, &clean) {
            seen.key_samples
                .entry(app_name(&found))
                .or_default()
                .entry("unanswered".to_owned())
                .or_default()
                .push(1);
        }
        if !clean.contains("DRM render phase summary") {
            continue;
        }
        let mut fields = Value::map();
        for (name, number) in number_fields(&RENDER_PHASE_FIELD, &clean) {
            fields.set(&name, number);
        }
        if ["frames", "window_us", "readback_bytes", "output_bytes"]
            .iter()
            .all(|key| fields.get(key).is_some())
        {
            seen.render_phases.push(fields);
        }
    }
    seen
}

/// Launch timing per package, from the event lines.
fn launch_samples(event_lines: &[&str]) -> Samples {
    let mut samples = Samples::new();
    for line in event_lines {
        let clean = without_ansi(line);
        let fields = number_fields(&LAUNCH_FIELD, &clean);
        if fields.is_empty() {
            continue;
        }
        let Some(package) = captures(&LAUNCH_PACKAGE, &clean).and_then(|found| found.name("id"))
        else {
            continue;
        };
        let metrics = samples.entry(package.as_str().to_owned()).or_default();
        for (name, number) in fields {
            metrics.entry(name).or_default().push(number);
        }
    }
    samples
}

/// Launches that ended before a window, counted per package.
fn app_failures(event_lines: &[&str]) -> Value {
    let mut failures: BTreeMap<String, i64> = BTreeMap::new();
    for line in event_lines {
        if !line.contains("application stopped before presenting a window") {
            continue;
        }
        let clean = without_ansi(line);
        let id = captures(&APP_FAILURE, &clean)
            .and_then(|found| found.name("id").map(|id| id.as_str().to_owned()))
            .unwrap_or_else(|| "unknown".to_owned());
        let count = failures.entry(id).or_default();
        *count = count.saturating_add(1);
    }
    let mut value = Value::map();
    for (id, count) in failures {
        value.set(&id, count);
    }
    value
}

/// The most ticks one idle wake-up storm ran: consecutive unpainted samples
/// whose presentation and backpressure counters did not move.
fn idle_wake_storm(paint: &[Value]) -> i64 {
    let field =
        |sample: &Value, key: &str| sample.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut longest = 0_i64;
    for pair in paint.windows(2) {
        let [previous, sample] = pair else {
            continue;
        };
        if field(sample, "painted").as_deref() == Some("0")
            && field(sample, "present") == field(previous, "present")
            && field(sample, "backpressure") == field(previous, "backpressure")
        {
            let ticks = field(sample, "ticks")
                .and_then(|ticks| ticks.parse().ok())
                .unwrap_or(0);
            longest = longest.max(ticks);
        }
    }
    longest
}

fn latency_stats(samples: &Samples, percentiles: &[usize]) -> Value {
    let mut all = Value::map();
    for (name, metrics) in samples {
        let mut stats_by_metric = Value::map();
        for (metric, values) in metrics {
            if metric == "unanswered" {
                stats_by_metric.set(metric, values.len());
            } else {
                stats_by_metric.set(metric, stats(values, percentiles));
            }
        }
        all.set(name, stats_by_metric);
    }
    all
}

fn matching(lines: &[&str], keep: impl Fn(&str) -> bool) -> Vec<String> {
    lines
        .iter()
        .filter(|line| keep(line))
        .map(|line| (*line).to_owned())
        .collect()
}

/// The flight-record part of the report: the full record at `path`, and the
/// recorder's event record at `events_path`, where launch timing and launch
/// failures outlive hours of measurement lines. A record from before the
/// recorder kept events apart has them in the full record only.
#[must_use]
pub fn flight_report(path: &Path, events_path: Option<&Path>) -> Value {
    if !path.is_file() {
        return unavailable(path, None);
    }
    let text = match read_lossy(path) {
        Ok(text) => text,
        Err(error) => return unavailable(path, Some(error.to_string())),
    };
    let lines = split_lines(&text);
    let events_text = events_path
        .filter(|events| events.is_file())
        .and_then(|events| read_lossy(events).ok());
    let event_lines = events_text
        .as_deref()
        .map_or_else(|| lines.clone(), split_lines);

    let paint: Vec<Value> = lines
        .iter()
        .filter_map(|line| captures(&PAINT, line))
        .map(|found| {
            let mut sample = Value::map();
            for name in ["painted", "ticks", "hz", "present", "backpressure"] {
                sample.set(name, group_value(&found, name));
            }
            sample
        })
        .collect();
    let slow = matching(&lines, |line| line.contains("SLOW-ITER "));
    let panics = matching(&lines, |line| {
        line.contains("panicked at ")
            || (line.contains("tilo-flight-record-v1") && line.contains("status=134"))
    });
    let fallback: Vec<String> = matching(&lines, |line| {
        line.contains("DRM direct scanout unavailable")
            || line.contains("DRM direct frame failed")
            || line.contains("DRM direct scanout degraded")
    })
    .iter()
    .map(|line| without_ansi(line))
    .collect();
    let seen = presentation(&lines);

    let mut report = Value::map();
    report.set("available", true);
    report.set("path", path.display().to_string());
    report.set(
        "bytes",
        fs::metadata(path).map_or(0, |metadata| metadata.len()),
    );
    report.set("lines", lines.len());
    report.set("idle_wake_storm_max_ticks", idle_wake_storm(&paint));
    report.set("paint_last", paint.last().cloned());
    report.set("paint_samples", Value::List(paint));
    report.set("app_failures", app_failures(&event_lines));
    report.set("slow_iterations", slow.len());
    report.set("slow_iteration_tail", tail(&slow, 20));
    report.set("fatal_panic", !panics.is_empty());
    report.set("panic_tail", tail(&panics, 10));
    report.set(
        "direct_scanout_enabled",
        lines
            .iter()
            .any(|line| line.contains("DRM direct scanout enabled")),
    );
    report.set(
        "direct_scanout_status_last",
        seen.direct_status.last().cloned(),
    );
    report.set("direct_scanout_fallback_tail", tail(&fallback, 10));
    report.set("render_phase_last", seen.render_phases.last().cloned());
    report.set("render_phase_samples", Value::List(seen.render_phases));
    report.set("kms_modes", Value::List(seen.kms_modes));
    report.set(
        "launch_latency_us",
        latency_stats(&launch_samples(&event_lines), &[50, 95]),
    );
    report.set(
        "key_latency_us",
        latency_stats(&seen.key_samples, &[50, 95, 99]),
    );
    report.set(
        "xwayland_ready",
        lines
            .iter()
            .any(|line| line.contains("Connected to Xwayland")),
    );
    report.set(
        "xwayland_unavailable",
        lines
            .iter()
            .any(|line| line.contains("X11 compatibility unavailable")),
    );
    report
}

/// The whole report for the records under `root`; `previous` reports the
/// prior login.
#[must_use]
pub fn session_report(root: &Path, previous: bool) -> Value {
    let (flight, events, trace) = if previous {
        (
            "previous.log",
            "previous-events.log",
            "previous-frame-trace.csv",
        )
    } else {
        ("latest.log", "events.log", "frame-trace.csv")
    };
    let mut report = Value::map();
    report.set("schema", SCHEMA);
    report.set(
        "flight",
        flight_report(&root.join(flight), Some(&root.join(events))),
    );
    report.set("frames", trace_report(&root.join(trace)));
    report
}

/// The render gate's budgets and requirement.
#[derive(Clone, Copy, Debug)]
pub struct RenderBudgets {
    /// Direct GPU scanout must present even if it never started.
    pub require_direct: bool,
    /// Average microseconds a frame may take to reach the display.
    pub render_us: i64,
    /// 95th-percentile microseconds of a painted frame's main-thread work.
    pub frame_work_us: i64,
}

fn int(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_int).unwrap_or(0)
}

/// Why this login's presentation misses the render-path contract, if it does.
///
/// Direct GPU scanout is the presenter wherever the renderer and the display
/// share a GPU; once it ran, it must still run at the end (a failure only
/// hands frames to the readback presenter until a retry). Frame times stay
/// within the budgets on every path.
#[must_use]
pub fn render_gate(report: &Value, budgets: RenderBudgets) -> Vec<String> {
    let flight = report.get("flight").cloned().unwrap_or(Value::Null);
    let frames = report.get("frames").cloned().unwrap_or(Value::Null);
    if !flight.get("available").is_some_and(Value::is_true) {
        let path = flight.get("path").map_or_else(
            || "None".to_owned(),
            |path| path.as_str().map_or_else(|| path.repr(), str::to_owned),
        );
        return vec![format!("no session flight record at {path}")];
    }
    let phases: Vec<&Value> = match flight.get("render_phase_samples") {
        Some(Value::List(samples)) => samples
            .iter()
            .filter(|phase| int(phase.get("frames")) > 0)
            .collect(),
        _ => Vec::new(),
    };
    let Some(last) = phases.last() else {
        return vec!["the compositor logged no DRM render phase summary with frames".to_owned()];
    };
    let mut failures = Vec::new();
    let status_last = match flight.get("direct_scanout_status_last") {
        Some(map @ Value::Map(_)) => map.clone(),
        _ => Value::map(),
    };
    let display = |value: Option<&Value>| match value {
        Some(Value::Str(text)) => text.clone(),
        Some(other) => other.repr(),
        None => "None".to_owned(),
    };
    let status = status_last.get("status");
    if budgets.require_direct
        || flight
            .get("direct_scanout_enabled")
            .is_some_and(Value::is_true)
    {
        let presenting = status.and_then(Value::as_str) == Some("enabled")
            && int(last.get("readback_bytes")) == 0;
        if !presenting {
            failures.push(format!(
                "direct GPU scanout is not presenting (status={} reason={} readback_bytes={}); degradations: {}",
                display(status),
                display(status_last.get("reason")),
                display(last.get("readback_bytes")),
                flight
                    .get("direct_scanout_fallback_tail")
                    .map_or_else(|| "[]".to_owned(), Value::repr),
            ));
        }
    }
    for phase in &phases {
        let direct = int(phase.get("direct_frames"));
        let average = int(phase.get("avg_direct_us"));
        if direct > 0 && average > budgets.render_us {
            failures.push(format!(
                "direct frames averaged {average} us over {direct} frames, budget {} us",
                budgets.render_us
            ));
        }
        let readback = int(phase.get("frames")).saturating_sub(direct);
        if readback > 0 {
            let average = int(phase.get("bridge_us")).div_euclid(readback);
            if average > budgets.render_us {
                failures.push(format!(
                    "readback frames averaged {average} us over {readback} frames, budget {} us",
                    budgets.render_us
                ));
            }
        }
    }
    if frames.get("available").is_some_and(Value::is_true) {
        let work = int(frames.get("frame_work_us").and_then(|work| work.get("p95")));
        if work > budgets.frame_work_us {
            failures.push(format!(
                "painted frames' main-thread work p95 is {work} us, budget {} us",
                budgets.frame_work_us
            ));
        }
    }
    failures
}

/// The `--key-latency` table: key-to-present latency of typing, per app.
#[must_use]
pub fn key_latency_table(report: &Value) -> String {
    let mut out = String::new();
    let keys = report
        .get("flight")
        .and_then(|flight| flight.get("key_latency_us"));
    let entries = match keys {
        Some(Value::Map(entries)) if !entries.is_empty() => entries.clone(),
        _ => {
            out.push_str("no typed keys were answered in this login's record\n");
            Vec::new()
        }
    };
    for (app, metrics) in entries {
        let unanswered = int(metrics.get("unanswered"));
        let _ = writeln!(out, "{app}: unanswered={unanswered}");
        for stage in ["key_to_commit_us", "key_to_submit_us", "key_to_present_us"] {
            let Some(stats) = metrics.get(stage) else {
                continue;
            };
            #[allow(
                clippy::cast_precision_loss,
                reason = "microsecond latencies printed in milliseconds"
            )]
            let ms = |key: &str| int(stats.get(key)) as f64 / 1000.0;
            let label = stage.strip_suffix("_us").unwrap_or(stage);
            let _ = writeln!(
                out,
                "  {label:<16} n={:<5} p50={:.1}ms p95={:.1}ms p99={:.1}ms max={:.1}ms",
                int(stats.get("count")),
                ms("p50"),
                ms("p95"),
                ms("p99"),
                ms("max"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_compiles() {
        for pattern in [
            &PAINT,
            &ANSI,
            &APP_FAILURE,
            &RENDER_PHASE_FIELD,
            &DIRECT_STATUS,
            &KMS_MODE,
            &KEY_LATENCY,
            &KEY_UNANSWERED,
            &LAUNCH_PACKAGE,
            &LAUNCH_FIELD,
        ] {
            assert!(pattern.is_some());
        }
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[18_000, 50_000], 50), 18_000);
        assert_eq!(percentile(&[18_000, 50_000], 95), 50_000);
        assert_eq!(percentile(&[5, 1, 3], 99), 5);
    }

    #[test]
    fn lines_split_as_python_splits_them() {
        assert_eq!(
            split_lines("a\nb\r\nc\rd\u{2028}e\n"),
            vec!["a", "b", "c", "d", "e"]
        );
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("tail"), vec!["tail"]);
    }

    #[test]
    fn csv_rows_follow_the_header_and_skip_blank_lines() {
        let rows = csv_rows("a,b\n1,2\n\n\"3,x\",4\n5\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1]["a"], "3,x");
        assert_eq!(rows[2].get("b"), None);
    }
}
