//! What a desktop keeps of the slots its apps put in their windows' bars
//! ([`crate::window_toolbar`]), and how it shows them: every desktop, the
//! session's shell and the browser's alike, holds one [`WindowSlots`].
//!
//! An app reaches its window's bar one of two ways:
//!
//! - **in a process of its own**, over its connection to the desktop: the
//!   host passes on what it declared ([`WindowSlots::declare`],
//!   [`WindowSlots::offer`]) and sends what the person did back on the same
//!   connection ([`WindowSlots::take_outbox`]);
//! - **in the host's own process** (every app of the browser desktop, the
//!   shell's Terminal), on the window the host shows it in
//!   ([`crate::hosted_window::HostedWindow`]): [`WindowSlots::sync_hosted`]
//!   reads each window's slots and hands its app what the person did.
//!
//! Either way [`WindowSlots::sync_groups`] puts each tab's slots in its
//! window group's bar and takes what the person did there.
//!
//! The desktop adds items of its own to a window's overflow menu ("Share to
//! channel…", [`WindowSlots::set_host_items`]): they join the slots its app
//! declared, and what the person chose of them comes back to the desktop
//! ([`WindowSlots::take_host_actions`]), never to the app.

use std::collections::HashMap;

use crate::hosted_window::HostedWindow;
use crate::widget::WindowGroup;
use crate::window_toolbar::{
    MAX_DECLARED_ACTIONS, ToolbarAction, ToolbarEvent, WindowOffer, WindowToolbar,
};

/// A desktop tab's id.
pub type TabId = u64;

/// The connection id the slots of an app in the host's own process are
/// kept under: what the person does with them goes back on its window,
/// never on a connection.
const IN_PROCESS: u64 = u64::MAX;

/// What the ids of the desktop's own menu items start with: an app's never
/// do, so a choice of one reaches the desktop alone.
pub const HOST_ACTION_PREFIX: &str = "tilo.desktop.";

/// Every tab's declared slots and the connection of the app that declared
/// them, and what goes back to the apps on the next pump.
#[derive(Default)]
pub struct WindowSlots {
    by_tab: HashMap<TabId, (u64, WindowToolbar)>,
    /// Tabs whose slots went away since the groups last saw them.
    cleared: Vec<TabId>,
    /// What each tab's app offers the bar's search and History control, and
    /// the connection it said so on.
    offers: HashMap<TabId, (u64, WindowOffer)>,
    /// Tabs whose offer went away since the groups last saw them.
    offers_cleared: Vec<TabId>,
    outbox: Vec<(u64, ToolbarEvent)>,
    /// For an app in the host's own process: its window and the revision of
    /// its slots last taken, by tab.
    hosted_seen: HashMap<TabId, (HostedWindow, u64)>,
    /// What goes back to apps in the host's own process, by tab.
    hosted_outbox: Vec<(TabId, ToolbarEvent)>,
    /// The desktop's own items in each tab's overflow menu.
    host_items: HashMap<TabId, Vec<ToolbarAction>>,
    /// The desktop's items the person chose, by tab, not taken yet.
    host_actions: Vec<(TabId, String)>,
}

/// What [`WindowSlots::sync_hosted`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HostedSync {
    /// The desktop shows something new, or an app has something to hear.
    pub changed: bool,
    /// Tabs whose app asked to close them (a terminal whose shell exited).
    pub closing: Vec<TabId>,
    /// An app asked for its window's search and its group opened it.
    pub searched: bool,
    /// Tabs whose app declared something new for its bar (its location
    /// among it), in the order their windows were given.
    pub declared: Vec<TabId>,
}

impl WindowSlots {
    /// `connection`'s app declared `toolbar` for `tab`. Returns whether it
    /// changed anything.
    pub fn declare(&mut self, tab: TabId, connection: u64, toolbar: Option<WindowToolbar>) -> bool {
        let Some(toolbar) = toolbar
            .map(WindowToolbar::bounded)
            .filter(|toolbar| !toolbar.is_empty())
        else {
            let had = self.by_tab.remove(&tab).is_some();
            if had {
                self.cleared.push(tab);
            }
            return had;
        };
        let next = (connection, toolbar);
        if self.by_tab.get(&tab) == Some(&next) {
            return false;
        }
        self.by_tab.insert(tab, next);
        true
    }

    /// `connection`'s app said what `tab`'s window offers the bar's search
    /// and History control. Returns whether it changed anything.
    pub fn offer(&mut self, tab: TabId, connection: u64, offer: WindowOffer) -> bool {
        let next = (connection, offer);
        if self.offers.get(&tab) == Some(&next) {
            return false;
        }
        self.offers.insert(tab, next);
        self.offers_cleared.retain(|cleared| *cleared != tab);
        true
    }

