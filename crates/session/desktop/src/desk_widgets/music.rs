//! The music desk widget's content: what plays now, as the session reads it.
//!
//! The reading is not made here. The session takes it from the player it
//! already shows in Quick Settings ([`NowPlaying`], from the audio service's
//! reading of the players) and keeps it here every frame, so the widget is a
//! view of that one reading and never a reader of its own; the buttons it
//! carries ask the session for the transport, which hands the same request
//! to the player the reading names ([`MusicAction`]).
//!
//! Nothing plays: there is no reading, and the widget has nothing to show.

use std::cell::RefCell;
use std::rc::Rc;

use ui::widget::TransportArt;

/// What the widget shows of the player: its song, whether it plays, and the
/// transport it offers.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "what the player can do now, as it reports it: independent flags"
)]
pub struct NowPlaying {
    pub title: String,
    /// Who it is by.
    pub subtitle: String,
    /// The app that plays it.
    pub player: String,
    pub art: TransportArt,
    pub playing: bool,
    pub can_toggle: bool,
    pub can_previous: bool,
    pub can_next: bool,
    /// Where the song is, and how long it is; the length is zero when the
    /// player does not say, and nothing shows where it is then.
    pub position_ms: f64,
    pub duration_ms: f64,
}

impl NowPlaying {
    /// The song as the toolkit's one song face paints it.
    #[must_use]
    pub fn song(&self) -> ui::widget::Song {
        ui::widget::Song {
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            detail: String::new(),
            error: false,
            art: self.art.clone(),
            art_name: self.title.clone(),
        }
    }

    /// How far into the song it is, from 0 to 1, while the player says how
    /// long it is.
    #[must_use]
    pub fn progress(&self) -> Option<f64> {
        (self.duration_ms > 0.0).then(|| (self.position_ms / self.duration_ms).clamp(0.0, 1.0))
    }
}

/// What the widget's controls do: play or pause, go back a track, skip to
/// the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicAction {
    PlayPause,
    Previous,
    Next,
}

/// The reading the widget draws: the session writes it every frame, and the
/// layer reads it as it paints.
#[derive(Clone, Default)]
pub struct DeskMusic {
    playing: Rc<RefCell<Option<NowPlaying>>>,
}

impl DeskMusic {
    /// Show `playing` now, or nothing.
    pub fn set(&self, playing: Option<NowPlaying>) {
        replace(&self.playing, playing);
    }

    /// What plays now.
    #[must_use]
    pub fn get(&self) -> Option<NowPlaying> {
        read(&self.playing)
    }
}

fn read(playing: &Rc<RefCell<Option<NowPlaying>>>) -> Option<NowPlaying> {
    playing.borrow().clone()
}

fn replace(playing: &Rc<RefCell<Option<NowPlaying>>>, now: Option<NowPlaying>) {
    *playing.borrow_mut() = now;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_song_says_how_far_into_it_it_is_only_when_it_is_known() {
        let mut playing = NowPlaying {
            position_ms: 30_000.0,
            duration_ms: 120_000.0,
            ..NowPlaying::default()
        };
        assert_eq!(playing.progress(), Some(0.25));
        playing.duration_ms = 0.0;
        assert_eq!(playing.progress(), None);
        playing.duration_ms = 60_000.0;
        playing.position_ms = 90_000.0;
        assert_eq!(
            playing.progress(),
            Some(1.0),
            "a playhead past the end stays in the song"
        );
    }
}
