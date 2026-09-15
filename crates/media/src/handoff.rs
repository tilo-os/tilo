//! Process-local registry of intentionally audible playback engines.
//!
//! It carries logical content identity and transport state only. Decoder
//! handles, local paths, signed URLs and `PipeWire` node ids never leave the
//! process, which lets the shell advertise handoff without leaking bearer
//! material or pretending a hardware node is portable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::Source;

pub const MAX_PLAYBACK_REPORTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffSource {
    LocalFile { path: PathBuf },
    Workspace { path: PathBuf },
    RemoteStream { url: String },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlaybackReport {
    pub session: PlaybackSession,
    pub workspace_id: String,
    pub source: HandoffSource,
    pub controller_bus_name: Option<String>,
    pub accepted_handoff_request: Option<[u8; 16]>,
}

impl PlaybackReport {
    /// Check the private report against the app/workspace identity certified
    /// by the local broker connection.
    pub fn validate_route(&self, app_id: &str, workspace_id: &str) -> bool {
        if self.session.app_id != app_id
            || self.workspace_id != workspace_id
            || self.session.id.is_empty()
            || self.session.id.len() > 512
            || !self.session.id.starts_with(app_id)
            || self.session.title.len() > 512
            || self.session.title.chars().any(char::is_control)
            || self.session.content_id.len() > 256
            || !self.session.content_id.starts_with("blake3:")
            || self.accepted_handoff_request == Some([0; 16])
            || self
                .session
                .duration_ms
                .is_some_and(|duration| self.session.position_ms > duration)
        {
            return false;
        }
        if self.controller_bus_name.as_ref().is_some_and(|name| {
            name.len() > 256
                || !name.starts_with("org.mpris.MediaPlayer2.")
                || name.chars().any(char::is_control)
        }) {
            return false;
        }
        match &self.source {
            HandoffSource::LocalFile { path } => {
                path.is_absolute()
                    && path.as_os_str().len() <= 4096
                    && !path.to_string_lossy().chars().any(char::is_control)
            }
            HandoffSource::Workspace { path } => {
                !path.as_os_str().is_empty()
                    && path.as_os_str().len() <= 4096
                    && path.is_relative()
                    && path.components().all(|component| {
                        matches!(
                            component,
                            std::path::Component::Normal(_) | std::path::Component::CurDir
                        )
                    })
            }
            HandoffSource::RemoteStream { url } => valid_remote_url(url),
        }
    }
}

#[derive(Clone, Debug)]
struct RegisteredPlayback {
    report: PlaybackReport,
}

#[derive(Clone, Copy)]
pub(crate) struct EnginePlayback<'a> {
    pub source: Option<&'a Source>,
    pub audible: bool,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub has_video: bool,
    pub controller_bus_name: Option<&'a str>,
}

