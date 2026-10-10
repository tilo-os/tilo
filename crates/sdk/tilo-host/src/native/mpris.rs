//! MPRIS — `org.mpris.MediaPlayer2` on the session bus — as the native
//! [`crate::host::MediaSessionFactory`]. See [`crate::mpris`] for the
//! portable player-state and command core.
//!
//! Real D-Bus serving requires Unix and the `mpris` feature (via zbus).
//! The service runs on its own worker thread with a private Tokio runtime so
//! the UI thread is never blocked. On macOS the same feature publishes the
//! player to the system's now-playing controls instead
//! (`tilo_media::NowPlayingSession`), and other platforms start no session.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the D-Bus MPRIS service runs on its own thread"
)]

#[cfg(all(feature = "mpris", not(freedesktop)))]
use std::sync::mpsc::Receiver;

#[cfg(feature = "mpris")]
use crate::mpris::MprisCommand;
use crate::mpris::{MprisConfig, PlayerSnapshot};
#[cfg(all(feature = "mpris", freedesktop))]
use crate::mpris::{OBJECT_PATH, track_object_path};

/// The native [`crate::host::MediaSessionFactory`]: MPRIS on the session bus.
pub struct DesktopMediaSessions;

/// Serve MPRIS for one player. `None` on non-Unix platforms, without the
/// `mpris` feature, or when the app runtime cannot start. The bus connection
/// is made in the background; without a session bus the session stays inert.
pub fn start_desktop_session(
    config: MprisConfig,
    initial: PlayerSnapshot,
) -> Option<crate::host::MediaSession> {
    #[cfg(feature = "mpris")]
    {
        struct Worker(tokio::sync::mpsc::UnboundedSender<PlayerSnapshot>);
        impl crate::host::MediaSessionPublisher for Worker {
            fn publish(&mut self, snapshot: PlayerSnapshot) {
                // Unbounded send is sync + non-blocking; a dead worker just drops it.
                let _ = self.0.send(snapshot);
            }
        }
        if let Some(session) = now_playing::start(&initial) {
            return Some(session);
        }
        let (commands, publish_tx) = imp::spawn(config, initial)?;
        Some(crate::host::MediaSession {
            commands,
            publisher: Box::new(Worker(publish_tx)),
        })
    }
    #[cfg(not(feature = "mpris"))]
    {
        let _ = (config, initial);
        None
    }
}

/// The player in the system's now-playing controls where `tilo-media` has
/// them (macOS: Control Center and the media keys), before MPRIS. The
/// session holds platform objects, so it stays on the thread that started
/// it (the UI thread, which also publishes); the publisher reaches it there
/// by id.
#[cfg(feature = "mpris")]
mod now_playing {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tilo_media::{NowPlaying, NowPlayingAction, NowPlayingSession, NowPlayingState};

    use crate::mpris::{MprisCommand, PlaybackStatus, PlayerSnapshot};

