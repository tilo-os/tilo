//! Kernel and `PipeWire` privacy state for shell indicators.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
pub fn active_media_uses() -> Result<Vec<ActiveMediaUse>, String> {
    let json = crate::process::run_capture("pw-dump", &[])?;
    parse_pipewire_media(&json, Path::new("/proc"), effective_uid())
}

/// Immediately disconnect an offending `PipeWire` client. This is deliberately
/// an ephemeral stop, not a pretend persistent permission: portal-managed
/// grants remain owned by the portal permission store.
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
}
