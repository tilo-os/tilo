//! The system's now-playing surface: what is playing, and the transport
//! requests its controls send back.
//!
//! A [`MediaEngine`](crate::MediaEngine) on the web reports through the
//! page's Media Session. [`NowPlayingSession`] is the process-wide surface a
//! native host publishes a player into: `MPNowPlayingInfoCenter` and
//! `MPRemoteCommandCenter` on macOS. Linux has none here: the toolkit serves
//! MPRIS on the session bus itself.

use std::time::Duration;

#[cfg(target_os = "macos")]
#[path = "now_playing/macos.rs"]
mod macos;

/// What an engine is playing, for the system's now-playing controls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// An image URL the platform can fetch itself.
    pub artwork_url: Option<String>,
}

/// A request from the system's now-playing controls or media keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NowPlayingAction {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// Move the playhead to this position (a scrubber in the system's
    /// controls).
    SeekTo(Duration),
}

/// Where playback stands, beside [`NowPlaying`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NowPlayingState {
    pub status: PlaybackStatus,
    pub position: Duration,
    /// `None` for a live stream or an unknown length.
    pub duration: Option<Duration>,
    /// The playing rate (1.0 at normal speed).
    pub rate: f64,
}

/// Whether anything is playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

/// This process's entry in the system's now-playing controls: what it
/// publishes there; the requests those controls and the media keys send
/// back go to the handler it was started with. Dropping it withdraws the
/// entry. Keep it on the thread that started it (the UI thread).
pub struct NowPlayingSession {
    #[cfg(target_os = "macos")]
    inner: macos::Session,
}

/// Receives the system's now-playing requests, on whatever thread the
/// platform delivers them.
pub(crate) type NowPlayingHandler = std::sync::Arc<dyn Fn(NowPlayingAction) + Send + Sync>;

impl NowPlayingSession {
    /// Claim the system's now-playing entry, sending its requests to
    /// `on_action`. `None` where this crate has no such surface (Linux,
    /// where the toolkit serves MPRIS; the web, where a
    /// [`MediaEngine`](crate::MediaEngine) reports to the page).
    pub fn start(on_action: impl Fn(NowPlayingAction) + Send + Sync + 'static) -> Option<Self> {
        let on_action: NowPlayingHandler = std::sync::Arc::new(on_action);
        #[cfg(target_os = "macos")]
        return Some(Self {
            inner: macos::Session::start(&on_action),
        });
        #[allow(unreachable_code, reason = "targets without a surface")]
        {
            let _ = on_action;
            None
        }
    }

    /// Publish what is playing and where playback stands.
    pub fn update(&mut self, now: &NowPlaying, state: &NowPlayingState) {
        #[cfg(target_os = "macos")]
        self.inner.update(now, state);
        #[cfg(not(target_os = "macos"))]
        let _ = (now, state);
    }
}

impl NowPlayingAction {
    #[cfg_attr(
        not(web_page),
        allow(dead_code, reason = "only a page answers media-session actions")
    )]
    pub(crate) fn from_page(name: &str) -> Option<Self> {
        Some(match name {
            "play" => Self::Play,
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            "next" => Self::Next,
            "previous" => Self::Previous,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_action_names_round_trip() {
        for (name, action) in [
            ("play", NowPlayingAction::Play),
            ("pause", NowPlayingAction::Pause),
            ("stop", NowPlayingAction::Stop),
            ("next", NowPlayingAction::Next),
            ("previous", NowPlayingAction::Previous),
        ] {
            assert_eq!(NowPlayingAction::from_page(name), Some(action));
        }
        assert_eq!(NowPlayingAction::from_page("seekto"), None);
    }
}
