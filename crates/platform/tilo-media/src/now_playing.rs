//! The system's now-playing surface: what is playing, and the transport requests its
//! controls send back.
//!
//! On the web a [`MediaEngine`](crate::MediaEngine) reports through the page's Media
//! Session. [`NowPlayingSession`] is the process-wide surface a native host publishes
//! into (`MPNowPlayingInfoCenter` and `MPRemoteCommandCenter` on macOS); on Linux the
//! toolkit serves MPRIS on the session bus itself.

use std::time::Duration;

#[cfg(target_os = "macos")]
#[path = "now_playing/macos.rs"]
mod macos;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NowPlayingAction {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    SeekTo(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NowPlayingState {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub rate: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

pub struct NowPlayingSession {
    #[cfg(target_os = "macos")]
    inner: macos::Session,
}

pub(crate) type NowPlayingHandler = std::sync::Arc<dyn Fn(NowPlayingAction) + Send + Sync>;

impl NowPlayingSession {
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

    pub fn update(&mut self, now: &NowPlaying, state: &NowPlayingState) {
        #[cfg(target_os = "macos")]
        self.inner.update(now, state);
        #[cfg(not(target_os = "macos"))]
        let _ = (now, state);
    }
}

impl NowPlayingAction {
    #[cfg(any(web_page, test))]
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
