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
use serde::{Deserialize, Serialize};

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
    /// Approve the Grayjay invite currently held in the local-only input.
    PairDevice,
    SyncDevices,
    ApproveDevice(String),
    RevokeDevice(String),
}

impl Action {
    /// Concise, stable fallback used by Tube's custom clickable surfaces for
    /// screen readers, keyboard focus and hover help. Call sites may render a
    /// richer visible title, but a control must never become an unnamed box.
    pub fn accessible_label(&self) -> &'static str {
        match self {
            Self::Navigate(Route::Home) => "Open Home",
            Self::Navigate(Route::Trending) => "Open Trending",
            Self::Navigate(Route::Subscriptions) => "Open subscriptions",
            Self::Navigate(Route::History) => "Open watch history",
            Self::Navigate(Route::Settings) => "Open settings",
            Self::Navigate(_) => "Open view",
            Self::OpenVideo(_) => "Open video",
            Self::OpenChannel(_) => "Open creator",
            Self::OpenPlaylist(_) => "Open playlist",
            Self::ToggleSearchType(_) => "Toggle result type",
            Self::Back => "Go back",
            Self::ToggleSubscribe(_) => "Subscribe or unsubscribe",
            Self::Seek(_) => "Seek video",
            Self::TogglePlay => "Play or pause",
            Self::ToggleCategory(_) => "Change segment handling",
            Self::SkipSegment(_) => "Skip segment",
            Self::ToggleSetting(_) => "Change setting",
            Self::SubmitSearch => "Search",
            Self::SetQuality(_) => "Change playback quality",
            Self::ToggleLike(_) => "Like or unlike video",
            Self::Share(_) => "Copy share link",
            Self::ToggleSidebar => "Show or hide navigation",
            Self::OpenPalette => "Search Tube actions",
            Self::ClosePalette => "Close action search",
            Self::ActivateHit(_) => "Open search result",
            Self::PairDevice => "Pair Grayjay device",
            Self::SyncDevices => "Sync approved devices now",
            Self::ApproveDevice(_) => "Approve device access",
            Self::RevokeDevice(_) => "Revoke device access",
        }
    }
}

/// Named boolean settings the settings page can flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
