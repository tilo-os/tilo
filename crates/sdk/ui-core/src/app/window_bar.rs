//! The window bar's own controls for an app: its search in place and its
//! History control.
//!
//! The desktop draws one bar per window, and its search and History control
//! act on the window's app. For an app shown in a region of the shell's
//! scene the window reaches the app's widgets itself
//! ([`crate::widget::SearchableWindow`]); an app in a process of its own is
//! reached over its shell connection. The app says what its window offers
//! ([`WindowOffer`]: what the search acts on in place, the open search's
//! count, the History control) and the bar sends what the person did
//! ([`ToolbarEvent::Search`], [`ToolbarEvent::SearchStep`],
//! [`ToolbarEvent::SearchClose`], [`ToolbarEvent::History`]), which this
//! runs on the app's widgets with the same [`SearchDriver`] the window uses
//! and on the app's History door. Either way the app draws neither control
//! itself: its header drops the button that opens its search
//! ([`crate::widget::WindowHeader::set_window_presents_search`]) and its
//! History draws no toggle of its own.

#[cfg(feature = "native")]
use crate::widget::InPlace;
use crate::widget::SearchDriver;
#[cfg(feature = "native")]
use crate::widget::{FindAbilities, FindOptions};
use crate::window_toolbar::{SearchOffer, WindowOffer};
#[cfg(feature = "native")]
use crate::window_toolbar::{SearchOpening, ToolbarEvent};

use super::App;

/// The app's side of its window bar's search and History control.
#[cfg_attr(
    not(feature = "native"),
    expect(
        dead_code,
        reason = "the bar's search and History reach an app only through the native runner"
    )
)]
#[derive(Debug, Default)]
pub(crate) struct WindowBarRuntime {
    /// The window's bar presents search and History for this app.
    presents: bool,
    /// What the app's widgets offer the window's search, and what their
    /// document's find offers beyond matching; `None` until it is looked at
    /// again after the widget tree changed.
    offer: Option<(SearchOffer, crate::widget::FindAbilities)>,
    /// The search the bar opened on this app in its own process.
    driver: SearchDriver,
    query: String,
    /// The find options the bar's search runs under (its options' Match
    /// case, Whole word, mode); the defaults on each opening.
    options: crate::widget::FindOptions,
    /// The last find the app's document ran on its own that the search took
    /// over, for the bar's field to show.
    adopted: Option<crate::window_toolbar::AdoptedQuery>,
    /// What the shell was last told on this connection.
    sent: Option<WindowOffer>,
    /// The app declared window-bar slots when last looked at: its History
    /// then stands in the bar, not in a band above it.
    slots_declared: bool,
}

impl WindowBarRuntime {
    /// Whether the window's bar presents search and History for the app.
    #[cfg(feature = "native")]
    pub(crate) fn presents(&self) -> bool {
        self.presents
    }

    /// Note whether the app declares window-bar slots now. Returns whether
    /// that changed: the structure is rebuilt then, so its History leaves
    /// (or regains) the band above the app.
    pub(crate) fn note_slots_declared(&mut self) -> bool {
        let declared = crate::window_toolbar::declared().is_some();
        std::mem::replace(&mut self.slots_declared, declared) != declared
    }

    /// The widget tree changed: what it offers the search is looked at
    /// again when next asked.
    pub(crate) fn tree_changed(&mut self) {
        self.offer = None;
    }

    /// The shell connection went away: a new one is told afresh.
    #[cfg(feature = "native")]
    pub(crate) fn disconnected(&mut self) {
        self.sent = None;
    }
}

impl App {
    /// The window's bar presents search and History for this app (`false`:
    /// it stopped, or the app shows its own controls). Returns whether that
    /// changed; the app's structure is rebuilt then, so its header drops or
    /// regains the button that opens its search.
    #[cfg(feature = "native")]
    pub(crate) fn set_window_presents_bar(&mut self, presents: bool) -> bool {
        if self.floating.window_bar.presents == presents {
            return false;
        }
        self.floating.window_bar.presents = presents;
        self.layout.request_rebuild();
        true
    }

