//! Process-local registry of intentionally audible playback engines.
//!
//! It carries logical content identity and transport state only. Decoder
//! handles, local paths, signed URLs and `PipeWire` node ids never leave the
//! process, which lets the shell advertise handoff without leaking bearer
//! material or pretending a hardware node is portable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tilo_clock::Instant;

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
    LocalFile {
        path: PathBuf,
    },
    Workspace {
        path: PathBuf,
    },
    RemoteStream {
        url: String,
    },
    /// Stable app-owned resource identity. The destination asks the same app
    /// to resolve it locally instead of transferring an expiring bearer URL.
    AppResource {
        uri: String,
    },
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
            HandoffSource::AppResource { uri } => valid_app_resource_uri(uri),
        }
    }
}

/// Playback state for an app with its own resolver/transport rather than a
/// [`MediaEngine`](crate::engine::MediaEngine). The URI is a durable app-owned resource identity, not the
/// transient URL or device-local cache path currently feeding its decoder.
#[derive(Clone, Copy, Debug)]
pub struct AppPlayback<'a> {
    pub resource_uri: &'a str,
    pub title: &'a str,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub has_video: bool,
    pub audible: bool,
    pub controller_bus_name: Option<&'a str>,
}

/// Process-local registration owned by an app-native media transport.
/// Dropping it removes the session from the shell's private handoff feed.
#[derive(Debug)]
pub struct AppPlaybackHandle {
    id: u64,
}

