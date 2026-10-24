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

use crate::actions::TubeEffect;
use crate::model::{ChannelId, Route, SearchType, Settings, VideoId};
use crate::resource::Library;
use ui::tilo_resource::Edit;

/// Everything a click can request.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Navigate(Route),
    OpenVideo(VideoId),
    OpenChannel(ChannelId),
    /// Toggle a type in the multi-select filter; first click narrows to just
    /// that type (see [`crate::model::SearchTypeFilter`]).
    ToggleSearchType(SearchType),
    /// Pop the navigation stack (`ui::NavStack`); also reached via Escape /
    /// Alt-Left / edge-swipe.
    Back,
    /// Fraction of the current video's duration, in `[0, 1]`.
    Seek(f64),
    TogglePlay,
    /// Ask for the open video's stream again after no source played it.
    RetryPlayback,
    SubmitSearch,
    Share(VideoId),
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