    thread_local! {
        #[allow(
            clippy::zero_sized_map_values,
            reason = "a session holds nothing where the platform has no controls"
        )]
        static SESSIONS: RefCell<HashMap<u64, NowPlayingSession>> = RefCell::new(HashMap::new());
    }
    static NEXT: AtomicU64 = AtomicU64::new(1);

    /// `None` where the platform has no now-playing surface of its own.
    pub(super) fn start(initial: &PlayerSnapshot) -> Option<crate::host::MediaSession> {
        let (tx, commands) = std::sync::mpsc::channel();
        let wake = tilo_rt::wake::WakeHandle::new();
        let session = NowPlayingSession::start(move |action| {
            if tx.send(command(action)).is_ok() {
                wake.wake();
            }
        })?;
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        SESSIONS.with(|sessions| sessions.borrow_mut().insert(id, session));
        let mut publisher = Publisher { id };
        crate::host::MediaSessionPublisher::publish(&mut publisher, initial.clone());
        Some(crate::host::MediaSession {
            commands,
            publisher: Box::new(publisher),
        })
    }

    /// Publishes into the session this thread holds under `id`. Publishing
    /// or dropping it from another thread finds nothing there.
    struct Publisher {
        id: u64,
    }

    impl crate::host::MediaSessionPublisher for Publisher {
        fn publish(&mut self, snapshot: PlayerSnapshot) {
            let (now, state) = describe(&snapshot);
            SESSIONS.with(|sessions| {
                if let Some(session) = sessions.borrow_mut().get_mut(&self.id) {
                    session.update(&now, &state);
                }
            });
        }
    }

    impl Drop for Publisher {
        fn drop(&mut self) {
            let _ = SESSIONS.try_with(|sessions| sessions.borrow_mut().remove(&self.id));
        }
    }

    pub(super) fn command(action: NowPlayingAction) -> MprisCommand {
        match action {
            NowPlayingAction::Play => MprisCommand::Play,
            NowPlayingAction::Pause => MprisCommand::Pause,
            NowPlayingAction::Stop => MprisCommand::Stop,
            NowPlayingAction::Next => MprisCommand::Next,
            NowPlayingAction::Previous => MprisCommand::Previous,
            NowPlayingAction::SeekTo(position) => {
                MprisCommand::SetPosition(i64::try_from(position.as_micros()).unwrap_or(i64::MAX))
            }
        }
    }

    pub(super) fn describe(snapshot: &PlayerSnapshot) -> (NowPlaying, NowPlayingState) {
        let micros = |value: i64| Duration::from_micros(u64::try_from(value).unwrap_or(0));
        (
            NowPlaying {
                title: snapshot.title.clone(),
                artist: snapshot.artist.clone(),
                album: snapshot.album.clone(),
                artwork_url: (!snapshot.art_url.is_empty()).then(|| snapshot.art_url.clone()),
            },
            NowPlayingState {
                status: match snapshot.status {
                    PlaybackStatus::Playing => tilo_media::PlaybackStatus::Playing,
                    PlaybackStatus::Paused => tilo_media::PlaybackStatus::Paused,
                    PlaybackStatus::Stopped => tilo_media::PlaybackStatus::Stopped,
                },
                position: micros(snapshot.position_us),
                duration: (snapshot.length_us > 0).then(|| micros(snapshot.length_us)),
                rate: 1.0,
            },
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_player_reaches_the_now_playing_controls_and_their_requests_come_back() {
            let snapshot = PlayerSnapshot {
                status: PlaybackStatus::Playing,
                title: "Aria".into(),
                length_us: 180_000_000,
                position_us: 3_000_000,
                ..PlayerSnapshot::default()
            };
            let (now, state) = describe(&snapshot);
            assert_eq!(now.title, "Aria");
            assert_eq!(state.duration, Some(Duration::from_secs(180)));
            assert_eq!(state.status, tilo_media::PlaybackStatus::Playing);
            assert_eq!(
                command(NowPlayingAction::SeekTo(Duration::from_secs(2))),
                MprisCommand::SetPosition(2_000_000)
            );
            let session = start(&snapshot);
            assert_eq!(
                session.is_some(),
                cfg!(target_os = "macos"),
                "macOS has a now-playing surface; elsewhere MPRIS serves"
            );
            if let Some(session) = session {
                assert!(session.commands.try_recv().is_err());
                drop(session);
            }
            SESSIONS.with(|sessions| assert!(sessions.borrow().is_empty()));
        }
    }
}

#[cfg(all(feature = "mpris", not(freedesktop)))]
mod imp {
    use super::{MprisCommand, MprisConfig, PlayerSnapshot, Receiver};
    use tokio::sync::mpsc::UnboundedSender;

    // MPRIS is a freedesktop session-bus protocol. Keep the common application
    // API available on Android, macOS and Windows without claiming a bus
    // service; macOS reaches now-playing first and falls through here.
    pub(super) fn spawn(
        _config: MprisConfig,
        _initial: PlayerSnapshot,
    ) -> Option<(Receiver<MprisCommand>, UnboundedSender<PlayerSnapshot>)> {
        None
    }
}

