//! The slots the apps this desktop runs put in their windows' bars.
//! Every app here runs in the host's process, each on the window its tile is shown in
//! ([`ui::hosted_window::HostedWindow`], given as the tile is built): the desktop shows what each
//! declared in its tab's bar and hands it what the person did there, through the toolkit's
//! [`ui::window_slots::WindowSlots`], as the session's shell does.
//! An app in a process of its own reaches the same bar over its `shell-ui` connection: the session
//! passes on what it asked ([`DesktopHandle::declare_bar`] and its siblings), and takes back what
//! the person did there for the app's connection ([`DesktopHandle::take_bar_events`]).

use super::{DesktopHandle, DesktopSurface, descriptor, route_of};
use crate::navigation::Admitted;
use ui::hosted_window::HostedWindow;
use ui::widget::WindowGroup;
use ui::window_targets::{TargetAction, WindowTarget};
use ui::window_toolbar::{SearchOpening, ToolbarEvent, WindowOffer, WindowToolbar};

/// What the apps in processes of their own asked of their windows' bars since the last update pass,
/// and what the person did there for them.
/// An app reaches its window's bar over its `shell-ui` connection ([`ui::window_slots`]): the
/// session passes on what it declared and what it offers, its asks to edit the location or open the
/// search, and takes back what the person did there, to write on the same connection.
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
    /// What the person did in the bars, by connection, waiting for the session to write it to the
    /// app.
    events: Vec<(u64, ToolbarEvent)>,
    /// The name each app gives what its window shows (its bar's location: a note's title, a
    /// folder's path), by tab, as the bar last read it. A window's name is what it shows, so it
    /// stands in front of the window's own title everywhere the tab is named.
    names: std::collections::BTreeMap<u64, String>,
    /// The controls each window's app describes for its own content, by tab,
    /// with the connection that declared them.
    targets: std::collections::BTreeMap<u64, (u64, Vec<WindowTarget>)>,
    /// What the apps in processes of their own declared of their windows' content since the last
    /// update pass.
    declared_targets: Vec<(u64, u64, Vec<WindowTarget>)>,
    /// The controls the person pressed in the windows' content, by connection, waiting for the
    /// session to write them to the app.
    presses: Vec<(u64, String, String, TargetAction)>,
}

impl Bars {
    /// Take `names` as what the windows' apps call what they show; whether that changed anything.
    /// The bar reads them from the slots the apps declared, so an unchanged pass costs one
    /// comparison.
    fn take_names(&mut self, names: std::collections::BTreeMap<u64, String>) -> bool {
        if self.names == names {
            return false;
        }
        self.names = names;
        true
    }
}

impl DesktopHandle {
    /// The name `tab`'s app gives what its window shows (its bar's location:
    /// a note's title, a folder's path), as the bar last read it. A window's
    /// name is what it shows, so this stands in front of the window's own
    /// title ([`tilo_desktop_policy::DesktopPolicy::retitle`],
    /// [`DesktopHandle::declare_bar`]).
    #[must_use]
    pub fn window_name(&self, tab: u64) -> Option<String> {
        self.shared.borrow().bars.names.get(&tab).cloned()
    }

    /// `connection` 's app declared `toolbar` for `tab` , or none. Shown from the next pass on. The
    /// app is the one the session vouched for, so it only ever reaches a tab that shows it.
    pub fn declare_bar(&self, tab: u64, connection: u64, toolbar: Option<WindowToolbar>) {
        self.queue_bar_ask(|bars| bars.declared.push((tab, connection, toolbar)));
    }

    /// `connection` 's app said what `tab` 's window offers the bar's search and History control.
    pub fn offer_bar(&self, tab: u64, connection: u64, offer: WindowOffer) {
        self.queue_bar_ask(|bars| bars.offered.push((tab, connection, offer)));
    }

    /// `tab` 's app asked for its location in the bar to be edited, as its own Ctrl+L does.
    pub fn edit_bar_location(&self, tab: u64) {
        self.queue_bar_ask(|bars| bars.edits.push(tab));
    }

    /// `tab` 's app asked for its window's search in the bar to open, as its own Ctrl+F (or Ctrl+H,
    /// for `opening` ) does.
    pub fn open_bar_search(&self, tab: u64, opening: SearchOpening) {
        self.queue_bar_ask(|bars| bars.searches.push((tab, opening)));
    }