    /// Tell the app's header row whether the window's bar presents search,
    /// before a structural rebuild builds it. A row keeps what it is told,
    /// so this only needs to reach rows built since.
    pub(crate) fn offer_window_bar_to_header(&mut self) {
        self.floating.window_bar.tree_changed();
        if self.floating.window_bar.presents {
            crate::widget::window_header::offer_window_search(self.root_widget.as_mut(), true);
        }
    }

    /// What the app's widgets offer the window's search, looked at once per
    /// widget tree.
    #[cfg(feature = "native")]
    pub(crate) fn window_search_offer(&mut self) -> SearchOffer {
        self.window_search_offer_and_abilities().0
    }

    /// What the app's widgets offer the window's search and what their
    /// document's find offers beyond matching, looked at once per widget
    /// tree.
    #[cfg(feature = "native")]
    fn window_search_offer_and_abilities(&mut self) -> (SearchOffer, FindAbilities) {
        if let Some(offer) = self.floating.window_bar.offer {
            return offer;
        }
        let root = self.root_widget.as_mut();
        let search = SearchOffer::of(root);
        let abilities = if search == SearchOffer::Document {
            FindAbilities::of_root(root)
        } else {
            FindAbilities::default()
        };
        self.floating.window_bar.offer = Some((search, abilities));
        (search, abilities)
    }

    /// Whether the app's Find (Ctrl+F), or its Find and Replace (Ctrl+H),
    /// opens the window bar's search instead of a find of the app's own: the
    /// bar shows this window from another process and the app has something
    /// to search in place (for replacing, a document).
    #[cfg(feature = "native")]
    pub(crate) fn window_bar_takes(&mut self, opening: SearchOpening) -> bool {
        if !crate::window_toolbar::is_hosted() || !self.floating.window_bar.presents {
            return false;
        }
        match opening {
            SearchOpening::Find => self.window_search_offer() != SearchOffer::Nothing,
            SearchOpening::Replace => self.window_search_offer() == SearchOffer::Document,
        }
    }

    /// What the window offers the bar now.
    #[cfg(feature = "native")]
    pub(crate) fn window_offer(&mut self) -> WindowOffer {
        let (search, find) = self.window_search_offer_and_abilities();
        WindowOffer {
            search,
            count: self.floating.window_bar.driver.count().map(Into::into),
            history: self.window_history_control().map(Into::into),
            find,
            adopted: self.floating.window_bar.adopted.clone(),
        }
    }

    /// The offer to send the shell, when it changed since it was last told.
    #[cfg(feature = "native")]
    pub(crate) fn take_window_offer_change(&mut self) -> Option<WindowOffer> {
        // The document changed under an open search: its matches follow; a
        // query view's count follows its results.
        if matches!(
            self.floating.window_bar.driver.in_place(),
            InPlace::Document | InPlace::Query
        ) {
            self.drive_window_search(None, false);
        }
        let offer = self.window_offer();
        if self.floating.window_bar.sent.as_ref() == Some(&offer) {
            return None;
        }
        self.floating.window_bar.sent = Some(offer.clone());
        Some(offer)
    }

    /// Hand the app an event about its own slots
    /// ([`crate::window_toolbar::take_events`]). A press on a slot is a
    /// press on the app: it hands the app the keyboard
    /// ([`Self::hand_keyboard_to_app`]), as in the frame harness
    /// (`FrameHarness::press_window_slot`).
    #[cfg(feature = "native")]
    pub(crate) fn deliver_app_slot_event(&mut self, event: ToolbarEvent) {
        let hands_keyboard = crate::window_toolbar::hands_keyboard_to_app(&event);
        crate::window_toolbar::deliver(event);
        if hands_keyboard {
            self.hand_keyboard_to_app();
        }
    }

    /// The window took the keyboard (it opened, was brought forward, or the
    /// person pressed one of the app's slots in its bar): when nothing in it
    /// holds the keyboard, the app's keyboard entry
    /// ([`crate::widget::keyboard_entry_rank`]) takes it, so what is typed
    /// next reaches the app. Never a button merely for being first.
    #[cfg(feature = "native")]
    pub(crate) fn hand_keyboard_to_app(&mut self) {
        if self.focused_node.is_some() {
            return;
        }
        let entry = self
            .focusable_order
            .iter()
            .copied()
            .filter_map(|node| {
                let rank = self
                    .inspect_node(node, crate::widget::keyboard_entry_rank)
                    .ok()
                    .flatten()?;
                Some((rank, node))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, node)| node);
        if entry.is_some() {
            self.set_focused_node(entry);
        }
    }

