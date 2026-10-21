//! Kernel and `PipeWire` privacy state for shell indicators.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Microphone,
    Camera,
}

/// One actively-running `PipeWire` capture stream. `label` is derived from the
/// executable behind `PipeWire`'s server-assigned security PID, never from the
/// client-supplied `application.name`; an app cannot rename itself to another
/// app in the indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveMediaUse {
    pub client_id: u32,
    pub pid: u32,
    pub app_id: String,
    pub label: String,
    pub binary: PathBuf,
    pub kind: MediaKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillState {
    Unavailable,
    Allowed,
    SoftwareBlocked,
    HardwareBlocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyStatus {
    pub microphone_muted: Option<bool>,
    pub microphone_kill: KillState,
    pub camera_present: bool,
    pub camera_kill: KillState,
    pub wifi_kill: KillState,
    pub bluetooth_kill: KillState,
    pub mobile_kill: KillState,
    pub active_media: Vec<ActiveMediaUse>,
}

impl Default for PrivacyStatus {
    fn default() -> Self {
        Self {
            microphone_muted: None,
            microphone_kill: KillState::Unavailable,
            camera_present: false,
            camera_kill: KillState::Unavailable,
            wifi_kill: KillState::Unavailable,
            bluetooth_kill: KillState::Unavailable,
            mobile_kill: KillState::Unavailable,
            active_media: Vec::new(),
        }
    }
}

pub fn get_privacy_status() -> PrivacyStatus {
    let mut status = get_privacy_status_from(Path::new("/sys/class/rfkill"), Path::new("/dev"));
    status.active_media = active_media_uses().unwrap_or_default();
    status
}

fn get_privacy_status_from(rfkill: &Path, dev: &Path) -> PrivacyStatus {
    let mut status = PrivacyStatus {
        microphone_muted: crate::audio::get_source_state().ok().map(|s| s.muted),
        camera_present: std::fs::read_dir(dev).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("video"))
        }),
        ..PrivacyStatus::default()
    };
    if let Ok(entries) = std::fs::read_dir(rfkill) {
        for entry in entries.flatten() {
            let root = entry.path();
            let ty = read(&root.join("type")).unwrap_or_default();
            let state = kill_state(&root);
            match ty.as_str() {
                "wlan" => status.wifi_kill = state,
                "bluetooth" => status.bluetooth_kill = state,
                "wwan" => status.mobile_kill = state,
                // Some platforms expose physical camera/mic shutters through
                // rfkill even though these names are not mandated by Linux.
                "camera" => status.camera_kill = state,
                "microphone" => status.microphone_kill = state,
                _ => {}
            }
        }
    }
    status
}

/// Snapshot active capture streams through `PipeWire`'s own graph. `pw-dump`
/// is the reference client shipped with `PipeWire` and keeps compatibility
/// across native, Flatpak and portal clients. Attribution is independently
/// re-derived from `/proc`; client-authored labels are intentionally ignored.
///
/// # Errors
///
/// Returns an error message if `pw-dump` is not installed, the command fails, or its output is
/// not valid `PipeWire` graph JSON.
pub fn active_media_uses() -> Result<Vec<ActiveMediaUse>, String> {
    let json = crate::process::run_capture("pw-dump", &[])?;
    parse_pipewire_media(&json, Path::new("/proc"), effective_uid())
}

/// How long a settled device graph may sit with no further device facts
/// before the monitor process is stopped. Stream and client churn,
/// including `pw-dump` itself, does not extend this.
const MONITOR_QUIET: Duration = Duration::from_millis(200);

/// Whether `pw-dump --monitor` may keep running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorDisposition {
    /// The device graph is still settling, or has not been seen yet.
    Keep,
    /// Devices and hardware nodes are unchanged. The monitor must exit.
    Stop,
}

/// Device and hardware identity seen on the `PipeWire` graph.
///
/// Streams, clients, ports, links and volume are omitted. A `pw-dump`
/// client is itself a stream, so folding it into this signature kept the
/// monitor awake: every snapshot spawned another dump, and that client
/// showed up as a graph change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceGraphWatch {
    established: bool,
    signature: String,
}