    /// What `tab`'s app offers the bar, if it runs in a process of its own.
    #[must_use]
    pub fn window_offer(&self, tab: TabId) -> Option<&WindowOffer> {
        self.offers.get(&tab).map(|(_, offer)| offer)
    }

    /// `connection` closed: its app's slots and offers go with it.
    pub fn disconnected(&mut self, connection: u64) -> bool {
        let gone: Vec<TabId> = self
            .by_tab
            .iter()
            .filter(|(_, (owner, _))| *owner == connection)
            .map(|(tab, _)| *tab)
            .collect();
        for tab in &gone {
            self.by_tab.remove(tab);
        }
        let offered: Vec<TabId> = self
            .offers
            .iter()
            .filter(|(_, (owner, _))| *owner == connection)
            .map(|(tab, _)| *tab)
            .collect();
        for tab in &offered {
            self.offers.remove(tab);
        }
        let any = !gone.is_empty() || !offered.is_empty();
        self.cleared.extend(gone);
        self.offers_cleared.extend(offered);
        any
    }

    /// `tab`'s declared slots, as its bar shows them.
    #[must_use]
    pub fn toolbar(&self, tab: TabId) -> Option<&WindowToolbar> {
        self.by_tab.get(&tab).map(|(_, toolbar)| toolbar)
    }

    /// Put `items`, the desktop's own, in `tab`'s overflow menu, after its
    /// app's. They show only in a window whose app declared slots: a bar
    /// that had none keeps its title alone. Each id starts with
    /// [`HOST_ACTION_PREFIX`].
    pub fn set_host_items(&mut self, tab: TabId, items: Vec<ToolbarAction>) {
        let items: Vec<ToolbarAction> = items
            .into_iter()
            .filter(|item| item.id.starts_with(HOST_ACTION_PREFIX))
            .map(|item| ToolbarAction { menu: true, ..item })
            .collect();
        if items.is_empty() {
            self.host_items.remove(&tab);
        } else {
            self.host_items.insert(tab, items);
        }
    }

    /// The desktop's own items the person chose since the last call, by
    /// tab, each by its id.
    pub fn take_host_actions(&mut self) -> Vec<(TabId, String)> {
        std::mem::take(&mut self.host_actions)
    }

    /// `tab`'s bar as it shows: its app's slots, its menu ending in the
    /// desktop's items.
    fn shown_toolbar(&self, tab: TabId) -> Option<WindowToolbar> {
        let toolbar = self.toolbar(tab)?;
        let Some(items) = self.host_items.get(&tab) else {
            return Some(toolbar.clone());
        };
        let mut shown = toolbar.clone();
        shown
            .actions
            .truncate(MAX_DECLARED_ACTIONS.saturating_sub(items.len()));
        shown.actions.extend(items.iter().cloned());
        Some(shown)
    }

    /// Queue `event` for the app that declared `tab`'s slots or offered
    /// its window to the bar. Returns whether one did.
    pub fn route(&mut self, tab: TabId, event: ToolbarEvent) -> bool {
        if let ToolbarEvent::Action { id } = &event
            && id.starts_with(HOST_ACTION_PREFIX)
        {
            self.host_actions.push((tab, id.clone()));
            return true;
        }
        let connection = match &event {
            ToolbarEvent::Search { .. }
            | ToolbarEvent::SearchStep { .. }
            | ToolbarEvent::SearchClose
            | ToolbarEvent::SearchOptions { .. }
            | ToolbarEvent::SearchCover { .. }
            | ToolbarEvent::SearchReplace { .. }
            | ToolbarEvent::History => self.offers.get(&tab).map(|(connection, _)| *connection),
            ToolbarEvent::ToggleSidebar
            | ToolbarEvent::Back
            | ToolbarEvent::Forward
            | ToolbarEvent::Commit { .. }
            | ToolbarEvent::Edit { .. }
            | ToolbarEvent::Action { .. } => {
                self.by_tab.get(&tab).map(|(connection, _)| *connection)
            }
        };
        let Some(connection) = connection else {
            return false;
        };
        if connection == IN_PROCESS {
            self.hosted_outbox.push((tab, event));
        } else {
            self.outbox.push((connection, event));
        }
        true
    }

