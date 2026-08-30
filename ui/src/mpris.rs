//! MPRIS — `org.mpris.MediaPlayer2` desktop media control, in the toolkit.
//!
//! Shared, app-agnostic core that publishes now-playing metadata + transport
//! state to the desktop and receives media-key / system-widget commands back.
//! Lifted out of the Music app so any `ui` consumer (e.g.
//! [`crate::widget::video_player::VideoPlayer`], hence Tube) gets this free.
//!
//! Real D-Bus serving is gated behind the `mpris` feature (via [`zbus`]); the
//! pure state/command core always compiles so tests run without D-Bus.
//!
//! Runs on its own worker thread with a private Tokio runtime so the UI
//! thread is never blocked. Two channels bridge it to the widget: external
//! commands land as [`MprisCommand`]s drained each frame
//! ([`MprisHandle::drain_commands`]); the widget pushes a [`PlayerSnapshot`]
//! on change ([`MprisHandle::publish`]) and the worker emits `PropertiesChanged`.

use std::sync::mpsc::Receiver;

/// Transport state, mapped to the MPRIS `PlaybackStatus` string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    /// The exact MPRIS enum string (`"Playing"` / `"Paused"` / `"Stopped"`).
    pub fn as_mpris(self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Stopped => "Stopped",
        }
    }
}

/// Identity for the served MPRIS bus: the well-known name + human-readable
/// player name, so different consumers (Music, Tube, the local video player)
/// present distinctly in the system media widget.
#[derive(Clone, Debug)]
pub struct MprisConfig {
    /// Full well-known bus name, `org.mpris.MediaPlayer2.<app_id>`.
    pub bus_name: String,
    /// Human-readable player name shown by the desktop (`Identity`).
    pub identity: String,
    /// `DesktopEntry` — the `.desktop` basename, for the desktop to find the app.
    pub desktop_entry: String,
}

impl MprisConfig {
    pub fn new(app_id: &str, identity: &str) -> Self {
        Self {
            bus_name: format!("org.mpris.MediaPlayer2.{app_id}"),
            identity: identity.to_string(),
            desktop_entry: app_id.to_string(),
        }
    }
}

/// A snapshot of everything MPRIS exposes about the player.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSnapshot {
    pub status: PlaybackStatus,
    /// Stable track id → the `mpris:trackid` object path. Empty ⇒ no track.
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// `mpris:artUrl` — an http(s) cover URL (or empty).
    pub art_url: String,
    /// `mpris:length`, microseconds (0 ⇒ unknown).
    pub length_us: i64,
    /// `Position`, microseconds.
    pub position_us: i64,
    /// `Volume`, `0.0..=1.0`.
    pub volume: f64,
    pub can_next: bool,
    pub can_prev: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
}

impl Default for PlayerSnapshot {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            track_id: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            art_url: String::new(),
            length_us: 0,
            position_us: 0,
            volume: 0.8,
            can_next: false,
            can_prev: false,
            can_play: false,
            can_pause: false,
            can_seek: false,
        }
    }
}

impl PlayerSnapshot {
    /// Whether two snapshots differ in a field that appears in MPRIS
    /// `Metadata` (track identity / display / length).
    pub fn metadata_differs(&self, other: &Self) -> bool {
        self.track_id != other.track_id
            || self.title != other.title
            || self.artist != other.artist
            || self.album != other.album
            || self.art_url != other.art_url
            || self.length_us != other.length_us
    }

    /// Whether a re-publish is worthwhile: any material field changed, or the
    /// playhead moved enough that a polling client would want a fresh
    /// `Position`. Ignores sub-second position jitter to avoid signal spam.
    pub fn should_republish(&self, last: &Self) -> bool {
        self.status != last.status
            || self.volume != last.volume
            || self.can_next != last.can_next
            || self.can_prev != last.can_prev
            || self.can_play != last.can_play
            || self.can_pause != last.can_pause
            || self.can_seek != last.can_seek
            || self.metadata_differs(last)
            || (self.position_us - last.position_us).abs() >= 1_000_000
    }
}

