//! Windows shared live into a conversation ([`ui::window_share`]), as the
//! session's shell serves them to apps in processes of their own.
//!
//! Every app reaches it over its shell-ui connection, as it reaches its
//! window's bar ([`super::window_toolbars`]):
//!
//! - **Which windows there are.** Each app says which record its window
//!   shows ([`Message::WindowRecord`]). Every window on a record gets "Share
//!   to channel…" in its menu, or "Stop sharing in #x" once the sharing app
//!   says it shares it ([`Message::SharedWindows`]); the sharing app hears
//!   the windows for its picker ([`HostMessage::OpenWindows`]) and what the
//!   menus ask ([`HostMessage::WindowShareAsk`]). Sharing brings it forward,
//!   opening it when it is not.
//! - **The live views.** The sharing app's window says where it wants which
//!   resource drawn ([`Message::Embeds`]); the shell runs each resource's own
//!   app on this computer's copy of the record and draws its window there
//!   ([`crate::shell::live_embeds`]), and tells the app how each is coming
//!   along ([`HostMessage::EmbedStates`]).
//!
//! The menus, the asks and when a view starts and ends are every desktop's
//! ([`ui::window_share::host`]), the browser's too.

use std::collections::{HashMap, HashSet};

use ui::floating_surface::{
    FloatingSurfaceClientMessage as Message, FloatingSurfaceHostMessage as HostMessage,
};
use ui::window_share::host::{self, Views};
use ui::window_share::{
    Ask, Embed, KeyedEmbedState, OpenWindow, ResourceView, SHARING_APP, SharedWindow,
};

use super::ShellOverlayHost;
use crate::shell::live_embeds::{NativeView, Over};
use crate::shell::state::{TabId, TileContent, WorkspaceTree};

/// What the shell keeps of windows shared live.
#[derive(Default)]
pub(crate) struct NativeLiveShares {
    /// The record each tab's window shows, and the connection that said so.
    records: HashMap<TabId, (u64, String, String)>,
    /// The sharing app's connections, oldest first.
    sharing: Vec<u64>,
    /// What each of the sharing app's connections says it shares.
    shared: HashMap<u64, Vec<SharedWindow>>,
    /// The embeds each of the sharing app's windows asks for, by tab.
    embeds: HashMap<TabId, (u64, Vec<Embed>)>,
    /// The windows a share item stands in the menu of.
    offered: HashSet<TabId>,
    /// The windows on a record, as last listed, and as last sent.
    windows: Vec<OpenWindow>,
    windows_sent: Option<Vec<OpenWindow>>,
    /// The views running for the embeds.
    views: Views<NativeView>,
    /// The embeds' states last sent, by connection.
    states_sent: HashMap<u64, Vec<KeyedEmbedState>>,
    /// Asks waiting for the sharing app to connect.
    waiting: Vec<Ask>,
    /// What goes to apps on the next pump.
    outbox: Vec<(u64, HostMessage)>,
}

impl NativeLiveShares {
    /// Everything shared, as every connection of the sharing app says.
    fn all_shared(&self) -> Vec<SharedWindow> {
        self.sharing
            .iter()
            .filter_map(|connection| self.shared.get(connection))
            .flatten()
            .cloned()
            .collect()
    }
}

/// Every tab in `tree`, with the room of its workspace and its group's
/// front tab.
fn tabs(
    tree: &WorkspaceTree,
) -> impl Iterator<
    Item = (
        &crate::shell::state::Workspace,
        &crate::shell::state::Tab,
        bool,
    ),
> {
    tree.desktop.workspaces.iter().flat_map(|workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(move |group| {
                group
                    .tabs
                    .iter()
                    .map(move |tab| (workspace, tab, group.active_tab == tab.id))
            })
    })
}

