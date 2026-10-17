//! One signal-driven observer of the session's MPRIS players.
//!
//! Started once on the session runtime, the observer subscribes to the bus's
//! `NameOwnerChanged` for MPRIS names and to every player's
//! `PropertiesChanged` and `Seeked`, then seeds its [`PlayerTable`] with a
//! single `ListNames`. From then on it calls the bus only when something
//! happens: a player appears (`GetConnectionUnixProcessID` and one `GetAll`
//! per interface), a player invalidates properties (`GetAll` again), or a
//! status, rate or track change leaves the extrapolated playhead stale (one
//! `Get` of `Position`). An idle desktop makes no MPRIS calls at all.
//!
//! Readers on the compositor thread only copy the projections published
//! here, under a short lock; no bus call ever runs on that thread.

use std::collections::{BTreeSet, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use zbus::zvariant::OwnedValue;

use super::table::{PlayerEvent, PlayerTable};
use super::{
    InstalledMediaApp, MAX_NOW_PLAYING_PLAYERS, MPRIS_PATH, MPRIS_PREFIX, PLAYER_IFACE, ROOT_IFACE,
};
use crate::shell::now_playing::{Artwork, PlayerSnapshot};

const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// The longest the observer waits on one player. A player that does not
/// answer in time is skipped when it appears, or keeps its last state when
/// re-read; it never holds up the signals of every other player for long.
const PLAYER_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// Signals buffered per subscription while the observer waits on a player.
const MAX_QUEUED_SIGNALS: usize = 256;

/// Largest cover file the card will decode, and the edge it is scaled to.
const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;
const ARTWORK_EDGE: u32 = 192;

/// Whether the table describes the bus yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Seed {
    /// No observer is running.
    Stopped,
    /// An observer is subscribing and listing the bus.
    Pending,
    /// The table holds every player and follows their signals.
    Ready,
}

/// The observer's table and everything published from it.
pub(super) struct ObserverState {
    running: bool,
    seed: tokio::sync::watch::Sender<Seed>,
    pub(super) table: PlayerTable,
    /// The installed apps a handoff report is matched against, read on the
    /// I/O pool when the observer starts and whenever a player appears.
    apps: Vec<InstalledMediaApp>,
    /// Whether this machine's players are published as handoff reports;
    /// only while media continuity is permitted.
    handoff: bool,
    /// The now-playing card's players, artwork resolved.
    pub(super) now_playing: Vec<PlayerSnapshot>,
    /// Decoded artwork by `mpris:artUrl`, so a track's cover is decoded once.
    /// Pruned to the current players when it grows.
    artwork: HashMap<String, Option<Artwork>>,
    /// Artwork being decoded right now, so each URL is decoded at most once.
    artwork_pending: BTreeSet<String>,
}

impl Default for ObserverState {
    fn default() -> Self {
        Self {
            running: false,
            seed: tokio::sync::watch::Sender::new(Seed::Stopped),
            table: PlayerTable::default(),
            apps: Vec::new(),
            handoff: false,
            now_playing: Vec::new(),
            artwork: HashMap::new(),
            artwork_pending: BTreeSet::new(),
        }
    }
}

impl ObserverState {
    /// Recompute every projection from the table. Returns the artwork URLs
    /// still to decode.
    fn publish(&mut self, now: Instant) -> Vec<String> {
        let observed = self.table.now_playing();
        let wanted = observed
            .iter()
            .filter_map(|(_, url)| url.clone())
            .collect::<BTreeSet<_>>();
        let mut missing = Vec::new();
        self.now_playing = observed
            .into_iter()
            .map(|(mut player, url)| {
                if let Some(url) = url {
                    match self.artwork.get(&url) {
                        Some(artwork) => player.artwork.clone_from(artwork),
                        None => {
                            if self.artwork_pending.insert(url.clone()) {
                                missing.push(url);
                            }
                        }
                    }
                }
                player
            })
            .collect();
        if self.artwork.len() > MAX_NOW_PLAYING_PLAYERS {
            self.artwork.retain(|url, _| wanted.contains(url));
        }
        self.publish_handoff(now);
        super::now_playing_changed();
        missing
    }

    fn publish_handoff(&self, now: Instant) {
        if self.handoff {
            media::handoff::replace_system_reports(self.table.handoff_reports(&self.apps, now));
        }
    }
}