fn registry() -> &'static Mutex<BTreeMap<u64, RegisteredPlayback>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<u64, RegisteredPlayback>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn external_registry() -> &'static Mutex<BTreeMap<String, Vec<PlaybackReport>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Vec<PlaybackReport>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn outbound_handoffs() -> &'static Mutex<BTreeMap<[u8; 16], PlaybackReport>> {
    static HANDOFFS: OnceLock<Mutex<BTreeMap<[u8; 16], PlaybackReport>>> = OnceLock::new();
    HANDOFFS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(crate) fn update(engine_id: u64, playback: EnginePlayback<'_>) {
    let Ok(mut sessions) = registry().lock() else {
        return;
    };
    let Some(source) = playback.source.filter(|_| playback.audible) else {
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
    let workspace_id = std::env::var("QOS_WORKSPACE_ID").unwrap_or_default();
    let accepted_handoff_request = std::env::var("QOS_MEDIA_HANDOFF_REQUEST_ID")
        .ok()
        .and_then(|value| hex::decode(value).ok())
        .and_then(|bytes| bytes.try_into().ok())
        .filter(|request| request != &[0; 16]);
    sessions.insert(
        engine_id,
        RegisteredPlayback {
            report: PlaybackReport {
                session: PlaybackSession {
                    id: format!("{app_id}:{engine_id}"),
                    app_id,
                    title,
                    content_id,
                    position_ms: playback.position_ms,
                    duration_ms: playback.duration_ms,
                    playing: playback.playing,
                    has_video: playback.has_video,
                },
                workspace_id,
                source: handoff_source(source),
                controller_bus_name: playback.controller_bus_name.map(str::to_string),
                accepted_handoff_request,
            },
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
    let mut sessions = registry()
        .lock()
        .map(|sessions| {
            sessions
                .values()
                .map(|registered| registered.report.session.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Ok(external) = external_registry().lock() {
        sessions.extend(
            external
                .values()
                .flatten()
                .map(|report| report.session.clone()),
        );
    }
    sessions.truncate(MAX_PLAYBACK_REPORTS);
    sessions
}

/// Private local-process reports for the authenticated app-to-shell broker.
/// The caller supplies the broker-certified app/workspace route, replacing
/// executable-name and environment hints before serialization.
pub fn playback_reports(app_id: &str, workspace_id: &str) -> Vec<PlaybackReport> {
    registry()
        .lock()
        .map(|sessions| {
            sessions
                .values()
                .take(MAX_PLAYBACK_REPORTS)
                .map(|registered| {
                    let mut report = registered.report.clone();
                    report.session.app_id = app_id.to_string();
                    report.session.id = format!(
                        "{}:{}",
                        app_id,
                        report.session.id.rsplit(':').next().unwrap_or("media")
                    );
                    report.workspace_id = workspace_id.to_string();
                    report
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Replace all reports owned by one authenticated app connection. The shell
/// calls this only after binding app id, workspace and instance to the broker
/// route; stale sessions disappear level-triggeredly.
pub fn replace_external_reports(owner: String, reports: Vec<PlaybackReport>) {
    if let Ok(mut external) = external_registry().lock() {
        if reports.is_empty() {
            external.remove(&owner);
        } else {
            external.insert(
                owner,
                reports.into_iter().take(MAX_PLAYBACK_REPORTS).collect(),
            );
        }
    }
}

pub fn remove_external_reports(owner: &str) {
    if let Ok(mut external) = external_registry().lock() {
        external.remove(owner);
    }
}

/// Resolve private source/controller information for a selected logical
/// session without ever copying it into the convergent device snapshot.
pub fn playback_report(session_id: &str) -> Option<PlaybackReport> {
    if let Ok(local) = registry().lock()
        && let Some(report) = local
            .values()
            .map(|registered| &registered.report)
            .find(|report| report.session.id == session_id)
    {
        return Some(report.clone());
    }
    external_registry().lock().ok().and_then(|external| {
        external
            .values()
            .flatten()
            .find(|report| report.session.id == session_id)
            .cloned()
    })
}

/// Accepted handoff request ids observed from actually-opened target engines.
pub fn accepted_handoff_requests() -> Vec<[u8; 16]> {
    let mut requests = Vec::new();
    if let Ok(local) = registry().lock() {
        requests.extend(
            local
                .values()
                .filter_map(|registered| registered.report.accepted_handoff_request),
        );
    }
    if let Ok(external) = external_registry().lock() {
        requests.extend(
            external
                .values()
                .flatten()
                .filter_map(|report| report.accepted_handoff_request),
        );
    }
    requests.sort_unstable();
    requests.dedup();
    requests
}

pub fn remember_outbound_handoff(request_id: [u8; 16], report: PlaybackReport) {
    if let Ok(mut pending) = outbound_handoffs().lock() {
        if pending.len() >= MAX_PLAYBACK_REPORTS
            && let Some(oldest) = pending.keys().next().copied()
        {
            pending.remove(&oldest);
        }
        pending.insert(request_id, report);
    }
}

pub fn accept_outbound_handoff(request_id: [u8; 16], session_id: &str) -> Option<PlaybackReport> {
    let mut pending = outbound_handoffs().lock().ok()?;
    if pending
        .get(&request_id)
        .is_none_or(|report| report.session.id != session_id)
    {
        return None;
    }
    pending.remove(&request_id)
}

fn handoff_source(source: &Source) -> HandoffSource {
    match source {
        Source::LocalFile(path) => HandoffSource::LocalFile { path: path.clone() },
        Source::Workspace(path) => {
            let relative = if path.is_absolute() {
                path.strip_prefix(Source::workspace_root())
                    .map_or_else(|_| path.clone(), PathBuf::from)
            } else {
                path.clone()
            };
            HandoffSource::Workspace { path: relative }
        }
        Source::Remote { url } => HandoffSource::RemoteStream { url: url.clone() },
    }
}

fn valid_remote_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
        && !authority.contains('@')
        && url.len() <= 8192
        && !url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
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
        update(
            9001,
            EnginePlayback {
                source: Some(&source),
                audible: true,
                position_ms: 42,
                duration_ms: Some(100),
                playing: true,
                has_video: true,
                controller_bus_name: None,
            },
        );
        let session = playback_sessions()
            .into_iter()
            .find(|session| session.id.ends_with(":9001"))
            .unwrap();
        assert!(!session.content_id.contains("secret"));
        assert_eq!(session.title, "Streaming media");
        remove(9001);
    }
}
