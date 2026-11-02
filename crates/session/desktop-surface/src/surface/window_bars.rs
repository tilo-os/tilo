//! The slots the apps this desktop runs put in their windows' bars.
//!
//! Every app here runs in the host's process, each on the window its tile
//! is shown in ([`ui::hosted_window::HostedWindow`], given as the tile is
//! built): the desktop shows what each declared in its tab's bar and hands
//! it what the person did there, through the toolkit's
//! [`ui::window_slots::WindowSlots`], as the session's shell does.
//!
//! An app in a process of its own reaches the same bar over its `shell-ui`
//! connection: the session passes on what it asked
//! ([`DesktopHandle::declare_bar`] and its siblings), and takes back what the
//! person did there for the app's connection
//! ([`DesktopHandle::take_bar_events`]).

use super::{DesktopHandle, DesktopSurface, route_of};
use crate::navigation::Admitted;
use ui::hosted_window::HostedWindow;
use ui::widget::WindowGroup;
use ui::window_toolbar::{SearchOpening, ToolbarEvent, WindowOffer, WindowToolbar};

/// What the apps in processes of their own asked of their windows' bars since
/// the last update pass, and what the person did there for them.
///
/// An app reaches its window's bar over its `shell-ui` connection
/// ([`ui::window_slots`]): the session passes on what it declared and what it
/// offers, its asks to edit the location or open the search, and takes back
/// what the person did there, to write on the same connection.
#[derive(Default)]
pub(super) struct Bars {
    /// Slots declared for a tab, by the connection that declared them.
    declared: Vec<(u64, u64, Option<WindowToolbar>)>,
    /// What a tab's window offers the bar's search and History control.
    offered: Vec<(u64, u64, WindowOffer)>,
    /// Tabs whose app asked for its location to be edited.
    edits: Vec<u64>,
    /// Tabs whose app asked for its search, and how it opens.
    searches: Vec<(u64, SearchOpening)>,
    /// Connections that closed: what they declared goes with them.
    gone: Vec<u64>,
    /// What the person did in the bars, by connection, waiting for the
    /// session to write it to the app.
    events: Vec<(u64, ToolbarEvent)>,
}

impl DesktopHandle {
    /// The name `tab`'s app gives what its window shows (its bar's location:
    /// a note's title, a folder's path), as the bar last read it. A window's
    /// name is what it shows, so this stands in front of the window's own
    /// title ([`tilo_desktop_policy::DesktopPolicy::retitle`],
    /// [`DesktopHandle::declare_bar`]).
    #[must_use]
    pub fn window_name(&self, tab: u64) -> Option<String> {
        self.shared.borrow().window_names.get(&tab).cloned()
    }

    /// `connection`'s app declared `toolbar` for `tab`, or none. Shown from
    /// the next pass on. The app is the one the session vouched for, so it
    /// only ever reaches a tab that shows it.
    pub fn declare_bar(&self, tab: u64, connection: u64, toolbar: Option<WindowToolbar>) {
        self.shared
            .borrow_mut()
            .bars
            .declared
            .push((tab, connection, toolbar));
    }

    /// `connection`'s app said what `tab`'s window offers the bar's search
    /// and History control.
    pub fn offer_bar(&self, tab: u64, connection: u64, offer: WindowOffer) {
        self.shared
            .borrow_mut()
            .bars
            .offered
            .push((tab, connection, offer));
    }

    /// `tab`'s app asked for its location in the bar to be edited, as its own
    /// Ctrl+L does.
    pub fn edit_bar_location(&self, tab: u64) {
        self.shared.borrow_mut().bars.edits.push(tab);
    }

    /// `tab`'s app asked for its window's search in the bar to open, as its
    /// own Ctrl+F (or Ctrl+H, for `opening`) does.
    pub fn open_bar_search(&self, tab: u64, opening: SearchOpening) {
        self.shared.borrow_mut().bars.searches.push((tab, opening));
    }

    /// `connection` closed: the slots and offers its app declared go with it.
    pub fn bar_connection_closed(&self, connection: u64) {
        self.shared.borrow_mut().bars.gone.push(connection);
    }

    /// What the person did in the bars for the apps in processes of their
    /// own since the last call, by connection.
    pub fn take_bar_events(&self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.shared.borrow_mut().bars.events)
    }
}