#[cfg(all(feature = "mpris", freedesktop))]
mod imp {
    #![allow(
        clippy::used_underscore_binding,
        reason = "zbus's #[interface] macro re-binds set_position's _track_id and open_uri's \
                  _uri by their literal names in generated dispatch code that lands in this \
                  module but outside the annotated impl/fn, so neither an impl- nor fn-level \
                  allow reaches it; the method bodies never read either parameter"
    )]
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, Sender};

    use tokio::sync::mpsc::UnboundedSender;
    use zbus::interface;
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    use super::{MprisCommand, MprisConfig, OBJECT_PATH, PlayerSnapshot};

    /// Build the MPRIS `Metadata` map (`a{sv}`) from a snapshot.
    pub(super) fn build_metadata(snap: &PlayerSnapshot) -> HashMap<String, OwnedValue> {
        let mut m: HashMap<String, OwnedValue> = HashMap::new();
        let path = super::track_object_path(&snap.track_id);
        if let Ok(op) = ObjectPath::try_from(path)
            && let Ok(v) = OwnedValue::try_from(Value::from(op))
        {
            m.insert("mpris:trackid".into(), v);
        }
        if snap.length_us > 0
            && let Ok(v) = OwnedValue::try_from(Value::from(snap.length_us))
        {
            m.insert("mpris:length".into(), v);
        }
        if !snap.art_url.is_empty()
            && let Ok(v) = OwnedValue::try_from(Value::from(snap.art_url.clone()))
        {
            m.insert("mpris:artUrl".into(), v);
        }
        if !snap.title.is_empty()
            && let Ok(v) = OwnedValue::try_from(Value::from(snap.title.clone()))
        {
            m.insert("xesam:title".into(), v);
        }
        if !snap.artist.is_empty() {
            let artists = vec![snap.artist.clone()];
            if let Ok(v) = OwnedValue::try_from(Value::from(artists)) {
                m.insert("xesam:artist".into(), v);
            }
        }
        if !snap.album.is_empty()
            && let Ok(v) = OwnedValue::try_from(Value::from(snap.album.clone()))
        {
            m.insert("xesam:album".into(), v);
        }
        m
    }

    /// `org.mpris.MediaPlayer2` — the root application object.
    pub(super) struct RootIface {
        pub cmd: Sender<MprisCommand>,
        pub identity: String,
        pub desktop_entry: String,
        pub supported_uri_schemes: Vec<String>,
        pub supported_mime_types: Vec<String>,
    }

    impl RootIface {
        pub(super) fn new(config: MprisConfig, cmd: Sender<MprisCommand>) -> Self {
            Self {
                cmd,
                identity: config.identity,
                desktop_entry: config.desktop_entry,
                supported_uri_schemes: config.supported_uri_schemes,
                supported_mime_types: config.supported_mime_types,
            }
        }
    }

    // Every method below takes `&self` because the #[interface] macro
    // dispatches D-Bus calls by matching this exact method signature; a
    // fixed-constant property (`CanQuit`, `HasTrackList`, …) that doesn't
    // read `self` still can't drop the receiver without breaking the
    // interface.
    #[interface(name = "org.mpris.MediaPlayer2")]
    impl RootIface {
        fn raise(&self) {
            let _ = self.cmd.send(MprisCommand::Raise);
        }
        fn quit(&self) {
            let _ = self.cmd.send(MprisCommand::Quit);
        }
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn can_quit(&self) -> bool {
            true
        }
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn can_raise(&self) -> bool {
            true
        }
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn has_track_list(&self) -> bool {
            false
        }
        #[zbus(property)]
        fn identity(&self) -> String {
            self.identity.clone()
        }
        #[zbus(property)]
        fn desktop_entry(&self) -> String {
            self.desktop_entry.clone()
        }
        #[zbus(property)]
        fn supported_uri_schemes(&self) -> Vec<String> {
            self.supported_uri_schemes.clone()
        }
        #[zbus(property)]
        fn supported_mime_types(&self) -> Vec<String> {
            self.supported_mime_types.clone()
        }
    }

    /// `org.mpris.MediaPlayer2.Player` — the transport + metadata object.
    pub(super) struct PlayerIface {
        pub snap: PlayerSnapshot,
        pub cmd: Sender<MprisCommand>,
    }

    // Several MPRIS properties are fixed constants (`Rate`, `CanControl`, …)
    // or a no-op setter the spec still requires (`SetRate`); they don't
    // read or mutate `self`, but the #[interface] macro dispatches by
    // matching this exact method signature, so the receiver and its
    // mutability can't be dropped without breaking the interface.
    #[interface(name = "org.mpris.MediaPlayer2.Player")]
    impl PlayerIface {
        fn play(&self) {
            let _ = self.cmd.send(MprisCommand::Play);
        }
        fn pause(&self) {
            let _ = self.cmd.send(MprisCommand::Pause);
        }
        fn play_pause(&self) {
            let _ = self.cmd.send(MprisCommand::PlayPause);
        }
        fn stop(&self) {
            let _ = self.cmd.send(MprisCommand::Stop);
        }
        fn next(&self) {
            let _ = self.cmd.send(MprisCommand::Next);
        }
        fn previous(&self) {
            let _ = self.cmd.send(MprisCommand::Previous);
        }
        fn seek(&self, offset: i64) {
            let _ = self.cmd.send(MprisCommand::Seek(offset));
        }
        fn set_position(&self, _track_id: ObjectPath<'_>, position: i64) {
            let _ = self.cmd.send(MprisCommand::SetPosition(position));
        }
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn open_uri(&self, _uri: String) {}

        /// Emitted after a seek not caused by normal playback.
        #[zbus(signal)]
        async fn seeked(ctxt: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

        #[zbus(property)]
        fn playback_status(&self) -> String {
            self.snap.status.as_mpris().into()
        }
        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            build_metadata(&self.snap)
        }
        #[zbus(property)]
        fn position(&self) -> i64 {
            self.snap.position_us
        }
        #[zbus(property)]
        fn volume(&self) -> f64 {
            self.snap.volume
        }
        #[zbus(property)]
        fn set_volume(&mut self, volume: f64) {
            let v = volume.clamp(0.0, 1.0);
            self.snap.volume = v;
            let _ = self.cmd.send(MprisCommand::SetVolume(v));
        }
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn rate(&self) -> f64 {
            1.0
        }
        #[zbus(property)]
        #[allow(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            reason = "see the comment above this impl"
        )]
        fn set_rate(&mut self, _rate: f64) {}
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn minimum_rate(&self) -> f64 {
            1.0
        }
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn maximum_rate(&self) -> f64 {
            1.0
        }
        #[zbus(property)]
        #[allow(clippy::unused_self, reason = "see the comment above this impl")]
        fn can_control(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            self.snap.can_next
        }
        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            self.snap.can_prev
        }
        #[zbus(property)]
        fn can_play(&self) -> bool {
            self.snap.can_play
        }
        #[zbus(property)]
        fn can_pause(&self) -> bool {
            self.snap.can_pause
        }
        #[zbus(property)]
        fn can_seek(&self) -> bool {
            self.snap.can_seek
        }
    }

    /// Serve both interfaces on `connection` at [`OBJECT_PATH`].
    pub(super) async fn serve(
        connection: &zbus::Connection,
        player: PlayerIface,
        root: RootIface,
    ) -> zbus::Result<()> {
        connection.object_server().at(OBJECT_PATH, player).await?;
        connection.object_server().at(OBJECT_PATH, root).await?;
        Ok(())
    }

    /// Emit `PropertiesChanged` for whichever served properties changed between
    /// `old` and `new`, then store `new` as the served snapshot.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "`guard` must stay held across every `_changed` emission below: \
                  they all report state relative to the one `new` just written, \
                  and releasing the lock between signals would let a concurrent \
                  update interleave, leaving MPRIS clients briefly observing an \
                  inconsistent mix of old and new properties"
    )]
    pub(super) async fn apply_update(
        connection: &zbus::Connection,
        old: &PlayerSnapshot,
        new: &PlayerSnapshot,
    ) -> zbus::Result<()> {
        let iref = connection
            .object_server()
            .interface::<_, PlayerIface>(OBJECT_PATH)
            .await?;
        let ctxt = iref.signal_emitter().clone();
        {
            let mut guard = iref.get_mut().await;
            guard.snap = new.clone();
            if old.status != new.status {
                guard.playback_status_changed(&ctxt).await?;
            }
            if old.metadata_differs(new) {
                guard.metadata_changed(&ctxt).await?;
            }
            if (old.volume - new.volume).abs() > f64::EPSILON {
                guard.volume_changed(&ctxt).await?;
            }
            if old.can_next != new.can_next {
                guard.can_go_next_changed(&ctxt).await?;
            }
            if old.can_prev != new.can_prev {
                guard.can_go_previous_changed(&ctxt).await?;
            }
            if old.can_play != new.can_play {
                guard.can_play_changed(&ctxt).await?;
            }
            if old.can_pause != new.can_pause {
                guard.can_pause_changed(&ctxt).await?;
            }
            if old.can_seek != new.can_seek {
                guard.can_seek_changed(&ctxt).await?;
            }
        }
        if old.track_id == new.track_id
            && new.position_us.saturating_sub(old.position_us).abs() > 2_000_000
        {
            let _ = PlayerIface::seeked(&ctxt, new.position_us).await;
        }
        Ok(())
    }

    /// Start serving MPRIS on the app runtime. Returns the command receiver
    /// (bus → widget) and the publish sender (widget → bus) at once, or `None`
    /// when the app runtime cannot start.
    ///
    /// Connecting to the session bus and claiming the name are D-Bus round
    /// trips, so they run on the app runtime rather than on the caller — a
    /// player usually starts its session from the UI thread. Without a
    /// reachable session bus (headless / no D-Bus) the task ends: commands
    /// never arrive and publishes are dropped, like the stub.
    pub(super) fn spawn(
        config: MprisConfig,
        initial: PlayerSnapshot,
    ) -> Option<(Receiver<MprisCommand>, UnboundedSender<PlayerSnapshot>)> {
        let runtime = tilo_rt::try_app().ok()?;
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<MprisCommand>();
        let (pub_tx, mut pub_rx) = tokio::sync::mpsc::unbounded_channel::<PlayerSnapshot>();

        runtime.spawn(async move {
            let bus_name = config.bus_name.clone();
            let connected = async {
                zbus::connection::Builder::session()?
                    .name(bus_name)?
                    .build()
                    .await
            };
            let connection = match connected.await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::debug!(%error, "MPRIS: no session bus; media controls are off");
                    return;
                }
            };
            let root = RootIface::new(config, cmd_tx.clone());
            let player = PlayerIface {
                snap: initial.clone(),
                cmd: cmd_tx,
            };
            if let Err(error) = serve(&connection, player, root).await {
                tracing::debug!(%error, "MPRIS: could not serve the player; media controls are off");
                return;
            }
            let mut last = initial;
            while let Some(snap) = pub_rx.recv().await {
                if apply_update(&connection, &last, &snap).await.is_ok() {
                    last = snap;
                }
            }
        });

        Some((cmd_rx, pub_tx))
    }
}

