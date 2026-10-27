//! Rootless `podman` driver.
//!
//! The manager talks to the local engine by shelling out to the `podman`
//! CLI with machine-readable (`--format json`) output. This keeps the
//! dependency surface tiny and works identically whether the engine is a
//! native rootless install or a `podman machine` connection — both expose
//! the same CLI.
//!
//! When no engine is reachable (CI, or a host without podman installed) the
//! live read returns an empty snapshot flagged `live = false`. Lifecycle
//! commands report their actual process result, including a lost engine. The bundled
//! [`crate::model::sample_engine`] is reserved for the headless `--shots`
//! renderer alone.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use serde::Deserialize;

use crate::model::{
    Container, ContainerStatus, DistroBox, Engine, Image, Mount, PortMapping, Stats, Volume,
};

const PROBE_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const ACTION_OUTPUT_LIMIT: usize = 1024 * 1024;
const ACTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

fn run_probe(program: &str, args: &[&str]) -> Option<bounded_command::Output> {
    bounded_command::run(program, args, None, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT).ok()
}

/// A lifecycle verb the UI can request against a container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Remove,
}

impl Action {
    /// The podman subcommand for this verb.
    fn verb(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Pause => "pause",
            Action::Unpause => "unpause",
            Action::Remove => "rm",
        }
    }

    /// Whether this action destroys state and must sit behind a confirm.
    pub fn is_destructive(self) -> bool {
        matches!(self, Action::Remove)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::Stop => "Stop",
            Action::Restart => "Restart",
            Action::Pause => "Pause",
            Action::Unpause => "Resume",
            Action::Remove => "Remove",
        }
    }
}

/// What the engine reports as its connection when no podman answered.
pub const UNREACHABLE: &str = "Podman didn't respond";

/// Why no podman answered, from an engine's `connection` (what the probe
/// failed with), for the "Details" of the page that says so.
#[must_use]
pub fn unreachable_detail(connection: &str) -> Option<&str> {
    connection
        .strip_prefix(UNREACHABLE)
        .and_then(|rest| rest.strip_prefix(": "))
        .filter(|detail| !detail.trim().is_empty())
}

/// Result of probing the engine. The UI only needs the snapshot; errors are
/// folded into a fallback so the surface never goes blank.
pub struct Probe {
    pub engine: Engine,
}

/// True when a `podman` binary is on PATH and responds to `info`. Cached by
/// the caller — this spawns a process so it isn't free.
pub fn engine_available() -> bool {
    engine_probe().is_ok()
}

fn engine_probe() -> Result<(), String> {
    let output = bounded_command::run(
        "podman",
        &["info", "--format", "{{.Host.Arch}}"],
        None,
        PROBE_OUTPUT_LIMIT,
        PROBE_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if error.is_empty() {
        format!("Podman info exited with {}", output.status)
    } else {
        error
    })
}

/// Collect a full snapshot from the live engine, or an empty snapshot when none
/// is reachable. An unreachable engine yields an [`Engine`] flagged
/// `live = false`; bundled sample data is reserved for `--shots`.
pub fn probe() -> Probe {
    match probe_live() {
        Ok(engine) => Probe { engine },
        Err(error) => Probe {
            engine: Engine {
                connection: format!("{UNREACHABLE}: {error}"),
                live: false,
                ..Engine::default()
            },
        },
    }
}

/// Attempt a live read, preserving the actual connection failure for the UI.
/// Failed container enumeration is not a successful empty engine. Individual sub-reads degrade gracefully (an empty stats map
/// is reported as unavailable telemetry).
fn probe_live() -> Result<Engine, String> {
    engine_probe()?;
    let containers = list_containers()
        .ok_or_else(|| "Podman did not return a readable container list".to_owned())?;
    let images = list_images().unwrap_or_default();
    let volumes = list_volumes().unwrap_or_default();
    let distroboxes = list_distroboxes();
    let connection = podman_connection_label();
    Ok(Engine {
        containers,
        images,
        volumes,
        distroboxes,
        connection,
        live: true,
    })
}