/// A control request arriving from an external MPRIS client (media keys, the
/// GNOME media widget, `playerctl`, …).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MprisCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative seek, microseconds (may be negative).
    Seek(i64),
    /// Absolute seek, microseconds.
    SetPosition(i64),
    /// Set volume, `0.0..=1.0` (clamped by the consumer).
    SetVolume(f64),
    Raise,
    Quit,
}

/// The widget-side handle to the MPRIS worker. In the default (non-`mpris`)
/// build, and whenever the session bus is unavailable, this is an inert stub:
/// `publish` is a no-op and `drain_commands` yields nothing — so the widget code
/// path is identical everywhere.
pub struct MprisHandle {
    cmd_rx: Option<Receiver<MprisCommand>>,
    last: PlayerSnapshot,
    #[cfg(feature = "mpris")]
    publish_tx: Option<tokio::sync::mpsc::UnboundedSender<PlayerSnapshot>>,
}

impl MprisHandle {
    /// An inert handle (no D-Bus). Used by the default build, `--shots`, tests,
    /// and as the graceful fallback when no session bus is reachable.
    pub fn disabled() -> Self {
        Self {
            cmd_rx: None,
            last: PlayerSnapshot::default(),
            #[cfg(feature = "mpris")]
            publish_tx: None,
        }
    }

    /// Whether the MPRIS D-Bus service is actually running.
    pub fn is_active(&self) -> bool {
        self.cmd_rx.is_some()
    }

    /// Bring up the MPRIS service under `config`, publishing `initial` as the
    /// starting state. Never panics: without the `mpris` feature, or if the
    /// session bus can't be reached, it returns an inert [`Self::disabled`].
    pub fn spawn(config: MprisConfig, initial: PlayerSnapshot) -> Self {
        #[cfg(feature = "mpris")]
        {
            match imp::spawn(config, initial.clone()) {
                Some((cmd_rx, publish_tx)) => Self {
                    cmd_rx: Some(cmd_rx),
                    last: initial,
                    publish_tx: Some(publish_tx),
                },
                None => Self::disabled(),
            }
        }
        #[cfg(not(feature = "mpris"))]
        {
            let _ = (config, initial);
            Self::disabled()
        }
    }

    /// Drain every pending external control command (non-blocking).
    pub fn drain_commands(&self) -> Vec<MprisCommand> {
        let mut out = Vec::new();
        if let Some(rx) = &self.cmd_rx {
            while let Ok(cmd) = rx.try_recv() {
                out.push(cmd);
            }
        }
        out
    }

    /// Publish the current player state to the bus, emitting `PropertiesChanged`
    /// for whatever changed. Cheap and deduped: identical/insignificant updates
    /// are dropped so the bus isn't spammed each frame.
    pub fn publish(&mut self, snap: PlayerSnapshot) {
        if !snap.should_republish(&self.last) {
            return;
        }
        self.last = snap.clone();
        #[cfg(feature = "mpris")]
        if let Some(tx) = &self.publish_tx {
            // Unbounded send is sync + non-blocking; a dead worker just drops it.
            let _ = tx.send(snap);
        }
    }
}

impl Default for MprisHandle {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Map a track id to a valid D-Bus object path for `mpris:trackid`. D-Bus paths
/// allow only `[A-Za-z0-9_]` between `/` separators, so non-conforming bytes are
/// replaced with `_`. An empty id yields the MPRIS "no track" sentinel path.
pub fn track_object_path(track_id: &str) -> String {
    if track_id.is_empty() {
        return "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_string();
    }
    let mut s = String::with_capacity(track_id.len());
    for ch in track_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            s.push(ch);
        } else {
            s.push('_');
        }
    }
    format!("/org/mpris/qos/track/{s}")
}

/// The single object path both MPRIS interfaces live at.
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