/// The session's one observer. Replaces the separate pollers the handoff
/// reports and the now-playing card each ran.
pub(super) static OBSERVER: LazyLock<Mutex<ObserverState>> =
    LazyLock::new(|| Mutex::new(ObserverState::default()));

/// Start the observer on the session runtime unless it is running. Never
/// waits on the bus. An observer that loses its bus stops, and the next call
/// starts another.
pub(super) fn ensure_started() {
    let Ok(mut state) = OBSERVER.lock() else {
        return;
    };
    if state.running {
        return;
    }
    state.running = true;
    state.seed.send_replace(Seed::Pending);
    drop(state);
    let spawned = super::spawn_on_session(async {
        let _stopped = Stopped(&OBSERVER);
        let Some(connection) = super::session_bus().await else {
            return;
        };
        match zbus::Connection::session().await {
            Ok(listener) => observe(&connection, &listener, &OBSERVER).await,
            Err(error) => {
                tracing::debug!(target: "tilo::mpris", %error, "cannot follow MPRIS players");
            }
        }
    });
    if !spawned {
        drop(Stopped(&OBSERVER));
    }
}

/// Clears the table and its projections however an observer ends, so a
/// stopped observer never leaves phantom players behind.
struct Stopped(&'static Mutex<ObserverState>);

impl Drop for Stopped {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.running = false;
            state.table = PlayerTable::default();
            state.publish(Instant::now());
            state.seed.send_replace(Seed::Stopped);
        }
    }
}

/// Wait up to `wait` for `state`'s observer to have seeded its table. `false`
/// when it has no bus or is not running.
pub(super) async fn seeded(state: &'static Mutex<ObserverState>, wait: Duration) -> bool {
    let Ok(mut seed) = state.lock().map(|state| state.seed.subscribe()) else {
        return false;
    };
    tokio::time::timeout(wait, seed.wait_for(|seed| *seed != Seed::Pending))
        .await
        .is_ok_and(|seed| seed.is_ok_and(|seed| *seed == Seed::Ready))
}

/// Publish (or, with `enabled` false, withdraw) this machine's players as
/// handoff reports. Called once a second while enabled, which also keeps
/// them inside `media`'s freshness window with playheads extrapolated to
/// now. No bus call.
pub(super) fn set_handoff(enabled: bool) {
    let Ok(mut state) = OBSERVER.lock() else {
        return;
    };
    let was = std::mem::replace(&mut state.handoff, enabled);
    if enabled {
        state.publish_handoff(Instant::now());
    } else if was {
        media::handoff::replace_system_reports(Vec::new());
    }
}

/// Recompute the projections from the table as it stands, dropping any
/// optimistic edit a reader made that no signal confirmed.
pub(super) fn republish(state: &'static Mutex<ObserverState>) {
    let missing = state
        .lock()
        .map_or_else(|_| Vec::new(), |mut state| state.publish(Instant::now()));
    decode_artwork(state, missing);
}

fn apply(state: &'static Mutex<ObserverState>, event: PlayerEvent) -> super::table::Applied {
    let now = Instant::now();
    let (applied, missing) = state.lock().map_or_else(
        |_| Default::default(),
        |mut state| {
            let applied = state.table.apply(event, now);
            let missing = if applied.changed {
                state.publish(now)
            } else {
                Vec::new()
            };
            (applied, missing)
        },
    );
    decode_artwork(state, missing);
    applied
}

/// Try again to attribute players whose owner could not be attributed to a
/// launch when it appeared — all of them, or with `only`, just that
/// connection's. A bounded walk of `/proc` from the process recorded at
/// appearance; no bus call. Until it succeeds, a player is shown on the card
/// but never offered for handoff.
pub(super) fn retry_verification(state: &'static Mutex<ObserverState>, only: Option<&str>) {
    let unverified = state
        .lock()
        .map_or_else(|_| Vec::new(), |state| state.table.unverified());
    let verified = unverified
        .into_iter()
        .filter(|(_, unique_name, _)| only.is_none_or(|only| only == unique_name))
        .filter_map(|(bus_name, unique_name, pid)| {
            crate::shell::launcher::verified_package_for_descendant(pid)
                .map(|package_id| (bus_name, unique_name, package_id))
        })
        .collect::<Vec<_>>();
    if verified.is_empty() {
        return;
    }
    let missing = state.lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            for (bus_name, unique_name, package_id) in verified {
                state.table.verify(&bus_name, &unique_name, package_id);
            }
            state.publish(Instant::now())
        },
    );
    decode_artwork(state, missing);
}