    /// Queue desk work an app handed over, and ask the desk for its next pass: what an app asks
    /// reaches its window's bar in the pass that follows, not whenever the desk next draws
    /// something of its own.
    fn queue_bar_ask(&self, ask: impl FnOnce(&mut Bars)) {
        let mut shared = self.shared.borrow_mut();
        ask(&mut shared.bars);
        shared.changed();
    }

    /// `connection` 's app described the controls of `tab` 's content ([`ui::window_targets`]):
    /// what a session finds and presses there.
    pub fn declare_targets(&self, tab: u64, connection: u64, targets: Vec<WindowTarget>) {
        self.queue_bar_ask(|bars| bars.declared_targets.push((tab, connection, targets)));
    }

    /// The controls the windows the desk shows offer by name now, each with where it stands on the
    /// desk: where a test, a tool or an agent finds them. A window's controls are offered only
    /// while it is drawn, and its app's coordinates are offset by the tab's content, so where a
    /// control is and where the person sees it agree.
    #[must_use]
    pub fn window_controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        let shared = self.shared.borrow();
        let mut shown = Vec::new();
        for (tab, content) in shared.shown_tabs.content() {
            let Some((_, targets)) = shared.bars.targets.get(&tab) else {
                continue;
            };
            for target in targets {
                shown.push((
                    target.name.clone(),
                    (
                        content.x0 + target.rect[0],
                        content.y0 + target.rect[1],
                        target.rect[2] - target.rect[0],
                        target.rect[3] - target.rect[1],
                    ),
                ));
            }
        }
        shown
    }

    /// Press the control `name` of a window the desk shows, as an assistive
    /// technology does: the app acts on it through its own accessibility
    /// path and answers on the connection that declared it. The first window
    /// of the strip that shows the name answers, and the press names its role
    /// too, so the app finds the control it described and not another that
    /// goes by the same name.
    ///
    /// # Errors
    /// No window the desk draws shows a control by that name, or it answers
    /// nothing of `action`.
    pub fn press_window_control(&self, name: &str, action: TargetAction) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        for (tab, _) in shared.shown_tabs.content() {
            let Some((connection, targets)) = shared.bars.targets.get(&tab) else {
                continue;
            };
            let Some(target) = targets
                .iter()
                .find(|target| target.name == name && target.actions.contains(&action))
            else {
                continue;
            };
            let (name, role) = (target.name.clone(), target.role.clone());
            let connection = *connection;
            shared.bars.presses.push((connection, name, role, action));
            shared.changed();
            return Ok(());
        }
        Err(format!("no window shows a control named {name}"))
    }

    /// What the person, a tool or an agent pressed in the windows' content since the last call, by
    /// connection: what the session writes to each app.
    pub fn take_target_presses(&self) -> Vec<(u64, String, String, TargetAction)> {
        std::mem::take(&mut self.shared.borrow_mut().bars.presses)
    }

    /// `connection` closed: the slots and offers its app declared go with it.
    pub fn bar_connection_closed(&self, connection: u64) {
        self.queue_bar_ask(|bars| bars.gone.push(connection));
    }

    /// What the person did in the bars for the apps in processes of their own since the last call,
    /// by connection.
    pub fn take_bar_events(&self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.shared.borrow_mut().bars.events)
    }
}

impl DesktopHandle {
    /// Name the agent run that opened each window an agent opened for its person, by tab, as the
    /// workspace's shared desk says: the window's bar names the agent, and opens the run in History.
    /// Whether that changed anything.
    pub fn set_opened_by(
        &self,
        opened_by: std::collections::BTreeMap<u64, tilo_desktop_model::ownership::OpenedBy>,
    ) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.opened_by == opened_by {
            return false;
        }
        shared.opened_by = opened_by;
        shared.changed();
        true
    }
}

impl DesktopSurface {
    /// Bring the agent run `run` up in History, which opens or comes forward for it: what pressing
    /// the agent's mark in a window's bar does.
    pub(super) fn open_run(&self, run: String) {
        let mut shared = self.shared.borrow_mut();
        let open = crate::navigation::resolve_open(&shared.tree, super::ACTIVITY_APP);
        shared.activity_focus = Some((
            crate::navigation::ActivityView::Explore,
            crate::activity::ActivityDeepLink {
                group_id: Some(run),
                ..crate::activity::ActivityDeepLink::default()
            },
        ));
        shared.queue.push(open);
        shared.changed();
    }

    /// Show every window's declared slots in its bar, hand each app what the person did with them,
    /// and close the tabs whose app asked to. Returns whether the desktop shows something new or an
    /// app has something to hear.
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