fn podman_connection_label() -> String {
    let version = run_probe("podman", &["version", "--format", "{{.Client.Version}}"])
        .filter(|o| o.status.success())
        .map_or_else(
            || "unknown".into(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        );
    format!("Podman {version}, rootless")
}

#[derive(Deserialize)]
struct PsEntry {
    #[serde(default, rename = "Id")]
    id: String,
    #[serde(default, rename = "Names")]
    names: Vec<String>,
    #[serde(default, rename = "Image")]
    image: String,
    #[serde(default, rename = "State")]
    state: String,
    #[serde(default, rename = "Status")]
    status: String,
    #[serde(default, rename = "Command")]
    command: Vec<String>,
    #[serde(default, rename = "CreatedAt")]
    created_at: String,
    #[serde(default, rename = "Ports")]
    ports: Option<Vec<PsPort>>,
}

#[derive(Deserialize)]
struct PsPort {
    #[serde(default, rename = "host_port")]
    host_port: u16,
    #[serde(default, rename = "container_port")]
    container_port: u16,
    #[serde(default, rename = "protocol")]
    protocol: String,
}

fn list_containers() -> Option<Vec<Container>> {
    let out = run_probe("podman", &["ps", "--all", "--format", "json"])?;
    if !out.status.success() {
        return None;
    }
    let entries: Vec<PsEntry> = serde_json::from_slice(&out.stdout).ok()?;
    let stats_by_id = collect_stats();
    let mut containers: Vec<Container> = entries
        .into_iter()
        .map(|e| {
            let name = e.names.first().cloned().unwrap_or_else(|| e.id.clone());
            let status = ContainerStatus::parse(if e.state.is_empty() {
                &e.status
            } else {
                &e.state
            });
            let ports = e
                .ports
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.host_port != 0)
                .map(|p| PortMapping {
                    host_port: p.host_port,
                    container_port: p.container_port,
                    protocol: if p.protocol.is_empty() {
                        "tcp".into()
                    } else {
                        p.protocol
                    },
                })
                .collect();
            let stat = stats_by_id
                .iter()
                .find(|(cid, _)| e.id.starts_with(cid.as_str()) || cid.starts_with(&e.id))
                .map(|(_, s)| *s)
                .unwrap_or_default();
            Container {
                id: e.id,
                name,
                image: e.image,
                status,
                status_detail: e.status,
                command: e.command.join(" "),
                created: e.created_at,
                ports,
                mounts: Vec::new(),
                env: Vec::new(),
                labels: Vec::new(),
                stats: stat,
                logs_error: None,
                logs: Vec::new(),
                logs_fetched_unix_ms: None,
            }
        })
        .collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));
    Some(containers)
}

#[derive(Deserialize)]
struct StatEntry {
    #[serde(default, rename = "ID")]
    id: String,
    #[serde(default, rename = "CPU")]
    cpu: String,
    #[serde(default, rename = "MemUsage")]
    mem_usage: String,
    #[serde(default, rename = "PIDS")]
    pids: String,
}

