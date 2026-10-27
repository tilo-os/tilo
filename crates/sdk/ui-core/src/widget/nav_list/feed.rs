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
/// with [`Self::take_expands`], the menus they asked for with
/// [`Self::take_menus`] and the names they gave with [`Self::take_renames`].
/// The list reads the feed at every sync and update ([`NavList::fed_by`]).
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
    /// The person chose since the app last showed: the next show is the
    /// app's answer, read even where nothing changed, so a choice the app
    /// does not take (a call it joins, a row that is no place) is undone.
    answer_due: bool,
    /// The section actions the person ran, as (section id, action id).
    section_actions: Vec<(String, String)>,
    /// The rows the person checked or cleared, as (place id, checked).
    checks: Vec<(String, bool)>,
    /// The sections and rows the person opened or folded, as (id, open).
    expands: Vec<(String, bool)>,
    /// The item actions the person ran, as (place id, action id).
    item_actions: Vec<(String, String)>,
    /// The menus the person asked for, as (place or section id, the row's
    /// lift, where the menu opens).
    menus: Vec<(String, Rect, Point)>,
    /// The names the person gave in place, as (place or section id, name).
    renames: Vec<(String, String)>,
    /// The row the app asked to rename, until the list opens its field.
    rename: Option<String>,
    /// What the app asked to open or fold, until the list does.
    openings: Vec<(String, bool)>,
    /// The list's own layout node: where the keyboard goes to rename.
    node: Option<crate::taffy::NodeId>,
    /// Where the list's rows stood when it last painted, by place id.
    rows: Vec<(String, Rect)>,
    /// Where the list stood when it last painted.
    rect: Option<Rect>,
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
        let answer = std::mem::take(&mut feed.answer_due);
        if feed.shown != next || answer {
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

    /// The menus the person asked for since the last call, as (place or
    /// section id, the row's lift, where the menu opens), oldest first
    /// ([`NavList::fed_menus`]). Hand the lift to `ContextMenu::anchor_row`
    /// so the row stays marked while its menu is open.
    #[must_use]
    pub fn take_menus(&self) -> Vec<(String, Rect, Point)> {
        std::mem::take(&mut self.0.borrow_mut().menus)
    }

    /// The names the person gave rows or sections in place since the last
    /// call, as (id, name), oldest first ([`NavList::fed_renames`]).
    #[must_use]
    pub fn take_renames(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.0.borrow_mut().renames)
    }

    /// Open the rename field on the row or section `id` (its menu's
    /// Rename), handing the list the keyboard through `focus_target`, the
    /// one of the event that asked for it ([`crate::widget::EventCtx`]). The
    /// name comes back through [`Self::take_renames`].
    pub fn begin_rename(&self, id: &str, focus_target: &mut Option<crate::taffy::NodeId>) {
        let mut feed = self.0.borrow_mut();
        feed.rename = Some(id.to_owned());
        if let Some(node) = feed.node {
            *focus_target = Some(node);
        }
    }

    /// Open or fold the section or row `id` as the person would (its
    /// menu's Collapse): the list keeps what the person folds, so the app
    /// asks it rather than showing it folded.
    pub fn set_open(&self, id: &str, open: bool) {
        self.0.borrow_mut().openings.push((id.to_owned(), open));
    }

    /// Where the list stood when it last painted (window coordinates): with
    /// the app's own area, where a menu over the list may open.
    #[must_use]
    pub fn list_rect(&self) -> Option<Rect> {
        self.0.borrow().rect
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

    pub(super) fn publish_rows(&self, rect: Rect, rows: Vec<(String, Rect)>) {
        let mut feed = self.0.borrow_mut();
        feed.rect = Some(rect);
        feed.rows = rows;
    }

    pub(super) fn publish_node(&self, node: crate::taffy::NodeId) {
        self.0.borrow_mut().node = Some(node);
    }

    pub(super) fn take_rename_request(&self) -> Option<String> {
        self.0.borrow_mut().rename.take()
    }

    pub(super) fn take_openings(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.0.borrow_mut().openings)
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
            .on_select(move |id| {
                let mut feed = chooses.0.borrow_mut();
                feed.chosen.push(id);
                feed.answer_due = true;
            })
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

    /// Hand the menus the person asks for to `feed`
    /// ([`NavFeed::take_menus`]).
    #[must_use]
    pub fn fed_menus(self, feed: &NavFeed) -> Self {
        let asks = feed.clone();
        self.on_context_menu(move |id, lift, at| {
            asks.0.borrow_mut().menus.push((id.to_owned(), lift, at));
        })
    }

    /// Hand the names the person gives in place to `feed`
    /// ([`NavFeed::take_renames`]).
    #[must_use]
    pub fn fed_renames(self, feed: &NavFeed) -> Self {
        let names = feed.clone();
        self.on_rename(move |id, name| {
            names.0.borrow_mut().renames.push((id.to_owned(), name));
        })
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
