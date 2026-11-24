//! The `containers` resource: what [`tilo_system_resources::containers`] says it is, read by
//! running the rootless `podman` CLI with `--format json`.
//!
//! A container read is a probe: with no podman on this computer the reading says so and stays
//! empty. The service follows `podman events`, so a container starting, stopping or being
//! removed shows here without anything polling.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use tilo_actions::ResourceSpec;
use tilo_service::{offer, unavailable};
use tilo_system_resources::containers::{
    CONTAINERS, Container, ContainerAction, ContainerState, Containers, DistroBox, Image, Mount,
    Named, PortMapping, Volume,
};

use crate::i18n::name;
use crate::device::ContainerChange;

const PROBE_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const ACTION_OUTPUT_LIMIT: usize = 1024 * 1024;
const ACTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The log lines a reading keeps for each running container.
const LOG_TAIL: usize = 200;
/// Where podman says nothing answered.
pub const UNREACHABLE: &str = "Podman didn't respond";

/// The `containers` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.containers", label = "Containers")]
pub struct ContainersReading;

/// The `containers` resource reading `containers`.
#[must_use]
pub fn resource(containers: &Containers) -> ResourceSpec {
    let mut resource = offer::<ContainerAction>(tilo_resource::typed_resource::<ContainersReading>(
        CONTAINERS,
        name("so.tilo.system.containers", "Containers"),
    ));
    if !containers.serving {
        for action in [
            "start",
            "stop",
            "restart",
            "pause",
            "unpause",
            "remove",
            "tag",
            "remove-image",
            "create-volume",
            "remove-volume",
            "create",
        ] {
            unavailable(
                &mut resource,
                action,
                "no_podman",
                "This computer has no podman",
            );
        }
    }
    resource
}

fn run_probe(program: &str, args: &[&str]) -> Option<bounded_command::Output> {
    bounded_command::run(program, args, None, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT).ok()
}

fn finish(cmd: Command) -> Result<(), String> {
    let out = bounded_command::run_prepared(cmd, None, ACTION_OUTPUT_LIMIT, ACTION_TIMEOUT)
        .map_err(|error| error.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if error.is_empty() {
            format!("podman exited with {}", out.status)
        } else {
            error
        })
    }
}

fn podman(args: &[&str]) -> Command {
    let mut cmd = Command::new("podman");
    cmd.args(args);
    cmd
}