impl DesktopSurface {
    /// Show every window's declared slots in its bar, hand each app what
    /// the person did with them, and close the tabs whose app asked to.
    /// Returns whether the desktop shows something new or an app has
    /// something to hear.
    pub(super) fn sync_window_bars(&mut self) -> bool {
        let mut changed = self.take_bar_asks();
        let mut windows: Vec<(u64, HostedWindow)> = Vec::new();
        for group in self.rows.values_mut().flat_map(|row| row.groups.iter_mut()) {
            let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
            for id in ids {
                if let Some(window) = group
                    .host_for_tab_mut(id)
                    .and_then(|host| host.hosted_window())
                {
                    windows.push((id, window.clone()));
                }
            }
        }
        let sync = self.window_slots.sync_hosted(
            &windows,
            self.rows.values_mut().flat_map(|row| row.groups.iter_mut()),
        );
        if !sync.closing.is_empty() {
            let mut shared = self.shared.borrow_mut();
            for tab in sync.closing {
                if let Some(ids) = route_of(&shared.tree, tab) {
                    shared.queue.push(Admitted::Close(ids));
                }
            }
        }
        changed |= sync.changed;
        changed |= self.publish_window_names();
        changed |= self.ask_bar_changes();
        let events = self.window_slots.take_outbox();
        if !events.is_empty() {
            self.shared.borrow_mut().bars.events.extend(events);
            changed = true;
        }
        changed
    }

    /// Tell the desk what each app calls what its window shows: the bar's
    /// location label, a note's title or a folder's path. A window's name is
    /// what it shows, so the tab's name — in its bar, the overview and
    /// search — is this, and the window's own title only stands where its
    /// app names nothing. Returns whether that changed.
    fn publish_window_names(&self) -> bool {
        let mut names = std::collections::BTreeMap::new();
        for group in self.rows.values().flat_map(|row| row.groups.iter()) {
            for tab in group.tabs() {
                let name = self
                    .window_slots
                    .toolbar(tab.id)
                    .and_then(|toolbar| toolbar.location.as_ref())
                    .map(|location| location.label.trim())
                    .filter(|label| !label.is_empty());
                if let Some(name) = name {
                    names.insert(tab.id, name.to_owned());
                }
            }
        }
        self.shared.borrow_mut().take_window_names(names)
    }

    /// Take what the apps in processes of their own asked of their windows'
    /// bars: their slots, what their windows offer, and the connections that
    /// closed.
    fn take_bar_asks(&mut self) -> bool {
        let (declared, offered, gone) = {
            let mut shared = self.shared.borrow_mut();
            (
                std::mem::take(&mut shared.bars.declared),
                std::mem::take(&mut shared.bars.offered),
                std::mem::take(&mut shared.bars.gone),
            )
        };
        let mut changed = false;
        for connection in gone {
            changed |= self.window_slots.disconnected(connection);
        }
        for (tab, connection, toolbar) in declared {
            changed |= self.window_slots.declare(tab, connection, toolbar);
        }
        for (tab, connection, offer) in offered {
            changed |= self.window_slots.offer(tab, connection, offer);
        }
        changed
    }

    /// Run what the apps in processes of their own asked of their bars' own
    /// controls: edit the location, open the search. Only the tab its window
    /// shows edits or searches.
    fn ask_bar_changes(&mut self) -> bool {
        let (edits, searches) = {
            let mut shared = self.shared.borrow_mut();
            (
                std::mem::take(&mut shared.bars.edits),
                std::mem::take(&mut shared.bars.searches),
            )
        };
        let mut changed = false;
        for tab in edits {
            if let Some(group) = active_group(&mut self.rows, tab) {
                changed |= group.edit_location();
            }
        }
        for (tab, opening) in searches {
            if let Some(group) = active_group(&mut self.rows, tab) {
                changed |= group.open_search_for_app(tab, opening);
            }
        }
        changed
    }
}

impl super::Shared {
    /// Take `names` as what the windows' apps call what they show; whether
    /// that changed anything. The bar reads them from the slots the apps
    /// declared, so an unchanged pass costs one comparison.
    fn take_window_names(&mut self, names: std::collections::BTreeMap<u64, String>) -> bool {
        if self.window_names == names {
            return false;
        }
        self.window_names = names;
        self.changed();
        true
    }
}

/// The group that shows `tab` in front, if one does: a request from a tab
/// behind another one changes nothing.
fn active_group(
    rows: &mut std::collections::HashMap<tilo_desktop_model::StripId, ui::widget::GroupRow>,
    tab: u64,
) -> Option<&mut WindowGroup> {
    rows.values_mut()
        .flat_map(|row| row.groups.iter_mut())
        .find(|group| group.active_tab_id() == tab)
}

#[cfg(test)]
#[path = "window_bars_tests.rs"]
mod tests;
