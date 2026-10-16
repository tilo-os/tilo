//! Driving whatever is playing, over MPRIS.
//!
//! The transport keys (play/pause, next, previous) address *an
//! application*, not the machine, so they go over `org.mpris.MediaPlayer2`
//! on the session bus — the interface every real player and Tilo's own
//! Music app (`apps/music/src/mpris.rs`) implements.
//!
//! Called from the compositor's input path, but a D-Bus round trip has no
//! natural bound (tens of seconds). So every bus conversation runs as an
//! async task on the session runtime (`crate::session_runtime`), over one
//! shared session-bus connection, and nothing here waits for it: results
//! reach the compositor loop through the caller's delivery (a transport
//! key's OSD) or shared state it already samples (handoff reports and
//! accepted resumes).
//!
//! Which players exist, and what each reports, comes from one signal-driven
//! observer ([`observer`]) and the table it keeps ([`table`]); nothing here
//! polls the bus. Commands still go to a player directly, and a resume still
//! reads a player's `Position` while it verifies the playhead, because MPRIS
//! has no signal for a playhead that merely advances.

mod observer;
mod table;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use media::handoff::{HandoffSource, PlaybackReport, PlaybackSession};
use zbus::zvariant::OwnedValue;

use table::PlayerTable;

/// A transport command's outcome older than this is not shown: an OSD for a
/// key pressed long ago would describe the wrong moment.
const DEADLINE: Duration = Duration::from_millis(250);

/// The MPRIS well-known name prefix. Every player owns one.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";

/// How long a command or resume waits for a just-started observer to have
/// listed the bus before it gives up.
const SEED_WAIT: Duration = Duration::from_secs(2);

/// How long after a now-playing command the card keeps its optimistic state
/// before it shows the table again: a player that took the command has
/// signalled by then, and one that refused it is shown as it is.
const COMMAND_SETTLE: Duration = Duration::from_millis(500);

const RESUME_DEADLINE: Duration = Duration::from_secs(10);
const RESUME_POLL: Duration = Duration::from_millis(100);
const RESUME_TOLERANCE_US: i64 = 500_000;
const MAX_PENDING_RESUMES: usize = 16;
const MAX_CANCELLED_RESUMES: usize = MAX_PENDING_RESUMES * 4;
const CANCELLED_RESUME_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct ResumeState {
    pending: BTreeMap<[u8; 16], Arc<ResumeGate>>,
    accepted: VecDeque<AcceptedResume>,
    cancelled: BTreeMap<[u8; 16], Instant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AcceptedResume {
    pub(crate) request_id: [u8; 16],
    pub(crate) controller_bus_name: Option<String>,
    pub(crate) expected_uri: Option<String>,
}

static RESUMES: LazyLock<Mutex<ResumeState>> = LazyLock::new(|| Mutex::new(ResumeState::default()));

/// Whether one resume may still act on a player, and the lock its player
/// mutations hold so that revocation is ordered after any in flight.
struct ResumeGate {
    active: AtomicBool,
    mutation: tokio::sync::Mutex<()>,
}

impl ResumeGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicBool::new(true),
            mutation: tokio::sync::Mutex::new(()),
        })
    }
}

#[derive(Clone)]
struct ResumeAuthority {
    request_id: [u8; 16],
    gate: Arc<ResumeGate>,
}

impl ResumeAuthority {
    fn is_active(&self) -> bool {
        self.gate.active.load(AtomicOrdering::Acquire)
    }

    /// Run `operation` (one player mutation) only while still authorized.
    async fn mutate<F: Future>(&self, operation: F) -> Option<F::Output> {
        let _mutation = self.gate.mutation.lock().await;
        if !self.is_active() {
            return None;
        }
        Some(operation.await)
    }

    /// Stop every later mutation, after waiting out one already issued, so
    /// that once this returns no mutation for the resume is in flight.
    /// Called from synchronous code only, never on an async worker.
    fn revoke(&self) {
        self.gate.active.store(false, AtomicOrdering::Release);
        drop(self.gate.mutation.blocking_lock());
    }
}

impl ResumeState {
    fn prune_cancelled(&mut self, now: Instant) {
        self.cancelled.retain(|_, cancelled| {
            now.checked_duration_since(*cancelled)
                .is_none_or(|elapsed| elapsed < CANCELLED_RESUME_TTL)
        });
        while self.cancelled.len() > MAX_CANCELLED_RESUMES {
            let Some(oldest) = self
                .cancelled
                .iter()
                .min_by_key(|(_, cancelled)| *cancelled)
                .map(|(request_id, _)| *request_id)
            else {
                break;
            };
            self.cancelled.remove(&oldest);
        }
    }

    fn begin(&mut self, request_id: [u8; 16], now: Instant) -> Option<ResumeAuthority> {
        self.prune_cancelled(now);
        if self.pending.len() >= MAX_PENDING_RESUMES
            || self.pending.contains_key(&request_id)
            || self.cancelled.contains_key(&request_id)
        {
            return None;
        }
        let gate = ResumeGate::new();
        self.pending.insert(request_id, Arc::clone(&gate));
        Some(ResumeAuthority { request_id, gate })
    }

    fn cancel(&mut self, request_id: [u8; 16], now: Instant) -> bool {
        self.prune_cancelled(now);
        let pending = self.pending.remove(&request_id);
        if let Some(gate) = pending.as_ref() {
            ResumeAuthority {
                request_id,
                gate: Arc::clone(gate),
            }
            .revoke();
        }
        let accepted = self
            .accepted
            .iter()
            .any(|accepted| accepted.request_id == request_id);
        self.accepted
            .retain(|accepted| accepted.request_id != request_id);
        self.cancelled.insert(request_id, now);
        self.prune_cancelled(now);
        pending.is_some() || accepted
    }

    #[cfg(test)]
    fn finish(&mut self, authority: &ResumeAuthority, accepted: bool) {
        self.finish_with_player(authority, accepted.then_some(None));
    }

