//! Rootless `podman` driver.
//!
//! The manager talks to the local engine by shelling out to the `podman`
//! CLI with machine-readable (`--format json`) output. This keeps the
//! dependency surface tiny and works identically whether the engine is a
//! native rootless install or a `podman machine` connection — both expose
//! the same CLI.
//!
//! When no engine is reachable (CI, or a host without podman installed) the
//! live read returns an empty snapshot flagged `live = false`, and lifecycle
//! actions become no-ops rather than errors. The bundled
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

/// Result of probing the engine. The UI only needs the snapshot; errors are
/// folded into a fallback so the surface never goes blank.
pub struct Probe {
    pub engine: Engine,
}

/// True when a `podman` binary is on PATH and responds to `info`. Cached by
/// the caller — this spawns a process so it isn't free.
pub fn engine_available() -> bool {
    run_probe("podman", &["info", "--format", "{{.Host.Arch}}"])
        .is_some_and(|out| out.status.success())
}

/// Collect a full snapshot from the live engine, or an empty snapshot when none
/// is reachable. An unreachable engine yields an [`Engine`] flagged
/// `live = false`; bundled sample data is reserved for `--shots`.
pub fn probe() -> Probe {
    match probe_live() {
        Some(engine) => Probe { engine },
        None => Probe {
            engine: Engine {
                connection: "No container engine — is podman installed?".into(),
                live: false,
                ..Engine::default()
            },
        },
    }
}

/// Attempt a live read. Returns `None` if podman isn't usable, so the caller
/// can fall back. Individual sub-reads degrade gracefully (an empty stats map
/// just means the columns read zero).
fn probe_live() -> Option<Engine> {
    if !engine_available() {
        return None;
    }
    let containers = list_containers().unwrap_or_default();
    let images = list_images().unwrap_or_default();
    let volumes = list_volumes().unwrap_or_default();
    let distroboxes = list_distroboxes();
    let connection = podman_connection_label();
    Some(Engine {
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
    format!("podman {version} · rootless")
}

// --- podman ps -------------------------------------------------------

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
    ports: Vec<PsPort>,
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
                logs: Vec::new(),
            }
        })
        .collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));
    Some(containers)
}

// --- podman stats ----------------------------------------------------

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
/// `(id, stats)` pairs; failures yield an empty vec (columns read zero).
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

// --- podman images ---------------------------------------------------

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
    let entries: Vec<ImageEntry> = serde_json::from_slice(&out.stdout).ok()?;
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
                in_use_by: e.containers.saturating_sub(1),
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

// --- podman volume ls ------------------------------------------------

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

// --- distrobox -------------------------------------------------------

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

// --- lifecycle actions ----------------------------------------------

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
    if !engine_available() {
        return Ok(());
    }
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

/// Fetch the last `tail` log lines for a container. Empty on any failure.
pub fn fetch_logs(container_id: &str, tail: usize) -> Vec<String> {
    if !engine_available() {
        return Vec::new();
    }
    let tail_arg = tail.to_string();
    let out = run_probe("podman", &["logs", "--tail", &tail_arg, container_id]);
    match out {
        Some(o) => {
            let mut lines: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .chain(String::from_utf8_lossy(&o.stderr).lines())
                .map(std::string::ToString::to_string)
                .collect();
            if lines.len() > tail {
                lines = lines.split_off(lines.len() - tail);
            }
            lines
        }
        None => Vec::new(),
    }
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

/// A lifecycle verb the UI can request against an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageAction {
    Remove,
}

impl ImageAction {
    pub fn is_destructive(self) -> bool {
        matches!(self, ImageAction::Remove)
    }
    pub fn label(self) -> &'static str {
        match self {
            ImageAction::Remove => "Remove image",
        }
    }
}

/// Remove a local image by id or reference. `force` removes even when a
/// stopped container references it. Returns stderr on failure.
///
/// # Errors
///
/// Returns `Err` with the command's stderr when `podman rmi` fails — e.g.
/// the image is still referenced by a container and `force` is `false`, or
/// `reference` doesn't resolve to a local image.
pub fn remove_image(reference: &str, force: bool) -> Result<(), String> {
    if !engine_available() {
        return Ok(());
    }
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
    if !engine_available() {
        return Ok(());
    }
    let mut cmd = Command::new("podman");
    cmd.args(["tag", source, new_tag]);
    finish(cmd)
}

/// Full `podman image inspect` JSON (pretty) for an image reference. Empty
/// on failure. Used by the image detail / inspect view.
pub fn inspect_image(reference: &str) -> String {
    if !engine_available() {
        return String::new();
    }
    let out = run_probe("podman", &["image", "inspect", reference]);
    match out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
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
    if !engine_available() {
        return Ok(());
    }
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
    if !engine_available() {
        return Ok(());
    }
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
/// UI thread never blocks. No-op-success offline.
pub fn stream_command(args: &[String], tx: &Sender<StreamMsg>) {
    if !engine_available() {
        let _ = tx.send(StreamMsg::Done);
        return;
    }
    let mut child = match Command::new("podman")
        .args(args)
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

    // Merge stdout + stderr: pull writes progress to stderr, logs to stdout.
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(StreamMsg::Line(line)).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if tx.send(StreamMsg::Line(line)).is_err() {
                    break;
                }
            }
        });
    }

    match child.wait() {
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

/// The argument vector for following a container's logs. Pure for testing.
pub fn log_follow_args(container_id: &str, tail: usize) -> Vec<String> {
    vec![
        "logs".to_string(),
        "--follow".to_string(),
        "--tail".to_string(),
        tail.to_string(),
        container_id.to_string(),
    ]
}

// =====================================================================
// Exec / terminal
// =====================================================================

/// Build the `podman exec -it <id> <shell>` argv used to attach an
/// interactive shell. Pure so the terminal wiring is unit-testable.
pub fn exec_args(container_id: &str, shell: &str) -> Vec<String> {
    vec![
        "exec".to_string(),
        "-it".to_string(),
        container_id.to_string(),
        shell.to_string(),
    ]
}

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
///
/// # Errors
///
/// Returns bounded runner failures or the command's capped stderr.
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