#[cfg(test)]
#[cfg(feature = "mpris")]
mod tests {
    use super::*;
    #[cfg(freedesktop)]
    use crate::mpris::PlaybackStatus;

    #[cfg(all(feature = "mpris", not(freedesktop)))]
    #[test]
    fn unsupported_platform_starts_no_session() {
        assert!(
            start_desktop_session(
                MprisConfig::new("tilo-player", "Tilo Player"),
                PlayerSnapshot::default(),
            )
            .is_none()
        );
    }

    /// Hermetic round-trip over an in-process p2p socket pair (no session
    /// bus): proves the served MPRIS interfaces *respond* and *reflect* the
    /// player state a `VideoPlayer` publishes.
    #[cfg(all(feature = "mpris", freedesktop))]
    #[tokio::test]
    async fn mpris_interface_responds_and_reflects_state() {
        use std::sync::mpsc::channel;
        use tokio::net::UnixStream;
        use zbus::connection::Builder;

        const GUID: &str = "0123456789abcdef0123456789abcdef";

        let (cmd_tx, cmd_rx) = channel::<MprisCommand>();
        // `mpsc::Receiver` isn't `Sync`, so a plain `&Receiver` held across
        // an `.await` in `assert_commands_forwarded` makes that future
        // `!Send`; a `Mutex` around it is `Sync` and fixes that with no
        // behaviour change (this test never contends the lock).
        let cmd_rx = std::sync::Mutex::new(cmd_rx);
        let snap = PlayerSnapshot {
            status: PlaybackStatus::Playing,
            track_id: "yt:abc123".into(),
            title: "Runaway".into(),
            artist: "Aurora".into(),
            length_us: 240_000_000,
            can_next: true,
            can_pause: true,
            ..PlayerSnapshot::default()
        };

        let (server_sock, client_sock) = UnixStream::pair().expect("socket pair");
        let player = imp::PlayerIface {
            snap: snap.clone(),
            cmd: cmd_tx.clone(),
        };
        let mut config = MprisConfig::new("tilo-player", "Tilo Player");
        config.supported_uri_schemes = vec!["http".into(), "https".into()];
        config.supported_mime_types = vec!["audio/ogg".into(), "audio/mpeg".into()];
        let root = imp::RootIface::new(config, cmd_tx.clone());
        let server_builder = Builder::unix_stream(server_sock)
            .server(GUID)
            .expect("guid")
            .p2p()
            .serve_at(OBJECT_PATH, player)
            .expect("serve player")
            .serve_at(OBJECT_PATH, root)
            .expect("serve root");
        let client_builder = Builder::unix_stream(client_sock).p2p();
        let (server_conn, client_conn) =
            tokio::join!(server_builder.build(), client_builder.build());
        let server_conn = server_conn.expect("server conn");
        let client_conn = client_conn.expect("client conn");

        let root_proxy = zbus::Proxy::new(
            &client_conn,
            "org.mpris.MediaPlayer2.tilo-player",
            OBJECT_PATH,
            "org.mpris.MediaPlayer2",
        )
        .await
        .expect("root proxy");
        assert_eq!(
            root_proxy
                .get_property::<Vec<String>>("SupportedUriSchemes")
                .await
                .expect("URI schemes"),
            ["http", "https"],
        );
        assert_eq!(
            root_proxy
                .get_property::<Vec<String>>("SupportedMimeTypes")
                .await
                .expect("MIME types"),
            ["audio/ogg", "audio/mpeg"],
        );

        let player_proxy: zbus::Proxy = zbus::proxy::Builder::new(&client_conn)
            .destination("org.mpris.MediaPlayer2.tilo-player")
            .expect("proxy destination")
            .path(OBJECT_PATH)
            .expect("proxy path")
            .interface("org.mpris.MediaPlayer2.Player")
            .expect("proxy interface")
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .expect("player proxy");

        assert_served_state_matches(&player_proxy).await;
        assert_commands_forwarded(&player_proxy, &cmd_rx).await;
        assert_update_reflected(&server_conn, &player_proxy, &snap).await;

        drop(client_conn);
        drop(server_conn);
    }