    /// `accepted_player` is `None` when the launch was declined, otherwise
    /// the accepted launch and the player it resolved to, if any.
    #[expect(
        clippy::option_option,
        reason = "declined, accepted without a player, and accepted with a player are all distinct"
    )]
    fn finish_with_player(
        &mut self,
        authority: &ResumeAuthority,
        accepted_player: Option<Option<(String, String)>>,
    ) {
        let still_authorized = self
            .pending
            .get(&authority.request_id)
            .is_some_and(|gate| Arc::ptr_eq(gate, &authority.gate))
            && authority.is_active();
        self.pending.remove(&authority.request_id);
        if accepted_player.is_some()
            && still_authorized
            && !self.cancelled.contains_key(&authority.request_id)
            && !self
                .accepted
                .iter()
                .any(|accepted| accepted.request_id == authority.request_id)
        {
            if self.accepted.len() >= MAX_PENDING_RESUMES {
                self.accepted.pop_front();
            }
            self.accepted.push_back(AcceptedResume {
                request_id: authority.request_id,
                controller_bus_name: accepted_player
                    .as_ref()
                    .and_then(|player| player.as_ref().map(|(bus_name, _)| bus_name.clone())),
                expected_uri: accepted_player
                    .flatten()
                    .map(|(_, expected_uri)| expected_uri),
            });
        }
    }

    fn take_accepted_resumes(&mut self) -> Vec<AcceptedResume> {
        self.accepted.drain(..).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeSource {
    Uri(String),
    LocalFile(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeRequest {
    pub request_id: [u8; 16],
    pub package_id: String,
    pub source: ResumeSource,
    pub position_ms: u64,
    pub playing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstalledMediaApp {
    package_id: String,
    uri_schemes: Vec<String>,
    extensions: Vec<String>,
}

#[derive(Debug)]
struct PlayerObservation {
    bus_name: String,
    package_id: String,
    identity: Option<String>,
    status: String,
    metadata: HashMap<String, OwnedValue>,
    position_us: i64,
}

/// Publish this machine's players as handoff reports, while media
/// continuity is permitted. Starts the player observer if it is not running
/// and re-stamps the reports from its table, playheads extrapolated to now.
/// No D-Bus round trip, on this thread or any other: called about once a
/// second, this is what keeps the reports inside `media`'s freshness window
/// while the observer is healthy. They expire there if it stops.
pub fn refresh_handoff_reports() {
    observer::ensure_started();
    observer::set_handoff(true);
}

/// Stop publishing this machine's players as handoff reports: media
/// continuity is not permitted.
pub fn withdraw_handoff_reports() {
    observer::set_handoff(false);
}

/// Read the player table once the observer has listed the bus, starting the
/// observer if it is not running. `None` when there is no bus, or the bus
/// was not listed within [`SEED_WAIT`].
async fn with_players<T, F>(read: F) -> Option<T>
where
    F: FnOnce(&PlayerTable) -> T + Send,
{
    observer::ensure_started();
    if !observer::seeded(&observer::OBSERVER, SEED_WAIT).await {
        return None;
    }
    observer::OBSERVER
        .lock()
        .ok()
        .map(|state| read(&state.table))
}

/// Run `task` on the session runtime. `false` when that runtime is
/// unavailable, so the caller can report the request as not made.
fn spawn_on_session<F>(task: F) -> bool
where
    F: Future<Output = ()> + Send + 'static,
{
    match crate::session_runtime::runtime() {
        Ok(runtime) => {
            drop(runtime.spawn(task));
            true
        }
        Err(error) => {
            tracing::warn!(target: "tilo::mpris", %error, "no session runtime for MPRIS");
            false
        }
    }
}

/// The session bus, connected once on the session runtime and shared by
/// every MPRIS conversation. `None` when there is no session bus; the next
/// call tries again.
///
/// This crate's unit tests have no bus at all: a media command or pause sent
/// by one of them must never reach a player of whoever runs the tests. The
/// integration tests hand the process a scratch bus address instead
/// (`tests/dbus_runtime_context.rs`).
async fn session_bus() -> Option<zbus::Connection> {
    static BUS: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();
    if cfg!(test) {
        return None;
    }
    BUS.get_or_try_init(zbus::Connection::session)
        .await
        .map_err(|error| {
            tracing::debug!(target: "tilo::mpris", %error, "no session bus");
        })
        .ok()
        .cloned()
}

/// A proxy for one player object. Properties are read fresh on every call:
/// a player does not signal `Position` changes, so a cached value would go
/// stale while a resume is being verified.
async fn player_proxy(
    connection: &zbus::Connection,
    bus_name: &str,
    interface: &'static str,
) -> Option<zbus::Proxy<'static>> {
    zbus::proxy::Builder::<zbus::Proxy<'static>>::new(connection)
        .destination(bus_name.to_owned())
        .ok()?
        .path(MPRIS_PATH)
        .ok()?
        .interface(interface)
        .ok()?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()
}

/// Ask a newly launched, launcher-verified player to assume an exact media
/// state. Completion is reported only after the player itself reflects the
/// requested source, playhead and transport status.
pub fn resume_after_launch(request: ResumeRequest) -> bool {
    if request.request_id == [0; 16]
        || request.package_id.is_empty()
        || request.package_id.len() > 256
        || request.package_id.chars().any(char::is_control)
        || request.position_ms > i64::MAX as u64 / 1_000
        || resume_uri(&request.source).is_none()
    {
        return false;
    }
    let Ok(mut resumes) = RESUMES.lock() else {
        return false;
    };
    let Some(authority) = resumes.begin(request.request_id, Instant::now()) else {
        return false;
    };
    let request_id = request.request_id;
    // The lock is released before spawning: the task takes it to finish.
    drop(resumes);
    let spawned = spawn_on_session(async move {
        let accepted = run_resume(&request, &authority).await;
        if let Ok(mut resumes) = RESUMES.lock() {
            resumes.finish_with_player(&authority, accepted.map(Some));
        }
    });
    if !spawned {
        cancel_resume_after_launch(request_id);
    }
    spawned
}

/// Revoke one exact destination resume request.
///
/// The tombstone is deliberately short-lived and bounded: it closes the race
/// with a worker that has not started yet without turning request ids into
/// durable history. A cancelled worker cannot issue another MPRIS mutation or
/// publish a late acceptance.
pub(crate) fn cancel_resume_after_launch(request_id: [u8; 16]) -> bool {
    if request_id == [0; 16] {
        return false;
    }
    RESUMES
        .lock()
        .is_ok_and(|mut resumes| resumes.cancel(request_id, Instant::now()))
}

pub(crate) fn take_accepted_handoff_resumes() -> Vec<AcceptedResume> {
    RESUMES.lock().map_or_else(
        |_| Vec::new(),
        |mut resumes| resumes.take_accepted_resumes(),
    )
}

async fn run_resume(
    request: &ResumeRequest,
    authority: &ResumeAuthority,
) -> Option<(String, String)> {
    let expected_uri = resume_uri(&request.source)?;
    let connection = session_bus().await?;
    let started = Instant::now();
    let mut opened = BTreeSet::new();
    while started.elapsed() < RESUME_DEADLINE {
        if !authority.is_active() {
            return None;
        }
        for (bus_name, reported_uri) in verified_players(&request.package_id).await {
            if !authority.is_active() {
                return None;
            }
            if reported_uri.as_deref() == Some(expected_uri.as_str()) {
                let Some(player) = player_proxy(&connection, &bus_name, PLAYER_IFACE).await else {
                    continue;
                };
                if drive_and_verify_player(&player, request.position_ms, request.playing, authority)
                    .await
                {
                    return (authority.is_active() && started.elapsed() <= RESUME_DEADLINE)
                        .then(|| (bus_name.clone(), expected_uri.clone()));
                }
            } else if opened.insert(bus_name.clone()) {
                let _ =
                    open_uri_if_supported(&connection, &bus_name, &expected_uri, authority).await;
            }
        }
        if authority.is_active() {
            // The player has no signal for "now at this source and
            // position"; it is asked again until the deadline.
            tokio::time::sleep(RESUME_POLL).await;
        }
    }
    None
}

/// The players verified to belong to `package_id`, each with the source it
/// reports. A player whose owner could not yet be attributed to a launch
/// when it appeared is tried again first: no bus call.
async fn verified_players(package_id: &str) -> Vec<(String, Option<String>)> {
    observer::retry_verification(&observer::OBSERVER, None);
    with_players(|table| table.players_of(package_id))
        .await
        .unwrap_or_default()
}

async fn player_reports_uri(player: &zbus::Proxy<'_>, expected_uri: &str) -> bool {
    player
        .get_property::<HashMap<String, OwnedValue>>("Metadata")
        .await
        .ok()
        .and_then(|metadata| metadata_string(&metadata, "xesam:url"))
        .is_some_and(|uri| uri == expected_uri)
}

/// Call `method` on `proxy` only while `authority` is still active.
async fn call_authorized<B>(
    authority: &ResumeAuthority,
    proxy: &zbus::Proxy<'_>,
    method: &'static str,
    body: &B,
) -> bool
where
    B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
{
    authority
        .mutate(proxy.call_method(method, body))
        .await
        .is_some_and(|result| result.is_ok())
}

async fn drive_and_verify_player(
    player: &zbus::Proxy<'_>,
    position_ms: u64,
    playing: bool,
    authority: &ResumeAuthority,
) -> bool {
    if !authority.is_active() {
        return false;
    }
    let target_us = i64::try_from(position_ms.saturating_mul(1_000)).unwrap_or(i64::MAX);
    let can_seek = player
        .get_property::<bool>("CanSeek")
        .await
        .unwrap_or(false);
    let current_us = player.get_property::<i64>("Position").await.unwrap_or(-1);
    if current_us < 0 {
        return false;
    }
    match required_seek(current_us, target_us, can_seek) {
        SeekDecision::Satisfied => {}
        SeekDecision::Offset(offset)
            if call_authorized(authority, player, "Seek", &(offset,)).await => {}
        SeekDecision::Offset(_) | SeekDecision::Impossible => return false,
    }
    let method = if playing { "Play" } else { "Pause" };
    if !call_authorized(authority, player, method, &()).await {
        return false;
    }
    for _ in 0..5 {
        if !authority.is_active() {
            return false;
        }
        let status_matches = player
            .get_property::<String>("PlaybackStatus")
            .await
            .is_ok_and(|status| status == if playing { "Playing" } else { "Paused" });
        let position_matches = player
            .get_property::<i64>("Position")
            .await
            .is_ok_and(|position| position.abs_diff(target_us) <= RESUME_TOLERANCE_US as u64);
        if status_matches && position_matches {
            return true;
        }
        tokio::time::sleep(RESUME_POLL).await;
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeekDecision {
    Satisfied,
    Offset(i64),
    Impossible,
}

fn required_seek(current_us: i64, target_us: i64, can_seek: bool) -> SeekDecision {
    if current_us < 0 || target_us < 0 {
        return SeekDecision::Impossible;
    }
    if current_us.abs_diff(target_us) <= RESUME_TOLERANCE_US as u64 {
        return SeekDecision::Satisfied;
    }
    if can_seek {
        SeekDecision::Offset(target_us.saturating_sub(current_us))
    } else {
        SeekDecision::Impossible
    }
}

async fn open_uri_if_supported(
    connection: &zbus::Connection,
    bus_name: &str,
    uri: &str,
    authority: &ResumeAuthority,
) -> bool {
    let Some(root) = player_proxy(connection, bus_name, ROOT_IFACE).await else {
        return false;
    };
    let scheme = uri.split_once(':').map_or("", |(scheme, _)| scheme);
    if !root
        .get_property::<Vec<String>>("SupportedUriSchemes")
        .await
        .is_ok_and(|schemes| schemes.iter().any(|supported| supported == scheme))
    {
        return false;
    }
    call_authorized(authority, &root, "OpenUri", &(uri,)).await
}

fn resume_uri(source: &ResumeSource) -> Option<String> {
    match source {
        ResumeSource::Uri(uri) => safe_remote_locator(uri).then(|| uri.clone()),
        ResumeSource::LocalFile(path) if path.is_absolute() => {
            let path = path.to_str()?;
            if path.chars().any(char::is_control) {
                return None;
            }
            url::Url::from_file_path(path).ok().map(String::from)
        }
        ResumeSource::LocalFile(_) => None,
    }
}

fn report_from_observation(
    observation: &PlayerObservation,
    apps: &[InstalledMediaApp],
) -> Option<PlaybackReport> {
    let playing = match observation.status.as_str() {
        "Playing" => true,
        "Paused" => false,
        _ => return None,
    };
    let locator = metadata_string(&observation.metadata, "xesam:url")?;
    let source = portable_source(&locator)?;
    let scheme = match &source {
        HandoffSource::RemoteStream { .. } => locator.split_once(':')?.0,
        HandoffSource::LocalFile { .. } => "file",
        HandoffSource::Workspace { .. } | HandoffSource::AppResource { .. } => return None,
    };
    let package_id = resolve_package(&observation.package_id, apps, scheme, &source)?;
    let title = metadata_string(&observation.metadata, "xesam:title")
        .or_else(|| observation.identity.clone())
        .filter(|title| !title.is_empty() && !title.chars().any(char::is_control))
        .unwrap_or_else(|| "Media".to_string());
    let duration_ms = metadata_i64(&observation.metadata, "mpris:length")
        .filter(|value| *value >= 0)
        .map(|value| u64::try_from(value / 1_000).unwrap_or(u64::MAX));
    let position_ms = u64::try_from(observation.position_us.max(0) / 1_000).unwrap_or(0);
    if duration_ms.is_some_and(|duration| position_ms > duration) {
        return None;
    }
    let content_id = format!("blake3:{}", blake3::hash(locator.as_bytes()).to_hex());
    let session_suffix = blake3::hash(format!("{}\0{locator}", observation.bus_name).as_bytes());
    let session_suffix = session_suffix.to_hex();
    let session_suffix = session_suffix.as_str().get(..16)?;
    let report = PlaybackReport {
        session: PlaybackSession {
            id: format!("{package_id}:mpris:{session_suffix}"),
            app_id: package_id.clone(),
            title,
            content_id,
            position_ms,
            duration_ms,
            playing,
            has_video: locator_has_video(&locator),
        },
        workspace_id: String::new(),
        source,
        controller_bus_name: Some(observation.bus_name.clone()),
        accepted_handoff_request: None,
    };
    report.validate_route(&package_id, "").then_some(report)
}

fn resolve_package(
    verified_package_id: &str,
    apps: &[InstalledMediaApp],
    scheme: &str,
    source: &HandoffSource,
) -> Option<String> {
    let extension = match source {
        HandoffSource::LocalFile { path } => path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase),
        _ => None,
    };
    let matches = apps
        .iter()
        .filter(|app| {
            app.package_id == verified_package_id
                && if scheme == "file" {
                    extension
                        .as_ref()
                        .is_some_and(|extension| app.extensions.contains(extension))
                } else {
                    app.uri_schemes.iter().any(|claimed| claimed == scheme)
                }
        })
        .map(|app| app.package_id.as_str())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [package_id] => Some((*package_id).to_string()),
        _ => None,
    }
}

fn metadata_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
}

fn metadata_i64(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    metadata
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| i64::try_from(value).ok())
}

fn portable_source(locator: &str) -> Option<HandoffSource> {
    if safe_remote_locator(locator) {
        return Some(HandoffSource::RemoteStream {
            url: locator.to_string(),
        });
    }
    let encoded = locator.strip_prefix("file://")?;
    if !encoded.starts_with('/') || encoded.contains(['?', '#']) {
        return None;
    }
    let decoded = percent_decode(encoded)?;
    let path = PathBuf::from(decoded);
    path.is_absolute()
        .then_some(HandoffSource::LocalFile { path })
}

fn safe_remote_locator(locator: &str) -> bool {
    let Some(rest) = locator
        .strip_prefix("https://")
        .or_else(|| locator.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || locator.len() > 8192
        || locator
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return false;
    }
    let Some(query) = locator.split_once('?').map(|(_, query)| query) else {
        return true;
    };
    let query = query.split('#').next().unwrap_or(query);
    !query.split('&').any(|pair| {
        let key = pair.split_once('=').map_or(pair, |(key, _)| key);
        let key = percent_decode(key)
            .unwrap_or_else(|| key.to_string())
            .to_ascii_lowercase();
        matches!(
            key.as_str(),
            "access_token"
                | "auth"
                | "authorization"
                | "expire"
                | "expires"
                | "key"
                | "sig"
                | "signature"
                | "token"
        ) || key.starts_with("x-amz-")
    })
}

fn percent_decode(value: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()?;
    (!decoded.chars().any(char::is_control)).then(|| decoded.into_owned())
}

fn locator_has_video(locator: &str) -> bool {
    let path = locator.split(['?', '#']).next().unwrap_or(locator);
    matches!(
        path.rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("avi" | "m4v" | "mkv" | "mov" | "mp4" | "webm")
    )
}

/// A transport command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Pause,
    PlayPause,
    Next,
    Previous,
    Stop,
}

