//! The pages a pane drills into: a network's page under the Wi-Fi list, a
//! VPN's under the VPN list.
//!
//! A pane opens a page by pushing it here ([`PanePages::open`]); the window
//! shows the page's title in the content header with the shell's Back chip
//! ([`ui::widget::NavShell::on_back`]), and Back, Escape or the narrow
//! window's back gesture pop it. The pane itself decides what a page shows:
//! it reads the page on top ([`PanePages::top`]) when it composes, and its
//! model notices a pop by the revision ([`PanePages::revision`]). Opening
//! another pane, or a page nobody can show any more, leaves the trail empty.
//!
//! Every clone shares one trail: the window and every pane's context hold
//! the same one.

use std::cell::RefCell;
use std::rc::Rc;

use crate::categories::Category;

/// One page a pane drilled into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// The pane it belongs to.
    pub category: Category,
    /// What the pane calls it: the pane's own key (`network:<ssid>`).
    pub key: String,
    /// What the content header says while it is shown.
    pub title: String,
}

#[derive(Debug, Default)]
struct Trail {
    pages: Vec<Page>,
    revision: u64,
}

/// The trail of pages the person drilled into. See the [module docs](self).
#[derive(Clone, Debug, Default)]
pub struct PanePages(Rc<RefCell<Trail>>);

impl PanePages {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drill into `key` of `category`'s pane, titled `title`. A page of
    /// another pane is left: the trail is one pane's. Opening the page on
    /// top again only retitles it.
    pub fn open(&self, category: Category, key: impl Into<String>, title: impl Into<String>) {
        let key = key.into();
        let title = title.into();
        let mut trail = self.0.borrow_mut();
        trail.pages.retain(|page| page.category == category);
        match trail.pages.last_mut() {
            Some(top) if top.key == key => {
                if top.title == title {
                    return;
                }
                top.title = title;
            }
            _ => trail.pages.push(Page {
                category,
                key,
                title,
            }),
        }
        trail.revision += 1;
    }

    /// Replace the page on top with `key` (a page that became another: a
    /// network added under a new name), or open it when none is shown.
    pub fn replace(&self, category: Category, key: impl Into<String>, title: impl Into<String>) {
        {
            let mut trail = self.0.borrow_mut();
            if trail
                .pages
                .last()
                .is_some_and(|page| page.category == category)
            {
                trail.pages.pop();
            }
        }
        self.open(category, key, title);
    }

    /// Go back one page. Whether there was one to leave.
    pub fn back(&self) -> bool {
        let mut trail = self.0.borrow_mut();
        if trail.pages.pop().is_none() {
            return false;
        }
        trail.revision += 1;
        true
    }

    /// Leave every page: the pane shows its first page.
    pub fn clear(&self) {
        let mut trail = self.0.borrow_mut();
        if trail.pages.is_empty() {
            return;
        }
        trail.pages.clear();
        trail.revision += 1;
    }

    /// Leave `key` and every page above it, wherever it is in the trail:
    /// what it showed is gone (a network forgotten).
    pub fn close(&self, category: Category, key: &str) {
        let mut trail = self.0.borrow_mut();
        let Some(at) = trail
            .pages
            .iter()
            .position(|page| page.category == category && page.key == key)
        else {
            return;
        };
        trail.pages.truncate(at);
        trail.revision += 1;
    }

    /// The page `category`'s pane shows on top, if it drilled into one.
    #[must_use]
    pub fn top(&self, category: Category) -> Option<Page> {
        self.0
            .borrow()
            .pages
            .last()
            .filter(|page| page.category == category)
            .cloned()
    }

    /// Every page of `category`'s pane, the first one opened first.
    #[must_use]
    pub fn trail(&self, category: Category) -> Vec<Page> {
        self.0
            .borrow()
            .pages
            .iter()
            .filter(|page| page.category == category)
            .cloned()
            .collect()
    }

    /// Whether Back has a page to leave.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.0.borrow().pages.is_empty()
    }

    /// Changes with every page opened, retitled or left.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.borrow().revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_open_on_top_and_back_leaves_them_one_at_a_time() {
        let pages = PanePages::new();
        let start = pages.revision();
        assert!(!pages.can_go_back());
        pages.open(Category::Network, "all", "All networks");
        pages.open(Category::Network, "wifi:Home", "Home");
        assert_eq!(pages.top(Category::Network).unwrap().key, "wifi:Home");
        assert!(pages.top(Category::Bluetooth).is_none());
        assert!(pages.revision() > start);
        let before = pages.revision();
        pages.open(Category::Network, "wifi:Home", "Home");
        assert_eq!(
            pages.revision(),
            before,
            "opening the page on top again is nothing"
        );
        assert!(pages.back());
        assert_eq!(pages.top(Category::Network).unwrap().key, "all");
        assert!(pages.back());
        assert!(!pages.back(), "nothing left to leave");
        assert!(pages.top(Category::Network).is_none());
    }

    #[test]
    fn a_closed_page_takes_the_pages_above_it_and_another_pane_starts_afresh() {
        let pages = PanePages::new();
        pages.open(Category::Network, "known", "Known networks");
        pages.open(Category::Network, "wifi:Cafe", "Cafe");
        pages.close(Category::Network, "known");
        assert!(pages.trail(Category::Network).is_empty());
        pages.open(Category::Network, "vpn", "VPN");
        pages.open(Category::Bluetooth, "device", "Headphones");
        assert_eq!(pages.trail(Category::Network), Vec::new());
        assert_eq!(pages.trail(Category::Bluetooth).len(), 1);
        pages.replace(Category::Bluetooth, "other", "Speaker");
        assert_eq!(pages.top(Category::Bluetooth).unwrap().title, "Speaker");
        assert_eq!(pages.trail(Category::Bluetooth).len(), 1);
    }
}