impl DeviceGraphWatch {
    /// Fold one `pw-dump` JSON array into the device signature.
    ///
    /// The first document that names devices establishes the signature and
    /// keeps the monitor up long enough to confirm it. A later document
    /// with the same devices, or with only streams and clients, stops it.
    pub fn observe_document(&mut self, json: &str) -> MonitorDisposition {
        let Some(facts) = device_facts(json) else {
            return self.non_device_document();
        };
        if facts.is_empty() {
            return self.non_device_document();
        }
        let signature = facts.join("\n");
        if !self.established {
            self.signature = signature;
            self.established = true;
            MonitorDisposition::Keep
        } else if signature == self.signature {
            MonitorDisposition::Stop
        } else {
            self.signature = signature;
            MonitorDisposition::Keep
        }
    }

    /// No document arrived before [`MONITOR_QUIET`]. A graph we have already
    /// seen is unchanged; a monitor that has not produced one yet stays up.
    pub fn observe_quiet(&mut self) -> MonitorDisposition {
        if self.established {
            MonitorDisposition::Stop
        } else {
            MonitorDisposition::Keep
        }
    }

    fn non_device_document(&self) -> MonitorDisposition {
        if self.established {
            MonitorDisposition::Stop
        } else {
            MonitorDisposition::Keep
        }
    }

    fn is_established(&self) -> bool {
        self.established
    }
}

/// Subscribe to `PipeWire` until the device graph is unchanged, then stop
/// the monitor process. Capture snapshots come from the monitor's own JSON,
/// not from a second `pw-dump`: that second process is a stream, and
/// treating it as a graph change kept `--monitor` running for the whole
/// idle window.
///
/// # Errors
///
/// Returns an error message if `pw-dump` is not installed, the monitor
/// process fails to start, or it exits before any device graph arrives.
pub fn monitor_media_usage(mut on_change: impl FnMut(Vec<ActiveMediaUse>)) -> Result<(), String> {
    if !crate::process::which("pw-dump") {
        return Err("pw-dump not installed".into());
    }
    let child = Command::new("pw-dump")
        .args(["--monitor", "--no-colors"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("PipeWire monitor could not start: {error}"))?;
    run_media_monitor(child, &mut on_change)
}

fn run_media_monitor(
    mut child: std::process::Child,
    on_change: &mut impl FnMut(Vec<ActiveMediaUse>),
) -> Result<(), String> {
    let Some(stdout) = child.stdout.take() else {
        bounded_command::terminate_tree(&mut child, Duration::ZERO);
        return Err("PipeWire monitor has no output".into());
    };
    // Four fixed-size chunks bound the reader independently of graph size.
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    let reader = std::thread::Builder::new()
        .name("os-pw-dump-read".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            let mut chunk = [0_u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let Some(bytes) = chunk.get(..n) else {
                            break;
                        };
                        if tx.send(bytes.to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|error| {
            bounded_command::terminate_tree(&mut child, Duration::ZERO);
            format!("PipeWire monitor reader could not start: {error}")
        })?;
    let result = drive_media_monitor(&rx, crate::DEFAULT_TIMEOUT, on_change);
    drop(rx);
    bounded_command::terminate_tree(&mut child, Duration::ZERO);
    let _ = reader.join();
    result
}

/// Follow the monitor's output on `rx`, failing once a graph has taken
/// longer than `deadline` to arrive.
fn drive_media_monitor(
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    deadline: Duration,
    on_change: &mut impl FnMut(Vec<ActiveMediaUse>),
) -> Result<(), String> {
    let mut decoder = MonitorJson::default();
    let mut watch = DeviceGraphWatch::default();
    let mut started = Instant::now();
    loop {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err("PipeWire monitor graph deadline exceeded".into());
        }
        let wait = if watch.is_established() && !decoder.incomplete() {
            remaining.min(MONITOR_QUIET)
        } else {
            remaining
        };
        match rx.recv_timeout(wait) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if decoder.incomplete() {
                    return Err("PipeWire monitor exited during a graph".into());
                }
                return established_or_lost(watch.is_established());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !decoder.incomplete() && watch.observe_quiet() == MonitorDisposition::Stop {
                    return Ok(());
                }
                return Err("PipeWire monitor graph deadline exceeded".into());
            }
            Ok(chunk) => {
                for byte in chunk {
                    if let Some(doc) = decoder.push(byte)? {
                        let text = std::str::from_utf8(&doc)
                            .map_err(|_| "PipeWire monitor graph is not UTF-8")?;
                        publish_media_snapshot(text, on_change);
                        if watch.observe_document(text) == MonitorDisposition::Stop {
                            return Ok(());
                        }
                        if watch.is_established() {
                            started = Instant::now();
                        }
                    }
                }
            }
        }
    }
}

