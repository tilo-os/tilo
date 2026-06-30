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

use crate::model::{Category, ChannelId, Route, VideoId};

/// Everything a click can request.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Navigate(Route),
    OpenVideo(VideoId),
    OpenChannel(ChannelId),
    ToggleSubscribe(ChannelId),
    /// Seek to a fraction [0,1] of the current video's duration.
    Seek(f64),
    TogglePlay,
    /// Toggle a SponsorBlock category on the settings page.
    ToggleCategory(Category),
    /// Flip a named boolean setting.
    ToggleSetting(SettingFlag),
    /// Run a search using the current contents of the search box.
    SubmitSearch,
    SetQuality(String),
    Like,
    /// Flip the navigation sidebar (the hamburger toggle). Only has a visible
    /// effect on a narrow window, where the rail is an overlay flap.
    ToggleSidebar,
}

/// Named boolean settings the settings page can flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingFlag {
    SponsorBlock,
    AutoSkip,
    DeArrow,
    DeArrowThumbnails,
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