impl Command {
    fn method(self) -> &'static str {
        match self {
            Command::Pause => "Pause",
            Command::PlayPause => "PlayPause",
            Command::Next => "Next",
            Command::Previous => "Previous",
            Command::Stop => "Stop",
        }
    }
}

/// So the OSD reports the resulting state, not the button pressed —
/// `PlayPause` is a toggle, and a wrong label would be worse than none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    NowPlaying,
    NowPaused,
    Skipped,
    Stopped,
}

/// Send `cmd` to the active player on the session runtime and return at
/// once. `on_outcome` runs there with the resulting state once the player
/// accepted the command within `DEADLINE`; it should only publish (wake the
/// compositor loop). With no player, no bus, a refusal or a late answer it
/// is not called — the caller shows nothing rather than claim something
/// happened. Returns `false` when the command could not be dispatched.
pub fn send<F>(cmd: Command, on_outcome: F) -> bool
where
    F: FnOnce(Outcome) + Send + 'static,
{
    spawn_on_session(async move {
        let asked = Instant::now();
        let Some(outcome) = run(cmd).await else {
            return;
        };
        if asked.elapsed() <= DEADLINE {
            on_outcome(outcome);
        } else {
            tracing::debug!(target: "tilo::mpris", "player answered after the deadline; no OSD");
        }
    })
}

