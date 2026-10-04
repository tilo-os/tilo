//! The action bus: how leaf widgets (cards, nav items, toggles, the
//! scrubber) tell the root [`crate::app::TubeApp`] what the user did.
//!
//! Tube's screens are pure functions of the model, so their controls do not
//! hold the root's state: every interactive widget holds a clone of the
//! `ActionBus` and pushes an [`Action`] (open *this* video, skip to *this*
//! fraction, set *this* like). The root drains the queue after each event:
//! library edits and confirmed effects go through the framework's door,
//! the rest changes what this window shows.

use std::cell::RefCell;
use std::rc::Rc;

use crate::actions::{TubeAction, TubeEffect};
use crate::model::{ChannelId, PlaylistId, Route, SearchType, VideoId};
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
    /// Fraction of the current video's duration, in `[0, 1]`.
    Seek(f64),
    TogglePlay,
    /// Skip to `to` seconds, offered for a `Show`-mode segment the playhead
    /// is currently inside.
    SkipSegment(f64),
    SubmitSearch,
    Share(VideoId),
    /// Only visible on a narrow window, where the rail is an overlay flap.
    ToggleSidebar,
    /// Ask the host to open its search palette, which finds Tube's videos,
    /// creators, playlists and watch history among everything else.
    OpenSearch,
    /// A change to the shared library, submitted as it is through the
    /// framework's door.
    Edit(TubeAction),
    /// An effect the framework confirms before it runs: clearing the watch
    /// history, approving or revoking a device, syncing.
    Confirm(TubeEffect),
    /// Pair with the Grayjay invite currently held in the local-only input.
    PairDevice,
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
            Self::Seek(_) => "Seek video",
            Self::TogglePlay => "Play or pause",
            Self::SkipSegment(_) => "Skip segment",
            Self::SubmitSearch => "Search",
            Self::Share(_) => "Copy share link",
            Self::ToggleSidebar => "Show or hide navigation",
            Self::OpenSearch => "Open search",
            Self::Edit(TubeAction::SetVideoLiked { .. }) => "Like or unlike video",
            Self::Edit(TubeAction::SetQueueMembership { .. }) => "Add to or remove from queue",
            Self::Edit(TubeAction::SetSubscription { .. }) => "Subscribe or unsubscribe",
            Self::Edit(TubeAction::SetSetting { .. }) => "Change setting",
            Self::Edit(TubeAction::SetPreferredQuality { .. }) => "Change playback quality",
            Self::Edit(TubeAction::SetSponsorCategory { .. }) => "Change segment handling",
            Self::Edit(TubeAction::SetCommentLiked { .. }) => "Like or unlike comment",
            Self::Edit(TubeAction::SetCommentReply { .. }) => "Reply to comment",
            Self::Confirm(effect) => effect.label(),
            Self::PairDevice => "Pair Grayjay device",
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
