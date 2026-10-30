//! The session's one MPRIS client: following every media player on the
//! session bus and driving them.
//!
//! The transport keys (play/pause, next, previous) address *an
//! application*, not the machine, so they go over `org.mpris.MediaPlayer2`
//! on the session bus — the interface every real player and Tilo's own
//! Music app implements. So do the now-playing controls, a cross-device
//! handoff's resume and the pause an unplugged headset asks for.
//!
//! Which players exist, and what each reports, comes from one signal-driven
//! observer ([`observer`]) and the table it keeps ([`table`]); nothing here
//! polls the bus. Every change is published as [`Event::Players`]. Commands
//! go to a player directly, and a resume still reads a player's `Position`
//! while it verifies the playhead, because MPRIS has no signal for a
//! playhead that merely advances.

mod observer;
mod resume;
mod table;
#[cfg(test)]
pub(crate) mod test_bus;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::{
    ArtworkImage, Event, MAX_PLAYERS, Outcome, PlayerCommand, Request, Transport, handoff,
};
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

/// How long after a now-playing command the players are published again: a
/// player that took the command has signalled by then, so the compositor
/// drops its optimistic state for what the player really did.
const COMMAND_SETTLE: Duration = Duration::from_millis(500);

/// The longest one player call may take. A player that does not answer in
/// time is skipped; it never holds up every other player for long.
const PLAYER_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest cover file decoded, and the edge it is scaled to.
const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;
const ARTWORK_EDGE: u32 = 192;

/// Whether the table describes the bus yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seed {
    /// No observer is running.
    Stopped,
    /// An observer is subscribing and listing the bus.
    Pending,
    /// The table holds every player and follows their signals.
    Ready,
}

struct State {
    running: bool,
    table: PlayerTable,
    /// Covers decoded or being decoded, by `mpris:artUrl`, so each is
    /// decoded and sent once. Pruned to the current players when it grows.
    artwork: BTreeSet<String>,
}

type Emit = Arc<dyn Fn(Event) + Send + Sync>;

struct Shared {
    state: Mutex<State>,
    seed: tokio::sync::watch::Sender<Seed>,
    /// The bus to use: the session's, or a test's own.
    address: Option<String>,
    /// Calls go out on this one connection, made once.
    calls: tokio::sync::OnceCell<zbus::Connection>,
    resumes: resume::Resumes,
    emit: Emit,
    runtime: tokio::runtime::Handle,
}

/// The MPRIS client. Cheap to clone; every clone drives the same players.
#[derive(Clone)]
pub struct Mpris(Arc<Shared>);

impl Mpris {
    /// Follow the session bus's players on `runtime`, telling `emit` every
    /// change and every answer.
    pub fn start(
        runtime: tokio::runtime::Handle,
        emit: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        Self::start_on(runtime, None, emit)
    }

    /// [`Self::start`] on the bus at `address` instead of the session's.
    pub fn start_on(
        runtime: tokio::runtime::Handle,
        address: Option<String>,
        emit: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        Self::with_calls(runtime, address, None, emit)
    }

    /// [`Self::start_on`], its calls going out on `calls` when given.
    fn with_calls(
        runtime: tokio::runtime::Handle,
        address: Option<String>,
        calls: Option<zbus::Connection>,
        emit: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        let mpris = Self(Arc::new(Shared {
            state: Mutex::new(State {
                running: false,
                table: PlayerTable::default(),
                artwork: BTreeSet::new(),
            }),
            seed: tokio::sync::watch::Sender::new(Seed::Stopped),
            address,
            calls: tokio::sync::OnceCell::new_with(calls),
            resumes: resume::Resumes::default(),
            emit: Arc::new(emit),
            runtime,
        }));
        mpris.ensure_started();
        mpris
    }