impl ShellOverlayHost {
    /// `app_id` connected on shell-ui `connection`: it hears that this shell
    /// draws embeds, and the sharing app the windows and any ask waiting for
    /// it.
    pub(crate) fn connect_window_share(&mut self, connection: u64, app_id: &str) {
        let shares = &mut self.live_shares;
        shares.outbox.push((connection, HostMessage::DrawsEmbeds));
        if app_id != SHARING_APP {
            return;
        }
        shares.sharing.push(connection);
        shares.outbox.push((
            connection,
            HostMessage::OpenWindows {
                windows: shares.windows.clone(),
            },
        ));
        for ask in std::mem::take(&mut shares.waiting) {
            shares
                .outbox
                .push((connection, HostMessage::WindowShareAsk { ask }));
        }
    }

    /// Shell-ui `connection` closed: what it said goes with it.
    pub(crate) fn forget_window_share(&mut self, connection: u64) {
        let shares = &mut self.live_shares;
        shares.records.retain(|_, (owner, ..)| *owner != connection);
        shares.embeds.retain(|_, (owner, _)| *owner != connection);
        shares.sharing.retain(|owner| *owner != connection);
        shares.shared.remove(&connection);
        shares.states_sent.remove(&connection);
        shares.outbox.retain(|(owner, _)| *owner != connection);
    }

    /// Carry one message an app in its own process sent about sharing
    /// windows live. `app_id` is the one the launcher vouched for: an app
    /// only ever names its own window's record, and only the sharing app
    /// embeds views or says what it shares. Returns `None` for a message
    /// about something else, else whether the desktop shows something new.
    pub fn receive_window_share_message(
        &mut self,
        connection: u64,
        app_id: &str,
        message: &Message,
    ) -> Option<bool> {
        let anchor = |host: &Self, instance: Option<u64>| {
            let chrome = host.chrome.as_ref()?;
            let state = chrome.borrow();
            crate::shell::app_tabs::anchor_for_app(&state.tree(), app_id, instance)
                .map(|anchor| anchor.tab)
        };
        Some(match message {
            Message::WindowRecord {
                record,
                kind,
                instance,
            } => {
                let Some(tab) = anchor(self, *instance) else {
                    return Some(false);
                };
                self.live_shares.records.insert(
                    tab,
                    (
                        connection,
                        ui::window_share::bounded(record),
                        ui::window_share::bounded(kind),
                    ),
                );
                true
            }
            Message::Embeds { embeds, instance } => {
                if app_id != SHARING_APP {
                    return Some(false);
                }
                let Some(tab) = anchor(self, *instance) else {
                    return Some(false);
                };
                let embeds: Vec<Embed> = embeds
                    .iter()
                    .take(ui::window_share::MAX_EMBEDS)
                    .filter_map(ui::window_share::DeclaredEmbed::embed)
                    .collect();
                if embeds.is_empty() {
                    self.live_shares.embeds.remove(&tab);
                } else {
                    self.live_shares.embeds.insert(tab, (connection, embeds));
                }
                true
            }
            Message::SharedWindows { shared } => {
                if app_id != SHARING_APP {
                    return Some(false);
                }
                let shared = shared
                    .iter()
                    .take(ui::window_share::MAX_SHARED)
                    .map(SharedWindow::bounded)
                    .collect();
                self.live_shares.shared.insert(connection, shared);
                true
            }
            _ => return None,
        })
    }

    /// List the windows on a record and keep the share items in their
    /// menus, hand the sharing app what the menus asked, and run the views
    /// the sharing app's shown windows embed. Runs every frame, after the
    /// groups took the frame's input. Returns whether the desktop shows
    /// something new.
    pub(super) fn sync_window_shares(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let windows = {
            let state = chrome.borrow();
            let tree = state.tree();
            self.list_shareable_windows(&tree)
        };
        if self.live_shares.windows_sent.as_ref() != Some(&windows) {
            for connection in &self.live_shares.sharing {
                self.live_shares.outbox.push((
                    *connection,
                    HostMessage::OpenWindows {
                        windows: windows.clone(),
                    },
                ));
            }
            self.live_shares.windows_sent = Some(windows.clone());
        }
        let shared = self.live_shares.all_shared();
        host::offer(
            &mut self.window_toolbars,
            &windows,
            &shared,
            &mut self.live_shares.offered,
        );
        self.live_shares.windows = windows;
        let chosen = self.window_toolbars.take_host_actions();
        let asks = host::asks(chosen, &self.live_shares.windows, &shared);
        let mut changed = !asks.is_empty();
        for ask in asks {
            self.ask_sharing_app(ask);
        }
        changed |= self.follow_embeds();
        changed
    }

