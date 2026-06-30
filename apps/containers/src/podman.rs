//! Rootless `podman` driver.
//!
//! The manager talks to the local engine by shelling out to the `podman`
//! CLI with machine-readable (`--format json`) output. This keeps the
//! dependency surface tiny and works identically whether the engine is a
//! native rootless install or a `podman machine` connection — both expose
//! the same CLI.
//!
//! When no engine is reachable (headless `--shots`, CI, or a host without
//! podman installed) every read falls back to [`crate::model::sample_engine`]
//! so the UI always has something coherent to render, and lifecycle actions
//! become no-ops rather than errors.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::model::{
    Container, ContainerStatus, DistroBox, Engine, Image, Mount, PortMapping, Stats,
};

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
    Command::new("podman")
        .arg("info")
        .arg("--format")
        .arg("{{.Host.Arch}}")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Collect a full snapshot from the live engine, or the bundled sample when
/// none is reachable. Never returns an error — the worst case is sample data.
pub fn probe() -> Probe {
    match probe_live() {
        Some(engine) => Probe { engine },
        None => Probe { engine: crate::model::sample_engine() },
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
    let distroboxes = list_distroboxes();
    let connection = podman_connection_label();
    Some(Engine {
        containers,
        images,
        distroboxes,
        connection,
        live: true,
    })
}

fn podman_connection_label() -> String {
    let version = Command::new("podman")
        .args(["version", "--format", "{{.Client.Version}}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
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
    let out = Command::new("podman")
        .args(["ps", "--all", "--format", "json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let entries: Vec<PsEntry> = serde_json::from_slice(&out.stdout).ok()?;
    let stats = collect_stats();
    let mut containers: Vec<Container> = entries
        .into_iter()
        .map(|e| {
            let name = e.names.first().cloned().unwrap_or_else(|| e.id.clone());
            let status = ContainerStatus::parse(if !e.state.is_empty() { &e.state } else { &e.status });
            let ports = e
                .ports
                .into_iter()
                .filter(|p| p.host_port != 0)
                .map(|p| PortMapping {
                    host_port: p.host_port,
                    container_port: p.container_port,
                    protocol: if p.protocol.is_empty() { "tcp".into() } else { p.protocol },
                })
                .collect();
            let stat = stats
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
    let out = match Command::new("podman")
        .args(["stats", "--no-stream", "--format", "json"])
        .output()
    {
        Ok(o) if o.status.success() => o,
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
    let used = parts.next().map(parse_size).unwrap_or(0);
    let limit = parts.next().map(parse_size).unwrap_or(0);
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
    let unit: String = s[digits.len()..].trim().to_ascii_lowercase();
    let mult = match unit.as_str() {
        "b" | "" => 1.0,
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
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
    let out = Command::new("podman")
        .args(["images", "--format", "json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let entries: Vec<ImageEntry> = serde_json::from_slice(&out.stdout).ok()?;
    let mut images: Vec<Image> = entries
        .into_iter()
        .map(|e| {
            let (repository, tag) = e
                .names
                .first()
                .map(|n| split_reference(n))
                .unwrap_or_else(|| ("<none>".into(), "<none>".into()));
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
    images.sort_by(|a, b| a.reference().cmp(&b.reference()));
    Some(images)
}

fn split_reference(reference: &str) -> (String, String) {
    // Split on the LAST colon that isn't part of a registry:port host.
    match reference.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => (repo.to_string(), tag.to_string()),
        _ => (reference.to_string(), "latest".to_string()),
    }
}

// --- distrobox -------------------------------------------------------

/// distrobox boxes are plain containers with the `manager:distrobox` label.
/// `distrobox list` is the canonical reader; when the CLI is absent we fall
/// back to filtering podman's container list by label.
fn list_distroboxes() -> Vec<DistroBox> {
    let out = match Command::new("distrobox").args(["list", "--no-color"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut boxes = Vec::new();
    // distrobox list is a `|`-delimited table: ID | NAME | STATUS | IMAGE
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        if cols.len() < 4 {
            continue;
        }
        boxes.push(DistroBox {
            name: cols[1].to_string(),
            image: cols[3].to_string(),
            status: ContainerStatus::parse(cols[2]),
            status_detail: cols[2].to_string(),
        });
    }
    boxes
}

// --- lifecycle actions ----------------------------------------------

/// Run a lifecycle verb against a container by id. Returns the command's
/// stderr on failure (for surfacing a toast); `Ok(())` on success or when no
/// engine is present (the live UI re-probes after, which reflects reality).
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
    let out = cmd.output().map_err(|e| e.to_string())?;
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
    let out = Command::new("podman")
        .args(["logs", "--tail", &tail.to_string(), container_id])
        .output();
    match out {
        Ok(o) => {
            let mut lines: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .chain(String::from_utf8_lossy(&o.stderr).lines())
                .map(|l| l.to_string())
                .collect();
            if lines.len() > tail {
                lines = lines.split_off(lines.len() - tail);
            }
            lines
        }
        Err(_) => Vec::new(),
    }
}

/// Inspect a container, filling in env / mounts that `ps` doesn't expose.
/// Best-effort: leaves fields untouched on any failure.
pub fn inspect_into(container: &mut Container) {
    if !engine_available() {
        return;
    }
    let out = match Command::new("podman")
        .args(["inspect", "--type", "container", &container.id])
        .output()
    {
        Ok(o) if o.status.success() => o,
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
                source: m.get("Source").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                destination: m.get("Destination").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                read_only: !m.get("RW").and_then(|v| v.as_bool()).unwrap_or(true),
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

/// Polling cadence for live stats refresh in the running app.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_handles_units() {
        assert_eq!(parse_size("512B"), 512);
        assert_eq!(parse_size("2KiB"), 2048);
        assert_eq!(parse_size("18.0MB"), 18 * 1024 * 1024);
        assert_eq!(parse_size("1.0GiB"), 1024 * 1024 * 1024);
        assert_eq!(parse_size(""), 0);
    }

    #[test]
    fn parse_mem_usage_splits_used_and_limit() {
        let (used, limit) = parse_mem_usage("18.0MB / 256MB");
        assert_eq!(used, 18 * 1024 * 1024);
        assert_eq!(limit, 256 * 1024 * 1024);
    }

    #[test]
    fn parse_percent_strips_sign() {
        assert_eq!(parse_percent("12.5%"), 12.5);
        assert_eq!(parse_percent("0.00%"), 0.0);
    }

    #[test]
    fn split_reference_separates_tag() {
        assert_eq!(split_reference("docker.io/library/nginx:1.27"), ("docker.io/library/nginx".into(), "1.27".into()));
        // registry-with-port host must not be mistaken for a tag.
        assert_eq!(split_reference("registry.local:5000/app"), ("registry.local:5000/app".into(), "latest".into()));
    }

    #[test]
    fn destructive_actions_flagged() {
        assert!(Action::Remove.is_destructive());
        assert!(!Action::Stop.is_destructive());
        assert!(!Action::Start.is_destructive());
    }
}