/// The three subscriptions, made before the bus is listed so nothing that
/// happens during seeding is missed.
struct Signals {
    owners: zbus::MessageStream,
    properties: zbus::MessageStream,
    seeked: zbus::MessageStream,
}

impl Signals {
    async fn subscribe(connection: &zbus::Connection) -> zbus::Result<Self> {
        let signal = || zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal);
        let owners = signal()
            .sender(BUS_NAME)?
            .interface(BUS_NAME)?
            .member("NameOwnerChanged")?
            .path(BUS_PATH)?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build();
        let properties = signal()
            .interface(PROPERTIES_IFACE)?
            .member("PropertiesChanged")?
            .path(MPRIS_PATH)?
            .build();
        let seeked = signal()
            .interface(PLAYER_IFACE)?
            .member("Seeked")?
            .path(MPRIS_PATH)?
            .build();
        let stream =
            |rule| zbus::MessageStream::for_match_rule(rule, connection, Some(MAX_QUEUED_SIGNALS));
        Ok(Self {
            owners: stream(owners).await?,
            properties: stream(properties).await?,
            seeked: stream(seeked).await?,
        })
    }
}

/// Follow every MPRIS player on the bus into `state` until a connection
/// closes.
///
/// Signals arrive on `listener`, a connection of their own; every call goes
/// out on `connection`. zbus stops reading a connection while one of its
/// signal queues is full, so a player flooding signals while the observer
/// waits on a call must only ever hold up the listener, never the replies
/// the observer, the transport keys and the card's controls wait for.
pub(super) async fn observe(
    connection: &zbus::Connection,
    listener: &zbus::Connection,
    state: &'static Mutex<ObserverState>,
) {
    let mut signals = match Signals::subscribe(listener).await {
        Ok(signals) => signals,
        Err(error) => {
            tracing::debug!(target: "tilo::mpris", %error, "cannot follow MPRIS players");
            return;
        }
    };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(connection).await else {
        return;
    };
    let Ok(names) = dbus.list_names().await else {
        return;
    };
    reload_apps(state).await;
    for name in names {
        if !is_player_name(name.as_str()) {
            continue;
        }
        if !state
            .lock()
            .is_ok_and(|state| state.table.admits(name.as_str()))
        {
            break;
        }
        let Ok(owner) = dbus
            .get_name_owner(zbus::names::BusName::from(name.clone()))
            .await
        else {
            continue;
        };
        if let Some(event) =
            observe_owner(connection, &dbus, name.to_string(), owner.to_string()).await
        {
            apply(state, event);
        }
    }
    if let Ok(state) = state.lock() {
        state.seed.send_replace(Seed::Ready);
    }
    loop {
        tokio::select! {
            message = signals.owners.next() => {
                let Some(message) = message else { break };
                if let Ok(message) = message {
                    on_owner_changed(connection, &dbus, state, &message).await;
                }
            }
            message = signals.properties.next() => {
                let Some(message) = message else { break };
                if let Ok(message) = message {
                    on_properties_changed(connection, state, &message).await;
                }
            }
            message = signals.seeked.next() => {
                let Some(message) = message else { break };
                if let Ok(message) = message {
                    on_seeked(state, &message);
                }
            }
        }
    }
}

fn is_player_name(name: &str) -> bool {
    name.starts_with(MPRIS_PREFIX) && name.len() <= 256
}

async fn on_owner_changed(
    connection: &zbus::Connection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    state: &'static Mutex<ObserverState>,
    message: &zbus::Message,
) {
    let Ok((name, _old_owner, new_owner)) =
        message.body().deserialize::<(String, String, String)>()
    else {
        return;
    };
    if !is_player_name(&name) {
        return;
    }
    if new_owner.is_empty() {
        apply(state, PlayerEvent::Vanished { bus_name: name });
        return;
    }
    if !state.lock().is_ok_and(|state| state.table.admits(&name)) {
        return;
    }
    match observe_owner(connection, dbus, name.clone(), new_owner).await {
        Some(event) => {
            reload_apps(state).await;
            apply(state, event);
        }
        // An owner that does not answer is not a player the shell can show
        // or drive; whatever the name described before is gone.
        None => {
            apply(state, PlayerEvent::Vanished { bus_name: name });
        }
    }
}

