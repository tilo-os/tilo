//! Process-local registry of intentionally audible playback engines.
//!
//! It carries logical content identity and transport state only. Decoder
//! handles, local paths, signed URLs and `PipeWire` node ids never leave the
//! process, which lets the shell advertise handoff without leaking bearer
//! material or pretending a hardware node is portable.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::Source;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackSession {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub content_id: String,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub has_video: bool,
}

fn registry() -> &'static Mutex<BTreeMap<u64, PlaybackSession>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<u64, PlaybackSession>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(crate) fn update(
    engine_id: u64,
    source: Option<&Source>,
    audible: bool,
    position_ms: u64,
    duration_ms: Option<u64>,
    playing: bool,
) {
    let Ok(mut sessions) = registry().lock() else {
        return;
    };
    let Some(source) = source.filter(|_| audible) else {
        sessions.remove(&engine_id);
        return;
    };
    let app_id = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "media".into());
    let (title, identity_material) = source_identity(source);
    let content_id = format!(
        "blake3:{}",
        blake3::hash(identity_material.as_bytes()).to_hex()
    );
    sessions.insert(
        engine_id,
        PlaybackSession {
            id: format!("{app_id}:{engine_id}"),
            app_id,
            title,
            content_id,
            position_ms,
            duration_ms,
            playing,
            has_video: true,
        },
    );
}

pub(crate) fn remove(engine_id: u64) {
    if let Ok(mut sessions) = registry().lock() {
        sessions.remove(&engine_id);
    }
}

/// Snapshot every audible engine in this process for the OS fabric bridge.
pub fn playback_sessions() -> Vec<PlaybackSession> {
    registry()
        .lock()
        .map(|sessions| sessions.values().cloned().collect())
        .unwrap_or_default()
}

fn source_identity(source: &Source) -> (String, String) {
    match source {
        Source::Workspace(path) => (
            path.file_name().map_or_else(
                || "Workspace media".into(),
                |name| name.to_string_lossy().into_owned(),
            ),
            format!("workspace:{}", path.display()),
        ),
        Source::LocalFile(path) => (
            path.file_name().map_or_else(
                || "Local media".into(),
                |name| name.to_string_lossy().into_owned(),
            ),
            format!("local:{}", path.display()),
        ),
        Source::Remote { url } => ("Streaming media".into(), format!("remote:{url}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_session_never_exposes_source_uri() {
        let source = Source::Remote {
            url: "https://example.test/video?token=secret".into(),
        };
        update(9001, Some(&source), true, 42, Some(100), true);
        let session = playback_sessions()
            .into_iter()
            .find(|session| session.id.ends_with(":9001"))
            .unwrap();
        assert!(!session.content_id.contains("secret"));
        assert_eq!(session.title, "Streaming media");
        remove(9001);
    }
}
