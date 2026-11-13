//! Control hints: a small lit card that teaches one control at the moment it is useful, beside where it
//! is done, and goes the moment it is done. A host registers its set of hints as data ([`HintSpec`]):
//! when each is useful ([`Cue`]), what must hold while it shows, which of the person's actions mean
//! they know it, and the gesture its picture shows.

mod art;
mod card;
pub mod coach;
mod layer;
pub mod performed;

#[cfg(test)]
mod tests;

pub use coach::{Coach, Current, Phase, Timing};
pub use layer::{HintLayer, HintLook, HintPlace, Stand};

use crate::time::Duration;

/// One control hint, as data.
#[derive(Debug)]
pub struct HintSpec<S: 'static> {
    /// Its stable name in the person's record.
    pub id: &'static str,
    /// When it is useful: any one of these.
    pub when: &'static [Cue<S>],
    /// What must hold while it may begin: all of these.
    pub only_while: &'static [S],
    /// The person's actions that mean they know it: any one of these.
    pub learned_by: &'static [S],
    /// What its picture shows.
    pub gesture: Gesture,
    /// Its words: a catalogue key, a few words at most.
    pub words: &'static str,
    /// Its words for a person who points by touch, where a finger does it differently: a catalogue key.
    pub touch_words: Option<&'static str>,
}

/// A moment a hint is useful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue<S> {
    /// This long into the session.
    After(Duration),
    /// The person just did this (fresh for [`coach::CUE_FRESH`]).
    Saw(S),
    /// This has now.
    Holds(S),
}

/// Why no hint may begin now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    Typing,
    /// The person is in a call: a hint that shows goes.
    Call,
    /// The person follows someone: a hint that shows goes.
    Following,
    /// Something else has the person's attention (a menu, search, a sheet): the hint waits.
    Busy,
}

/// The gesture a hint's picture shows, drawn with vector shapes and looped while it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// A key chord pressed and a window coming up from search.
    OpenFromSearch,
    /// A pointer taking a window by its bar and carrying it.
    MoveWindow,
    /// A pointer drawing a window's edge out and back.
    ResizeEdge,
    /// The windows sliding sideways under the person's fingers: two on a touchpad scrolling sideways,
    /// or one swiping a touchscreen.
    PanView,
}

/// What the person record keeps about control hints: which they know, how often each showed, and
/// whether hints show at all.
pub use settings_store::control_hints::Learned;