#[cfg(feature = "mpris")]
mod imp {
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, Sender};

    use tokio::sync::mpsc::UnboundedSender;
    use zbus::interface;
    use zbus::object_server::SignalContext;
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
    }

    #[interface(name = "org.mpris.MediaPlayer2")]
    impl RootIface {
        fn raise(&self) {
            let _ = self.cmd.send(MprisCommand::Raise);
        }
        fn quit(&self) {
            let _ = self.cmd.send(MprisCommand::Quit);
        }
        #[zbus(property)]
        fn can_quit(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn can_raise(&self) -> bool {
            true
        }
        #[zbus(property)]
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
            vec!["http".into(), "https".into(), "file".into()]
        }
        #[zbus(property)]
        fn supported_mime_types(&self) -> Vec<String> {
            vec![
                "video/mp4".into(),
                "video/webm".into(),
                "audio/mpeg".into(),
                "audio/mp4".into(),
            ]
        }
    }

    /// `org.mpris.MediaPlayer2.Player` — the transport + metadata object.
    pub(super) struct PlayerIface {
        pub snap: PlayerSnapshot,
        pub cmd: Sender<MprisCommand>,
    }

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
        fn open_uri(&self, _uri: String) {}

        /// Emitted after a seek not caused by normal playback.
        #[zbus(signal)]
        async fn seeked(ctxt: &SignalContext<'_>, position: i64) -> zbus::Result<()>;

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
        fn rate(&self) -> f64 {
            1.0
        }
        #[zbus(property)]
        fn set_rate(&mut self, _rate: f64) {}
        #[zbus(property)]
        fn minimum_rate(&self) -> f64 {
            1.0
        }
        #[zbus(property)]
        fn maximum_rate(&self) -> f64 {
            1.0
        }
        #[zbus(property)]
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
    pub(super) async fn apply_update(
        connection: &zbus::Connection,
        old: &PlayerSnapshot,
        new: &PlayerSnapshot,
    ) -> zbus::Result<()> {
        let iref = connection
            .object_server()
            .interface::<_, PlayerIface>(OBJECT_PATH)
            .await?;
        let ctxt = iref.signal_context().clone();
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
        if old.track_id == new.track_id && (new.position_us - old.position_us).abs() > 2_000_000 {
            let _ = PlayerIface::seeked(&ctxt, new.position_us).await;
        }
        Ok(())
    }

    /// Spawn the MPRIS worker thread. Returns the command receiver (bus →
    /// widget) and the publish sender (widget → bus), or `None` if the session
    /// bus is unreachable (headless / no D-Bus) — the caller degrades to the stub.
    pub(super) fn spawn(
        config: MprisConfig,
        initial: PlayerSnapshot,
    ) -> Option<(Receiver<MprisCommand>, UnboundedSender<PlayerSnapshot>)> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<MprisCommand>();
        let (pub_tx, mut pub_rx) = tokio::sync::mpsc::unbounded_channel::<PlayerSnapshot>();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let bus_name = config.bus_name.clone();
        let connection = rt
            .block_on(async {
                zbus::connection::Builder::session()?
                    .name(bus_name)?
                    .build()
                    .await
            })
            .ok()?;

        let root = RootIface {
            cmd: cmd_tx.clone(),
            identity: config.identity.clone(),
            desktop_entry: config.desktop_entry,
        };
        let player = PlayerIface {
            snap: initial.clone(),
            cmd: cmd_tx,
        };
        if rt.block_on(serve(&connection, player, root)).is_err() {
            return None;
        }

        std::thread::Builder::new()
            .name("qos-ui-mpris".into())
            .spawn(move || {
                rt.block_on(async move {
                    let mut last = initial;
                    while let Some(snap) = pub_rx.recv().await {
                        if apply_update(&connection, &last, &snap).await.is_ok() {
                            last = snap;
                        }
                    }
                    drop(connection);
                });
            })
            .ok()?;

        Some((cmd_rx, pub_tx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_status_maps_to_mpris_strings() {
        assert_eq!(PlaybackStatus::Playing.as_mpris(), "Playing");
        assert_eq!(PlaybackStatus::Paused.as_mpris(), "Paused");
        assert_eq!(PlaybackStatus::Stopped.as_mpris(), "Stopped");
    }

    #[test]
    fn config_builds_mpris_bus_name() {
        let c = MprisConfig::new("qos-tube", "qOS Tube");
        assert_eq!(c.bus_name, "org.mpris.MediaPlayer2.qos-tube");
        assert_eq!(c.identity, "qOS Tube");
        assert_eq!(c.desktop_entry, "qos-tube");
    }

    #[test]
    fn track_object_path_is_valid_and_sanitised() {
        assert_eq!(
            track_object_path(""),
            "/org/mpris/MediaPlayer2/TrackList/NoTrack"
        );
        let p = track_object_path("yt:abc-123");
        assert_eq!(p, "/org/mpris/qos/track/yt_abc_123");
        assert!(
            p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'_')
        );
    }

    #[test]
    fn republish_dedups_but_reacts_to_material_change() {
        let base = PlayerSnapshot::default();
        let mut same = base.clone();
        same.position_us = 200_000;
        assert!(
            !same.should_republish(&base),
            "sub-second position jitter is ignored"
        );
        let mut playing = base.clone();
        playing.status = PlaybackStatus::Playing;
        assert!(
            playing.should_republish(&base),
            "a status change republishes"
        );
        let mut track = base.clone();
        track.title = "Runaway".into();
        assert!(track.metadata_differs(&base));
        assert!(track.should_republish(&base));
    }

    #[test]
    fn disabled_handle_is_inert() {
        let mut h = MprisHandle::disabled();
        assert!(!h.is_active());
        assert!(h.drain_commands().is_empty());
        h.publish(PlayerSnapshot {
            status: PlaybackStatus::Playing,
            title: "x".into(),
            ..PlayerSnapshot::default()
        });
    }

    /// Hermetic round-trip over an in-process p2p socket pair (no session
    /// bus): proves the served MPRIS interfaces *respond* and *reflect* the
    /// player state a `VideoPlayer` publishes.
    #[cfg(feature = "mpris")]
    #[tokio::test]
    async fn mpris_interface_responds_and_reflects_state() {
        use std::sync::mpsc::channel;
        use tokio::net::UnixStream;
        use zbus::connection::Builder;

        const GUID: &str = "0123456789abcdef0123456789abcdef";

        let (cmd_tx, cmd_rx) = channel::<MprisCommand>();
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
        let root = imp::RootIface {
            cmd: cmd_tx.clone(),
            identity: "qOS Player".into(),
            desktop_entry: "qos-player".into(),
        };
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

        let player_proxy: zbus::Proxy = zbus::proxy::Builder::new(&client_conn)
            .destination("org.mpris.MediaPlayer2.qos-player")
            .expect("proxy destination")
            .path(OBJECT_PATH)
            .expect("proxy path")
            .interface("org.mpris.MediaPlayer2.Player")
            .expect("proxy interface")
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .expect("player proxy");

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
            got.push(cmd_rx.recv().expect("command delivered"));
        }
        assert_eq!(
            got,
            vec![MprisCommand::Pause, MprisCommand::Next, MprisCommand::Play],
            "external control reaches the widget as commands"
        );

        let mut updated = snap.clone();
        updated.status = PlaybackStatus::Paused;
        imp::apply_update(&server_conn, &snap, &updated)
            .await
            .expect("apply update");
        let status: String = player_proxy
            .get_property("PlaybackStatus")
            .await
            .expect("re-read PlaybackStatus");
        assert_eq!(status, "Paused", "a Pause reflects on PlaybackStatus");

        drop(client_conn);
        drop(server_conn);
    }
}