    /// Carry out one request from the compositor. Never waits on the bus,
    /// except that revoking a resume waits out a player call already made
    /// for it.
    pub fn handle(&self, request: Request) {
        self.ensure_started();
        match request {
            Request::Transport { id, command } => self.transport(id, command),
            Request::Control { bus_name, command } => self.control(bus_name, command),
            Request::Pause { bus_name, if_uri } => self.pause(bus_name, if_uri),
            Request::Resume { request } => self.resume(request),
            Request::CancelResume { request_id } => self.0.resumes.cancel(request_id),
            Request::Verified {
                bus_name,
                unique_name,
                package_id,
            } => {
                let changed =
                    self.0.state.lock().is_ok_and(|mut state| {
                        state.table.verify(&bus_name, &unique_name, package_id)
                    });
                if changed {
                    self.publish();
                }
            }
        }
    }

    /// Pause every player playing now. `false` when there is no bus to ask.
    pub fn pause_playing(&self) -> bool {
        self.ensure_started();
        let mpris = self.clone();
        self.spawn(async move {
            let Some(playing) = mpris.with_table(PlayerTable::playing).await else {
                return;
            };
            for bus_name in playing {
                mpris.call(&bus_name, "Pause", &()).await;
            }
        });
        self.0.address.is_some() || std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
    }

    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        drop(self.0.runtime.spawn(task));
    }

    async fn connect(&self) -> zbus::Result<zbus::Connection> {
        match &self.0.address {
            Some(address) => {
                zbus::connection::Builder::address(address.as_str())?
                    .build()
                    .await
            }
            None => zbus::Connection::session().await,
        }
    }

    /// The connection calls go out on. `None` when there is no bus; the
    /// next call tries again.
    async fn bus(&self) -> Option<zbus::Connection> {
        self.0
            .calls
            .get_or_try_init(|| self.connect())
            .await
            .map_err(|error| tracing::debug!(target: "tilo::mpris", %error, "no session bus"))
            .ok()
            .cloned()
    }

    /// Start the observer unless it is running. An observer that loses its
    /// bus stops, and the next request starts another.
    fn ensure_started(&self) {
        let Ok(mut state) = self.0.state.lock() else {
            return;
        };
        if std::mem::replace(&mut state.running, true) {
            return;
        }
        drop(state);
        self.0.seed.send_replace(Seed::Pending);
        let mpris = self.clone();
        self.spawn(async move {
            if let (Some(connection), Ok(listener)) = (mpris.bus().await, mpris.connect().await) {
                observer::observe(&mpris, &connection, &listener).await;
            }
            if let Ok(mut state) = mpris.0.state.lock() {
                state.running = false;
                state.table = PlayerTable::default();
            }
            mpris.publish();
            mpris.0.seed.send_replace(Seed::Stopped);
        });
    }

    /// Read the table once the observer has listed the bus. `None` when it
    /// was not listed within [`SEED_WAIT`].
    async fn with_table<T>(&self, read: impl FnOnce(&PlayerTable) -> T) -> Option<T> {
        let mut seed = self.0.seed.subscribe();
        let ready = tokio::time::timeout(SEED_WAIT, seed.wait_for(|seed| *seed != Seed::Pending))
            .await
            .is_ok_and(|seed| seed.is_ok_and(|seed| *seed == Seed::Ready));
        if !ready {
            return None;
        }
        self.0.state.lock().ok().map(|state| read(&state.table))
    }

    /// Tell the compositor every player as it stands, and decode any cover
    /// not decoded yet.
    fn publish(&self) {
        let Ok(mut state) = self.0.state.lock() else {
            return;
        };
        let players = state.table.players(Instant::now());
        let wanted = players
            .iter()
            .filter_map(|player| player.art_url.clone())
            .collect::<BTreeSet<_>>();
        if state.artwork.len() > MAX_PLAYERS {
            state.artwork.retain(|url| wanted.contains(url));
        }
        let missing = wanted
            .into_iter()
            .filter(|url| state.artwork.insert(url.clone()))
            .collect::<Vec<_>>();
        drop(state);
        (self.0.emit)(Event::Players { players });
        for url in missing {
            let emit = Arc::clone(&self.0.emit);
            drop(self.0.runtime.spawn_blocking(move || {
                let image = decode_artwork(&url);
                emit(Event::Artwork { url, image });
            }));
        }
    }

    /// Call `method` on exactly `bus_name`'s player, bounded. Whether the
    /// player took it.
    async fn call<B>(&self, bus_name: &str, method: &str, body: &B) -> bool
    where
        B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
    {
        let Some(connection) = self.bus().await else {
            return false;
        };
        let call =
            connection.call_method(Some(bus_name), MPRIS_PATH, Some(PLAYER_IFACE), method, body);
        match tokio::time::timeout(PLAYER_CALL_TIMEOUT, call).await {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                tracing::debug!(target: "tilo::mpris", %error, method, "player rejected the command");
                false
            }
            Err(_) => false,
        }
    }

    /// Send `command` to the player the person last listened to, and answer
    /// with what it did — but only when it answered within [`DEADLINE`].
    fn transport(&self, id: u64, command: Transport) {
        let mpris = self.clone();
        self.spawn(async move {
            let asked = Instant::now();
            // Status is taken *before* acting: reading after would race the
            // player's own (asynchronous) state change.
            let mut outcome = None;
            if let Some((bus_name, was_playing)) =
                mpris.with_table(PlayerTable::active_player).await.flatten()
                && mpris.call(&bus_name, command.method(), &()).await
            {
                outcome = Some(match command {
                    Transport::Pause => Outcome::NowPaused,
                    Transport::PlayPause if was_playing => Outcome::NowPaused,
                    Transport::PlayPause => Outcome::NowPlaying,
                    Transport::Next | Transport::Previous => Outcome::Skipped,
                    Transport::Stop => Outcome::Stopped,
                });
            }
            let outcome = outcome.filter(|_| asked.elapsed() <= DEADLINE);
            (mpris.0.emit)(Event::Transported {
                id,
                command,
                outcome,
            });
        });
    }

    /// Send `command` to exactly `bus_name`, never to whichever other player
    /// happens to be active, then publish what the player made of it.
    fn control(&self, bus_name: String, command: PlayerCommand) {
        if !is_player_name(&bus_name) {
            return;
        }
        let mpris = self.clone();
        self.spawn(async move {
            match &command {
                PlayerCommand::PlayPause => mpris.call(&bus_name, "PlayPause", &()).await,
                PlayerCommand::Pause => mpris.call(&bus_name, "Pause", &()).await,
                PlayerCommand::Next => mpris.call(&bus_name, "Next", &()).await,
                PlayerCommand::Previous => mpris.call(&bus_name, "Previous", &()).await,
                PlayerCommand::SetPosition {
                    track_id,
                    position_us,
                } => {
                    let Ok(track) = zbus::zvariant::ObjectPath::try_from(track_id.as_str()) else {
                        return;
                    };
                    mpris
                        .call(&bus_name, "SetPosition", &(track, *position_us))
                        .await
                }
            };
            tokio::time::sleep(COMMAND_SETTLE).await;
            mpris.publish();
        });
    }

    /// Pause exactly `bus_name`, never falling back to an unrelated player;
    /// with `if_uri`, only while it still plays that source, so undoing a
    /// handoff never pauses playback that started since.
    fn pause(&self, bus_name: String, if_uri: Option<String>) {
        if !is_player_name(&bus_name)
            || if_uri
                .as_ref()
                .is_some_and(|uri| uri.is_empty() || uri.len() > 8192)
        {
            return;
        }
        let mpris = self.clone();
        self.spawn(async move {
            if let Some(expected) = if_uri
                && !mpris.reports_uri(&bus_name, &expected).await
            {
                return;
            }
            mpris.call(&bus_name, "Pause", &()).await;
        });
    }

    /// Whether `bus_name` plays `expected` now, read fresh from the player.
    async fn reports_uri(&self, bus_name: &str, expected: &str) -> bool {
        self.property::<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>(
            bus_name,
            PLAYER_IFACE,
            "Metadata",
        )
        .await
        .and_then(|metadata| table::metadata_string(&metadata, "xesam:url"))
        .is_some_and(|uri| uri == expected)
    }

    /// One property, read fresh: a player does not signal `Position`, so a
    /// cached value would go stale while a resume is being verified.
    async fn property<T>(&self, bus_name: &str, interface: &str, name: &str) -> Option<T>
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
    {
        let connection = self.bus().await?;
        let reply = tokio::time::timeout(
            PLAYER_CALL_TIMEOUT,
            connection.call_method(
                Some(bus_name),
                MPRIS_PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(interface, name),
            ),
        )
        .await
        .ok()?
        .ok()?;
        let value = reply
            .body()
            .deserialize::<zbus::zvariant::OwnedValue>()
            .ok()?;
        T::try_from(value).ok()
    }

    fn resume(&self, request: crate::ResumeRequest) {
        let request_id = request.request_id;
        let Some(authority) = (if request_id == [0; 16]
            || request.package_id.is_empty()
            || request.package_id.len() > 256
            || request.package_id.chars().any(char::is_control)
            || request.position_ms > i64::MAX as u64 / 1_000
            || handoff::resume_uri(&request.source).is_none()
        {
            None
        } else {
            self.0.resumes.begin(request_id)
        }) else {
            (self.0.emit)(Event::Resumed {
                request_id,
                player: None,
            });
            return;
        };
        let mpris = self.clone();
        self.spawn(async move {
            let player = resume::run(&mpris, &request, &authority).await;
            mpris.0.resumes.finish(&authority);
            (mpris.0.emit)(Event::Resumed {
                request_id,
                player: player.filter(|_| authority.is_active()),
            });
        });
    }
}