    /// What goes back to apps in processes of their own, by connection.
    pub fn take_outbox(&mut self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.outbox)
    }

    /// Tab `tab`, shown in the host's own process, is gone: its slots go.
    pub fn forget_hosted(&mut self, tab: TabId) {
        self.hosted_seen.remove(&tab);
        self.host_items.remove(&tab);
        if self.by_tab.remove(&tab).is_some() {
            self.cleared.push(tab);
        }
    }

    /// Every group shows its tabs' declared slots, and what the person did
    /// with them is queued for the apps. Run it every frame; a group whose
    /// slots did not change does nothing.
    pub fn sync_groups<'a>(&mut self, groups: impl IntoIterator<Item = &'a mut WindowGroup>) {
        let cleared = std::mem::take(&mut self.cleared);
        let offers_cleared = std::mem::take(&mut self.offers_cleared);
        let mut events = Vec::new();
        for group in groups {
            let ids: Vec<TabId> = group.tabs().iter().map(|tab| tab.id).collect();
            for id in ids {
                if self.host_items.contains_key(&id)
                    && let Some(toolbar) = self.shown_toolbar(id)
                {
                    if group.tab_toolbar(id) != Some(&toolbar) {
                        group.set_tab_toolbar(id, Some(toolbar));
                    }
                } else if let Some(toolbar) = self.toolbar(id) {
                    // Declarations are stored bounded, as the bar keeps
                    // them: an unchanged one is compared, not copied.
                    if group.tab_toolbar(id) != Some(toolbar) {
                        group.set_tab_toolbar(id, Some(toolbar.clone()));
                    }
                } else if cleared.contains(&id) {
                    group.set_tab_toolbar(id, None);
                }
                if let Some(offer) = self.window_offer(id) {
                    // Unchanged offers change nothing in the group.
                    group.set_tab_window_offer(id, Some(offer.clone()));
                } else if offers_cleared.contains(&id) {
                    group.set_tab_window_offer(id, None);
                }
            }
            events.extend(group.take_toolbar_events());
            events.extend(group.take_location_drafts());
        }
        for (tab, event) in events {
            self.route(tab, event);
        }
    }

    /// Look at the windows of the apps the host shows in its own process,
    /// `windows` being every one of them by tab: show the slots each
    /// declared in its tab's bar, run its asks to edit its location or open
    /// its search, and hand it what the person did with its slots. A tab
    /// whose window is no longer among `windows` loses its slots. Run it
    /// every frame, after the groups took the frame's input.
    pub fn sync_hosted<'a>(
        &mut self,
        windows: &[(TabId, HostedWindow)],
        groups: impl IntoIterator<Item = &'a mut WindowGroup>,
    ) -> HostedSync {
        let mut groups: Vec<&mut WindowGroup> = groups.into_iter().collect();
        let mut sync = HostedSync::default();
        let gone: Vec<TabId> = self
            .hosted_seen
            .iter()
            .filter(|(tab, (seen, _))| {
                !windows
                    .iter()
                    .any(|(shown, window)| shown == *tab && window.is(seen))
            })
            .map(|(tab, _)| *tab)
            .collect();
        for tab in gone {
            self.forget_hosted(tab);
            sync.changed = true;
        }
        for (tab, window) in windows {
            let seen = self.hosted_seen.get(tab).map(|(_, revision)| *revision);
            let revision = window.revision();
            if seen != Some(revision) {
                self.hosted_seen.insert(*tab, (window.clone(), revision));
                if self.declare(*tab, IN_PROCESS, window.declared()) {
                    sync.changed = true;
                    sync.declared.push(*tab);
                }
            }
            if window.take_close() {
                sync.closing.push(*tab);
            }
        }
        self.sync_groups(groups.iter_mut().map(|group| &mut **group));
        for (tab, window) in windows {
            let edit = window.take_edit_request();
            let search = window.take_search_request();
            if !edit && search.is_none() {
                continue;
            }
            // Only the tab its window shows edits or searches.
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.active_tab_id() == *tab)
            {
                sync.changed |= edit && group.edit_location();
                if let Some(opening) = search
                    && group.open_search_for_app(*tab, opening)
                {
                    sync.searched = true;
                    sync.changed = true;
                }
            }
        }
        for (tab, event) in std::mem::take(&mut self.hosted_outbox) {
            if let Some((_, window)) = windows.iter().find(|(shown, _)| *shown == tab) {
                window.deliver(event);
                // The app hears it on its next frame.
                sync.changed = true;
            }
        }
        sync.changed |= !sync.closing.is_empty();
        sync
    }
}

#[cfg(test)]
#[path = "window_slots_tests.rs"]
mod tests;
