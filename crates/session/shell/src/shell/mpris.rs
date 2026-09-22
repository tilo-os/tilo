//! Driving whatever is playing, over MPRIS.
//!
//! The transport keys (play/pause, next, previous) address *an
//! application*, not the machine, so they go over `org.mpris.MediaPlayer2`
//! on the session bus — the interface every real player and Tilo's own
//! Music app (`apps/music/src/mpris.rs`) implements.
//!
//! Called from the compositor's input path, but a D-Bus round trip has no
//! natural bound (tens of seconds) and `zbus::block_on` can't run inside
//! the compositor's tokio runtime — so the bus work happens on a throwaway
//! `std::thread`, and the caller waits on a channel with a short deadline:
//! a healthy player answers in under a millisecond, an unhealthy one costs
//! one dropped OSD instead of a frozen desktop.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};

use media::handoff::{HandoffSource, PlaybackReport, PlaybackSession};
use zbus::zvariant::OwnedValue;

/// How long the caller waits for the player to answer before giving up.
const DEADLINE: Duration = Duration::from_millis(250);

/// The MPRIS well-known name prefix. Every player owns one.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct RefreshState {
    running: bool,
    last_started: Option<Instant>,
}

static REFRESH: LazyLock<Mutex<RefreshState>> =
    LazyLock::new(|| Mutex::new(RefreshState::default()));