    /// Run what the bar's search or History control asked of this app.
    /// `None` for an event about the app's slots, which the app reads
    /// itself ([`crate::window_toolbar::take_events`]); else whether
    /// anything the app shows changed.
    #[cfg(feature = "native")]
    pub(crate) fn apply_window_bar_event(&mut self, event: &ToolbarEvent) -> Option<bool> {
        Some(match event {
            ToolbarEvent::Search { query } => {
                self.open_window_search_driver();
                query.clone_into(&mut self.floating.window_bar.query);
                self.drive_window_search(None, false)
            }
            ToolbarEvent::SearchOptions { options } => {
                self.open_window_search_driver();
                self.floating.window_bar.options = *options;
                self.drive_window_search(None, false)
            }
            ToolbarEvent::SearchCover { cover } => {
                self.floating
                    .window_bar
                    .driver
                    .set_cover(cover.map(crate::window_toolbar::WholeRect::rect));
                self.drive_window_search(None, false)
            }
            ToolbarEvent::SearchReplace { with, all } => {
                let bar = &mut self.floating.window_bar;
                let touched = bar.driver.replace(
                    self.root_widget.as_mut(),
                    &bar.query,
                    bar.options,
                    with,
                    *all,
                );
                self.window_search_touched(touched);
                touched
            }
            ToolbarEvent::SearchStep { forward } => self.drive_window_search(Some(*forward), false),
            ToolbarEvent::SearchClose => {
                let touched = self.drive_window_search(None, true);
                self.floating.window_bar.query.clear();
                self.floating.window_bar.options = FindOptions::default();
                self.floating.window_bar.adopted = None;
                touched
            }
            ToolbarEvent::History => match self.window_history_control() {
                Some(control) => match self.invoke_window_history_control(control) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::debug!(%error, "window bar: History control refused");
                        false
                    }
                },
                None => false,
            },
            ToolbarEvent::ToggleSidebar
            | ToolbarEvent::Back
            | ToolbarEvent::Forward
            | ToolbarEvent::Commit { .. }
            | ToolbarEvent::Edit { .. }
            | ToolbarEvent::Action { .. } => return None,
        })
    }

    /// Carry the bar's search to the app's widgets. Returns whether anything
    /// the app shows changed.
    #[cfg(feature = "native")]
    fn drive_window_search(&mut self, step: Option<bool>, finishing: bool) -> bool {
        let bar = &mut self.floating.window_bar;
        let touched = bar.driver.drive(
            self.root_widget.as_mut(),
            &bar.query,
            bar.options,
            step,
            finishing,
        );
        if let Some((query, options)) = bar.driver.take_adopted() {
            // The document went to a match on its own: the search runs its
            // query now, and the bar's field shows it.
            query.clone_into(&mut bar.query);
            bar.options = options;
            let serial = bar.adopted.as_ref().map_or(1, |shown| shown.serial + 1);
            bar.adopted = Some(crate::window_toolbar::AdoptedQuery {
                serial,
                query,
                options,
            });
        }
        self.window_search_touched(touched);
        touched
    }

    /// The bar's search opened on this app: what it acts on is what the
    /// app's widgets offer now.
    #[cfg(feature = "native")]
    fn open_window_search_driver(&mut self) {
        if self.floating.window_bar.driver.in_place() == InPlace::Nothing {
            let offer = self.window_search_offer();
            self.floating.window_bar.driver.open(offer.into());
        }
    }

    /// What the search found shows on the app's next frame: matches
    /// painted, a match scrolled into view, a list narrowed, a match
    /// replaced (the widgets write it when they settle, in that frame's
    /// sync).
    #[cfg(feature = "native")]
    fn window_search_touched(&mut self, touched: bool) {
        if touched {
            self.mark_semantic_tree_stale();
            self.layout.layout_dirty = true;
            // A replacement is written when the widgets settle, in the sync.
            self.needs_sync = true;
        }
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
