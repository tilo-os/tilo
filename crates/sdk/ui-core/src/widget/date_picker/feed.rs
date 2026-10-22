//! A month navigator kept by an app that owns the date it shows
//! ([`DateFeed`]).

use std::cell::RefCell;
use std::rc::Rc;

use chrono::NaiveDate;

use super::DatePicker;

/// What the app shows in a [`DatePicker`] it feeds: the chosen day, the
/// day it calls today and the days that hold something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateShown {
    pub selected: NaiveDate,
    pub today: NaiveDate,
    pub marked: Vec<NaiveDate>,
}

/// The app's side of a [`DatePicker`] that stands outside the app's own
/// surface (a calendar's month navigator in the shell's sidebar): the app
/// shows its day with [`Self::show`] and takes the days the person chose
/// with [`Self::take_chosen`]. The app's day stays the one truth; the
/// picker reads the feed at every sync and update ([`DatePicker::fed_by`]).
#[derive(Clone, Default)]
pub struct DateFeed(Rc<RefCell<Feed>>);

#[derive(Default)]
struct Feed {
    shown: Option<DateShown>,
    /// Bumped whenever `shown` changes: each picker reading the feed
    /// keeps the revision it last read.
    revision: u64,
    chosen: Vec<NaiveDate>,
}

impl DateFeed {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Show this. Cheap when nothing changed; a changed day brings its
    /// month into view.
    pub fn show(&self, shown: DateShown) {
        let mut feed = self.0.borrow_mut();
        if feed.shown.as_ref() != Some(&shown) {
            feed.shown = Some(shown);
            feed.revision = feed.revision.wrapping_add(1);
        }
    }

    /// Put the picker away (something else has the room); the next
    /// [`Self::show`] brings it back. Pair it with
    /// [`crate::widget::When`] on [`Self::is_shown`].
    pub fn withdraw(&self) {
        let mut feed = self.0.borrow_mut();
        if feed.shown.take().is_some() {
            feed.revision = feed.revision.wrapping_add(1);
        }
    }

    /// What the app last showed.
    #[must_use]
    pub fn shown(&self) -> Option<DateShown> {
        self.0.borrow().shown.clone()
    }

    /// Whether the app shows the picker now.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.0.borrow().shown.is_some()
    }

    /// The days the person chose since the last call, oldest first.
    #[must_use]
    pub fn take_chosen(&self) -> Vec<NaiveDate> {
        std::mem::take(&mut self.0.borrow_mut().chosen)
    }

    /// What a picker that last read revision `seen` has not read yet.
    fn pull(&self, seen: &mut Option<u64>) -> Option<DateShown> {
        let feed = self.0.borrow();
        if *seen == Some(feed.revision) {
            return None;
        }
        *seen = Some(feed.revision);
        feed.shown.clone()
    }
}

impl DatePicker {
    /// Show what `feed` shows, and hand it the days the person chooses.
    #[must_use]
    pub fn fed_by(self, feed: &DateFeed) -> Self {
        let chooses = feed.clone();
        let mut picker = self.on_change(move |day| chooses.0.borrow_mut().chosen.push(day));
        picker.feed = Some(feed.clone());
        picker.pull_feed();
        picker
    }

    /// Read the feed; whether anything changed.
    pub(super) fn pull_feed(&mut self) -> bool {
        let Some(feed) = self.feed.as_ref() else {
            return false;
        };
        let Some(shown) = feed.pull(&mut self.feed_seen) else {
            return false;
        };
        let moved = shown.selected != self.selected;
        self.today = Some(shown.today);
        self.set_marked(shown.marked);
        if moved {
            self.set_selected(shown.selected);
        }
        true
    }
}
