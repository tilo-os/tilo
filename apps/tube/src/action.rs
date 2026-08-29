//! The action bus: how leaf widgets (cards, nav items, toggles, the
//! scrubber) tell the root [`crate::app::TubeApp`] what the user did.
//!
//! The toolkit's own widgets mutate the CRDT directly on click. Our
//! navigation is richer than a boolean flag per control (open *this*
//! video, subscribe to *that* channel, skip to *this* fraction), so we
//! use a tiny shared queue instead: every interactive widget holds a
//! clone of the `ActionBus` and pushes an [`Action`]; the root drains the
//! queue after each event, applies it to the model, and flags a rebuild.

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::{Category, ChannelId, PlaylistId, Route, SearchType, VideoId};

/// Everything a click can request.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Navigate(Route),
    OpenVideo(VideoId),
    OpenChannel(ChannelId),
    OpenPlaylist(PlaylistId),
    /// Toggle a type in the multi-select filter; first click narrows to just
    /// that type (see [`crate::model::SearchTypeFilter`]).
    ToggleSearchType(SearchType),
    /// Pop the navigation stack (`ui::NavStack`); also reached via Escape /
    /// Alt-Left / edge-swipe.
    Back,
    ToggleSubscribe(ChannelId),
    /// Fraction of the current video's duration, in `[0, 1]`.
    Seek(f64),
    TogglePlay,
    /// Cycle a `SponsorBlock` category through auto-skip → show → off.
    ToggleCategory(Category),
    /// Skip to `to` seconds, offered for a `Show`-mode segment the playhead
    /// is currently inside.
    SkipSegment(f64),
    ToggleSetting(SettingFlag),
    SubmitSearch,
    SetQuality(String),
    ToggleLike(VideoId),
    Share(VideoId),
    /// Only visible on a narrow window, where the rail is an overlay flap.
    ToggleSidebar,
    OpenPalette,
    ClosePalette,
    /// `hit` is a deep-link (`tube://watch/<id>`, `tube://channel/<id>`);
    /// routes to the right screen and closes the palette.
    ActivateHit(String),
}

/// Named boolean settings the settings page can flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingFlag {
    SponsorBlock,
    DeArrow,
    DeArrowTitles,
    DeArrowThumbnails,
    ReturnDislike,
    Autoplay,
    ShowComments,
}

/// Shared, single-threaded queue of pending actions.
pub type ActionBus = Rc<RefCell<Vec<Action>>>;

pub fn new_bus() -> ActionBus {
    Rc::new(RefCell::new(Vec::new()))
}

pub fn push(bus: &ActionBus, action: Action) {
    bus.borrow_mut().push(action);
}

pub fn drain(bus: &ActionBus) -> Vec<Action> {
    std::mem::take(&mut *bus.borrow_mut())
}