    /// The windows the person has open on a record, every one but the
    /// sharing app's.
    fn list_shareable_windows(&self, tree: &WorkspaceTree) -> Vec<OpenWindow> {
        tabs(tree)
            .filter_map(|(_, tab, _)| {
                let (_, record, kind) = self.live_shares.records.get(&tab.id)?;
                let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
                let app_title = match &tab.tile {
                    TileContent::LocalApp { label, .. } => label.clone(),
                    _ => tab.app_title.clone().unwrap_or_else(|| tab.title.clone()),
                };
                // What the window says it shows (a note's title) names the
                // share better than its tab, which may name the app; what it
                // anchors (the note) is what the share shares.
                let location = self
                    .window_toolbars
                    .toolbar(tab.id)
                    .and_then(|toolbar| toolbar.location.as_ref());
                let item = location.and_then(|location| location.anchor.clone());
                let title = location
                    .map(|location| location.label.clone())
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| tab.title.clone());
                Some(OpenWindow {
                    tab: tab.id,
                    app: app.as_str().to_owned(),
                    app_title,
                    title,
                    record: record.clone(),
                    kind: kind.clone(),
                    item,
                })
            })
            .filter(|window| window.app != SHARING_APP)
            .take(ui::window_share::MAX_WINDOWS)
            .collect()
    }

    /// Hand the sharing app `ask`: the window of it in the asking window's
    /// workspace, else its oldest. A share brings it forward to ask where,
    /// opening it when it is not; the ask waits for it to connect.
    fn ask_sharing_app(&mut self, ask: Ask) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let window_tab = match &ask {
            Ask::Share(window) => Some(window.tab),
            Ask::Stop { .. } => None,
        };
        let (connection, chat_tab, room) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let room = window_tab.and_then(|window| {
                tabs(&tree)
                    .find(|(_, tab, _)| tab.id == window)
                    .map(|(workspace, ..)| workspace.room())
            });
            let chat_tabs: Vec<(TabId, crate::shell::state::RoomId)> = tabs(&tree)
                .filter(|(_, tab, _)| {
                    tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                        .is_some_and(|app| app.as_str() == SHARING_APP)
                })
                .map(|(workspace, tab, _)| (tab.id, workspace.room()))
                .collect();
            drop(tree);
            drop(state);
            let chat_tab = chat_tabs
                .iter()
                .find(|(_, chat_room)| Some(*chat_room) == room)
                .or_else(|| chat_tabs.first())
                .map(|(tab, _)| *tab);
            let connection = chat_tab
                .and_then(|tab| self.live_shares.records.get(&tab))
                .map(|(connection, ..)| *connection)
                .filter(|connection| self.live_shares.sharing.contains(connection))
                .or_else(|| self.live_shares.sharing.first().copied());
            (connection, chat_tab, room)
        };
        if matches!(ask, Ask::Share(_)) {
            self.bring_sharing_app_forward(chat_tab, room);
        }
        match connection {
            Some(connection) => self
                .live_shares
                .outbox
                .push((connection, HostMessage::WindowShareAsk { ask })),
            None => self.live_shares.waiting.push(ask),
        }
    }

    /// Show the sharing app's window `chat_tab`, or open the sharing app in
    /// `room` when it has none.
    fn bring_sharing_app_forward(
        &self,
        chat_tab: Option<TabId>,
        room: Option<crate::shell::state::RoomId>,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if let Some(chat_tab) = chat_tab {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let address = tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group.tabs.iter().any(|tab| tab.id == chat_tab).then_some((
                            workspace.id,
                            strip.id,
                            group.id,
                        ))
                    })
                })
            });
            if let Some((workspace, strip, group)) = address {
                crate::shell::tab_binding::bring_forward(
                    &mut tree, workspace, strip, group, chat_tab,
                );
            }
            drop(tree);
            drop(state);
            chrome.borrow_mut().bump();
            return;
        }
        let mut state = chrome.borrow_mut();
        let anchor = room.and_then(|room| {
            let tree = state.tree();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .and_then(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .next()
                        .map(|group| crate::shell::app_tabs::TabAnchor {
                            room,
                            group: group.id,
                            index: group.tabs.len().saturating_sub(1),
                            tab: group.active_tab,
                        })
                })
        });
        if let Err(error) =
            crate::shell::app_tabs::open_beside(&mut state, SHARING_APP, None, anchor, false)
        {
            tracing::info!(?error, "the sharing app did not open to share a window");
        }
        state.bump();
    }

    /// Run the views the sharing app's shown windows embed, put each over
    /// its window, and tell each window how its views are coming along.
    /// Whether any view changed.
    fn follow_embeds(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        // The embedding windows on show: their surfaces, their workspaces
        // and what they embed.
        let shown: Vec<(u64, u64, crate::shell::state::Workspace, Vec<Embed>)> = {
            let state = chrome.borrow();
            let tree = state.tree();
            tabs(&tree)
                .filter(|(_, _, front)| *front)
                .filter_map(|(workspace, tab, _)| {
                    let (connection, embeds) = self.live_shares.embeds.get(&tab.id)?;
                    let TileContent::Foreign { surface_id, .. } = &tab.tile else {
                        return None;
                    };
                    Some((*surface_id, *connection, workspace.clone(), embeds.clone()))
                })
                .collect()
        };
        let mut workspaces: HashMap<ResourceView, crate::shell::state::Workspace> = HashMap::new();
        for (_, _, workspace, embeds) in &shown {
            for embed in embeds {
                workspaces
                    .entry(embed.view.clone())
                    .or_insert_with(|| workspace.clone());
            }
        }
        let followed = self
            .live_shares
            .views
            .follow(
                workspaces.keys(),
                ui::time::now(),
                |view| match workspaces.get(view) {
                    Some(workspace) => NativeView::start(view, workspace),
                    None => Err(crate::shell::i18n::t("live-view-gone")),
                },
            );
        // Their windows close as they go.
        drop(followed.dropped);
        let mut changed = followed.changed;
        let mut over: HashMap<u64, Vec<Over>> = HashMap::new();
        let mut states: HashMap<u64, Vec<KeyedEmbedState>> = HashMap::new();
        for (surface, connection, _, embeds) in &shown {
            for embed in embeds {
                if let Some(state) = self.live_shares.views.state(&embed.view) {
                    states
                        .entry(*connection)
                        .or_default()
                        .push(KeyedEmbedState {
                            key: embed.key.clone(),
                            state,
                        });
                }
                if let Some(view_surface) = self
                    .live_shares
                    .views
                    .get_mut(&embed.view)
                    .and_then(|view| view.surface())
                {
                    over.entry(*surface).or_default().push(Over {
                        surface: view_surface,
                        app: embed.view.app.clone(),
                        rect: embed.rect,
                        clip: embed.clip,
                    });
                }
            }
        }
        changed |= crate::shell::live_embeds::set_over(over);
        let mut connections: Vec<u64> = self
            .live_shares
            .embeds
            .values()
            .map(|(connection, _)| *connection)
            .collect();
        connections.sort_unstable();
        connections.dedup();
        for connection in connections {
            let now = states.remove(&connection).unwrap_or_default();
            if self.live_shares.states_sent.get(&connection) != Some(&now) {
                self.live_shares.outbox.push((
                    connection,
                    HostMessage::EmbedStates {
                        states: now.clone(),
                    },
                ));
                self.live_shares.states_sent.insert(connection, now);
            }
        }
        changed
    }

    /// What goes back to the apps about windows shared live.
    pub fn take_window_share_outbox(&mut self) -> Vec<(u64, HostMessage)> {
        std::mem::take(&mut self.live_shares.outbox)
    }
}

#[cfg(test)]
#[path = "live_shares_tests.rs"]
mod tests;
