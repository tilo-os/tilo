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
use crate::window_toolbar::ToolbarEvent;
use crate::window_toolbar::{SearchOffer, WindowOffer};

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
    /// What the app's widgets offer the window's search; `None` until it is
    /// looked at again after the widget tree changed.
    offer: Option<SearchOffer>,
    /// The search the bar opened on this app in its own process.
    driver: SearchDriver,
    query: String,
    /// What the shell was last told on this connection.
    sent: Option<WindowOffer>,
}

impl WindowBarRuntime {
    /// Whether the window's bar presents search and History for the app.
    #[cfg(feature = "native")]
    pub(crate) fn presents(&self) -> bool {
        self.presents
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
        if let Some(offer) = self.floating.window_bar.offer {
            return offer;
        }
        let offer = SearchOffer::of(self.root_widget.as_mut());
        self.floating.window_bar.offer = Some(offer);
        offer
    }

    /// Whether the app's Find (Ctrl+F) opens the window bar's search instead
    /// of a find of the app's own: the bar shows this window from another
    /// process and the app has something to search in place.
    #[cfg(feature = "native")]
    pub(crate) fn window_bar_takes_find(&mut self) -> bool {
        crate::window_toolbar::is_hosted()
            && self.floating.window_bar.presents
            && self.window_search_offer() != SearchOffer::Nothing
    }

    /// What the window offers the bar now.
    #[cfg(feature = "native")]
    pub(crate) fn window_offer(&mut self) -> WindowOffer {
        WindowOffer {
            search: self.window_search_offer(),
            count: self.floating.window_bar.driver.count().map(Into::into),
            history: self.window_history_control().map(Into::into),
        }
    }

    /// The offer to send the shell, when it changed since it was last told.
    #[cfg(feature = "native")]
    pub(crate) fn take_window_offer_change(&mut self) -> Option<WindowOffer> {
        // The document changed under an open search: its matches follow.
        if self.floating.window_bar.driver.in_place() == InPlace::Document {
            self.drive_window_search(None, false);
        }
        let offer = self.window_offer();
        if self.floating.window_bar.sent == Some(offer) {
            return None;
        }
        self.floating.window_bar.sent = Some(offer);
        Some(offer)
    }

    /// Run what the bar's search or History control asked of this app.
    /// `None` for an event about the app's slots, which the app reads
    /// itself ([`crate::window_toolbar::take_events`]); else whether
    /// anything the app shows changed.
    #[cfg(feature = "native")]
    pub(crate) fn apply_window_bar_event(&mut self, event: &ToolbarEvent) -> Option<bool> {
        Some(match event {
            ToolbarEvent::Search { query } => {
                if self.floating.window_bar.driver.in_place() == InPlace::Nothing {
                    let offer = self.window_search_offer();
                    self.floating.window_bar.driver.open(offer.into());
                }
                query.clone_into(&mut self.floating.window_bar.query);
                self.drive_window_search(None, false)
            }
            ToolbarEvent::SearchStep { forward } => self.drive_window_search(Some(*forward), false),
            ToolbarEvent::SearchClose => {
                let touched = self.drive_window_search(None, true);
                self.floating.window_bar.query.clear();
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
        let touched = bar
            .driver
            .drive(self.root_widget.as_mut(), &bar.query, step, finishing);
        if touched {
            // What the search found shows on the app's next frame: matches
            // painted, a match scrolled into view, a list narrowed.
            self.mark_semantic_tree_stale();
            self.layout.layout_dirty = true;
        }
        touched
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
