//! The window's search in a group: carried to the tab's app, served,
//! following the tab, taking Find, and placed at the end of the bar's row.

use super::WindowGroup;
use crate::widget::{TileHost, Widget};
use vello::kurbo::Rect;
use winit::event::WindowEvent;

impl WindowGroup {
    /// Carry the window's search to its tab's app ([`crate::widget::WindowSearch::drive`]).
    pub(super) fn drive_search(&mut self) {
        // What the search's options cover is told to the app in its own
        // coordinates, which start at its tile's corner.
        if let Some(frame) = self
            .search_tab
            .and_then(|tab| self.content_rect_for_tab(tab))
        {
            self.search.set_frame_origin(frame.origin());
        }
        if self.search.is_remote() {
            // An app in a process of its own is told, and drives itself.
            if let Some(tab) = self.search_tab {
                let events = self.search.take_remote_events();
                self.window_events
                    .extend(events.into_iter().map(|event| (tab, event)));
            }
            return;
        }
        if !self.search.wants_drive() {
            return;
        }
        let Some(tab) = self.search_tab else {
            return;
        };
        let search = &mut self.search;
        let driven = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == tab)
            .and_then(|(_, host)| host.visit_search_root(&mut |root| search.drive(root)));
        if driven.is_none() {
            // The tab's app is gone: nothing is left to tell.
            self.search.detach();
        }
    }

    /// Carry out what the person asked of the window's search.
    pub(super) fn service_search(&mut self) -> bool {
        let requests = self.search.take_requests();
        let acted = !requests.is_empty();
        for request in requests {
            match request {
                crate::widget::WindowSearchRequest::Open => {
                    self.open_search(crate::tokens::MotionSource::Pointer);
                }
                crate::widget::WindowSearchRequest::Close => {
                    self.close_search(crate::motion::input_source());
                }
                crate::widget::WindowSearchRequest::Widen(reach) => {
                    // The platform search takes the query on from here.
                    self.pending_search_open = Some(crate::widget::SearchHandoff {
                        tab: self.bar.active_id(),
                        reach,
                        query: self.search.query().to_owned(),
                    });
                    self.close_search(crate::motion::input_source());
                }
            }
        }
        self.drive_search();
        acted
    }

    /// The search follows the window's active tab: switching tabs ends it on
    /// the tab it was on.
    pub(super) fn follow_search_tab(&mut self) {
        if let Some(tab) = self.search_tab
            && tab != self.bar.active_id()
        {
            self.close_search(crate::tokens::MotionSource::Pointer);
        }
    }

    /// Whether `event` is Find (Ctrl+F, ⌘F), or Find and Replace (Ctrl+H,
    /// ⌘⌥F), and this window can search in place (for replacing, a
    /// document): the window opens its search, or gives the open one the
    /// keyboard, instead of the app opening a find of its own.
    pub(super) fn takes_find(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(opening) = crate::widget::window_search::search_chord(
            event,
            modifiers.state(),
            crate::platform::PlatformConventions::default(),
        ) else {
            return false;
        };
        if !self.focused || !self.navigation_bar_visible() {
            return false;
        }
        let replace = opening == crate::window_toolbar::SearchOpening::Replace;
        if self.search.is_open() {
            if self.search.has_keyboard() {
                // Find again widens, and the replace chord goes to the
                // replacement: the search's own key handling.
                return false;
            }
            if replace {
                self.search.focus_replacement();
            } else {
                self.search.take_keyboard();
            }
            return true;
        }
        let active = self.bar.active_id();
        let in_place = match self.window_offer(active) {
            Some(offer) => offer.search.into(),
            None => self
                .active_host_mut()
                .map_or(crate::widget::InPlace::Nothing, TileHost::in_place_search),
        };
        if in_place == crate::widget::InPlace::Nothing
            || (replace && in_place != crate::widget::InPlace::Document)
        {
            return false;
        }
        self.open_search_as(crate::tokens::MotionSource::Keyboard, opening);
        true
    }

    /// Place the search control where the window's bar shows it: after the tabs, or, while the
    /// app's own row holds the bar, in that row before the window's controls, from where its open
    /// search reaches back over the row.
    pub(super) fn place_search(&mut self) {
        if self.held_bar.is_some() {
            let furniture = self.held;
            if furniture.search.area() > 0.0 {
                self.search
                    .place_trailing(furniture.search, furniture.row.x0, furniture.row.x0);
            } else {
                self.search.place(Rect::ZERO, 0.0);
            }
            return;
        }
        if !self.navigation_bar_visible() {
            self.search.place(Rect::ZERO, 0.0);
            return;
        }
        place_search(&self.bar, &mut self.search);
    }
}

/// Place `search` in `bar`'s row after its tabs, and let it reach up to the
/// window's own controls: the rule every host lays a window's bar out by.
pub(crate) fn place_search(bar: &crate::widget::TabBar, search: &mut crate::widget::WindowSearch) {
    let px = crate::widget::window_controls::CONTROL_PX;
    if let Some((row, reach)) = bar.toolbar_search_slot() {
        // The app put its slots in the bar: the control ends their row, and
        // the open search grows back over the location.
        let x0 = row.x1 + crate::widget::window_controls::CONTROL_GAP;
        let y0 = row.center().y - px / 2.0;
        search.place_trailing(
            Rect::new(x0, y0, x0 + px, y0 + px),
            reach.min(x0),
            row.x0.min(x0),
        );
        return;
    }
    let row = bar.get_rect();
    let x0 = bar.tabs_end() + crate::widget::window_controls::CONTROL_GAP;
    let y0 = row.center().y - px / 2.0;
    let limit = bar.window_controls().content_limit(row);
    search.place(Rect::new(x0, y0, x0 + px, y0 + px), limit.max(x0 + px));
    search.set_cramped_start(bar.search_cramped_start());
}
