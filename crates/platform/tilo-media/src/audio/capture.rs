//! Who is using the camera or the microphone right now, attributed from
//! `PipeWire`'s own graph.
//!
//! The application behind a capture stream is not what the application says about
//! itself: the server attaches the client's security PID, and the executable behind
//! that PID in `/proc` names the app, so an app cannot rename itself into another
//! one in the indicator. The `pw-dump` the graph is read from carries the clients and
//! their PIDs, so no second process is needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Microphone,
    Camera,
}

/// One actively-running `PipeWire` capture stream. `label` is derived from
/// the executable behind `PipeWire`'s server-assigned security PID, never
/// from the client-supplied `application.name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaUse {
    pub client_id: u32,
    pub pid: u32,
    pub app_id: String,
    pub label: String,
    pub binary: PathBuf,
    pub kind: MediaKind,
}

/// The capture streams of the live graph.
///
/// # Errors
///
/// As [`super::dump`], or when what it printed is not a `PipeWire` graph.
#[cfg(not(target_arch = "wasm32"))]
pub fn media_uses() -> Result<Vec<MediaUse>, String> {
    of_dump(&super::dump()?)
}

/// The capture streams in a `pw-dump` graph, each attributed through this
/// machine's `/proc`.
///
/// # Errors
///
/// When the dump is not a `PipeWire` graph.
pub fn of_dump(json: &str) -> Result<Vec<MediaUse>, String> {
    from_dump(json, Path::new("/proc"), effective_uid())
}

/// The capture streams in a `pw-dump` graph, each attributed through
/// `proc_root`.
///
/// # Errors
///
/// When the dump is not a `PipeWire` graph.
pub fn from_dump(json: &str, proc_root: &Path, expected_uid: u32) -> Result<Vec<MediaUse>, String> {
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
        out.push(MediaUse {
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

#[cfg(unix)]
fn immutable_system_binary(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    path.is_absolute()
        && std::fs::metadata(path).is_ok_and(|metadata| {
            metadata.is_file() && metadata.uid() == 0 && metadata.permissions().mode() & 0o022 == 0
        })
}

#[cfg(not(unix))]
fn immutable_system_binary(_path: &Path) -> bool {
    false
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
            let uses = from_dump(json, proc.path(), 1000).unwrap();
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
                from_dump(json, Path::new("/proc"), effective_uid())
                    .unwrap()
                    .is_empty(),
                "a client-named pid attributes nothing"
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
            let uses = from_dump(json, proc.path(), 1000).unwrap();
            assert_eq!(uses[0].app_id, "flatpak:org.example.Chat");
            assert_eq!(uses[0].label, "Chat");
        }
    }

    #[test]
    fn only_a_running_capture_stream_of_this_uid_counts() {
        use std::os::unix::fs::symlink;
        let proc = tempfile::tempdir().unwrap();
        for (pid, uid) in [(4242, 1000), (4243, 1001)] {
            let process = proc.path().join(pid.to_string());
            std::fs::create_dir_all(&process).unwrap();
            std::fs::write(
                process.join("status"),
                format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
            )
            .unwrap();
            symlink("/app/bin/chat", process.join("exe")).unwrap();
        }
        let json = r#"[
          {"id":7,"type":"PipeWire:Interface:Client","info":{"props":{"pipewire.sec.pid":4242}}},
          {"id":8,"type":"PipeWire:Interface:Client","info":{"props":{"pipewire.sec.pid":4243}}},
          {"id":9,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
            "client.id":7,"media.class":"Stream/Input/Video"}}},
          {"id":10,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
            "client.id":7,"media.class":"Stream/Output/Audio"}}},
          {"id":11,"type":"PipeWire:Interface:Node","info":{"state":"suspended","props":{
            "client.id":7,"media.class":"Stream/Input/Audio"}}},
          {"id":12,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{
            "client.id":8,"media.class":"Stream/Input/Audio"}}}
        ]"#;
        let uses = from_dump(json, proc.path(), 1000).unwrap();
        assert_eq!(
            uses,
            vec![MediaUse {
                client_id: 7,
                pid: 4242,
                app_id: "local-process:/app/bin/chat".into(),
                label: "Local process · Chat".into(),
                binary: PathBuf::from("/app/bin/chat"),
                kind: MediaKind::Camera,
            }],
            "one running camera stream: not a suspended one, not a playback one, not another user's"
        );
    }
}