fn is_player_name(name: &str) -> bool {
    name.starts_with(MPRIS_PREFIX) && name.len() <= 256
}

/// Decode the local cover at `url`, scaled down. A missing, oversized or
/// undecodable file has none.
fn decode_artwork(url: &str) -> Option<ArtworkImage> {
    let path = url.strip_prefix("file://")?;
    let path = handoff::percent_decode(path.split(['?', '#']).next().unwrap_or(path))?;
    let path = std::path::Path::new(&path);
    if !path.is_absolute() {
        return None;
    }
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
    Some(ArtworkImage {
        width: image.width(),
        height: image.height(),
        rgba: base64::engine::general_purpose::STANDARD.encode(image.as_raw()),
    })
}

#[cfg(test)]
mod tests {
    use super::test_bus::{
        FakePlayer, FakeRoot, PROBE, PrivateBus, called, connect, eventually, record_calls,
    };
    use super::*;
    use crate::{ResumeRequest, ResumeSource};

    #[test]
    fn the_well_known_names_match_the_spec() {
        assert_eq!(MPRIS_PREFIX, "org.mpris.MediaPlayer2.");
        assert_eq!(MPRIS_PATH, "/org/mpris/MediaPlayer2");
        assert_eq!(PLAYER_IFACE, "org.mpris.MediaPlayer2.Player");
        assert!(is_player_name("org.mpris.MediaPlayer2.music"));
        assert!(!is_player_name("org.freedesktop.DBus"));
        assert!(!is_player_name(&format!(
            "{MPRIS_PREFIX}{}",
            "x".repeat(300)
        )));
    }

