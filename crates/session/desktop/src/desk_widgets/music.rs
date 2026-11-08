//! The music desk widget's content: what plays now, as the session reads it.
//! The reading is not made here. The session takes it from the one now-playing reading the panel
//! and the lock screen also show ([`NowPlaying`], made from the audio service's reading of the
//! players) and keeps it here every frame, so the widget is a view of that one reading and never a
//! reader of its own; the buttons it carries ask the session for the transport, which hands the
//! same request to the player the reading names ([`MusicAction`]).
//! Nothing plays: there is no reading, and the widget has nothing to show.

use std::cell::RefCell;
use std::rc::Rc;

pub use tilo_desktop_surface::now_playing::NowPlaying;

/// What the widget's controls do: play or pause, go back a track, skip to the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicAction {
    PlayPause,
    Previous,
    Next,
}

/// The reading the widget draws: the session writes it every frame, and the layer reads it as it
/// paints.
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