/// Pause one exact player after its cross-device successor has reported a
/// successfully opened source. Unlike media-key dispatch, this never falls
/// back to whichever unrelated player happens to be active. Returns whether
/// the request was dispatched; the pause itself happens on the session
/// runtime.
pub fn pause_named(bus_name: &str) -> bool {
    if !bus_name.starts_with(MPRIS_PREFIX) || bus_name.len() > 256 {
        return false;
    }
    let owned = bus_name.to_string();
    spawn_on_session(async move {
        let Some(connection) = session_bus().await else {
            return;
        };
        if let Some(proxy) = player_proxy(&connection, &owned, PLAYER_IFACE).await
            && let Err(error) = proxy.call_method("Pause", &()).await
        {
            tracing::debug!(target: "tilo::mpris", %error, "handoff source did not pause");
        }
    })
}

/// Pause every player that is playing right now — the private output it was
/// playing to has gone. Only `Playing` players are touched, and nothing
/// records them for a later resume: plugging the output back in must never
/// start sound on its own. Returns whether the pause was dispatched.
pub fn pause_playing_players() -> bool {
    spawn_on_session(async {
        let Some(playing) = with_players(PlayerTable::playing).await else {
            return;
        };
        let Some(connection) = session_bus().await else {
            return;
        };
        for bus_name in playing {
            let Some(proxy) = player_proxy(&connection, &bus_name, PLAYER_IFACE).await else {
                continue;
            };
            if let Err(error) = proxy.call_method("Pause", &()).await {
                tracing::debug!(target: "tilo::mpris", %error, "player did not pause on unplug");
            }
        }
    })
}