impl AppPlaybackHandle {
    #[must_use]
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
        Self {
            // MediaEngine ids grow upward from one; app-owned ids grow down
            // from MAX so their process-local registry keys cannot collide.
            id: NEXT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Publish the latest state, or withdraw it when the source is no longer
    /// meaningfully audible/selected. Invalid descriptors fail closed.
    pub fn publish(&self, playback: AppPlayback<'_>) {
        let Ok(mut sessions) = registry().lock() else {
            return;
        };
        if !playback.audible
            || playback.title.len() > 512
            || playback.title.chars().any(char::is_control)
            || playback
                .duration_ms
                .is_some_and(|duration| playback.position_ms > duration)
            || !valid_app_resource_uri(playback.resource_uri)
        {
            sessions.remove(&self.id);
            return;
        }
        let content_id = format!(
            "blake3:{}",
            blake3::hash(playback.resource_uri.as_bytes()).to_hex()
        );
        sessions.insert(
            self.id,
            RegisteredPlayback {
                report: PlaybackReport {
                    session: PlaybackSession {
                        id: format!("media:{}", self.id),
                        app_id: "media".into(),
                        title: playback.title.to_string(),
                        content_id,
                        position_ms: playback.position_ms,
                        duration_ms: playback.duration_ms,
                        playing: playback.playing,
                        has_video: playback.has_video,
                    },
                    workspace_id: String::new(),
                    source: HandoffSource::AppResource {
                        uri: playback.resource_uri.to_string(),
                    },
                    controller_bus_name: playback.controller_bus_name.map(str::to_string),
                    accepted_handoff_request: accepted_handoff_request(),
                },
            },
        );
    }

    /// Withdraw this app-owned session without fabricating a placeholder
    /// resource. Used while a selected item is still resolving or disappears.
    pub fn clear(&self) {
        remove(self.id);
    }
}

impl Default for AppPlaybackHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AppPlaybackHandle {
    fn drop(&mut self) {
        remove(self.id);
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

#[derive(Default)]
struct SystemReports {
    replaced_at: Option<Instant>,
    reports: Vec<PlaybackReport>,
}

fn system_registry() -> &'static Mutex<SystemReports> {
    static REGISTRY: OnceLock<Mutex<SystemReports>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(SystemReports::default()))
}

/// Discovered player state is only a short-lived observation. If the shell's
/// MPRIS worker stalls or disappears, do not advertise a phantom player.
const SYSTEM_REPORT_TTL: Duration = Duration::from_secs(5);

fn system_reports_are_fresh(replaced_at: Option<Instant>, now: Instant) -> bool {
    replaced_at
        .is_some_and(|updated| system_report_age_is_fresh(now.saturating_duration_since(updated)))
}

fn system_report_age_is_fresh(age: Duration) -> bool {
    age <= SYSTEM_REPORT_TTL
}

fn append_discovered_reports(reports: &mut Vec<PlaybackReport>, discovered: &[PlaybackReport]) {
    let mut controller_names = reports
        .iter()
        .filter_map(|report| report.controller_bus_name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for report in discovered {
        if report
            .controller_bus_name
            .as_ref()
            .is_some_and(|name| !controller_names.insert(name.clone()))
        {
            continue;
        }
        reports.push(report.clone());
    }
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
    let workspace_id = std::env::var("TILO_WORKSPACE_ID").unwrap_or_default();
    let accepted_handoff_request = accepted_handoff_request();
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
    let mut reports = registry()
        .lock()
        .map(|sessions| {
            sessions
                .values()
                .map(|registered| registered.report.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Ok(external) = external_registry().lock() {
        reports.extend(external.values().flatten().cloned());
    }
    if let Ok(system) = system_registry().lock()
        && system_reports_are_fresh(system.replaced_at, Instant::now())
    {
        append_discovered_reports(&mut reports, &system.reports);
    }
    let mut sessions = reports
        .into_iter()
        .map(|report| report.session)
        .collect::<Vec<_>>();
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

/// Replace the shell-owned snapshot of standards-discovered players.
///
/// Unlike authenticated app reports, discovery has no broker route to trust.
/// Every item is therefore revalidated from its own installed-package route,
/// and observations expire if the discovery worker stops refreshing them.
pub fn replace_system_reports(reports: Vec<PlaybackReport>) {
    let reports = reports
        .into_iter()
        .filter(|report| {
            report.accepted_handoff_request.is_none()
                && report.validate_route(&report.session.app_id, &report.workspace_id)
        })
        .take(MAX_PLAYBACK_REPORTS)
        .collect();
    if let Ok(mut system) = system_registry().lock() {
        system.reports = reports;
        system.replaced_at = Some(Instant::now());
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
    if let Some(report) = external_registry().lock().ok().and_then(|external| {
        external
            .values()
            .flatten()
            .find(|report| report.session.id == session_id)
            .cloned()
    }) {
        return Some(report);
    }
    system_registry().lock().ok().and_then(|system| {
        system_reports_are_fresh(system.replaced_at, Instant::now()).then_some(())?;
        system
            .reports
            .iter()
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

/// Forget one exact outbound request after a typed rejection or local cancel.
/// A mismatched session cannot consume another pending handoff.
pub fn cancel_outbound_handoff(request_id: [u8; 16], session_id: &str) -> Option<PlaybackReport> {
    accept_outbound_handoff(request_id, session_id)
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

fn valid_app_resource_uri(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.len() <= 64
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && matches!(byte, b'0'..=b'9' | b'+' | b'-' | b'.'))
        })
        && !rest.is_empty()
        && uri.len() <= 8192
        && !uri.chars().any(char::is_control)
}

fn accepted_handoff_request() -> Option<[u8; 16]> {
    std::env::var("TILO_MEDIA_HANDOFF_REQUEST_ID")
        .ok()
        .and_then(|value| hex::decode(value).ok())
        .and_then(|bytes| bytes.try_into().ok())
        .filter(|request| request != &[0; 16])
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

    fn report(id: &str, bus_name: &str) -> PlaybackReport {
        PlaybackReport {
            session: PlaybackSession {
                id: format!("so.tilo.player:{id}"),
                app_id: "so.tilo.player".into(),
                title: id.into(),
                content_id: format!("blake3:{}", "0".repeat(64)),
                position_ms: 0,
                duration_ms: None,
                playing: true,
                has_video: false,
            },
            workspace_id: String::new(),
            source: HandoffSource::RemoteStream {
                url: "https://example.test/media".into(),
            },
            controller_bus_name: Some(bus_name.into()),
            accepted_handoff_request: None,
        }
    }

    #[test]
    fn handoff_reports_expose_identity_not_uris_and_expire_when_stale() {
        // Phase `public_session_never_exposes_source_uri`.
        {
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
        // Phase `app_resource_reports_stable_identity_without_decoder_url`.
        {
            let handle = AppPlaybackHandle::new();
            handle.publish(AppPlayback {
                resource_uri: "music://track/track-42",
                title: "A durable song",
                position_ms: 4_200,
                duration_ms: Some(9_000),
                playing: true,
                has_video: false,
                audible: true,
                controller_bus_name: Some("org.mpris.MediaPlayer2.tilo-music"),
            });
            let report = playback_reports("so.tilo.music", "workspace")
                .into_iter()
                .find(|report| report.session.title == "A durable song")
                .expect("app-owned playback is reported");
            assert_eq!(report.session.app_id, "so.tilo.music");
            assert_eq!(report.workspace_id, "workspace");
            assert!(matches!(
                report.source,
                HandoffSource::AppResource { ref uri } if uri == "music://track/track-42"
            ));
            assert!(!format!("{report:?}").contains("googlevideo"));
        }
        // Phase `malformed_app_resources_are_withdrawn`.
        {
            let handle = AppPlaybackHandle::new();
            handle.publish(AppPlayback {
                resource_uri: "1music://track/foreign",
                title: "Foreign",
                position_ms: 0,
                duration_ms: None,
                playing: true,
                has_video: false,
                audible: true,
                controller_bus_name: None,
            });
            assert!(
                playback_sessions()
                    .iter()
                    .all(|session| session.title != "Foreign")
            );
        }
        // Phase `explicit_reports_win_over_discovery_for_the_same_player`.
        {
            let explicit = report("explicit", "org.mpris.MediaPlayer2.player");
            let discovered = report("discovered", "org.mpris.MediaPlayer2.player");
            let other = report("other", "org.mpris.MediaPlayer2.other");
            let mut reports = vec![explicit.clone()];

            append_discovered_reports(&mut reports, &[discovered, other.clone()]);

            assert_eq!(reports, vec![explicit, other]);
        }
        // Phase `discovered_reports_expire_when_refresh_stalls`.
        {
            let now = Instant::now();
            assert!(system_reports_are_fresh(Some(now), now));
            assert!(!system_report_age_is_fresh(
                SYSTEM_REPORT_TTL.saturating_add(Duration::from_millis(1))
            ));
            assert!(!system_reports_are_fresh(None, now));
        }
    }
}