    /// The properties zbus serves for the snapshot [`spawn`] published match
    /// what was published.
    #[cfg(all(feature = "mpris", freedesktop))]
    async fn assert_served_state_matches(player_proxy: &zbus::Proxy<'_>) {
        let status: String = player_proxy
            .get_property("PlaybackStatus")
            .await
            .expect("read PlaybackStatus");
        assert_eq!(status, "Playing", "served status reflected over D-Bus");

        let metadata: std::collections::HashMap<String, zbus::zvariant::OwnedValue> = player_proxy
            .get_property("Metadata")
            .await
            .expect("read Metadata");
        let title: String = metadata
            .get("xesam:title")
            .and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_string))
            .or_else(|| {
                metadata
                    .get("xesam:title")
                    .and_then(|v| String::try_from(v.try_clone().unwrap()).ok())
            })
            .expect("title in metadata");
        assert_eq!(title, "Runaway", "served now-playing reflected over D-Bus");
    }

    /// A client calling `Pause`/`Next`/`Play` over the proxy is forwarded to
    /// the widget as [`MprisCommand`]s, in order.
    #[cfg(all(feature = "mpris", freedesktop))]
    async fn assert_commands_forwarded(
        player_proxy: &zbus::Proxy<'_>,
        cmd_rx: &std::sync::Mutex<std::sync::mpsc::Receiver<MprisCommand>>,
    ) {
        player_proxy
            .call_method("Pause", &())
            .await
            .expect("Pause call");
        player_proxy
            .call_method("Next", &())
            .await
            .expect("Next call");
        player_proxy
            .call_method("Play", &())
            .await
            .expect("Play call");
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(
                cmd_rx
                    .lock()
                    .expect("test-local mutex is never poisoned")
                    .recv()
                    .expect("command delivered"),
            );
        }
        assert_eq!(
            got,
            vec![MprisCommand::Pause, MprisCommand::Next, MprisCommand::Play],
            "external control reaches the widget as commands"
        );
    }

    /// Publishing an update through [`imp::apply_update`] is reflected back
    /// over the served property.
    #[cfg(all(feature = "mpris", freedesktop))]
    async fn assert_update_reflected(
        server_conn: &zbus::Connection,
        player_proxy: &zbus::Proxy<'_>,
        snap: &PlayerSnapshot,
    ) {
        let mut updated = snap.clone();
        updated.status = PlaybackStatus::Paused;
        imp::apply_update(server_conn, snap, &updated)
            .await
            .expect("apply update");
        let status: String = player_proxy
            .get_property("PlaybackStatus")
            .await
            .expect("re-read PlaybackStatus");
        assert_eq!(status, "Paused", "a Pause reflects on PlaybackStatus");
    }
}