async fn on_properties_changed(
    connection: &zbus::Connection,
    state: &'static Mutex<ObserverState>,
    message: &zbus::Message,
) {
    let Some(unique_name) = message.header().sender().map(ToString::to_string) else {
        return;
    };
    let Ok((interface, changed, invalidated)) =
        message
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    else {
        return;
    };
    if interface != PLAYER_IFACE && interface != ROOT_IFACE {
        return;
    }
    let reread = !invalidated.is_empty();
    let applied = apply(
        state,
        PlayerEvent::Changed {
            unique_name: unique_name.clone(),
            changed,
            invalidated,
        },
    );
    if !applied.changed {
        return;
    }
    // A player that changes state is the moment its launch is most likely
    // recorded, if it was not when the player appeared.
    retry_verification(state, Some(&unique_name));
    if reread && let Some(changed) = get_all(connection, &unique_name, &interface).await {
        apply(
            state,
            PlayerEvent::Changed {
                unique_name: unique_name.clone(),
                changed,
                invalidated: Vec::new(),
            },
        );
    }
    if applied.position_stale
        && let Some(position_us) = get_position(connection, &unique_name).await
    {
        apply(
            state,
            PlayerEvent::Seeked {
                unique_name,
                position_us,
            },
        );
    }
}

fn on_seeked(state: &'static Mutex<ObserverState>, message: &zbus::Message) {
    let Some(unique_name) = message.header().sender().map(ToString::to_string) else {
        return;
    };
    let Ok(position_us) = message.body().deserialize::<i64>() else {
        return;
    };
    apply(
        state,
        PlayerEvent::Seeked {
            unique_name,
            position_us,
        },
    );
}

/// Everything about a new owner of `bus_name`: who launched it, verified
/// once for this owner, and every property of both interfaces.
async fn observe_owner(
    connection: &zbus::Connection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    bus_name: String,
    unique_name: String,
) -> Option<PlayerEvent> {
    let owner = zbus::names::BusName::try_from(unique_name.as_str()).ok()?;
    let pid = bounded(dbus.get_connection_unix_process_id(owner)).await;
    let package_id = pid.and_then(crate::shell::launcher::verified_package_for_descendant);
    let mut properties = get_all(connection, &unique_name, ROOT_IFACE)
        .await
        .unwrap_or_default();
    properties.extend(get_all(connection, &unique_name, PLAYER_IFACE).await?);
    Some(PlayerEvent::Appeared {
        bus_name,
        unique_name,
        pid,
        package_id,
        properties,
    })
}

async fn bounded<T, E>(call: impl Future<Output = Result<T, E>> + Send) -> Option<T>
where
    T: Send,
    E: Send,
{
    tokio::time::timeout(PLAYER_CALL_TIMEOUT, call)
        .await
        .ok()?
        .ok()
}

/// One `GetAll`, addressed to the owner itself so an answer can never
/// describe a later owner of the name.
async fn get_all(
    connection: &zbus::Connection,
    unique_name: &str,
    interface: &str,
) -> Option<HashMap<String, OwnedValue>> {
    let reply = bounded(connection.call_method(
        Some(unique_name),
        MPRIS_PATH,
        Some(PROPERTIES_IFACE),
        "GetAll",
        &(interface,),
    ))
    .await?;
    reply.body().deserialize().ok()
}

async fn get_position(connection: &zbus::Connection, unique_name: &str) -> Option<i64> {
    let reply = bounded(connection.call_method(
        Some(unique_name),
        MPRIS_PATH,
        Some(PROPERTIES_IFACE),
        "Get",
        &(PLAYER_IFACE, "Position"),
    ))
    .await?;
    let position = reply.body().deserialize::<OwnedValue>().ok()?;
    i64::try_from(position).ok()
}

