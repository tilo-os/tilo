//! A navigation list kept by an app that paints the rest of its window
//! itself ([`NavFeed`]).

use std::cell::RefCell;
use std::rc::Rc;

use super::{NavList, NavSection};

/// The app's side of a [`NavList`] that stands in a shell around it: the
/// app shows its places and the chosen one with [`Self::show`], and takes
/// what the person chose with [`Self::take_chosen`] and the section
/// actions they ran ("New playlist") with [`Self::take_section_actions`]. The list reads the
/// feed at every sync and update ([`NavList::fed_by`]).
#[derive(Clone, Default)]
pub struct NavFeed(Rc<RefCell<Feed>>);

#[derive(Default)]
struct Feed {
    /// What the list shows, as the app last said.
    shown: Option<(Vec<NavSection>, Option<String>)>,
    /// Whether the list has yet to read `shown`.
    fresh: bool,
    /// What the person chose since the app last looked, oldest first.
    chosen: Vec<String>,
    /// The section actions the person ran, as (section id, action id).
    section_actions: Vec<(String, String)>,
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
            feed.fresh = true;
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

    /// What the list has not read yet.
    fn pull(&self) -> Option<(Vec<NavSection>, Option<String>)> {
        let mut feed = self.0.borrow_mut();
        if !std::mem::take(&mut feed.fresh) {
            return None;
        }
        feed.shown.clone()
    }
}

impl NavList {
    /// Show what `feed` shows, and hand it the person's choices.
    #[must_use]
    pub fn fed_by(self, feed: &NavFeed) -> Self {
        let (reads, chooses, acts) = (feed.clone(), feed.clone(), feed.clone());
        self.follow(move || reads.pull())
            .on_select(move |id| chooses.0.borrow_mut().chosen.push(id))
            .on_section_action(move |section, action| {
                acts.0
                    .borrow_mut()
                    .section_actions
                    .push((section.to_owned(), action.to_owned()));
            })
    }
}