/// Pause one exact player only while it still owns the URI that this handoff
/// opened. This prevents compensation for a failed acknowledgement from
/// pausing unrelated playback that started immediately afterwards. Returns
/// whether the request was dispatched; the check and pause happen on the
/// session runtime.
pub(crate) fn pause_named_if_uri(bus_name: &str, expected_uri: &str) -> bool {
    if !bus_name.starts_with(MPRIS_PREFIX)
        || bus_name.len() > 256
        || expected_uri.is_empty()
        || expected_uri.len() > 8192
    {
        return false;
    }
    let owned = bus_name.to_string();
    let expected = expected_uri.to_string();
    spawn_on_session(async move {
        let Some(connection) = session_bus().await else {
            return;
        };
        let Some(proxy) = player_proxy(&connection, &owned, PLAYER_IFACE).await else {
            return;
        };
        if player_reports_uri(&proxy, &expected).await
            && let Err(error) = proxy.call_method("Pause", &()).await
        {
            tracing::debug!(target: "tilo::mpris", %error, "compensating pause failed");
        }
    })
}

/// Most players the now-playing card lists.
const MAX_NOW_PLAYING_PLAYERS: usize = 16;

/// The players last observed. Empty until the observer has listed the bus.
pub fn now_playing_players() -> Vec<crate::shell::now_playing::PlayerSnapshot> {
    observer::OBSERVER
        .lock()
        .map_or_else(|_| Vec::new(), |state| state.now_playing.clone())
}

/// Make sure the players are being observed. The observer follows their
/// signals, so there is nothing to poll; this never waits on the bus.
pub fn refresh_now_playing() {
    observer::ensure_started();
}

/// Send `command` to exactly the player at `bus_name`, never to whichever
/// other player happens to be active. Returns whether it was dispatched; the
/// call itself happens on the session runtime.
pub fn control(bus_name: &str, command: crate::shell::now_playing::PlayerCommand) -> bool {
    use crate::shell::now_playing::{PlaybackStatus, PlayerCommand};
    if !bus_name.starts_with(MPRIS_PREFIX) || bus_name.len() > 256 {
        return false;
    }
    // Show the toggle at once; the next observation confirms or corrects it.
    if matches!(command, PlayerCommand::PlayPause | PlayerCommand::Pause)
        && let Ok(mut state) = observer::OBSERVER.lock()
        && let Some(player) = state
            .now_playing
            .iter_mut()
            .find(|player| player.bus_name == bus_name)
    {
        let now = Instant::now();
        player.position_us = player.position_at(now);
        player.observed_at = now;
        player.status = if player.status == PlaybackStatus::Playing {
            PlaybackStatus::Paused
        } else if command == PlayerCommand::Pause {
            player.status
        } else {
            PlaybackStatus::Playing
        };
    }
    let owned = bus_name.to_string();
    spawn_on_session(async move {
        let Some(connection) = session_bus().await else {
            return;
        };
        let Some(proxy) = player_proxy(&connection, &owned, PLAYER_IFACE).await else {
            return;
        };
        let result = match &command {
            PlayerCommand::PlayPause => proxy.call_method("PlayPause", &()).await,
            PlayerCommand::Pause => proxy.call_method("Pause", &()).await,
            PlayerCommand::Next => proxy.call_method("Next", &()).await,
            PlayerCommand::Previous => proxy.call_method("Previous", &()).await,
            PlayerCommand::SetPosition {
                track_id,
                position_us,
            } => {
                let Ok(track) = zbus::zvariant::ObjectPath::try_from(track_id.as_str()) else {
                    return;
                };
                proxy
                    .call_method("SetPosition", &(track, *position_us))
                    .await
            }
        };
        if let Err(error) = result {
            tracing::debug!(target: "tilo::mpris", %error, ?command, "player rejected the command");
        }
        tokio::time::sleep(COMMAND_SETTLE).await;
        observer::republish(&observer::OBSERVER);
    })
}