/// Read the installed apps a handoff report is matched against. The package
/// registry is on disk; that happens on the I/O pool.
async fn reload_apps(state: &'static Mutex<ObserverState>) {
    let apps =
        tilo_rt::io::run(|| Ok(crate::shell::launcher::installed_apps().unwrap_or_default()))
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|app| app.has_gui)
            .map(|app| InstalledMediaApp {
                package_id: app.package_id,
                uri_schemes: app.opens_uri_schemes,
                extensions: app.opens_extensions,
            })
            .collect::<Vec<_>>();
    if let Ok(mut state) = state.lock() {
        state.apps = apps;
    }
}

/// Decode each newly named cover on the I/O pool, then publish it. A missing,
/// oversized or undecodable file is remembered as having no artwork so it is
/// not tried again.
fn decode_artwork(state: &'static Mutex<ObserverState>, urls: Vec<String>) {
    for url in urls {
        let pending = url.clone();
        let spawned = super::spawn_on_session(async move {
            let path = url
                .strip_prefix("file://")
                .and_then(|path| {
                    super::percent_decode(path.split(['?', '#']).next().unwrap_or(path))
                })
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute());
            let decoded = match path {
                Some(path) => tilo_rt::io::run(move || Ok(decode_artwork_file(&path)))
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            let missing = state.lock().map_or_else(
                |_| Vec::new(),
                |mut state| {
                    state.artwork_pending.remove(&url);
                    state.artwork.insert(url, decoded);
                    state.publish(Instant::now())
                },
            );
            decode_artwork(state, missing);
        });
        if !spawned && let Ok(mut state) = state.lock() {
            state.artwork_pending.remove(&pending);
        }
    }
}

