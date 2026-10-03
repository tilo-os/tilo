//! MPRIS — `org.mpris.MediaPlayer2` desktop media control, in the toolkit.
//!
//! Shared, app-agnostic core that publishes now-playing metadata + transport
//! state to the desktop and receives media-key / system-widget commands back.
//! Lifted out of the Music app so any `ui` consumer (e.g.
//! `ui::widget::video_player::VideoPlayer`, hence Tube) gets this free.
//!
//! This is the pure state/command core; it always compiles so tests run
//! without D-Bus. The session itself comes from the host's
//! [`crate::host::MediaSessionFactory`] — natively, D-Bus serving in
//! `ui::native::mpris`, which requires Unix and the `mpris` feature. Other
//! hosts and platforms leave the handle inactive.
//!
//! Two channels bridge the session to the widget: external commands land as
//! [`MprisCommand`]s drained each frame ([`MprisHandle::drain_commands`]); the
//! widget pushes a [`PlayerSnapshot`] on change ([`MprisHandle::publish`]) and
//! the session emits `PropertiesChanged`.

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
    /// URI schemes this player accepts (`SupportedUriSchemes`).
    pub supported_uri_schemes: Vec<String>,
    /// Media formats this player accepts (`SupportedMimeTypes`).
    pub supported_mime_types: Vec<String>,
}

impl MprisConfig {
    pub fn new(app_id: &str, identity: &str) -> Self {
        Self {
            bus_name: format!("org.mpris.MediaPlayer2.{app_id}"),
            identity: identity.to_string(),
            desktop_entry: app_id.to_string(),
            supported_uri_schemes: vec!["http".into(), "https".into(), "file".into()],
            supported_mime_types: vec![
                "video/mp4".into(),
                "video/webm".into(),
                "audio/mpeg".into(),
                "audio/mp4".into(),
            ],
        }
    }
}

/// A snapshot of everything MPRIS exposes about the player.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag mirrors one independent MPRIS `CanXxx` property; \
              `apps/music` and `tilo_ui::widget::video_player` construct this by \
              field name, so reshaping it into flags is out of scope for a \
              lint fix"
)]
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
            || (self.volume - last.volume).abs() > f64::EPSILON
            || self.can_next != last.can_next
            || self.can_prev != last.can_prev
            || self.can_play != last.can_play
            || self.can_pause != last.can_pause
            || self.can_seek != last.can_seek
            || self.metadata_differs(last)
            || self.position_us.saturating_sub(last.position_us).abs() >= 1_000_000
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

/// The widget-side handle to the host's media controls, started through the
/// current [`crate::host::MediaSessionFactory`]. When the host has none — the
/// default (non-`mpris`) build, non-Unix platforms, no session bus, headless
/// hosts — this is an inert stub: `publish` is a no-op and `drain_commands`
/// yields nothing, so the widget code path is identical everywhere.
pub struct MprisHandle {
    cmd_rx: Option<Receiver<MprisCommand>>,
    last: PlayerSnapshot,
    publisher: Option<Box<dyn crate::host::MediaSessionPublisher>>,
}

impl MprisHandle {
    /// An inert handle (no D-Bus). Used by the default build, `--shots`, tests,
    /// and as the graceful fallback when no session bus is reachable.
    pub fn disabled() -> Self {
        Self {
            cmd_rx: None,
            last: PlayerSnapshot::default(),
            publisher: None,
        }
    }

    /// Whether the host's media session is running.
    pub fn is_active(&self) -> bool {
        self.cmd_rx.is_some()
    }

    /// Register a player under `config` with the current host, publishing
    /// `initial` as the starting state. Never panics: when the host has no
    /// media controls it returns an inert [`Self::disabled`].
    pub fn spawn(config: MprisConfig, initial: PlayerSnapshot) -> Self {
        match crate::host::current()
            .media_session
            .start(config, initial.clone())
        {
            Some(session) => Self {
                cmd_rx: Some(session.commands),
                last: initial,
                publisher: Some(session.publisher),
            },
            None => Self::disabled(),
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

    /// Publish the current player state to the host, which for MPRIS emits
    /// `PropertiesChanged` for whatever changed. Cheap and deduped:
    /// identical/insignificant updates are dropped so the bus isn't spammed
    /// each frame.
    pub fn publish(&mut self, snap: PlayerSnapshot) {
        if !snap.should_republish(&self.last) {
            return;
        }
        if let Some(publisher) = &mut self.publisher {
            publisher.publish(snap.clone());
        }
        self.last = snap;
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
    format!("/org/mpris/tilo/track/{s}")
}

/// The single object path both MPRIS interfaces live at.
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpris_names_paths_and_status_map_to_valid_bus_values() {
        // Case `playback_status_maps_to_mpris_strings`.
        {
            assert_eq!(PlaybackStatus::Playing.as_mpris(), "Playing");
            assert_eq!(PlaybackStatus::Paused.as_mpris(), "Paused");
            assert_eq!(PlaybackStatus::Stopped.as_mpris(), "Stopped");
        }

        // Case `config_builds_mpris_bus_name`.
        {
            let c = MprisConfig::new("tilo-tube", "Tilo Tube");
            assert_eq!(c.bus_name, "org.mpris.MediaPlayer2.tilo-tube");
            assert_eq!(c.identity, "Tilo Tube");
            assert_eq!(c.desktop_entry, "tilo-tube");
        }

        // Case `track_object_path_is_valid_and_sanitised`.
        {
            assert_eq!(
                track_object_path(""),
                "/org/mpris/MediaPlayer2/TrackList/NoTrack"
            );
            let p = track_object_path("yt:abc-123");
            assert_eq!(p, "/org/mpris/tilo/track/yt_abc_123");
            assert!(
                p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'_')
            );
        }
    }

    #[test]
    fn mpris_republish_dedups_and_disabled_handles_are_inert() {
        // Case `republish_dedups_but_reacts_to_material_change`.
        {
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

        // Case `disabled_handle_is_inert`.
        {
            let mut h = MprisHandle::disabled();
            assert!(!h.is_active());
            assert!(h.drain_commands().is_empty());
            h.publish(PlayerSnapshot {
                status: PlaybackStatus::Playing,
                title: "x".into(),
                ..PlayerSnapshot::default()
            });
        }
    }
}