/// The scalar properties of one player, read before parsing.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each MPRIS capability is an independent property the player reports"
)]
struct NowPlayingProperties {
    identity: Option<String>,
    status: String,
    position_us: i64,
    can_play: bool,
    can_pause: bool,
    can_seek: bool,
    can_go_next: bool,
    can_go_previous: bool,
}

fn clean_text(value: &str) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty() && !value.chars().any(char::is_control)).then_some(value)
}

/// Parse a player's properties into a snapshot, plus the local artwork file
/// it names (`file://` only). `None` for a player with no transport state.
fn now_playing_from_properties(
    bus_name: &str,
    properties: &NowPlayingProperties,
    metadata: &HashMap<String, OwnedValue>,
    observed_at: Instant,
) -> Option<(crate::shell::now_playing::PlayerSnapshot, Option<String>)> {
    let status = crate::shell::now_playing::PlaybackStatus::from_mpris(&properties.status)?;
    let identity = properties
        .identity
        .clone()
        .as_deref()
        .and_then(clean_text)
        .unwrap_or_else(|| {
            bus_name
                .trim_start_matches(MPRIS_PREFIX)
                .split('.')
                .next()
                .unwrap_or_default()
                .to_string()
        });
    let artist = metadata
        .get("xesam:artist")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| clean_text(artist))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|artists| !artists.is_empty());
    let track_id = metadata
        .get("mpris:trackid")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| {
            let fallback = value.try_clone().ok();
            zbus::zvariant::OwnedObjectPath::try_from(value)
                .map(|path| path.as_str().to_string())
                .ok()
                .or_else(|| fallback.and_then(|value| String::try_from(value).ok()))
        })
        .filter(|track| track != "/org/mpris/MediaPlayer2/TrackList/NoTrack");
    let art_url = metadata_string(metadata, "mpris:artUrl")
        .filter(|url| url.starts_with("file://") && url.len() <= 4096);
    let length_us = metadata_i64(metadata, "mpris:length")
        .or_else(|| {
            metadata
                .get("mpris:length")
                .and_then(|value| value.try_clone().ok())
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| i64::try_from(value).ok())
        })
        .filter(|length| *length > 0);
    Some((
        crate::shell::now_playing::PlayerSnapshot {
            bus_name: bus_name.to_string(),
            identity,
            status,
            title: metadata_string(metadata, "xesam:title").and_then(|title| clean_text(&title)),
            artist,
            artwork: None,
            track_id,
            length_us,
            position_us: properties.position_us.max(0),
            can_play: properties.can_play,
            can_pause: properties.can_pause,
            can_seek: properties.can_seek,
            can_go_next: properties.can_go_next,
            can_go_previous: properties.can_go_previous,
            observed_at,
        },
        art_url,
    ))
}