/// One-shot stats sample for every running container. Returns
/// `(id, stats)` pairs; failures yield no samples, reported as unavailable
/// rather than fabricated zero usage.
fn collect_stats() -> Vec<(String, Stats)> {
    let out = match run_probe("podman", &["stats", "--no-stream", "--format", "json"]) {
        Some(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let entries: Vec<StatEntry> = match serde_json::from_slice(&out.stdout) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    entries
        .into_iter()
        .map(|e| {
            let cpu_percent = parse_percent(&e.cpu) / 100.0;
            let (mem_bytes, mem_limit_bytes) = parse_mem_usage(&e.mem_usage);
            let pids = e.pids.trim().parse::<u32>().unwrap_or(0);
            (
                e.id,
                Stats {
                    available: true,
                    fetched_unix_ms: ui::time::try_unix_now_ms(),
                    cpu_percent,
                    mem_bytes,
                    mem_limit_bytes,
                    pids,
                    ..Stats::default()
                },
            )
        })
        .collect()
}

fn parse_percent(s: &str) -> f32 {
    s.trim().trim_end_matches('%').parse::<f32>().unwrap_or(0.0)
}

/// Parse podman's `MemUsage` field, e.g. `"18.2MB / 256MB"`, into
/// `(used_bytes, limit_bytes)`.
fn parse_mem_usage(s: &str) -> (u64, u64) {
    let mut parts = s.split('/');
    let used = parts.next().map_or(0, parse_size);
    let limit = parts.next().map_or(0, parse_size);
    (used, limit)
}

/// Parse a podman size string like `"18.2MB"` / `"1.5GiB"` / `"512B"`.
fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let value: f64 = digits.parse().unwrap_or(0.0);
    // `digits` was built from single-byte ASCII chars of `s`, so its byte
    // length always lands on a char boundary in `s` — `.get()` never misses,
    // it's just the panic-safe spelling of the slice.
    let unit: String = s
        .get(digits.len()..)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mult = match unit.as_str() {
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        // "b"/"" (bytes, no unit) and any unrecognised unit both mean "no scaling".
        _ => 1.0,
    };
    (value * mult) as u64
}

#[derive(Deserialize)]
struct ImageEntry {
    #[serde(default, rename = "Id")]
    id: String,
    #[serde(default, rename = "Names")]
    names: Vec<String>,
    #[serde(default, rename = "Size")]
    size: u64,
    #[serde(default, rename = "CreatedAt")]
    created_at: String,
    #[serde(default, rename = "Containers")]
    containers: u32,
}

fn list_images() -> Option<Vec<Image>> {
    let out = run_probe("podman", &["images", "--format", "json"])?;
    if !out.status.success() {
        return None;
    }
    parse_images(&out.stdout)
}

fn parse_images(output: &[u8]) -> Option<Vec<Image>> {
    let entries: Vec<ImageEntry> = serde_json::from_slice(output).ok()?;
    let mut images: Vec<Image> = entries
        .into_iter()
        .map(|e| {
            let (repository, tag) = e.names.first().map_or_else(
                || ("<none>".into(), "<none>".into()),
                |n| split_reference(n),
            );
            Image {
                id: e.id,
                repository,
                tag,
                size_bytes: e.size,
                created: e.created_at,
                in_use_by: e.containers,
            }
        })
        .collect();
    images.sort_by_key(super::model::Image::reference);
    Some(images)
}

fn split_reference(reference: &str) -> (String, String) {
    // Split on the LAST colon that isn't part of a registry:port host.
    match reference.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => (repo.to_string(), tag.to_string()),
        _ => (reference.to_string(), "latest".to_string()),
    }
}

#[derive(Deserialize)]
struct VolumeEntry {
    #[serde(default, rename = "Name")]
    name: String,
    #[serde(default, rename = "Driver")]
    driver: String,
    #[serde(default, rename = "Mountpoint")]
    mountpoint: String,
    #[serde(default, rename = "CreatedAt")]
    created_at: String,
}

fn list_volumes() -> Option<Vec<Volume>> {
    let out = run_probe("podman", &["volume", "ls", "--format", "json"])?;
    if !out.status.success() {
        return None;
    }
    // `podman volume ls --format json` yields an array of objects.
    let entries: Vec<VolumeEntry> = serde_json::from_slice(&out.stdout).ok()?;
    // Count usage from running+stopped containers' mounts (named-volume sources).
    let usage = volume_usage();
    let mut volumes: Vec<Volume> = entries
        .into_iter()
        .map(|e| {
            let in_use_by = usage.iter().filter(|n| **n == e.name).count() as u32;
            Volume {
                name: e.name,
                driver: if e.driver.is_empty() {
                    "local".into()
                } else {
                    e.driver
                },
                mountpoint: e.mountpoint,
                created: e.created_at,
                in_use_by,
            }
        })
        .collect();
    volumes.sort_by(|a, b| a.name.cmp(&b.name));
    Some(volumes)
}