    /// Name each window by what it shows: the bar's location label, a note's title or a folder's
    /// path. A window's name is what it shows, so it is written into the tab — the bar, the
    /// overview and search all read the tab's name — and the window's own title only stands where
    /// its app names nothing. Returns whether that changed.
    fn publish_window_names(&mut self) -> bool {
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
        let (changed, renamed) = {
            let mut shared = self.shared.borrow_mut();
            let named_before = shared.bars.names.clone();
            let mut changed = shared.bars.take_names(names.clone());
            if changed {
                shared.changed();
            }
            let tabs = shared.tree.workspaces.iter_mut().flat_map(|workspace| {
                workspace
                    .strips
                    .iter_mut()
                    .flat_map(|strip| strip.window_groups.iter_mut())
                    .flat_map(|group| group.tabs.iter_mut())
            });
            // A person's own name for a tab, where they gave one, stands in front of the app's: the
            // tab keeps it and holds the app's for when the person clears theirs.
            let mut renamed = Vec::new();
            for tab in tabs {
                let before = tab.title.clone();
                if let Some(name) = names.get(&tab.id) {
                    changed |= tab.set_window_title(name);
                } else if named_before
                    .get(&tab.id)
                    .is_some_and(|name| tab.title == *name)
                    && let Some(app) = app_of(tab)
                {
                    // The app names nothing any more — the note it showed is closed — so the window
                    // goes back to its app's name rather than keeping a place it has left.
                    changed |= tab.set_window_title(&app);
                }
                if tab.title != before {
                    renamed.push(descriptor(tab));
                }
            }
            (changed, renamed)
        };
        // A window's name is what it shows, not a window the desk gained or lost: the rows rename
        // their pills in place, and the desk's layout stands as it was.
        for descriptor in renamed {
            for group in self.rows.values_mut().flat_map(|row| row.groups.iter_mut()) {
                group.update_tab_descriptor(descriptor.clone());
            }
        }
        changed
    }

    /// Take what the apps in processes of their own asked of their windows' bars: their slots, what
    /// their windows offer, and the connections that closed.
    fn take_bar_asks(&mut self) -> bool {
        let (declared, offered, targets, gone) = {
            let mut shared = self.shared.borrow_mut();
            (
                std::mem::take(&mut shared.bars.declared),
                std::mem::take(&mut shared.bars.offered),
                std::mem::take(&mut shared.bars.declared_targets),
                std::mem::take(&mut shared.bars.gone),
            )
        };
        let mut changed = false;
        for connection in gone {
            changed |= self.window_slots.disconnected(connection);
            // What its app described of a window's content goes with it.
            let mut shared = self.shared.borrow_mut();
            let before = shared.bars.targets.len();
            shared
                .bars
                .targets
                .retain(|_, (owner, _)| *owner != connection);
            changed |= shared.bars.targets.len() != before;
        }
        for (tab, connection, toolbar) in declared {
            changed |= self.window_slots.declare(tab, connection, toolbar);
        }
        for (tab, connection, offer) in offered {
            changed |= self.window_slots.offer(tab, connection, offer);
        }
        for (tab, connection, described) in targets {
            let mut shared = self.shared.borrow_mut();
            if shared
                .bars
                .targets
                .get(&tab)
                .is_some_and(|(owner, targets)| *owner == connection && *targets == described)
            {
                continue;
            }
            shared.bars.targets.insert(tab, (connection, described));
            // What a window offers by name is part of what the desk publishes.
            shared.changed();
            changed = true;
        }
        changed
    }

    /// Run what the apps in processes of their own asked of their bars' own controls: edit the
    /// location, open the search. Only the tab its window shows edits or searches.
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

/// The group that shows `tab` in front, if one does: a request from a tab behind another one
/// changes nothing.
fn active_group(
    rows: &mut std::collections::HashMap<tilo_desktop_model::StripId, ui::widget::GroupRow>,
    tab: u64,
) -> Option<&mut WindowGroup> {
    rows.values_mut()
        .flat_map(|row| row.groups.iter_mut())
        .find(|group| group.active_tab_id() == tab)
}

/// The name the app showing `tab` goes by: the label the tile carries, or,
/// for a tile that names only the app, the name the app is listed under.
fn app_of(tab: &tilo_desktop_model::Tab) -> Option<String> {
    match &tab.tile {
        tilo_desktop_model::TileContent::LocalApp { label, .. } => Some(label.clone()),
        tile => tilo_desktop_model::apps::canonical_app_id(tile)
            .map(|app| tilo_desktop_search::names::app_name(&app)),
    }
}

#[cfg(test)]
#[path = "window_bars_tests.rs"]
mod tests;
