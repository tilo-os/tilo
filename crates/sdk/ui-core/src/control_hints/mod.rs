//! Control hints: a small lit card that teaches one control at the moment
//! it is useful, beside where it is done, and goes the moment it is done.
//!
//! A host registers its set of hints as data ([`HintSpec`]): when each is
//! useful ([`Cue`]), what must hold while it shows, which of the person's
//! actions mean they know it, and the gesture its picture shows. It feeds
//! the person's actions in as they happen ([`Coach::observe`], usually from
//! [`performed`]), and every frame says what holds and whether now is a
//! quiet moment ([`Quiet`]). The [`Coach`] decides; [`HintLayer`] draws.
//!
//! The rules every hint keeps:
//!
//! - A hint whose action the person already did never shows: experts see
//!   nothing. One that is on screen acknowledges it and goes.
//! - One hint at a time, and at most one beginning a minute; none while
//!   the person types, is in a call or follows someone.
//! - An unanswered hint goes on its own after [`coach::IGNORED_AFTER`] and
//!   may come back once, much later. No buttons, no counter, no scrim.
//! - What is learned is the person's own record ([`Learned`]), kept with
//!   their profile so another device does not teach it again; one setting
//!   turns hints off.

mod art;
mod card;
pub mod coach;
mod layer;
pub mod performed;

#[cfg(test)]
mod tests;

pub use coach::{Coach, Current, Phase, Timing};
pub use layer::{HintLayer, HintLook, HintPlace};

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
}

/// A moment a hint is useful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue<S> {
    /// This long into the session.
    After(Duration),
    /// The person just did this (fresh for [`coach::CUE_FRESH`]).
    Saw(S),
    /// This holds now.
    Holds(S),
}

/// Why no hint may begin now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    /// The person is typing.
    Typing,
    /// The person is in a call: a hint that shows goes.
    Call,
    /// The person follows someone: a hint that shows goes.
    Following,
    /// Something else holds the person's attention (a menu, search, a
    /// sheet): the hint waits.
    Busy,
}

/// The gesture a hint's picture shows, drawn with vector shapes and looped
/// while it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// A key chord pressed and a window coming up from search.
    OpenFromSearch,
    /// A pointer taking a window by its bar and carrying it.
    MoveWindow,
    /// A pointer drawing a window's edge out and back.
    ResizeEdge,
    /// A pointer dragging the canvas, the windows sliding with it.
    PanView,
}

/// What the person's own record keeps about control hints: which they
/// know, how often each showed, and whether hints show at all. It travels
/// with their personal settings, so a hint learned on one device is learned
/// on all.
pub use settings_store::control_hints::Learned;