fn established_or_lost(established: bool) -> Result<(), String> {
    if established {
        Ok(())
    } else {
        Err("PipeWire monitor exited before a device graph".into())
    }
}

fn publish_media_snapshot(json: &str, on_change: &mut impl FnMut(Vec<ActiveMediaUse>)) {
    // A delta that only names a stream has no client security context.
    // Publishing it would clear a real indicator. The full dump carries
    // both, and that is the snapshot the indicator is allowed to show.
    if !json.contains("Interface:Client") {
        return;
    }
    if let Ok(media) = parse_pipewire_media(json, Path::new("/proc"), effective_uid()) {
        on_change(media);
    }
}

fn device_facts(json: &str) -> Option<Vec<String>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let mut facts = Vec::new();
    for object in &values {
        if let Some(fact) = device_fact(object) {
            facts.push(fact);
        }
    }
    facts.sort();
    facts.dedup();
    Some(facts)
}

fn device_fact(object: &serde_json::Value) -> Option<String> {
    let ty = object.get("type")?.as_str()?;
    let props = object
        .pointer("/info/props")
        .or_else(|| object.get("props"))?;
    if ty.ends_with(":Device") {
        let name = props.get("device.name")?.as_str()?;
        if name.is_empty() {
            return None;
        }
        return Some(format!("device:{name}"));
    }
    if ty.ends_with(":Node") {
        let class = props.get("media.class")?.as_str()?;
        if class.is_empty() || class.starts_with("Stream/") || class.starts_with("Midi/") {
            return None;
        }
        let name = props.get("node.name")?.as_str()?;
        if name.is_empty() {
            return None;
        }
        return Some(format!("node:{class}:{name}"));
    }
    None
}

/// Carry JSON framing state between reads: no previously received prefix is
/// rescanned when a large graph arrives in many small fragments.
#[derive(Default)]
struct MonitorJson {
    bytes: Vec<u8>,
    depth: usize,
    in_string: bool,
    escape: bool,
    scanned: usize,
    #[cfg(test)]
    total_scanned: usize,
}

impl MonitorJson {
    fn incomplete(&self) -> bool {
        self.depth != 0
    }