    #[test]
    fn covers_are_decoded_only_from_local_files_and_scaled_down() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cover art.png");
        image::RgbaImage::from_pixel(400, 200, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let url = url::Url::from_file_path(&path).unwrap().to_string();
        let image = decode_artwork(&url).expect("a local cover decodes");
        assert_eq!(
            (image.width, image.height),
            (ARTWORK_EDGE, ARTWORK_EDGE / 2)
        );
        let rgba = base64::engine::general_purpose::STANDARD
            .decode(image.rgba)
            .unwrap();
        assert_eq!(rgba.len(), (ARTWORK_EDGE * ARTWORK_EDGE / 2 * 4) as usize);
        assert_eq!(rgba[..4], [10, 20, 30, 255]);

        assert!(decode_artwork("https://tracker.example/cover.png").is_none());
        assert!(decode_artwork("file://relative/cover.png").is_none());
        assert!(decode_artwork(&format!("{url}.missing")).is_none());
    }

    const SONG: &str = "https://media.example/song.ogg";

    /// The probe player on `bus`, owned by this process, paused at 1 s on
    /// [`SONG`].
    async fn serve_probe(bus: &PrivateBus) -> zbus::Connection {
        zbus::connection::Builder::address(bus.address())
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
                    status: "Paused".into(),
                    title: "Song".into(),
                    url: SONG.into(),
                    position_us: 1_000_000,
                    controllable: true,
                    ..FakePlayer::default()
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// Transport keys, exact pauses, a verified resume and an unplug's pause
    /// all reach the player, over the observer's own call connection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requests_drive_the_player_over_one_connection() {
        let bus = PrivateBus::start();
        let connection = connect(&bus).await;
        let calls = record_calls(&bus, connection.unique_name().unwrap().as_str()).await;
        let player = serve_probe(&bus).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let mpris = Mpris::with_calls(
            tokio::runtime::Handle::current(),
            Some(bus.address().to_owned()),
            Some(connection),
            move |event| seen.lock().unwrap().push(event),
        );
        assert!(
            mpris.with_table(|_| ()).await.is_some(),
            "the observer lists the bus"
        );
        let event = |matches: fn(&Event) -> bool| {
            let events = Arc::clone(&events);
            move || events.lock().unwrap().iter().any(matches)
        };

        mpris.handle(Request::Transport {
            id: 1,
            command: Transport::PlayPause,
        });
        eventually(
            "the key's answer",
            event(|event| {
                matches!(
                    event,
                    Event::Transported {
                        id: 1,
                        outcome: Some(Outcome::NowPlaying),
                        ..
                    }
                )
            }),
        )
        .await;

        mpris.handle(Request::Pause {
            bus_name: PROBE.into(),
            if_uri: Some("https://media.example/other.ogg".into()),
        });
        mpris.handle(Request::Pause {
            bus_name: PROBE.into(),
            if_uri: Some(SONG.into()),
        });
        eventually("the exact pause", || {
            called_now(&player).ends_with(&["Pause".to_string()])
        })
        .await;
        assert_eq!(
            called(&player).await,
            ["PlayPause", "Pause"],
            "a pause for another source never reaches the player"
        );

        // Our own process owns the probe: the compositor attributes it.
        let unique_name = player.unique_name().unwrap().to_string();
        mpris.handle(Request::Verified {
            bus_name: PROBE.into(),
            unique_name,
            package_id: "so.tilo.music".into(),
        });
        mpris.handle(Request::Resume {
            request: ResumeRequest {
                request_id: [7; 16],
                package_id: "so.tilo.music".into(),
                source: ResumeSource::Uri(SONG.into()),
                position_ms: 5_000,
                playing: true,
            },
        });
        eventually(
            "the resume's acceptance",
            event(|event| {
                matches!(
                    event,
                    Event::Resumed { request_id: [7, ..], player: Some((bus, uri)) }
                        if bus == PROBE && uri == SONG
                )
            }),
        )
        .await;
        assert_eq!(called(&player).await[2..], ["Seek", "Play"]);

        eventually(
            "the resumed player playing",
            event(|event| {
                matches!(event, Event::Players { players }
                    if players.first().is_some_and(|player| player.status == crate::Status::Playing))
            }),
        )
        .await;
        assert!(mpris.pause_playing());
        eventually("the unplug's pause", || {
            called_now(&player).last().map(String::as_str) == Some("Pause")
        })
        .await;

        let sent = calls.lock().unwrap().clone();
        for method in ["ListNames", "PlayPause", "Seek", "Play", "Pause"] {
            assert!(sent.iter().any(|call| call == method), "{method}: {sent:?}");
        }
    }

    /// [`called`] from a synchronous condition.
    fn called_now(player: &zbus::Connection) -> Vec<String> {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(called(player)))
    }
}
