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
use std::collections::VecDeque;
use std::rc::Rc;

use crate::model::{Category, ChannelId, PlaylistId, Route, SkipMode, VideoId};
use serde::{Deserialize, Serialize};

/// Everything a click can request.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Navigate(Route),
    OpenVideo(VideoId),
    /// Clear this signed-in profile's synchronised watch-history collection.
    /// This only affects the signed-in owner's local history store.
    ClearHistory,
    OpenChannel(ChannelId),
    OpenPlaylist(PlaylistId),
    /// Pop the navigation stack (`ui::NavStack`); also reached via Escape /
    /// Alt-Left / edge-swipe.
    Back,
    ToggleSubscribe(ChannelId),
    /// Fraction of the current video's duration, in `[0, 1]`.
    Seek(f64),
    TogglePlay,
    /// Set how a `SponsorBlock` category is handled.
    SetCategory(Category, SkipMode),
    /// Skip to `to` seconds, offered for a `Show`-mode segment the playhead
    /// is currently inside.
    SkipSegment(f64),
    ToggleSetting(SettingFlag),
    SetQuality(String),
    ToggleLike(VideoId),
    Share(VideoId),
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
            Self::ClearHistory => "Clear watch history",
            Self::OpenChannel(_) => "Open creator",
            Self::OpenPlaylist(_) => "Open playlist",
            Self::Back => "Go back",
            Self::ToggleSubscribe(_) => "Subscribe or unsubscribe",
            Self::Seek(_) => "Seek video",
            Self::TogglePlay => "Play or pause",
            Self::SetCategory(..) => "Change segment handling",
            Self::SkipSegment(_) => "Skip segment",
            Self::ToggleSetting(_) => "Change setting",
            Self::SetQuality(_) => "Change playback quality",
            Self::ToggleLike(_) => "Like or unlike video",
            Self::Share(_) => "Copy share link",
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

/// Resource invocations raised by controls that report through a closure
/// (the navigation list, the watch toolbar). The root hands them to the host,
/// which resolves each against the published resources before it runs.
pub type Invocations = Rc<RefCell<VecDeque<tilo_actions::ControlInvocation>>>;

pub fn new_invocations() -> Invocations {
    Rc::new(RefCell::new(VecDeque::new()))
}

/// Queue an invocation of `binding`, when the control has one.
pub fn invoke(queue: &Invocations, binding: Option<tilo_actions::ActionBinding<()>>) {
    if let Some(invocation) = binding.and_then(|binding| binding.invoke(()).ok()) {
        queue.borrow_mut().push_back(invocation);
    }
}