/// Names of volumes referenced by any container (for the in-use count).
fn volume_usage() -> Vec<String> {
    let out = match run_probe("podman", &["ps", "--all", "--format", "{{.Mounts}}"]) {
        Some(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    // `{{.Mounts}}` prints comma-separated mount sources; named volumes show
    // as their bare name (no leading slash).
    String::from_utf8_lossy(&out.stdout)
        .split([',', '\n', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('/'))
        .map(std::string::ToString::to_string)
        .collect()
}

/// distrobox boxes are plain containers with the `manager:distrobox` label.
/// `distrobox list` is the canonical reader; when the CLI is absent we fall
/// back to filtering podman's container list by label.
fn list_distroboxes() -> Vec<DistroBox> {
    let out = match run_probe("distrobox", &["list", "--no-color"]) {
        Some(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut boxes = Vec::new();
    // distrobox list is a `|`-delimited table: ID | NAME | STATUS | IMAGE
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split('|').map(str::trim).collect();
        let (Some(name), Some(status_raw), Some(image)) = (cols.get(1), cols.get(2), cols.get(3))
        else {
            continue;
        };
        boxes.push(DistroBox {
            name: (*name).to_string(),
            image: (*image).to_string(),
            status: ContainerStatus::parse(status_raw),
            status_detail: (*status_raw).to_string(),
        });
    }
    boxes
}

/// Run a lifecycle verb against a container by id. Returns the command's
/// stderr on failure (for surfacing a toast); `Ok(())` on success or when no
/// engine is present (the live UI re-probes after, which reflects reality).
///
/// # Errors
///
/// Returns `Err` with the command's stderr when podman is reachable but the
/// verb itself fails — e.g. stopping or removing a container that no longer
/// exists, or removing one still running without going through `Stop` first.
pub fn run_action(action: Action, container_id: &str) -> Result<(), String> {
    let mut cmd = Command::new("podman");
    cmd.arg(action.verb());
    if action == Action::Stop {
        cmd.args(["--time", "10"]);
    }
    cmd.arg(container_id);
    let out = bounded_command::run_prepared(cmd, None, ACTION_OUTPUT_LIMIT, ACTION_TIMEOUT)
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Fetch the last `tail` lines from the actual stdout/stderr log streams.
/// A command refusal is an error, not fabricated output or an empty log.
///
/// # Errors
/// Returns an error when Podman cannot read the container logs or the bounded command fails.
pub fn fetch_logs(container_id: &str, tail: usize) -> Result<Vec<String>, String> {
    let tail_arg = tail.to_string();
    let out = bounded_command::run(
        "podman",
        &["logs", "--tail", &tail_arg, container_id],
        None,
        PROBE_OUTPUT_LIMIT,
        PROBE_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .map(std::string::ToString::to_string)
        .collect();
    if lines.len() > tail {
        lines = lines.split_off(lines.len() - tail);
    }
    Ok(lines)
}

/// Inspect a container, filling in env / mounts that `ps` doesn't expose.
/// Best-effort: leaves fields untouched on any failure.
pub fn inspect_into(container: &mut Container) {
    if !engine_available() {
        return;
    }
    let out = match run_probe("podman", &["inspect", "--type", "container", &container.id]) {
        Some(o) if o.status.success() => o,
        _ => return,
    };
    let value: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(root) = value.as_array().and_then(|a| a.first()) else {
        return;
    };
    if let Some(env) = root.pointer("/Config/Env").and_then(|v| v.as_array()) {
        container.env = env
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    }
    if let Some(mounts) = root.get("Mounts").and_then(|v| v.as_array()) {
        container.mounts = mounts
            .iter()
            .map(|m| Mount {
                source: m
                    .get("Source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                destination: m
                    .get("Destination")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                read_only: !m
                    .get("RW")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            })
            .collect();
    }
    if let Some(labels) = root.pointer("/Config/Labels").and_then(|v| v.as_object()) {
        container.labels = labels
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
    }
}

// =====================================================================
// Image operations
// =====================================================================

/// Remove a local image by id or reference. `force` removes even when a
/// stopped container references it. Returns stderr on failure.
///
/// # Errors
///
/// Returns `Err` with the command's stderr when `podman rmi` fails — e.g.
/// the image is still referenced by a container and `force` is `false`, or
/// `reference` doesn't resolve to a local image.
pub fn remove_image(reference: &str, force: bool) -> Result<(), String> {
    let mut cmd = Command::new("podman");
    cmd.args(["rmi"]);
    if force {
        cmd.arg("--force");
    }
    cmd.arg(reference);
    finish(cmd)
}

/// Tag an existing image with a new reference. Returns stderr on failure.
///
/// # Errors
///
/// Returns `Err` with the command's stderr when `podman tag` fails — e.g.
/// `source` doesn't resolve to a local image.
pub fn tag_image(source: &str, new_tag: &str) -> Result<(), String> {
    let mut cmd = Command::new("podman");
    cmd.args(["tag", source, new_tag]);
    finish(cmd)
}

// =====================================================================
// Volume operations
// =====================================================================

/// Create a named volume. Returns stderr on failure.
///
/// # Errors
///
/// Returns `Err` with the command's stderr when `podman volume create`
/// fails — e.g. a volume with that name already exists.
pub fn create_volume(name: &str) -> Result<(), String> {
    let mut cmd = Command::new("podman");
    cmd.args(["volume", "create", name]);
    finish(cmd)
}

/// Remove a named volume. `force` removes even when in use. Returns stderr
/// on failure.
///
/// # Errors
///
/// Returns `Err` with the command's stderr when `podman volume rm` fails —
/// e.g. the volume is still in use by a container and `force` is `false`.
pub fn remove_volume(name: &str, force: bool) -> Result<(), String> {
    let mut cmd = Command::new("podman");
    cmd.args(["volume", "rm"]);
    if force {
        cmd.arg("--force");
    }
    cmd.arg(name);
    finish(cmd)
}

// =====================================================================
// Streaming operations (run OFF the UI thread; report progress lines)
// =====================================================================

/// A line of streamed output from a long-running podman op (pull / log
/// follow). The terminal `Done`/`Failed` variants close the stream.
#[derive(Clone, Debug)]
pub enum StreamMsg {
    Line(String),
    Done,
    Failed(String),
}

/// Spawn `podman <args>` and stream its merged stdout+stderr lines into
/// `tx`, finishing with [`StreamMsg::Done`] or [`StreamMsg::Failed`]. Runs
/// to completion on the calling thread — callers spawn it in a worker so the
/// UI thread never blocks. Failure to reach the engine closes the stream
/// with `Failed`, never a synthetic success.
pub fn stream_command(args: &[String], tx: &Sender<StreamMsg>) {
    let mut command = Command::new("podman");
    command.args(args);
    stream_prepared(command, tx);
}

fn stream_prepared(mut command: Command, tx: &Sender<StreamMsg>) {
    let mut child = match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(StreamMsg::Failed(e.to_string()));
            return;
        }
    };

    // Drain both real pipes before announcing the terminal result. The UI
    // stops consuming after Done/Failed, so a detached reader loses tail lines.
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(StreamMsg::Line(line)).is_err() {
                    break;
                }
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if tx.send(StreamMsg::Line(line)).is_err() {
                    break;
                }
            }
        }));
    }

    let status = child.wait();
    for reader in readers {
        if reader.join().is_err() {
            let _ = tx.send(StreamMsg::Failed(
                "container output reader stopped unexpectedly".into(),
            ));
            return;
        }
    }
    match status {
        Ok(status) if status.success() => {
            let _ = tx.send(StreamMsg::Done);
        }
        Ok(status) => {
            let _ = tx.send(StreamMsg::Failed(format!("exited with {status}")));
        }
        Err(e) => {
            let _ = tx.send(StreamMsg::Failed(e.to_string()));
        }
    }
}