fn decode_artwork_file(path: &std::path::Path) -> Option<Artwork> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .ok()?
        .thumbnail(ARTWORK_EDGE, ARTWORK_EDGE)
        .to_rgba8();
    Some(Artwork {
        width: image.width(),
        height: image.height(),
        rgba: std::sync::Arc::new(image.into_raw()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::now_playing::PlaybackStatus;
    use std::sync::Arc;
    use zbus::message::Type as MessageType;

    const PROBE: &str = "org.mpris.MediaPlayer2.probe";

    /// A session bus of its own, as `dbus-run-session` starts one, stopped
    /// on drop. The observer under test never sees the caller's real bus.
    struct PrivateBus {
        daemon: std::process::Child,
        address: String,
        _directory: tempfile::TempDir,
    }

    impl PrivateBus {
        fn start() -> Self {
            use std::io::BufRead;
            let directory = tempfile::tempdir().unwrap();
            let mut daemon = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .arg(format!(
                    "--address=unix:path={}",
                    directory.path().join("bus").display()
                ))
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("the session bus needs dbus-daemon, which the builder image installs");
            let mut address = String::new();
            std::io::BufReader::new(daemon.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
            Self {
                daemon,
                address: address.trim().to_owned(),
                _directory: directory,
            }
        }

        async fn connect(&self) -> zbus::Connection {
            zbus::connection::Builder::address(self.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap()
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    struct FakeRoot {
        identity: String,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl FakeRoot {
        #[zbus(property)]
        fn identity(&self) -> String {
            self.identity.clone()
        }
    }

    /// A player that reports what its fields say, and signals only when the
    /// test makes it.
    struct FakePlayer {
        status: String,
        title: String,
        position_us: i64,
        controllable: bool,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl FakePlayer {
        #[zbus(property)]
        fn playback_status(&self) -> String {
            self.status.clone()
        }

        #[zbus(property)]
        fn position(&self) -> i64 {
            self.position_us
        }

        #[zbus(property)]
        fn can_play(&self) -> bool {
            self.controllable
        }

        #[zbus(property)]
        fn can_pause(&self) -> bool {
            self.controllable
        }

        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            HashMap::from([(
                "xesam:title".to_string(),
                OwnedValue::try_from(zbus::zvariant::Value::from(self.title.clone())).unwrap(),
            )])
        }
    }

    /// Every method call `sender` makes on the bus, in order, as
    /// `dbus-monitor` would see them.
    async fn record_calls(bus: &PrivateBus, sender: &str) -> Arc<Mutex<Vec<String>>> {
        let monitor = bus.connect().await;
        let mut messages = zbus::MessageStream::from(&monitor);
        let rule = zbus::MatchRule::builder()
            .msg_type(MessageType::MethodCall)
            .sender(sender.to_owned())
            .unwrap()
            .build();
        zbus::fdo::MonitoringProxy::new(&monitor)
            .await
            .unwrap()
            .become_monitor(&[rule], 0)
            .await
            .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        drop(tokio::spawn(async move {
            let _monitor = monitor;
            while let Some(Ok(message)) = messages.next().await {
                if message.message_type() == MessageType::MethodCall
                    && let Some(member) = message.header().member()
                {
                    recorded.lock().unwrap().push(member.to_string());
                }
            }
        }));
        calls
    }

    async fn eventually(what: &str, mut condition: impl FnMut() -> bool + Send) {
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .unwrap();
        while !condition() {
            assert!(Instant::now() < deadline, "{what} never happened");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The probe player on `bus`, playing "Song" at 12 s.
    async fn serve_playing_probe(bus: &PrivateBus) -> zbus::Connection {
        zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(PROBE)
            .unwrap()
            .serve_at(
                MPRIS_PATH,
                FakeRoot {
                    identity: "Probe".into(),
                },
            )
            .unwrap()
            .serve_at(
                MPRIS_PATH,
                FakePlayer {
                    status: "Playing".into(),
                    title: "Song".into(),
                    position_us: 12_000_000,
                    controllable: true,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// Pause the probe player and announce it the way MPRIS players do: one
    /// `PropertiesChanged`, no `Position`.
    async fn pause_and_signal(player: &zbus::Connection) {
        let player_iface = player
            .object_server()
            .interface::<_, FakePlayer>(MPRIS_PATH)
            .await
            .unwrap();
        player_iface.get_mut().await.status = "Paused".into();
        player_iface
            .get()
            .await
            .playback_status_changed(player_iface.signal_emitter())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signals_update_players_and_an_idle_player_costs_no_calls() {
        let bus = PrivateBus::start();
        let connection = bus.connect().await;
        let calls = record_calls(&bus, connection.unique_name().unwrap().as_str()).await;
        let player = serve_playing_probe(&bus).await;

        let state: &'static Mutex<ObserverState> = Box::leak(Box::default());
        state.lock().unwrap().seed.send_replace(Seed::Pending);
        let listener = bus.connect().await;
        let observer = tokio::spawn(async move { observe(&connection, &listener, state).await });
        assert!(
            seeded(state, tilo_test_env::hang_bound()).await,
            "the observer lists the bus"
        );
        let seen = state.lock().unwrap().now_playing.clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].bus_name, PROBE);
        assert_eq!(seen[0].identity, "Probe");
        assert_eq!(seen[0].title.as_deref(), Some("Song"));
        assert_eq!(seen[0].status, PlaybackStatus::Playing);
        assert_eq!(seen[0].position_us, 12_000_000);
        assert!(seen[0].can_toggle());

        // The monitor sees the start-up: one listing, then the player's two
        // interfaces read with one `GetAll` each.
        eventually("the start-up calls reaching the monitor", || {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|member| *member == "GetAll")
                .count()
                == 2
        })
        .await;
        let startup = calls.lock().unwrap().clone();
        assert_eq!(
            startup
                .iter()
                .filter(|member| *member == "ListNames")
                .count(),
            1,
            "{startup:?}"
        );
        assert!(
            !startup.iter().any(|member| member == "Get"),
            "no property is read one at a time: {startup:?}"
        );

        // A signal, not a poll, moves the card.
        pause_and_signal(&player).await;
        eventually("the paused status reaching the card", || {
            state
                .lock()
                .unwrap()
                .now_playing
                .first()
                .is_some_and(|player| player.status == PlaybackStatus::Paused)
        })
        .await;

        // An idle player costs nothing: the only call after start-up is the
        // one fresh playhead the status change made stale.
        eventually("the playhead re-read", || {
            calls.lock().unwrap().len() > startup.len()
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            calls.lock().unwrap().get(startup.len()..),
            Some(&["Get".to_string()][..]),
            "after start-up the observer only re-reads the playhead"
        );

        // Leaving is a signal too.
        player.release_name(PROBE).await.unwrap();
        eventually("the player leaving the card", || {
            state.lock().unwrap().now_playing.is_empty()
        })
        .await;
        assert_eq!(
            calls.lock().unwrap().get(startup.len()..),
            Some(&["Get".to_string()][..]),
            "a player leaving costs no call"
        );
        observer.abort();
    }
}