    fn push(&mut self, byte: u8) -> Result<Option<Vec<u8>>, String> {
        self.scanned = self.scanned.saturating_add(1);
        #[cfg(test)]
        {
            self.total_scanned = self.total_scanned.saturating_add(1);
        }
        if self.scanned > crate::process::MAX_CAPTURE_BYTES {
            return Err("PipeWire monitor graph exceeds output limit".into());
        }
        if self.depth == 0 && byte != b'[' {
            return Ok(None);
        }
        self.bytes.push(byte);
        if self.in_string {
            if self.escape {
                self.escape = false;
            } else if byte == b'\\' {
                self.escape = true;
            } else if byte == b'"' {
                self.in_string = false;
            }
        } else {
            match byte {
                b'"' => self.in_string = true,
                b'[' | b'{' => self.depth = self.depth.saturating_add(1),
                b']' | b'}' => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 0 {
                        if byte != b']' {
                            return Err("PipeWire monitor graph has invalid framing".into());
                        }
                        self.scanned = 0;
                        return Ok(Some(std::mem::take(&mut self.bytes)));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

/// Immediately disconnect an offending `PipeWire` client. This is deliberately
/// an ephemeral stop, not a pretend persistent permission: portal-managed
/// grants remain owned by the portal permission store.
///
/// # Errors
///
/// Returns an error message if `pw-cli` is not installed or the command fails.
pub fn revoke_media_client(client_id: u32) -> Result<(), String> {
    crate::process::run("pw-cli", &["destroy", &client_id.to_string()])
}

#[derive(Clone)]
struct VerifiedProcess {
    pid: u32,
    app_id: String,
    label: String,
    binary: PathBuf,
}

fn effective_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|line| {
                line.strip_prefix("Uid:")?
                    .split_ascii_whitespace()
                    .nth(1)?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(u32::MAX)
}

fn parse_pipewire_media(
    json: &str,
    proc_root: &Path,
    expected_uid: u32,
) -> Result<Vec<ActiveMediaUse>, String> {
    let objects: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid PipeWire graph: {e}"))?;
    let objects = objects
        .as_array()
        .ok_or("PipeWire graph is not an object array")?;
    let mut clients: HashMap<u32, VerifiedProcess> = HashMap::new();
    for object in objects {
        let ty = object.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !ty.ends_with(":Client") {
            continue;
        }
        let Some(id) = number_at(object, &["id"]) else {
            continue;
        };
        let Some(props) = object.pointer("/info/props").and_then(|v| v.as_object()) else {
            continue;
        };
        // `pipewire.sec.pid` is attached by the server/native protocol. The
        // familiar `application.process.id` is client input and is not an
        // attribution boundary.
        let Some(pid) = props.get("pipewire.sec.pid").and_then(value_u32) else {
            continue;
        };
        if let Some(process) = verify_process(proc_root, pid, expected_uid, props) {
            clients.insert(id, process);
        }
    }

    let mut out = Vec::new();
    for object in objects {
        let ty = object.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !ty.ends_with(":Node")
            || object.pointer("/info/state").and_then(|v| v.as_str()) != Some("running")
        {
            continue;
        }
        let Some(props) = object.pointer("/info/props").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(kind) = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .and_then(media_kind)
        else {
            continue;
        };
        let Some(client_id) = props.get("client.id").and_then(value_u32) else {
            continue;
        };
        let Some(process) = clients.get(&client_id) else {
            continue;
        };
        out.push(ActiveMediaUse {
            client_id,
            pid: process.pid,
            app_id: process.app_id.clone(),
            label: process.label.clone(),
            binary: process.binary.clone(),
            kind,
        });
    }
    out.sort_by(|a, b| (&a.label, a.kind as u8).cmp(&(&b.label, b.kind as u8)));
    out.dedup_by(|a, b| a.client_id == b.client_id && a.kind == b.kind);
    Ok(out)
}

fn number_at(value: &serde_json::Value, path: &[&str]) -> Option<u32> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    value_u32(cursor)
}

fn value_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

fn media_kind(class: &str) -> Option<MediaKind> {
    match class {
        "Stream/Input/Audio" => Some(MediaKind::Microphone),
        "Stream/Input/Video" | "Stream/Output/Video" => Some(MediaKind::Camera),
        _ => None,
    }
}

fn verify_process(
    proc_root: &Path,
    pid: u32,
    expected_uid: u32,
    security_props: &serde_json::Map<String, serde_json::Value>,
) -> Option<VerifiedProcess> {
    let root = proc_root.join(pid.to_string());
    let uid = read(&root.join("status"))?.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_ascii_whitespace()
            .next()?
            .parse::<u32>()
            .ok()
    })?;
    if uid != expected_uid {
        return None;
    }
    let binary = std::fs::read_link(root.join("exe")).ok()?;
    let leaf = binary.file_name()?.to_string_lossy();
    let portal_id = security_props
        .get("pipewire.access.portal.app_id")
        .and_then(|value| value.as_str())
        .filter(|value| valid_app_id(value));
    let is_flatpak = security_props
        .get("pipewire.sec.flatpak")
        .is_some_and(|value| value == true || value.as_str() == Some("true"));
    let (app_id, label) = if is_flatpak {
        let id = portal_id?;
        (
            format!("flatpak:{id}"),
            humanize_binary(id.rsplit('.').next().unwrap_or(id)),
        )
    } else if immutable_system_binary(&binary) {
        (
            format!("binary:{}", binary.display()),
            humanize_binary(&leaf),
        )
    } else {
        // A user can name their own executable `settings`; retaining a fixed
        // trust qualifier prevents it from rendering as the trusted app.
        (
            format!("local-process:{}", binary.display()),
            format!("Local process · {}", humanize_binary(&leaf)),
        )
    };
    Some(VerifiedProcess {
        pid,
        app_id,
        label,
        binary,
    })
}

fn valid_app_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.contains('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn immutable_system_binary(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    path.is_absolute()
        && std::fs::metadata(path).is_ok_and(|metadata| {
            metadata.is_file() && metadata.uid() == 0 && metadata.permissions().mode() & 0o022 == 0
        })
}

fn humanize_binary(binary: &str) -> String {
    let raw = binary
        .strip_prefix("tilo-")
        .unwrap_or(binary)
        .replace(['-', '_'], " ");
    raw.split_ascii_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn kill_state(root: &Path) -> KillState {
    if read(root.join("hard").as_path()).as_deref() == Some("1") {
        KillState::HardwareBlocked
    } else if read(root.join("soft").as_path()).as_deref() == Some("1")
        || read(root.join("state").as_path()).as_deref() == Some("0")
    {
        KillState::SoftwareBlocked
    } else {
        KillState::Allowed
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_use_is_attributed_only_from_server_side_security_evidence() {
        // Phase `hardware_camera_shutter_wins_over_software_state`.
        {
            let root = tempfile::tempdir().unwrap();
            let dev = tempfile::tempdir().unwrap();
            let camera = root.path().join("rfkill0");
            std::fs::create_dir(&camera).unwrap();
            std::fs::write(camera.join("type"), "camera\n").unwrap();
            std::fs::write(camera.join("soft"), "0\n").unwrap();
            std::fs::write(camera.join("hard"), "1\n").unwrap();

            let status = get_privacy_status_from(root.path(), dev.path());
            assert_eq!(status.camera_kill, KillState::HardwareBlocked);
        }
        // Phase `pipewire_attribution_uses_security_pid_and_kernel_executable`.
        {
            use std::os::unix::fs::symlink;
            let proc = tempfile::tempdir().unwrap();
            let process = proc.path().join("4242");
            std::fs::create_dir_all(&process).unwrap();
            std::fs::write(
                process.join("status"),
                "Name:\tbad\nUid:\t1000\t1000\t1000\t1000\n",
            )
            .unwrap();
            symlink("/usr/bin/tilo-video-chat", process.join("exe")).unwrap();
            let json = r#"[
              {"id":7,"type":"PipeWire:Interface:Client","info":{"props":{
                "pipewire.sec.pid":4242,"application.name":"Settings (spoofed)"}}},
              {"id":9,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
                "client.id":7,"media.class":"Stream/Input/Audio"}}}
            ]"#;
            let uses = parse_pipewire_media(json, proc.path(), 1000).unwrap();
            assert_eq!(uses.len(), 1);
            assert_eq!(uses[0].label, "Local process · Video Chat");
            assert_eq!(uses[0].kind, MediaKind::Microphone);
            assert!(!uses[0].label.contains("Settings"));
        }
        // Phase `client_claim_without_server_security_pid_is_not_attributed`.
        {
            let json = r#"[
              {"id":7,"type":"PipeWire:Interface:Client","info":{"props":{
                "application.process.id":1,"application.name":"Trusted App"}}},
              {"id":9,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
                "client.id":7,"media.class":"Stream/Input/Video"}}}
            ]"#;
            assert!(
                parse_pipewire_media(json, Path::new("/proc"), effective_uid())
                    .unwrap()
                    .is_empty()
            );
        }
        // Phase `portal_identity_requires_server_flatpak_security_marker`.
        {
            use std::os::unix::fs::symlink;
            let proc = tempfile::tempdir().unwrap();
            let process = proc.path().join("4242");
            std::fs::create_dir_all(&process).unwrap();
            std::fs::write(process.join("status"), "Uid:\t1000\t1000\t1000\t1000\n").unwrap();
            symlink("/app/bin/chat", process.join("exe")).unwrap();
            let json = r#"[
              {"id":7,"type":"PipeWire:Interface:Client","info":{"props":{
                "pipewire.sec.pid":4242,"pipewire.sec.flatpak":"true",
                "pipewire.access.portal.app_id":"org.example.Chat"}}},
              {"id":9,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
                "client.id":7,"media.class":"Stream/Input/Video"}}}
            ]"#;
            let uses = parse_pipewire_media(json, proc.path(), 1000).unwrap();
            assert_eq!(uses[0].app_id, "flatpak:org.example.Chat");
            assert_eq!(uses[0].label, "Chat");
        }
    }

    #[test]
    fn media_monitor_fragmented_graph_preserves_framing_and_quiet_exit() {
        let graph = serde_json::json!([
            {"id":1,"type":"PipeWire:Interface:Device", "info":{"props":{
                "device.name":"alsa_card", "description":"[\"escaped\\]".repeat(16000)}}},
            {"id":2,"type":"PipeWire:Interface:Client","info":{"props":{}}}
        ])
        .to_string();
        let mut decoder = MonitorJson::default();
        let mut documents = Vec::new();
        for byte in graph.bytes() {
            if let Some(doc) = decoder.push(byte).unwrap() {
                documents.push(doc);
            }
        }
        assert_eq!(
            decoder.total_scanned,
            graph.len(),
            "each byte is scanned once"
        );
        assert_eq!(documents, [graph.as_bytes()]);
        // This fixture proves tiny-fragment framing, not producer scheduling.
        // Queue its bounded graph before starting the production deadline;
        // the real reader still uses four bounded 8192-byte chunks.
        let (tx, rx) = std::sync::mpsc::channel();
        for chunk in graph.as_bytes().chunks(3) {
            tx.send(chunk.to_vec()).unwrap();
        }
        drop(tx);
        let mut published = 0;
        drive_media_monitor(&rx, tilo_test_env::hang_bound(), &mut |uses| {
            assert!(uses.is_empty());
            published += 1;
        })
        .expect("a complete authoritative device graph survives tiny fragments");
        assert_eq!(published, 1);
    }

    #[test]
    fn media_monitor_worker_rejects_oversized_and_truncated_graphs() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut oversized = vec![b' '; crate::process::MAX_CAPTURE_BYTES + 1];
        oversized[0] = b'[';
        tx.send(oversized).unwrap();
        drop(tx);
        let error = drive_media_monitor(&rx, tilo_test_env::hang_bound(), &mut |_| {
            panic!("incomplete graph published")
        })
        .unwrap_err();
        assert_eq!(error, "PipeWire monitor graph exceeds output limit");
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(b"[{\"info\": [".to_vec()).unwrap();
        drop(tx);
        assert_eq!(
            drive_media_monitor(&rx, tilo_test_env::hang_bound(), &mut |_| panic!(
                "truncated graph published"
            ))
            .unwrap_err(),
            "PipeWire monitor exited during a graph"
        );
    }

    #[test]
    fn media_monitor_worker_bounds_initial_graph_wait() {
        let (_tx, rx) = std::sync::mpsc::sync_channel(4);
        assert_eq!(
            drive_media_monitor(&rx, crate::DEFAULT_TIMEOUT, &mut |_| {
                panic!("absent graph published")
            })
            .unwrap_err(),
            "PipeWire monitor graph deadline exceeded"
        );
    }

    #[test]
    fn media_monitor_complete_graph_still_stops_when_stream_is_quiet() {
        let graph = serde_json::json!([
            {"type":"PipeWire:Interface:Device", "info":{"props":{"device.name":"alsa_card"}}},
            {"type":"PipeWire:Interface:Client", "info":{"props":{}}}
        ])
        .to_string()
        .into_bytes();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(graph).unwrap();
        let mut published = 0;
        drive_media_monitor(&rx, tilo_test_env::hang_bound(), &mut |_| published += 1)
            .expect("a complete stable graph stops without transport EOF");
        drop(tx);
        assert_eq!(published, 1);
    }

    #[test]
    fn media_monitor_fragmented_next_graph_survives_a_quiet_interval() {
        let graph = |name: &str| {
            serde_json::json!([
                {"type":"PipeWire:Interface:Device", "info":{"props":{"device.name":name}}},
                {"type":"PipeWire:Interface:Client", "info":{"props":{}}}
            ])
            .to_string()
            .into_bytes()
        };
        let initial = graph("alsa_card");
        let next = graph("bluez_card");
        let (tx, rx) = std::sync::mpsc::sync_channel(0);
        let sender = std::thread::spawn(move || {
            tx.send(initial).unwrap();
            // A rendezvous ensures the real worker received this fragment
            // before the quiet-sized gap begins.
            tx.send(next[..1].to_vec()).unwrap();
            std::thread::sleep(MONITOR_QUIET.saturating_mul(2));
            let _ = tx.send(next[1..].to_vec());
        });
        let mut published = 0;
        let result = drive_media_monitor(&rx, tilo_test_env::hang_bound(), &mut |_| published += 1);
        drop(rx);
        sender.join().unwrap();
        result.expect("an incomplete next graph waits for the graph deadline, not quiet-stop");
        assert_eq!(published, 2, "both complete graph snapshots were processed");
    }

    #[test]
    fn media_monitor_owned_child_is_reaped_after_bounded_reader_disconnect() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s' '[{\"type\":\"PipeWire:Interface:Device\",\"info\":{\"props\":{\"device.name\":\"alsa\"}}}][]'; while :; do printf '%8192s' ''; done"])
            .stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).process_group(0);
        let child = command.spawn().unwrap();
        let pid = child.id();
        run_media_monitor(child, &mut |_| {}).expect("stable graph stops owned monitor");
        assert!(
            !Path::new("/proc").join(pid.to_string()).exists(),
            "owned monitor was reaped"
        );
    }

    #[test]
    fn an_unchanged_device_graph_stops_the_pw_dump_monitor() {
        let device = |name: &str| {
            format!(
                r#"{{"id":1,"type":"PipeWire:Interface:Device","info":{{"props":{{"device.name":"{name}"}}}}}}"#
            )
        };
        let sink = |volume: &str| {
            format!(
                r#"{{"id":2,"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"Audio/Sink","node.name":"alsa_output.pci.analog-stereo","channelVolumes":[{volume}]}}}}}}"#
            )
        };
        let graph = format!("[{},{}]", device("alsa_card.pci"), sink("0.40"));
        let mut watch = DeviceGraphWatch::default();
        assert_eq!(
            watch.observe_document(&graph),
            MonitorDisposition::Keep,
            "the first device dump establishes the signature"
        );
        assert_eq!(
            watch.observe_quiet(),
            MonitorDisposition::Stop,
            "a quiet device graph must not keep pw-dump --monitor running"
        );

        let mut watch = DeviceGraphWatch::default();
        assert_eq!(watch.observe_document(&graph), MonitorDisposition::Keep);
        let dump_stream = r#"[{"id":99,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Stream/Output/Audio","node.name":"pw-dump","application.name":"pw-dump"}}},{"id":7,"type":"PipeWire:Interface:Client","info":{"props":{"application.name":"pw-dump"}}}]"#;
        assert_eq!(
            watch.observe_document(dump_stream),
            MonitorDisposition::Stop,
            "pw-dump is a stream, not a device; it must not keep the monitor up"
        );

        let mut watch = DeviceGraphWatch::default();
        assert_eq!(watch.observe_document(&graph), MonitorDisposition::Keep);
        let louder = format!("[{},{}]", device("alsa_card.pci"), sink("0.90"));
        assert_eq!(
            watch.observe_document(&louder),
            MonitorDisposition::Stop,
            "volume is not the device graph"
        );

        let mut watch = DeviceGraphWatch::default();
        assert_eq!(watch.observe_document(&graph), MonitorDisposition::Keep);
        let headset = format!(
            "[{},{},{}]",
            device("alsa_card.pci"),
            device("bluez_card.headset"),
            sink("0.40")
        );
        assert_eq!(
            watch.observe_document(&headset),
            MonitorDisposition::Keep,
            "a new device is a real graph change"
        );

        let mut decoder = MonitorJson::default();
        let mut documents = Vec::new();
        for byte in b"noise [1, {\"a\": [2]}] tail" {
            if let Some(doc) = decoder.push(*byte).unwrap() {
                documents.push(doc);
            }
        }
        assert_eq!(documents, [b"[1, {\"a\": [2]}]".to_vec()]);
        assert!(!decoder.incomplete());
    }
}
