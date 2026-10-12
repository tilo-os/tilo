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
use std::collections::VecDeque;
use std::rc::Rc;

use crate::actions::TubeEffect;
use crate::model::{ChannelId, PlaylistId, Route, SearchType, Settings, VideoId};
use crate::resource::Library;
use ui::tilo_resource::Edit;

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
    Edit(Edit<Library>),
    /// An effect, run through the framework's door, which confirms the ones
    /// that ask for it (clearing the watch history, approving or revoking a
    /// device) first.
    Effect(TubeEffect),
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
            Self::Edit(edit) => {
                let paths = Library::paths();
                let edits = |field: &[ui::tilo_resource::Segment]| edit.path().starts_with(field);
                if edits(paths.likes.segments()) {
                    "Like or unlike video"
                } else if edits(paths.queue.segments()) {
                    "Add to or remove from queue"
                } else if edits(paths.subscriptions.segments()) {
                    "Subscribe or unsubscribe"
                } else if edits(paths.comment_likes.segments()) {
                    "Like or unlike comment"
                } else {
                    "Change the library"
                }
            }
            Self::Effect(effect) => effect.label(),
            Self::PairDevice => "Pair Grayjay device",
        }
    }
}

/// Named boolean preferences the settings page can flip.
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

impl SettingFlag {
    /// Every flag, in the order the published settings list them.
    pub const ALL: [SettingFlag; 7] = [
        SettingFlag::Autoplay,
        SettingFlag::ShowComments,
        SettingFlag::SponsorBlock,
        SettingFlag::DeArrow,
        SettingFlag::DeArrowTitles,
        SettingFlag::DeArrowThumbnails,
        SettingFlag::ReturnDislike,
    ];

    /// The stable name an effect names this preference by.
    pub fn id(self) -> &'static str {
        match self {
            SettingFlag::SponsorBlock => "sponsorblock",
            SettingFlag::DeArrow => "dearrow",
            SettingFlag::DeArrowTitles => "dearrow-titles",
            SettingFlag::DeArrowThumbnails => "dearrow-thumbnails",
            SettingFlag::ReturnDislike => "dislikes",
            SettingFlag::Autoplay => "autoplay",
            SettingFlag::ShowComments => "comments",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|flag| flag.id() == id)
    }

    /// What the preference is called.
    pub fn name(self) -> &'static str {
        match self {
            SettingFlag::SponsorBlock => "SponsorBlock",
            SettingFlag::DeArrow => "DeArrow",
            SettingFlag::DeArrowTitles => "DeArrow titles",
            SettingFlag::DeArrowThumbnails => "DeArrow thumbnails",
            SettingFlag::ReturnDislike => "Dislike counts",
            SettingFlag::Autoplay => "Autoplay",
            SettingFlag::ShowComments => "Comments",
        }
    }

    /// Whether it is on in `settings`.
    pub fn get(self, settings: &Settings) -> bool {
        match self {
            SettingFlag::SponsorBlock => settings.sponsorblock_enabled,
            SettingFlag::DeArrow => settings.dearrow_enabled,
            SettingFlag::DeArrowTitles => settings.dearrow_titles,
            SettingFlag::DeArrowThumbnails => settings.dearrow_thumbnails,
            SettingFlag::ReturnDislike => settings.return_dislike_enabled,
            SettingFlag::Autoplay => settings.autoplay,
            SettingFlag::ShowComments => settings.show_comments,
        }
    }

    /// Turn it on or off in `settings`.
    pub fn set(self, settings: &mut Settings, enabled: bool) {
        *self.slot(settings) = enabled;
    }

    fn slot(self, settings: &mut Settings) -> &mut bool {
        match self {
            SettingFlag::SponsorBlock => &mut settings.sponsorblock_enabled,
            SettingFlag::DeArrow => &mut settings.dearrow_enabled,
            SettingFlag::DeArrowTitles => &mut settings.dearrow_titles,
            SettingFlag::DeArrowThumbnails => &mut settings.dearrow_thumbnails,
            SettingFlag::ReturnDislike => &mut settings.return_dislike_enabled,
            SettingFlag::Autoplay => &mut settings.autoplay,
            SettingFlag::ShowComments => &mut settings.show_comments,
        }
    }
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
pub type Invocations = Rc<RefCell<VecDeque<ui::tilo_actions::ControlInvocation>>>;

pub fn new_invocations() -> Invocations {
    Rc::new(RefCell::new(VecDeque::new()))
}

/// Queue an invocation of `binding`, when the control has one.
pub fn invoke(queue: &Invocations, binding: Option<ui::tilo_actions::ActionBinding<()>>) {
    if let Some(invocation) = binding.and_then(|binding| binding.invoke(()).ok()) {
        queue.borrow_mut().push_back(invocation);
    }
}
