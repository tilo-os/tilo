//! A navigation list kept by an app that paints the rest of its window
//! itself ([`NavFeed`]).

use std::cell::RefCell;
use std::rc::Rc;

use vello::kurbo::{Point, Rect};

use super::{NavList, NavSection};

/// The app's side of a [`NavList`] that stands in a shell around it: the
/// app shows its places and the chosen one with [`Self::show`], and takes
/// what the person chose with [`Self::take_chosen`] and the section
/// actions they ran ("New playlist") with [`Self::take_section_actions`],
/// the rows they checked with [`Self::take_checks`] and what they folded
/// with [`Self::take_expands`]. The list reads the feed at every sync and
/// update ([`NavList::fed_by`]).
#[derive(Clone, Default)]
pub struct NavFeed(Rc<RefCell<Feed>>);

#[derive(Default)]
struct Feed {
    /// What the list shows, as the app last said.
    shown: Option<(Vec<NavSection>, Option<String>)>,
    /// Bumped whenever `shown` changes: each list reading the feed keeps
    /// the revision it last read.
    revision: u64,
    /// What the person chose since the app last looked, oldest first.
    chosen: Vec<String>,
    /// The section actions the person ran, as (section id, action id).
    section_actions: Vec<(String, String)>,
    /// The rows the person checked or cleared, as (place id, checked).
    checks: Vec<(String, bool)>,
    /// The sections and rows the person opened or folded, as (id, open).
    expands: Vec<(String, bool)>,
    /// The item actions the person ran, as (place id, action id).
    item_actions: Vec<(String, String)>,
    /// Where the list's rows stood when it last painted, by place id.
    rows: Vec<(String, Rect)>,
}

impl NavFeed {
    /// An empty feed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Show these places, with `selected` chosen. Cheap when nothing
    /// changed.
    pub fn show(&self, sections: Vec<NavSection>, selected: Option<String>) {
        let next = Some((sections, selected));
        let mut feed = self.0.borrow_mut();
        if feed.shown != next {
            feed.shown = next;
            feed.revision = feed.revision.wrapping_add(1);
        }
    }

    /// What the app last showed: the places and the chosen one.
    #[must_use]
    pub fn shown(&self) -> Option<(Vec<NavSection>, Option<String>)> {
        self.0.borrow().shown.clone()
    }

    /// The places the person chose since the last call, oldest first.
    #[must_use]
    pub fn take_chosen(&self) -> Vec<String> {
        std::mem::take(&mut self.0.borrow_mut().chosen)
    }

    /// The section actions the person ran since the last call, as
    /// (section id, action id), oldest first.
    #[must_use]
    pub fn take_section_actions(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.0.borrow_mut().section_actions)
    }

    /// The rows the person checked or cleared since the last call, as
    /// (place id, now checked), oldest first.
    #[must_use]
    pub fn take_checks(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.0.borrow_mut().checks)
    }

    /// The sections and rows the person opened or folded since the last
    /// call, as (id, now open), oldest first: what the app keeps folded.
    #[must_use]
    pub fn take_expands(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.0.borrow_mut().expands)
    }

    /// The item actions the person ran since the last call, as (place id,
    /// action id), oldest first ([`NavList::fed_actions`]).
    #[must_use]
    pub fn take_item_actions(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.0.borrow_mut().item_actions)
    }

    /// The place whose row is at `point` (window coordinates) and its row,
    /// as the list last painted them: where a drag the app runs from its
    /// own content would be let go ("onto this playlist").
    #[must_use]
    pub fn place_at(&self, point: Point) -> Option<(String, Rect)> {
        self.0
            .borrow()
            .rows
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .cloned()
    }

    pub(super) fn publish_rows(&self, rows: Vec<(String, Rect)>) {
        self.0.borrow_mut().rows = rows;
    }

    /// What a list that last read revision `seen` has not read yet.
    fn pull(&self, seen: &mut Option<u64>) -> Option<(Vec<NavSection>, Option<String>)> {
        let feed = self.0.borrow();
        if *seen == Some(feed.revision) {
            return None;
        }
        *seen = Some(feed.revision);
        feed.shown.clone()
    }
}

impl NavList {
    /// Show what `feed` shows, and hand it the person's choices.
    #[must_use]
    pub fn fed_by(self, feed: &NavFeed) -> Self {
        let (reads, chooses, acts) = (feed.clone(), feed.clone(), feed.clone());
        let (checks, folds) = (feed.clone(), feed.clone());
        let mut list = self
            .follow({
                let mut seen = None;
                move || reads.pull(&mut seen)
            })
            .on_select(move |id| chooses.0.borrow_mut().chosen.push(id))
            .on_section_action(move |section, action| {
                acts.0
                    .borrow_mut()
                    .section_actions
                    .push((section.to_owned(), action.to_owned()));
            })
            .on_check(move |id, checked| {
                checks.0.borrow_mut().checks.push((id.to_owned(), checked));
            })
            .on_expand(move |id, open| folds.0.borrow_mut().expands.push((id.to_owned(), open)));
        list.feed = Some(feed.clone());
        list
    }

    /// Offer `actions` at the trailing edge of the rows that do not opt out
    /// ([`super::NavItem::without_actions`]), and hand what the person runs
    /// to `feed` ([`NavFeed::take_item_actions`]).
    #[must_use]
    pub fn fed_actions(self, feed: &NavFeed, actions: Vec<super::NavAction>) -> Self {
        let runs = feed.clone();
        self.item_actions(actions, move |place, action| {
            runs.0
                .borrow_mut()
                .item_actions
                .push((place.to_owned(), action.to_owned()));
        })
    }
}
