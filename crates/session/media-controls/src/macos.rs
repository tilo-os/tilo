//! macOS's now-playing controls: `MPNowPlayingInfoCenter` shows one player at a time, so the
//! surface carries the Playback the person touched last, and the remote command centre's
//! presses become that Playback's own actions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_media::PlaybackStatus as MediaStatus;
use tilo_media::{NowPlaying, NowPlayingAction, NowPlayingSession, NowPlayingState};

use crate::board::{Board, Change, Command, PlayerView, Status};

pub struct Server {
    session: Option<NowPlayingSession>,
    /// The Playback the surface is showing, as a command's callback reads it.
    current: Arc<Mutex<String>>,
    /// What was last shown, so a read that moves nothing is not published again.
    shown: Option<(String, NowPlaying, NowPlayingState)>,
}

impl Server {
    #[must_use]
    pub fn start(on_command: crate::OnCommand) -> Self {
        let current: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let shown = Arc::clone(&current);
        let session = NowPlayingSession::start(move |action| {
            let id = shown.lock().map(|id| id.clone()).unwrap_or_default();
            if id.is_empty() {
                return;
            }
            if let Some(command) = command_for(action) {
                on_command(&id, command);
            }
        });
        Self {
            session,
            current,
            shown: None,
        }
    }

    /// Show the most recently touched Playback, or nothing when none plays.
    pub fn apply(&mut self, board: &Board, _changes: &[Change]) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(view) = board.most_recent() else {
            if let Ok(mut id) = self.current.lock() {
                id.clear();
            }
            self.shown = None;
            return;
        };
        let (now, state) = describe(view);
        if let Ok(mut id) = self.current.lock() {
            id.clone_from(&view.id);
        }
        if self.shown.as_ref() == Some(&(view.id.clone(), now.clone(), state)) {
            return;
        }
        session.update(&now, &state);
        self.shown = Some((view.id.clone(), now, state));
    }
}

/// What macOS shows of one Playback.
fn describe(view: &PlayerView) -> (NowPlaying, NowPlayingState) {
    let now = NowPlaying {
        title: view.title.clone(),
        artist: view.artist.clone(),
        album: view.album.clone(),
        artwork_url: Some(view.art_url.clone()).filter(|url| !url.is_empty()),
    };
    let state = NowPlayingState {
        status: match view.status {
            Status::Playing => MediaStatus::Playing,
            Status::Paused => MediaStatus::Paused,
            Status::Stopped => MediaStatus::Stopped,
        },
        position: micros(view.position_us),
        duration: (view.length_us > 0).then(|| micros(view.length_us)),
        rate: view.rate,
    };
    (now, state)
}

/// A signed microseconds count as a `Duration`, clamped at zero.
fn micros(value: i64) -> Duration {
    Duration::from_micros(u64::try_from(value).unwrap_or(0))
}

/// What a press on the system's controls asks the shown Playback.
fn command_for(action: NowPlayingAction) -> Option<Command> {
    Some(match action {
        NowPlayingAction::Play => Command::Play,
        NowPlayingAction::Pause => Command::Pause,
        NowPlayingAction::Stop => Command::Stop,
        NowPlayingAction::Next => Command::Next,
        NowPlayingAction::Previous => Command::Previous,
        NowPlayingAction::SeekTo(at) => {
            Command::SeekTo(i64::try_from(at.as_micros()).unwrap_or(i64::MAX))
        }
    })
}