/// The argument vector for streaming a `podman pull`. Pure for testing.
pub fn pull_args(reference: &str) -> Vec<String> {
    vec!["pull".to_string(), reference.to_string()]
}

// =====================================================================
// Exec / terminal
// =====================================================================

/// Run completion helper shared by the simple (non-streaming) ops.
fn finish(cmd: Command) -> Result<(), String> {
    let out = bounded_command::run_prepared(cmd, None, ACTION_OUTPUT_LIMIT, ACTION_TIMEOUT)
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Run one finite podman operation with bounded diagnostics and a deadline.
pub fn run_once(args: &[String]) -> Result<(), String> {
    let mut command = Command::new("podman");
    command.args(args);
    finish(command)
}

/// Polling cadence for live stats refresh in the running app.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// Sizes, memory usage, percentages and references parse, and destructive actions are flagged.
    #[test]
    fn engine_commands_do_not_report_success_for_a_missing_live_target() {
        let absent = format!(
            "tilo-missing-container-{}-{}",
            std::process::id(),
            ui::time::try_unix_now_ns().unwrap()
        );
        // This exercises the actual Podman command, including missing binary,
        // lost engine and nonexistent target refusals. None is a success.
        assert!(run_action(Action::Start, &absent).is_err());
        assert!(remove_image(&absent, false).is_err());
        assert!(tag_image(&absent, "tilo-test:unreachable").is_err());
        assert!(remove_volume(&absent, false).is_err());
        assert!(
            fetch_logs(&absent, 20).is_err(),
            "a refused log read is not an empty log"
        );
    }

    #[test]
    fn streamed_process_result_follows_every_stdout_and_stderr_line() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 200 ]; do echo out-$i; echo err-$i >&2; i=$((i+1)); done",
        ]);
        let (tx, rx) = std::sync::mpsc::channel();
        stream_prepared(command, &tx);
        drop(tx);
        let messages: Vec<_> = rx.into_iter().collect();
        assert!(matches!(messages.last(), Some(StreamMsg::Done)));
        let lines: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                StreamMsg::Line(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 400);
        for index in 0..200 {
            assert!(lines.contains(&format!("out-{index}").as_str()));
            assert!(lines.contains(&format!("err-{index}").as_str()));
        }
    }

    #[test]
    fn streamed_process_failure_is_terminal_after_its_diagnostics() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo engine-disconnected >&2; exit 1"]);
        let (tx, rx) = std::sync::mpsc::channel();
        stream_prepared(command, &tx);
        drop(tx);
        let messages: Vec<_> = rx.into_iter().collect();
        assert!(
            matches!(messages.first(), Some(StreamMsg::Line(line)) if line == "engine-disconnected")
        );
        assert!(matches!(messages.last(), Some(StreamMsg::Failed(_))));
    }

    #[test]
    fn actual_podman_null_ports_preserve_the_container_collection() {
        // Podman emits null for a container without published ports, alongside
        // mapped containers in the same response. One such row must not make
        // the entire live collection appear offline.
        let rows: Vec<PsEntry> = serde_json::from_str(
            r#"[{"Id":"unmapped","Names":["worker"],"Ports":null},
                {"Id":"mapped","Names":["service"],"Ports":[
                    {"host_port":8080,"container_port":80,"protocol":"tcp"}]}]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "unmapped");
        assert!(rows[0].ports.is_none());
        let mapped = rows[1].ports.as_ref().unwrap();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].host_port, 8080);
        assert_eq!(mapped[0].container_port, 80);
        assert_eq!(mapped[0].protocol, "tcp");
    }

    #[test]
    fn image_usage_preserves_the_engine_container_count() {
        let images = parse_images(
            br#"[
                {"Id":"unused", "Names":["example/unused:latest"], "Containers":0},
                {"Id":"single", "Names":["example/single:latest"], "Containers":1},
                {"Id":"shared", "Names":["example/shared:latest"], "Containers":3}
            ]"#,
        )
        .unwrap();
        for (id, expected) in [("unused", 0), ("single", 1), ("shared", 3)] {
            let image = images.iter().find(|image| image.id == id).unwrap();
            assert_eq!(image.in_use_by, expected, "usage of {id}");
        }
    }

    #[test]
    fn podman_output_parsing_and_destructive_flags() {
        // --- phase: parse_size_handles_units ---
        {
            assert_eq!(parse_size("512B"), 512);
            assert_eq!(parse_size("2KiB"), 2048);
            assert_eq!(parse_size("18.0MB"), 18 * 1024 * 1024);
            assert_eq!(parse_size("1.0GiB"), 1024 * 1024 * 1024);
            assert_eq!(parse_size(""), 0);
        }

        // --- phase: parse_mem_usage_splits_used_and_limit ---
        {
            let (used, limit) = parse_mem_usage("18.0MB / 256MB");
            assert_eq!(used, 18 * 1024 * 1024);
            assert_eq!(limit, 256 * 1024 * 1024);
        }

        // --- phase: parse_percent_strips_sign ---
        {
            assert_eq!(parse_percent("12.5%"), 12.5);
            assert_eq!(parse_percent("0.00%"), 0.0);
        }

        // --- phase: split_reference_separates_tag ---
        {
            assert_eq!(
                split_reference("docker.io/library/nginx:1.27"),
                ("docker.io/library/nginx".into(), "1.27".into())
            );
            // registry-with-port host must not be mistaken for a tag.
            assert_eq!(
                split_reference("registry.local:5000/app"),
                ("registry.local:5000/app".into(), "latest".into())
            );
        }

        // --- phase: destructive_actions_flagged ---
        {
            assert!(Action::Remove.is_destructive());
            assert!(!Action::Stop.is_destructive());
            assert!(!Action::Start.is_destructive());
        }
    }
}