fn engine_probe() -> Result<(), String> {
    let output = run_probe("podman", &["info", "--format", "{{.Host.Arch}}"])
        .ok_or_else(|| "podman is not on this computer".to_owned())?;
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

fn connection_label() -> String {
    let version = run_probe("podman", &["version", "--format", "{{.Client.Version}}"])
        .filter(|out| out.status.success())
        .map_or_else(
            || "unknown".to_owned(),
            |out| String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        );
    format!("Podman {version}, rootless")
}

/// What this computer's engine holds now.
#[must_use]
pub fn reading() -> Containers {
    match live() {
        Ok(containers) => containers,
        Err(error) => Containers {
            connection: format!("{UNREACHABLE}: {error}"),
            ..Containers::default()
        },
    }
}

fn live() -> Result<Containers, String> {
    engine_probe()?;
    let containers = list_containers()
        .ok_or_else(|| "Podman did not return a readable container list".to_owned())?;
    Ok(Containers {
        serving: true,
        connection: connection_label(),
        live: true,
        containers,
        images: list_images().unwrap_or_default(),
        volumes: list_volumes(),
        distroboxes: list_distroboxes(),
        pending: Vec::new(),
        problem: None,
    })
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
    #[serde(default, rename = "Labels")]
    labels: Option<serde_json::Map<String, serde_json::Value>>,
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

fn state_of(raw: &str) -> ContainerState {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.starts_with("up") || lower.starts_with("running") {
        ContainerState::Running
    } else if lower.starts_with("exited") || lower.starts_with("stopped") {
        ContainerState::Exited
    } else if lower.starts_with("created") {
        ContainerState::Created
    } else if lower.starts_with("paused") {
        ContainerState::Paused
    } else if lower.starts_with("restarting") {
        ContainerState::Restarting
    } else {
        ContainerState::Dead
    }
}

fn list_containers() -> Option<Vec<Container>> {
    let out = run_probe("podman", &["ps", "--all", "--format", "json"])?;
    if !out.status.success() {
        return None;
    }
    let entries: Vec<PsEntry> = serde_json::from_slice(&out.stdout).ok()?;
    let samples = collect_stats();
    let inspected = inspect_all(&entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>());
    let mut containers: Vec<Container> = entries
        .into_iter()
        .map(|entry| {
            let id = entry.id;
            let name = entry
                .names
                .first()
                .cloned()
                .unwrap_or_else(|| id.get(..12).unwrap_or(&id).to_owned());
            let state = state_of(if entry.state.is_empty() {
                &entry.status
            } else {
                &entry.state
            });
            let ports = entry
                .ports
                .unwrap_or_default()
                .into_iter()
                .filter(|port| port.host_port != 0)
                .map(|port| PortMapping {
                    host_port: port.host_port,
                    container_port: port.container_port,
                    protocol: if port.protocol.is_empty() {
                        "tcp".to_owned()
                    } else {
                        port.protocol
                    },
                })
                .collect();
            let labels = entry
                .labels
                .map(|labels| {
                    labels
                        .into_iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|value| Named {
                                name: key,
                                value: value.to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let sample = samples
                .iter()
                .find(|(reported, _)| container_id_matches(reported, &id))
                .map(|(_, sample)| sample.clone())
                .unwrap_or_default();
            let mut container = Container {
                id: id.clone(),
                name,
                image: entry.image,
                state,
                status_detail: entry.status,
                command: entry.command.join(" "),
                created: entry.created_at,
                ports,
                mounts: Vec::new(),
                env: Vec::new(),
                labels,
                stats: sample,
                logs: Vec::new(),
                logs_error: None,
                logs_fetched_unix_ms: None,
            };
            if let Some(found) = inspected
                .iter()
                .find(|found| container_id_matches(&found.id, &id))
            {
                container.mounts.clone_from(&found.mounts);
                container.env.clone_from(&found.env);
                if !found.labels.is_empty() {
                    container.labels.clone_from(&found.labels);
                }
            }
            container
        })
        .collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));
    for container in &mut containers {
        if container.is_running() {
            match fetch_logs(&container.id, LOG_TAIL) {
                Ok(lines) => {
                    container.logs = lines;
                    container.logs_fetched_unix_ms = unix_now_ms();
                }
                Err(error) => container.logs_error = Some(error),
            }
        }
    }
    Some(containers)
}

/// Whether a stats entry's `ID` names the container `id`: podman may report the short or the
/// full form.
fn container_id_matches(reported: &str, id: &str) -> bool {
    !reported.is_empty() && (id.starts_with(reported) || reported.starts_with(id))
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
    #[serde(default, rename = "NetIO")]
    net_io: String,
    #[serde(default, rename = "BlockIO")]
    block_io: String,
}

/// One sample for every running container; a failure yields no samples rather than zero use.
fn collect_stats() -> Vec<(String, tilo_system_resources::containers::ContainerStats)> {
    let Some(out) = run_probe("podman", &["stats", "--no-stream", "--format", "json"]) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(entries) = serde_json::from_slice::<Vec<StatEntry>>(&out.stdout) else {
        return Vec::new();
    };
    let fetched = unix_now_ms();
    entries
        .into_iter()
        .map(|entry| {
            let (mem_bytes, mem_limit_bytes) = parse_mem_usage(&entry.mem_usage);
            let (received, sent) = parse_pair(&entry.net_io);
            let (read, written) = parse_pair(&entry.block_io);
            (
                entry.id,
                tilo_system_resources::containers::ContainerStats {
                    available: true,
                    cpu_percent: parse_percent(&entry.cpu) / 100.0,
                    mem_bytes,
                    mem_limit_bytes,
                    net_rx_bytes: received,
                    net_tx_bytes: sent,
                    block_read_bytes: read,
                    block_write_bytes: written,
                    pids: entry.pids.trim().parse().unwrap_or(0),
                    fetched_unix_ms: fetched,
                },
            )
        })
        .collect()
}

fn unix_now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

fn parse_percent(text: &str) -> f64 {
    text.trim().trim_end_matches('%').parse().unwrap_or(0.0)
}

/// podman's `MemUsage`, e.g. `18.2MB / 256MB`, as `(used, limit)` in bytes.
fn parse_mem_usage(text: &str) -> (u64, u64) {
    let mut parts = text.split('/');
    let used = parts.next().map_or(0, parse_size);
    let limit = parts.next().map_or(0, parse_size);
    (used, limit)
}

/// podman's `NetIO`/`BlockIO`, e.g. `4.8MB / 19MB`.
fn parse_pair(text: &str) -> (u64, u64) {
    let mut parts = text.split('/');
    (
        parts.next().map_or(0, parse_size),
        parts.next().map_or(0, parse_size),
    )
}

/// A podman size like `18.2MB`, `1.5GiB` or `512B`, in bytes.
fn parse_size(text: &str) -> u64 {
    let text = text.trim();
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let value: f64 = digits.parse().unwrap_or(0.0);
    let unit = text
        .get(digits.len()..)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let scale = match unit.as_str() {
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a byte count read from the engine's own words"
    )]
    let bytes = (value * scale) as u64;
    bytes
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
    let entries: Vec<ImageEntry> = serde_json::from_slice(&out.stdout).ok()?;
    let mut images: Vec<Image> = entries
        .into_iter()
        .map(|entry| {
            let (repository, tag) = entry.names.first().map_or_else(
                || ("<none>".to_owned(), "<none>".to_owned()),
                |name| split_reference(name),
            );
            Image {
                id: entry.id,
                repository,
                tag,
                size_bytes: entry.size,
                created: entry.created_at,
                in_use_by: entry.containers,
            }
        })
        .collect();
    images.sort_by_key(Image::reference);
    Some(images)
}

fn split_reference(reference: &str) -> (String, String) {
    match reference.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => (repository.to_owned(), tag.to_owned()),
        _ => (reference.to_owned(), "latest".to_owned()),
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

fn list_volumes() -> Vec<Volume> {
    let Some(out) = run_probe("podman", &["volume", "ls", "--format", "json"]) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(entries) = serde_json::from_slice::<Vec<VolumeEntry>>(&out.stdout) else {
        return Vec::new();
    };
    let usage = volume_usage();
    let mut volumes: Vec<Volume> = entries
        .into_iter()
        .map(|entry| Volume {
            in_use_by: u32::try_from(
                usage.iter().filter(|name| **name == entry.name).count(),
            )
            .unwrap_or(u32::MAX),
            name: entry.name,
            driver: if entry.driver.is_empty() {
                "local".to_owned()
            } else {
                entry.driver
            },
            mountpoint: entry.mountpoint,
            created: entry.created_at,
        })
        .collect();
    volumes.sort_by(|a, b| a.name.cmp(&b.name));
    volumes
}

/// The volumes any container mounts, for the in-use count.
fn volume_usage() -> Vec<String> {
    let Some(out) = run_probe("podman", &["ps", "--all", "--format", "{{.Mounts}}"]) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .split([',', '\n', ' '])
        .map(str::trim)
        .filter(|source| !source.is_empty() && !source.starts_with('/'))
        .map(str::to_owned)
        .collect()
}

/// distrobox boxes are containers labelled `manager:distrobox`; `distrobox list` is the
/// canonical reader.
fn list_distroboxes() -> Vec<DistroBox> {
    let Some(out) = run_probe("distrobox", &["list", "--no-color"]) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut boxes = Vec::new();
    for line in text.lines().skip(1) {
        let columns: Vec<&str> = line.split('|').map(str::trim).collect();
        let (Some(name), Some(status), Some(image)) =
            (columns.get(1), columns.get(2), columns.get(3))
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        boxes.push(DistroBox {
            name: (*name).to_owned(),
            image: (*image).to_owned(),
            state: state_of(status),
            status_detail: (*status).to_owned(),
        });
    }
    boxes
}

/// One container's mounts, environment and labels, as `inspect` fills them in.
struct Inspected {
    id: String,
    mounts: Vec<Mount>,
    env: Vec<Named>,
    labels: Vec<Named>,
}

fn inspect_all(ids: &[String]) -> Vec<Inspected> {
    if ids.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["inspect", "--type", "container"];
    args.extend(ids.iter().map(String::as_str));
    let Some(out) = run_probe("podman", &args) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|root| {
            let id = root.get("Id").and_then(serde_json::Value::as_str)?.to_owned();
            let mounts = root
                .get("Mounts")
                .and_then(serde_json::Value::as_array)
                .map(|mounts| {
                    mounts
                        .iter()
                        .map(|mount| Mount {
                            source: mount
                                .get("Source")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            destination: mount
                                .get("Destination")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            read_only: !mount
                                .get("RW")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(true),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let env = root
                .pointer("/Config/Env")
                .and_then(serde_json::Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter_map(|entry| entry.split_once('='))
                        .map(|(key, value)| Named {
                            name: key.to_owned(),
                            value: value.to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let labels = root
                .pointer("/Config/Labels")
                .and_then(serde_json::Value::as_object)
                .map(|labels| {
                    labels
                        .iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|value| Named {
                                name: key.clone(),
                                value: value.to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Inspected {
                id,
                mounts,
                env,
                labels,
            })
        })
        .collect()
}

/// The last `tail` lines of a container's log, both output streams together.
fn fetch_logs(container: &str, tail: usize) -> Result<Vec<String>, String> {
    let tail_arg = tail.to_string();
    let out = run_probe("podman", &["logs", "--tail", &tail_arg, container])
        .ok_or_else(|| "podman did not answer".to_owned())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .map(str::to_owned)
        .collect();
    if lines.len() > tail {
        lines = lines.split_off(lines.len().saturating_sub(tail));
    }
    Ok(lines)
}

/// Whether `value` can be one of podman's operands: never empty, never read as an option.
fn operand(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("a name is required".to_owned());
    }
    if value.starts_with('-') {
        return Err(format!("`{value}` is not a name"));
    }
    Ok(value)
}

/// Make `change` on the engine.
///
/// # Errors
/// The engine's refusal, in its own words.
pub fn change(change: &ContainerChange) -> Result<(), String> {
    match change {
        ContainerChange::Start(id) => finish({
            let mut cmd = podman(&["start"]);
            cmd.arg(operand(id)?);
            cmd
        }),
        ContainerChange::Stop(id) => finish({
            let mut cmd = podman(&["stop", "--time", "10"]);
            cmd.arg(operand(id)?);
            cmd
        }),
        ContainerChange::Restart(id) => finish({
            let mut cmd = podman(&["restart"]);
            cmd.arg(operand(id)?);
            cmd
        }),
        ContainerChange::Pause(id) => finish({
            let mut cmd = podman(&["pause"]);
            cmd.arg(operand(id)?);
            cmd
        }),
        ContainerChange::Unpause(id) => finish({
            let mut cmd = podman(&["unpause"]);
            cmd.arg(operand(id)?);
            cmd
        }),
        ContainerChange::Remove(id) => finish({
            let mut cmd = podman(&["rm"]);
            cmd.arg(operand(id)?);
            cmd
        }),
        ContainerChange::Tag { image, tag } => finish({
            let mut cmd = podman(&["tag"]);
            cmd.args([operand(image)?, operand(tag)?]);
            cmd
        }),
        ContainerChange::RemoveImage(reference) => finish({
            let mut cmd = podman(&["rmi"]);
            cmd.arg(operand(reference)?);
            cmd
        }),
        ContainerChange::CreateVolume(name) => finish({
            let mut cmd = podman(&["volume", "create"]);
            cmd.arg(operand(name)?);
            cmd
        }),
        ContainerChange::RemoveVolume(name) => finish({
            let mut cmd = podman(&["volume", "rm", "--force"]);
            cmd.arg(operand(name)?);
            cmd
        }),
        ContainerChange::Create {
            name,
            image,
            ports,
            env,
            volumes,
            command,
        } => {
            let image = operand(image)?;
            let mut cmd = podman(&["run", "-d"]);
            let name = name.trim();
            if !name.is_empty() {
                cmd.args(["--name", operand(name)?]);
            }
            for (flag, text) in [("-p", ports), ("-e", env), ("-v", volumes)] {
                for entry in split_list(text) {
                    cmd.args([flag, operand(&entry)?]);
                }
            }
            cmd.arg(image);
            for word in command.split_whitespace() {
                cmd.arg(word);
            }
            finish(cmd)
        }
    }
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Follow the engine's event stream: `changed` runs on each line, so a container starting,
/// stopping or being removed is read again. The handle keeps the subscription alive.
pub fn follow(changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn Send>> {
    let mut child = Command::new("podman")
        .args(["events", "--format", "{{.Status}}"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let handle = std::thread::Builder::new()
        .name("tilo-containers-events".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.is_err() {
                    break;
                }
                changed();
            }
            let _ = child.wait();
        })
        .ok()?;
    Some(Box::new(handle))
}