async fn run(cmd: Command) -> Option<Outcome> {
    // Status is taken *before* acting: reading after would race the
    // player's own (asynchronous) state change.
    let (name, was_playing) = with_players(PlayerTable::active_player).await??;
    let conn = session_bus().await?;
    let proxy = player_proxy(&conn, &name, PLAYER_IFACE).await?;

    proxy
        .call_method(cmd.method(), &())
        .await
        .map_err(|e| {
            tracing::debug!(target: "tilo::mpris", error = %e, method = cmd.method(), "player rejected the command");
        })
        .ok()?;

    Some(match cmd {
        Command::Pause => Outcome::NowPaused,
        Command::PlayPause => {
            if was_playing {
                Outcome::NowPaused
            } else {
                Outcome::NowPlaying
            }
        }
        Command::Next | Command::Previous => Outcome::Skipped,
        Command::Stop => Outcome::Stopped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use zbus::zvariant::Value;

    /// Drive one authorized mutation to completion on the calling thread.
    fn mutate_now<T>(authority: &ResumeAuthority, operation: impl FnOnce() -> T) -> Option<T> {
        tilo_rt::block_on(authority.mutate(async move { operation() })).unwrap()
    }

    fn text(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(value.to_string())).unwrap()
    }

    fn number(value: i64) -> OwnedValue {
        OwnedValue::try_from(Value::from(value)).unwrap()
    }

    fn now_playing_properties(status: &str) -> NowPlayingProperties {
        NowPlayingProperties {
            identity: Some(" Music ".into()),
            status: status.into(),
            position_us: 12_000_000,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: false,
            can_go_previous: true,
        }
    }

    #[test]
    fn now_playing_reads_metadata_and_only_local_artwork() {
        let metadata = HashMap::from([
            ("xesam:title".to_string(), text("Song")),
            (
                "xesam:artist".to_string(),
                OwnedValue::try_from(Value::from(vec!["One".to_string(), "Two".to_string()]))
                    .unwrap(),
            ),
            ("mpris:length".to_string(), number(180_000_000)),
            (
                "mpris:trackid".to_string(),
                OwnedValue::try_from(Value::from(
                    zbus::zvariant::ObjectPath::try_from("/org/tilo/track/7").unwrap(),
                ))
                .unwrap(),
            ),
            (
                "mpris:artUrl".to_string(),
                text("file:///home/me/Music/cover%20art.png"),
            ),
        ]);
        let (player, art) = now_playing_from_properties(
            "org.mpris.MediaPlayer2.music",
            &now_playing_properties("Playing"),
            &metadata,
            Instant::now(),
        )
        .expect("a playing player");
        assert_eq!(player.identity, "Music");
        assert_eq!(player.title.as_deref(), Some("Song"));
        assert_eq!(player.artist.as_deref(), Some("One, Two"));
        assert_eq!(player.length_us, Some(180_000_000));
        assert_eq!(player.track_id.as_deref(), Some("/org/tilo/track/7"));
        assert_eq!(
            player.status,
            crate::shell::now_playing::PlaybackStatus::Playing
        );
        assert!(player.seekable());
        assert!(!player.can_go_next);
        assert_eq!(
            art.as_deref(),
            Some("file:///home/me/Music/cover%20art.png")
        );

        let remote = HashMap::from([(
            "mpris:artUrl".to_string(),
            text("https://tracker.example/cover.jpg"),
        )]);
        let (player, art) = now_playing_from_properties(
            "org.mpris.MediaPlayer2.web",
            &NowPlayingProperties {
                identity: None,
                ..now_playing_properties("Paused")
            },
            &remote,
            Instant::now(),
        )
        .expect("a paused player");
        assert_eq!(art, None, "the shell never fetches remote artwork");
        assert_eq!(player.identity, "web");
        assert!(!player.seekable(), "no length, no seek bar");
        assert!(
            now_playing_from_properties(
                "org.mpris.MediaPlayer2.odd",
                &now_playing_properties("Buffering"),
                &HashMap::new(),
                Instant::now(),
            )
            .is_none()
        );
    }

    #[test]
    fn control_refuses_names_that_are_not_players() {
        use crate::shell::now_playing::PlayerCommand;
        assert!(!control("org.freedesktop.DBus", PlayerCommand::Pause));
        assert!(!control(
            &format!("{MPRIS_PREFIX}{}", "x".repeat(300)),
            PlayerCommand::Pause
        ));
    }

    fn observation(locator: &str) -> PlayerObservation {
        PlayerObservation {
            bus_name: "org.mpris.MediaPlayer2.browser".into(),
            package_id: "so.tilo.browser".into(),
            identity: Some("Browser".into()),
            status: "Playing".into(),
            metadata: HashMap::from([
                ("xesam:url".into(), text(locator)),
                ("xesam:title".into(), text("A film")),
                ("mpris:length".into(), number(9_000_000)),
            ]),
            position_us: 4_000_000,
        }
    }

    fn browser() -> InstalledMediaApp {
        InstalledMediaApp {
            package_id: "so.tilo.browser".into(),
            uri_schemes: vec!["http".into(), "https".into()],
            extensions: vec!["mp4".into()],
        }
    }

    #[test]
    fn mpris_names_commands_and_dispatch_follow_the_spec_without_blocking() {
        fn commands_map_to_the_spec_method_names() {
            // Wire names in the MPRIS spec, not ours to rename.
            assert_eq!(Command::PlayPause.method(), "PlayPause");
            assert_eq!(Command::Pause.method(), "Pause");
            assert_eq!(Command::Next.method(), "Next");
            assert_eq!(Command::Previous.method(), "Previous");
            assert_eq!(Command::Stop.method(), "Stop");
        }

        fn the_well_known_names_match_the_spec() {
            assert_eq!(MPRIS_PREFIX, "org.mpris.MediaPlayer2.");
            assert_eq!(MPRIS_PATH, "/org/mpris/MediaPlayer2");
            assert_eq!(PLAYER_IFACE, "org.mpris.MediaPlayer2.Player");
        }

        fn ambiguous_or_stopped_players_are_not_advertised() {
            let observed = observation("https://media.example/film.mp4");
            assert!(report_from_observation(&observed, &[browser(), browser()]).is_none());

            let mut stopped = observed;
            stopped.status = "Stopped".into();
            assert!(report_from_observation(&stopped, &[browser()]).is_none());
        }

        /// Must survive a machine with no session bus and no player. Unit
        /// tests have no bus (see `session_bus`), so the command reaches no
        /// player.
        fn send_dispatches_without_waiting_on_the_bus() {
            let started = std::time::Instant::now();
            let (tx, _outcomes) = mpsc::channel();
            let dispatched = send(Command::PlayPause, move |outcome| {
                let _ = tx.send(outcome);
            });
            let elapsed = started.elapsed();
            assert!(dispatched, "the session runtime takes the command");
            // Any bus round trip happens on the session runtime, never here.
            assert!(
                elapsed < DEADLINE,
                "the input path must not wait on the bus: took {elapsed:?}"
            );
        }

        commands_map_to_the_spec_method_names();
        the_well_known_names_match_the_spec();
        ambiguous_or_stopped_players_are_not_advertised();
        send_dispatches_without_waiting_on_the_bus();
    }

    #[test]
    fn player_reports_are_portable_signed_and_reject_unsafe_sources() {
        fn generic_player_becomes_a_portable_signed_app_report() {
            let observed = observation("https://media.example/film.mp4");
            let first = report_from_observation(&observed, &[browser()]).unwrap();
            let second = report_from_observation(&observed, &[browser()]).unwrap();

            assert_eq!(first, second, "session identity must be stable");
            assert_eq!(first.session.app_id, "so.tilo.browser");
            assert_eq!(first.session.position_ms, 4_000);
            assert_eq!(first.session.duration_ms, Some(9_000));
            assert!(first.session.has_video);
            assert!(matches!(
                first.source,
                HandoffSource::RemoteStream { ref url }
                    if url == "https://media.example/film.mp4"
            ));
            assert_eq!(
                first.controller_bus_name.as_deref(),
                Some("org.mpris.MediaPlayer2.browser")
            );
        }

        fn local_paths_stay_in_the_private_transfer_source() {
            let report = report_from_observation(
                &observation("file:///home/alice/Videos/My%20Film.mp4"),
                &[browser()],
            )
            .unwrap();

            assert!(matches!(
                report.source,
                HandoffSource::LocalFile { ref path }
                    if path == &PathBuf::from("/home/alice/Videos/My Film.mp4")
            ));
            assert!(!format!("{:?}", report.session).contains("/home/alice"));
        }

        fn unsafe_or_nonportable_urls_are_rejected() {
            assert!(
                report_from_observation(
                    &observation("https://alice:secret@media.example/film.mp4"),
                    &[browser()]
                )
                .is_none()
            );
            assert!(
                report_from_observation(
                    &observation("https://media.example/film.mp4?access_token=secret"),
                    &[browser()]
                )
                .is_none()
            );
            assert!(
                report_from_observation(
                    &observation("blob:https://media.example/ephemeral"),
                    &[browser()]
                )
                .is_none()
            );
            assert!(
                report_from_observation(&observation("file://server/share/film.mp4"), &[browser()])
                    .is_none()
            );
        }

        fn destination_resume_sources_are_canonical_and_bearer_free() {
            assert_eq!(
                resume_uri(&ResumeSource::LocalFile(PathBuf::from(
                    "/home/alice/My Film.mp4"
                ))),
                Some("file:///home/alice/My%20Film.mp4".into())
            );
            assert_eq!(
                resume_uri(&ResumeSource::Uri(
                    "https://media.example/film.mp4?v=42".into()
                )),
                Some("https://media.example/film.mp4?v=42".into())
            );
            assert!(
                resume_uri(&ResumeSource::Uri(
                    "https://media.example/film.mp4?%61ccess_token=secret".into()
                ))
                .is_none()
            );
        }

        fn destination_only_accepts_an_observed_or_seekable_exact_playhead() {
            assert_eq!(
                required_seek(4_100_000, 4_000_000, false),
                SeekDecision::Satisfied
            );
            assert_eq!(
                required_seek(1_000_000, 4_000_000, true),
                SeekDecision::Offset(3_000_000)
            );
            assert_eq!(
                required_seek(1_000_000, 4_000_000, false),
                SeekDecision::Impossible
            );
            assert_eq!(required_seek(-1, 4_000_000, true), SeekDecision::Impossible);
        }

        generic_player_becomes_a_portable_signed_app_report();
        local_paths_stay_in_the_private_transfer_source();
        unsafe_or_nonportable_urls_are_rejected();
        destination_resume_sources_are_canonical_and_bearer_free();
        destination_only_accepts_an_observed_or_seekable_exact_playhead();
    }

    #[test]
    fn resume_cancellation_is_linearized_and_blocks_late_effects() {
        fn cancelled_resume_cannot_start_or_publish_a_late_acceptance() {
            let request_id = [21; 16];
            let now = Instant::now();
            let mut resumes = ResumeState::default();

            assert!(!resumes.cancel(request_id, now));
            assert!(resumes.begin(request_id, now).is_none());
            assert!(resumes.take_accepted_resumes().is_empty());
        }

        fn cancellation_after_worker_launch_blocks_every_later_mutation() {
            let request_id = [22; 16];
            let now = Instant::now();
            let mut resumes = ResumeState::default();
            let authority = resumes.begin(request_id, now).unwrap();
            let mut mutations = Vec::new();

            assert_eq!(
                mutate_now(&authority, || mutations.push("open-uri")),
                Some(())
            );
            assert!(resumes.cancel(request_id, now));
            assert_eq!(mutate_now(&authority, || mutations.push("seek")), None);
            assert_eq!(mutate_now(&authority, || mutations.push("play")), None);
            resumes.finish(&authority, true);

            assert_eq!(mutations, ["open-uri"]);
            assert!(resumes.take_accepted_resumes().is_empty());
        }

        fn cancellation_is_linearized_after_an_in_flight_mutation() {
            let request_id = [24; 16];
            let now = Instant::now();
            let resumes = Arc::new(Mutex::new(ResumeState::default()));
            let authority = resumes.lock().unwrap().begin(request_id, now).unwrap();
            let mutation_authority = authority.clone();
            let (mutation_started_tx, mutation_started_rx) = mpsc::channel();
            let (release_mutation_tx, release_mutation_rx) = mpsc::channel();
            let mutation = std::thread::spawn(move || {
                mutate_now(&mutation_authority, || {
                    mutation_started_tx.send(()).unwrap();
                    release_mutation_rx.recv().unwrap();
                })
            });
            mutation_started_rx.recv().unwrap();

            let cancel_resumes = Arc::clone(&resumes);
            let (cancel_started_tx, cancel_started_rx) = mpsc::channel();
            let (cancelled_tx, cancelled_rx) = mpsc::channel();
            let cancellation = std::thread::spawn(move || {
                cancel_started_tx.send(()).unwrap();
                let cancelled = cancel_resumes.lock().unwrap().cancel(request_id, now);
                cancelled_tx.send(cancelled).unwrap();
            });
            cancel_started_rx.recv().unwrap();
            assert!(
                cancelled_rx
                    .recv_timeout(Duration::from_millis(20))
                    .is_err(),
                "cancel must wait for the already-started mutation to leave its gate"
            );

            release_mutation_tx.send(()).unwrap();
            assert_eq!(mutation.join().unwrap(), Some(()));
            assert!(tilo_test_env::recv(
                "a message on cancelled_rx",
                &cancelled_rx
            ));
            cancellation.join().unwrap();
            assert_eq!(mutate_now(&authority, || panic!("late mutation ran")), None);
            resumes.lock().unwrap().finish(&authority, true);
            assert!(resumes.lock().unwrap().take_accepted_resumes().is_empty());
        }

        fn cancellation_retracts_completion_and_tombstones_are_bounded() {
            let request_id = [23; 16];
            let now = Instant::now();
            let mut resumes = ResumeState::default();
            let authority = resumes.begin(request_id, now).unwrap();
            resumes.finish(&authority, true);
            assert!(resumes.cancel(request_id, now));
            assert!(resumes.take_accepted_resumes().is_empty());

            for index in 0..=MAX_CANCELLED_RESUMES {
                let mut id = [0; 16];
                id[..8].copy_from_slice(&u64::try_from(index).unwrap().to_be_bytes());
                id[15] = 1;
                let _ = resumes.cancel(id, now);
            }
            assert_eq!(resumes.cancelled.len(), MAX_CANCELLED_RESUMES);
        }

        cancelled_resume_cannot_start_or_publish_a_late_acceptance();
        cancellation_after_worker_launch_blocks_every_later_mutation();
        cancellation_is_linearized_after_an_in_flight_mutation();
        cancellation_retracts_completion_and_tombstones_are_bounded();
    }
}
