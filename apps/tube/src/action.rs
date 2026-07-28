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
    /// Open a playlist page (its title, creator and video list).
    OpenPlaylist(PlaylistId),
    /// Toggle a type in the search results' multi-select filter (first click
    /// narrows to just that type; see [`crate::model::SearchTypeFilter`]).
    ToggleSearchType(SearchType),
    /// Pop the navigation stack — return to the previous view. Backed by the
    /// toolkit's `ui::NavStack`; also reached via Escape / Alt-Left / edge-swipe.
    Back,
    ToggleSubscribe(ChannelId),
    /// Seek to a fraction [0,1] of the current video's duration.
    Seek(f64),
    TogglePlay,
    /// Cycle a SponsorBlock category's mode (auto-skip → show → off) on the
    /// settings page.
    ToggleCategory(Category),
    /// Manually skip the current watch video past a segment, to `to` seconds —
    /// the button offered for a `Show`-mode segment the playhead is inside.
    SkipSegment(f64),
    /// Flip a named boolean setting.
    ToggleSetting(SettingFlag),
    /// Run a search using the current contents of the search box.
    SubmitSearch,
    SetQuality(String),
    /// Toggle the local "liked" flag on a video (reflected on the watch page's
    /// like pill + count).
    ToggleLike(VideoId),
    /// Copy a shareable link to the given video to the system clipboard.
    Share(VideoId),
    /// Flip the navigation sidebar (the hamburger toggle). Only has a visible
    /// effect on a narrow window, where the rail is an overlay flap.
    ToggleSidebar,
    /// Open the Ctrl+K global command palette overlay.
    OpenPalette,
    /// Close the command palette.
    ClosePalette,
    /// Activate a palette deep-link hit (`tube://watch/<id>` or
    /// `tube://channel/<id>`) — routes to the right screen and closes the
    /// palette.
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