const RESUME_DEADLINE: Duration = Duration::from_secs(10);
const RESUME_POLL: Duration = Duration::from_millis(100);
const RESUME_TOLERANCE_US: i64 = 500_000;
const MAX_PENDING_RESUMES: usize = 16;
const MAX_CANCELLED_RESUMES: usize = MAX_PENDING_RESUMES * 4;
const CANCELLED_RESUME_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct ResumeState {
    pending: BTreeMap<[u8; 16], Arc<Mutex<bool>>>,
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

#[derive(Clone)]
struct ResumeAuthority {
    request_id: [u8; 16],
    gate: Arc<Mutex<bool>>,
}

impl ResumeAuthority {
    fn is_active(&self) -> bool {
        self.gate.lock().is_ok_and(|active| *active)
    }

    fn mutate<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let active = self.gate.lock().ok()?;
        (*active).then(operation)
    }

    fn revoke(&self) {
        match self.gate.lock() {
            Ok(mut active) => *active = false,
            Err(poisoned) => *poisoned.into_inner() = false,
        }
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
        let gate = Arc::new(Mutex::new(true));
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

    fn take_accepted(&mut self) -> Vec<[u8; 16]> {
        self.accepted
            .drain(..)
            .map(|accepted| accepted.request_id)
            .collect()
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

/// Refresh the shell-owned MPRIS projection without ever putting a D-Bus
/// round trip on the compositor's sampling path. The previous bounded
/// snapshot remains usable while this worker runs and expires in `media` if
/// discovery stops making progress.
pub fn refresh_handoff_reports() {
    let Ok(mut refresh) = REFRESH.lock() else {
        return;
    };
    if refresh.running
        || refresh
            .last_started
            .is_some_and(|started| started.elapsed() < DISCOVERY_INTERVAL)
    {
        return;
    }
    refresh.running = true;
    refresh.last_started = Some(Instant::now());
    if std::thread::Builder::new()
        .name("tilo-mpris-discovery".to_string())
        .spawn(|| {
            media::handoff::replace_system_reports(discover_handoff_reports());
            if let Ok(mut refresh) = REFRESH.lock() {
                refresh.running = false;
            }
        })
        .is_err()
    {
        refresh.running = false;
    }
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
    if std::thread::Builder::new()
        .name("tilo-mpris-resume".to_string())
        .spawn(move || {
            let accepted = run_resume(&request, &authority);
            if let Ok(mut resumes) = RESUMES.lock() {
                resumes.finish_with_player(&authority, accepted.map(Some));
            }
        })
        .is_err()
    {
        resumes.cancel(request_id, Instant::now());
        return false;
    }
    true
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

/// Drain handoffs whose destination player demonstrably reached the requested
/// state. The fabric runtime turns these into acknowledgements; failed or
/// unsupported resumes never appear here.
pub fn take_accepted_handoff_requests() -> Vec<[u8; 16]> {
    RESUMES
        .lock()
        .map_or_else(|_| Vec::new(), |mut resumes| resumes.take_accepted())
}

pub(crate) fn take_accepted_handoff_resumes() -> Vec<AcceptedResume> {
    RESUMES.lock().map_or_else(
        |_| Vec::new(),
        |mut resumes| resumes.take_accepted_resumes(),
    )
}

fn run_resume(request: &ResumeRequest, authority: &ResumeAuthority) -> Option<(String, String)> {
    let Some(expected_uri) = resume_uri(&request.source) else {
        return None;
    };
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return None;
    };
    let started = Instant::now();
    let mut opened = BTreeSet::new();
    while started.elapsed() < RESUME_DEADLINE {
        if !authority.is_active() {
            return None;
        }
        for bus_name in verified_player_names(&connection, &request.package_id) {
            if !authority.is_active() {
                return None;
            }
            let Ok(player) = zbus::blocking::Proxy::new(
                &connection,
                bus_name.as_str(),
                MPRIS_PATH,
                PLAYER_IFACE,
            ) else {
                continue;
            };
            if player_reports_uri(&player, &expected_uri) {
                if drive_and_verify_player(&player, request.position_ms, request.playing, authority)
                {
                    return (authority.is_active() && started.elapsed() <= RESUME_DEADLINE)
                        .then(|| (bus_name.clone(), expected_uri.clone()));
                }
            } else if opened.insert(bus_name.clone()) {
                let _ = open_uri_if_supported(&connection, &bus_name, &expected_uri, authority);
            }
        }
        if authority.is_active() {
            std::thread::sleep(RESUME_POLL);
        }
    }
    None
}

fn verified_player_names(connection: &zbus::blocking::Connection, package_id: &str) -> Vec<String> {
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(connection) else {
        return Vec::new();
    };
    let Ok(names) = dbus.list_names() else {
        return Vec::new();
    };
    names
        .into_iter()
        .map(|name| name.as_str().to_string())
        .filter(|name| name.starts_with(MPRIS_PREFIX))
        .filter(|name| {
            let Ok(name) = zbus::names::BusName::try_from(name.as_str()) else {
                return false;
            };
            dbus.get_connection_unix_process_id(name)
                .ok()
                .and_then(crate::shell::launcher::verified_package_for_descendant)
                .is_some_and(|verified| verified == package_id)
        })
        .collect()
}

fn player_reports_uri(player: &zbus::blocking::Proxy<'_>, expected_uri: &str) -> bool {
    player
        .get_property::<HashMap<String, OwnedValue>>("Metadata")
        .ok()
        .and_then(|metadata| metadata_string(&metadata, "xesam:url"))
        .is_some_and(|uri| uri == expected_uri)
}

fn drive_and_verify_player(
    player: &zbus::blocking::Proxy<'_>,
    position_ms: u64,
    playing: bool,
    authority: &ResumeAuthority,
) -> bool {
    if !authority.is_active() {
        return false;
    }
    let target_us = i64::try_from(position_ms.saturating_mul(1_000)).unwrap_or(i64::MAX);
    let can_seek = player.get_property::<bool>("CanSeek").unwrap_or(false);
    let current_us = player.get_property::<i64>("Position").unwrap_or(-1);
    if current_us < 0 {
        return false;
    }
    match required_seek(current_us, target_us, can_seek) {
        SeekDecision::Satisfied => {}
        SeekDecision::Offset(offset)
            if authority
                .mutate(|| player.call_method("Seek", &(offset,)))
                .is_some_and(|result| result.is_ok()) => {}
        SeekDecision::Offset(_) | SeekDecision::Impossible => return false,
    }
    let method = if playing { "Play" } else { "Pause" };
    if !authority
        .mutate(|| player.call_method(method, &()))
        .is_some_and(|result| result.is_ok())
    {
        return false;
    }
    for _ in 0..5 {
        if !authority.is_active() {
            return false;
        }
        let status_matches = player
            .get_property::<String>("PlaybackStatus")
            .is_ok_and(|status| status == if playing { "Playing" } else { "Paused" });
        let position_matches = player
            .get_property::<i64>("Position")
            .is_ok_and(|position| position.abs_diff(target_us) <= RESUME_TOLERANCE_US as u64);
        if status_matches && position_matches {
            return true;
        }
        std::thread::sleep(RESUME_POLL);
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

fn open_uri_if_supported(
    connection: &zbus::blocking::Connection,
    bus_name: &str,
    uri: &str,
    authority: &ResumeAuthority,
) -> bool {
    let Ok(root) = zbus::blocking::Proxy::new(connection, bus_name, MPRIS_PATH, ROOT_IFACE) else {
        return false;
    };
    let scheme = uri.split_once(':').map_or("", |(scheme, _)| scheme);
    if !root
        .get_property::<Vec<String>>("SupportedUriSchemes")
        .is_ok_and(|schemes| schemes.iter().any(|supported| supported == scheme))
    {
        return false;
    }
    authority
        .mutate(|| root.call_method("OpenUri", &(uri,)))
        .is_some_and(|result| result.is_ok())
}

fn resume_uri(source: &ResumeSource) -> Option<String> {
    match source {
        ResumeSource::Uri(uri) => safe_remote_locator(uri).then(|| uri.clone()),
        ResumeSource::LocalFile(path) if path.is_absolute() => {
            let path = path.to_str()?;
            if path.chars().any(char::is_control) {
                return None;
            }
            Some(format!("file://{}", percent_encode_path(path)))
        }
        ResumeSource::LocalFile(_) => None,
    }
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn discover_handoff_reports() -> Vec<PlaybackReport> {
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return Vec::new();
    };
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&connection) else {
        return Vec::new();
    };
    let Ok(names) = dbus.list_names() else {
        return Vec::new();
    };
    let apps = crate::shell::launcher::installed_apps()
        .unwrap_or_default()
        .into_iter()
        .filter(|app| app.has_gui)
        .map(|app| InstalledMediaApp {
            package_id: app.package_id,
            uri_schemes: app.opens_uri_schemes,
            extensions: app.opens_extensions,
        })
        .collect::<Vec<_>>();
    names
        .into_iter()
        .map(|name| name.as_str().to_string())
        .filter(|name| name.starts_with(MPRIS_PREFIX))
        .filter_map(|bus_name| {
            let name = zbus::names::BusName::try_from(bus_name.as_str()).ok()?;
            let pid = dbus.get_connection_unix_process_id(name).ok()?;
            let package_id = crate::shell::launcher::verified_package_for_descendant(pid)?;
            observe_player(&connection, bus_name, package_id)
        })
        .filter_map(|observation| report_from_observation(&observation, &apps))
        .take(media::handoff::MAX_PLAYBACK_REPORTS)
        .collect()
}

fn observe_player(
    connection: &zbus::blocking::Connection,
    bus_name: String,
    package_id: String,
) -> Option<PlayerObservation> {
    let player =
        zbus::blocking::Proxy::new(connection, bus_name.as_str(), MPRIS_PATH, PLAYER_IFACE).ok()?;
    let root =
        zbus::blocking::Proxy::new(connection, bus_name.as_str(), MPRIS_PATH, ROOT_IFACE).ok()?;
    let identity = root.get_property("Identity").ok();
    let status = player.get_property("PlaybackStatus").ok()?;
    let metadata = player.get_property("Metadata").ok()?;
    let position_us = player.get_property("Position").unwrap_or(0);
    drop(root);
    drop(player);
    Some(PlayerObservation {
        bus_name,
        package_id,
        identity,
        status,
        metadata,
        position_us,
    })
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
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = *bytes.get(index)?;
        if byte == b'%' {
            let high_index = index.checked_add(1)?;
            let low_index = index.checked_add(2)?;
            let high = hex_nibble(*bytes.get(high_index)?)?;
            let low = hex_nibble(*bytes.get(low_index)?)?;
            decoded.push(high.checked_shl(4)?.checked_add(low)?);
            index = index.saturating_add(3);
        } else {
            decoded.push(byte);
            index = index.saturating_add(1);
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.chars().any(char::is_control)).then_some(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => byte.checked_sub(b'0'),
        b'a'..=b'f' => byte.checked_sub(b'a')?.checked_add(10),
        b'A'..=b'F' => byte.checked_sub(b'A')?.checked_add(10),
        _ => None,
    }
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
}

impl Command {
    fn method(self) -> &'static str {
        match self {
            Command::Pause => "Pause",
            Command::PlayPause => "PlayPause",
            Command::Next => "Next",
            Command::Previous => "Previous",
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
}

/// Send `cmd` to the active player. Returns `None` when there is no
/// player, the bus is unreachable, or the player didn't answer inside
/// [`DEADLINE`] — cases where the caller should show nothing rather than
/// claim something happened.
pub fn send(cmd: Command) -> Option<Outcome> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("tilo-mpris".to_string())
        .spawn(move || {
            let _ = tx.send(run(cmd)); // dropped if the caller already gave up
        });
    if let Err(e) = spawned {
        tracing::warn!(target: "tilo::mpris", error = %e, "could not spawn mpris worker");
        return None;
    }
    if let Ok(outcome) = rx.recv_timeout(DEADLINE) {
        outcome
    } else {
        tracing::debug!(target: "tilo::mpris", "no player answered within the deadline");
        None
    }
}

/// Pause one exact player after its cross-device successor has reported a
/// successfully opened source. Unlike media-key dispatch, this never falls
/// back to whichever unrelated player happens to be active.
pub fn pause_named(bus_name: &str) -> bool {
    if !bus_name.starts_with(MPRIS_PREFIX) || bus_name.len() > 256 {
        return false;
    }
    let owned = bus_name.to_string();
    let (tx, rx) = mpsc::channel();
    if std::thread::Builder::new()
        .name("tilo-mpris-handoff".to_string())
        .spawn(move || {
            let result = zbus::blocking::Connection::session()
                .ok()
                .and_then(|connection| {
                    let proxy = zbus::blocking::Proxy::new(
                        &connection,
                        owned.as_str(),
                        MPRIS_PATH,
                        PLAYER_IFACE,
                    )
                    .ok()?;
                    proxy.call_method("Pause", &()).ok()?;
                    Some(true)
                })
                .unwrap_or(false);
            let _ = tx.send(result);
        })
        .is_err()
    {
        return false;
    }
    rx.recv_timeout(DEADLINE).unwrap_or(false)
}

/// Pause one exact player only while it still owns the URI that this handoff
/// opened. This prevents compensation for a failed acknowledgement from
/// pausing unrelated playback that started immediately afterwards.
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
    let (tx, rx) = mpsc::channel();
    if std::thread::Builder::new()
        .name("tilo-mpris-compensate".to_string())
        .spawn(move || {
            let result = zbus::blocking::Connection::session()
                .ok()
                .and_then(|connection| {
                    let proxy = zbus::blocking::Proxy::new(
                        &connection,
                        owned.as_str(),
                        MPRIS_PATH,
                        PLAYER_IFACE,
                    )
                    .ok()?;
                    if !player_reports_uri(&proxy, &expected) {
                        return None;
                    }
                    proxy.call_method("Pause", &()).ok()?;
                    Some(true)
                })
                .unwrap_or(false);
            let _ = tx.send(result);
        })
        .is_err()
    {
        return false;
    }
    rx.recv_timeout(DEADLINE).unwrap_or(false)
}

fn run(cmd: Command) -> Option<Outcome> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| {
            tracing::debug!(target: "tilo::mpris", error = %e, "no session bus");
        })
        .ok()?;

    let name = active_player(&conn)?;
    let proxy = zbus::blocking::Proxy::new(&conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE).ok()?;

    // Read status *before* acting: reading after would race the player's
    // own (asynchronous) state change.
    let was_playing = matches!(playback_status(&proxy).as_deref(), Some("Playing"));

    proxy
        .call_method(cmd.method(), &())
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
    })
}

/// A machine can have several players; the one reporting `Playing` wins,
/// else the first MPRIS name on the bus, so play/pause resumes whatever
/// was last open.
fn active_player(conn: &zbus::blocking::Connection) -> Option<String> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let names = dbus.list_names().ok()?;
    let players: Vec<String> = names
        .into_iter()
        .map(|n| n.as_str().to_string())
        .filter(|n| n.starts_with(MPRIS_PREFIX))
        .collect();
    if players.is_empty() {
        return None;
    }
    for name in &players {
        let Ok(proxy) = zbus::blocking::Proxy::new(conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE)
        else {
            continue;
        };
        if matches!(playback_status(&proxy).as_deref(), Some("Playing")) {
            return Some(name.clone());
        }
    }
    players.into_iter().next()
}

fn playback_status(proxy: &zbus::blocking::Proxy<'_>) -> Option<String> {
    proxy.get_property::<String>("PlaybackStatus").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn text(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(value.to_string())).unwrap()
    }

    fn number(value: i64) -> OwnedValue {
        OwnedValue::try_from(Value::from(value)).unwrap()
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
    fn commands_map_to_the_spec_method_names() {
        // Wire names in the MPRIS spec, not ours to rename.
        assert_eq!(Command::PlayPause.method(), "PlayPause");
        assert_eq!(Command::Pause.method(), "Pause");
        assert_eq!(Command::Next.method(), "Next");
        assert_eq!(Command::Previous.method(), "Previous");
    }

    #[test]
    fn the_well_known_names_match_the_spec() {
        assert_eq!(MPRIS_PREFIX, "org.mpris.MediaPlayer2.");
        assert_eq!(MPRIS_PATH, "/org/mpris/MediaPlayer2");
        assert_eq!(PLAYER_IFACE, "org.mpris.MediaPlayer2.Player");
    }

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
    fn cancelled_resume_cannot_start_or_publish_a_late_acceptance() {
        let request_id = [21; 16];
        let now = Instant::now();
        let mut resumes = ResumeState::default();

        assert!(!resumes.cancel(request_id, now));
        assert!(resumes.begin(request_id, now).is_none());
        assert!(resumes.take_accepted().is_empty());
    }

    #[test]
    fn cancellation_after_worker_launch_blocks_every_later_mutation() {
        let request_id = [22; 16];
        let now = Instant::now();
        let mut resumes = ResumeState::default();
        let authority = resumes.begin(request_id, now).unwrap();
        let mut mutations = Vec::new();

        assert_eq!(authority.mutate(|| mutations.push("open-uri")), Some(()));
        assert!(resumes.cancel(request_id, now));
        assert_eq!(authority.mutate(|| mutations.push("seek")), None);
        assert_eq!(authority.mutate(|| mutations.push("play")), None);
        resumes.finish(&authority, true);

        assert_eq!(mutations, ["open-uri"]);
        assert!(resumes.take_accepted().is_empty());
    }

    #[test]
    fn cancellation_is_linearized_after_an_in_flight_mutation() {
        let request_id = [24; 16];
        let now = Instant::now();
        let resumes = Arc::new(Mutex::new(ResumeState::default()));
        let authority = resumes.lock().unwrap().begin(request_id, now).unwrap();
        let mutation_authority = authority.clone();
        let (mutation_started_tx, mutation_started_rx) = mpsc::channel();
        let (release_mutation_tx, release_mutation_rx) = mpsc::channel();
        let mutation = std::thread::spawn(move || {
            mutation_authority.mutate(|| {
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
        assert!(cancelled_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        cancellation.join().unwrap();
        assert_eq!(authority.mutate(|| panic!("late mutation ran")), None);
        resumes.lock().unwrap().finish(&authority, true);
        assert!(resumes.lock().unwrap().take_accepted().is_empty());
    }

    #[test]
    fn cancellation_retracts_completion_and_tombstones_are_bounded() {
        let request_id = [23; 16];
        let now = Instant::now();
        let mut resumes = ResumeState::default();
        let authority = resumes.begin(request_id, now).unwrap();
        resumes.finish(&authority, true);
        assert!(resumes.cancel(request_id, now));
        assert!(resumes.take_accepted().is_empty());

        for index in 0..=MAX_CANCELLED_RESUMES {
            let mut id = [0; 16];
            id[..8].copy_from_slice(&u64::try_from(index).unwrap().to_be_bytes());
            id[15] = 1;
            let _ = resumes.cancel(id, now);
        }
        assert_eq!(resumes.cancelled.len(), MAX_CANCELLED_RESUMES);
    }

    #[test]
    fn ambiguous_or_stopped_players_are_not_advertised() {
        let observed = observation("https://media.example/film.mp4");
        assert!(report_from_observation(&observed, &[browser(), browser()]).is_none());

        let mut stopped = observed;
        stopped.status = "Stopped".into();
        assert!(report_from_observation(&stopped, &[browser()]).is_none());
    }

    /// Must survive a machine with no session bus and no player — which
    /// is exactly what CI is.
    #[test]
    fn send_is_bounded_and_reports_nothing_when_there_is_no_player() {
        let started = std::time::Instant::now();
        let outcome = send(Command::PlayPause);
        let elapsed = started.elapsed();
        assert!(
            elapsed < DEADLINE * 4,
            "the input path must not wait on the bus: took {elapsed:?}"
        );
        // A dev machine with a player running legitimately returns Some; on
        // a machine with none (CI), `outcome` is `None` — either is correct,
        // the assertion above is the only hard requirement.
        let _ = outcome;
    }
}
