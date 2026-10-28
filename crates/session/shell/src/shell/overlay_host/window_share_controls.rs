//! Sharing one window with a workspace where each person lays out their own
//! windows ([`tilo_desktop_model::window_share`]), as the window itself
//! offers and shows it:
//!
//! - **The control.** A window this device runs, in such a workspace, offers
//!   "Share with <workspace>" (or "Stop sharing" while shared) in its window
//!   menu ([`crate::shell::window_menu`]) and among the desktop's own items
//!   in its bar's menu.
//! - **The mark.** While shared, the window's bar says "Shared with
//!   <workspace>", with the faces of the people there who see it
//!   ([`ui::widget::SharedWith`]). On every other device, the window it
//!   opens for the share reads as the sharer's: "Ana's window", with her
//!   face ([`ui::widget::SharedBy`]).
//! - **The end.** A share whose window this device no longer shows in that
//!   workspace (closed, or moved to another one) is stopped, through the
//!   room's outbox, which asks again on the host's next event until the
//!   room no longer holds it. Leaving or deleting the workspace stops its
//!   shares on the way out
//!   ([`crate::shell::network::ShellNetwork::release_room`]).
//!
//! Every door asks the one request `tab.share` / `tab.unshare` make
//! ([`desktop_windows::share_request`]): the room's host records the share,
//! or its end, as a governance change attributed to the person.

use std::collections::{HashMap, HashSet};

use ui::widget::{GroupRow, SharedBy, SharedWith};
use ui::window_toolbar::ToolbarAction;

use super::ShellOverlayHost;
use super::peer_sync_dispatch::{FollowUp, Job};
use crate::shell::i18n::{t, t_args};
use crate::shell::peer_sync_overlay::PeerSyncRequest;
use crate::shell::search::providers::desktop_windows;
use crate::shell::state::{TabId, WorkspaceTree};

/// The bar menu's item that shares the window with its workspace.
pub(super) const SHARE_ITEM: &str = "tilo.desktop.share-with-workspace";
/// The bar menu's item that stops sharing it.
pub(super) const STOP_ITEM: &str = "tilo.desktop.stop-sharing-with-workspace";

/// What the shell keeps of the windows it shares with their workspaces.
#[derive(Default)]
pub(crate) struct WindowShareControls {
    /// The tree generation the marks and items were last read at.
    generation: Option<u64>,
    /// Each shareable window's bar-menu item and mark, as last read.
    shown: HashMap<TabId, Shown>,
    /// The windows a share item stands in the bar menu of.
    offered: HashSet<TabId>,
    /// The windows others shared that this device shows, each with whose
    /// it is, as last read.
    theirs: HashMap<TabId, SharedBy>,
}

/// One shareable window, as its bar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    item: ToolbarAction,
    mark: Option<SharedWith>,
}

/// Whether `id` is one of the bar menu's share items.
pub(super) fn is_share_item(id: &str) -> bool {
    id == SHARE_ITEM || id == STOP_ITEM
}

/// The bar menu's item for a window of `place`, shared or not.
fn bar_item(place: &str, shared: bool) -> ToolbarAction {
    if shared {
        ToolbarAction::new(STOP_ITEM, "eye-off", t("shell-window-menu-stop-sharing"))
    } else {
        ToolbarAction::new(
            SHARE_ITEM,
            "screen-share",
            t_args("shell-window-menu-share", &[("place", place.into())]),
        )
    }
}

/// Every window this device can share in `tree`, with its item and, while
/// shared, its mark: the workspace's name and the people there who see it
/// (everyone in it now but this person, `local`, and their own devices).
fn shareable_windows(
    tree: &WorkspaceTree,
    local: &dyn Fn(&crate::shell::state::Workspace) -> String,
    now_ms: u64,
) -> HashMap<TabId, Shown> {
    let mut shown = HashMap::new();
    for workspace in &tree.desktop.workspaces {
        let tabs = workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs);
        for tab in tabs {
            let Some(shared) = desktop_windows::sharing(tree, workspace, tab, now_ms) else {
                continue;
            };
            let mark = shared.then(|| {
                let me = local(workspace);
                SharedWith {
                    place: workspace.name.clone(),
                    people: workspace
                        .collaborators
                        .iter()
                        .filter(|person| {
                            person.session.is_some()
                                && !person.invited
                                && !person.own_device
                                && person.id != me
                        })
                        .map(|person| {
                            (
                                ui::permissions::UserId(person.id.clone()),
                                person.name.clone(),
                            )
                        })
                        .collect(),
                }
            });
            shown.insert(
                tab.id,
                Shown {
                    item: bar_item(&workspace.name, shared),
                    mark,
                },
            );
        }
    }
    shown
}

/// Every window others shared that this device shows in `tree`, with whose
/// it is: the member who shared it, while they are here.
pub(crate) fn windows_shared_by_others(
    tree: &WorkspaceTree,
    now_ms: u64,
) -> HashMap<TabId, SharedBy> {
    let local = tree.local_salt();
    let mut theirs = HashMap::new();
    for workspace in &tree.desktop.workspaces {
        for (tab, sharer) in crate::shell::shared_window_people(tree, workspace, now_ms) {
            if local != 0 && tilo_desktop_model::peer_salt_of(tab) == local {
                continue;
            }
            theirs.insert(
                tab,
                SharedBy {
                    sharer: (
                        ui::permissions::UserId(sharer.id.clone()),
                        sharer.name.clone(),
                    ),
                },
            );
        }
    }
    theirs
}

impl ShellOverlayHost {
    /// Whose the bar of the window `tab` says it is, while someone else
    /// shares it here. For tests, the `shots` renderer and the bench.
    pub fn tab_shared_by(&self, tab: TabId) -> Option<SharedBy> {
        self.group_row_ptrs.values().find_map(|&ptr| {
            // SAFETY: read-only use under the field's stable-row contract.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .find_map(|group| group.tab_shared_by(tab).cloned())
        })
    }

    /// Who the bar of the window `tab` says sees it, while it is marked
    /// shared. For tests, the `shots` renderer and the bench.
    pub fn tab_shared_with(&self, tab: TabId) -> Option<SharedWith> {
        self.group_row_ptrs.values().find_map(|&ptr| {
            // SAFETY: read-only use under the field's stable-row contract.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .find_map(|group| group.tab_shared_with(tab).cloned())
        })
    }

    /// Share the window `tab` with its workspace's people (`share`), or
    /// stop: the request `tab.share` / `tab.unshare` make, carried out on
    /// the network worker. A refusal is said once it is back. Whether it
    /// was asked: `false` when the tab is no window this device can share.
    pub(super) fn request_window_share(&mut self, tab: TabId, share: bool) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let found = desktop_windows::share_request(&chrome.borrow().tree(), tab, share);
        let Some((workspace, request)) = found else {
            return false;
        };
        self.dispatch_peer_sync(
            Job {
                request: request.clone(),
                require_pending: false,
                workspace: Some(workspace),
            },
            FollowUp::Directory { request },
        );
        true
    }

    /// What a window-share request answered: the room's record says the
    /// share itself as its change reaches this device, so only a refusal is
    /// said here.
    pub(super) fn window_share_answered(&self, request: &PeerSyncRequest, refused: Option<&str>) {
        let PeerSyncRequest::ShareWindow { share, .. } = request else {
            return;
        };
        let Some(error) = refused else {
            return;
        };
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let title = if *share {
            t("window-share-failed")
        } else {
            t("window-unshare-failed")
        };
        chrome.borrow_mut().push_warning_notification(title, error);
    }

    /// The bar-menu share items the person chose (`chosen`, by tab), asked
    /// as the window menu asks them.
    pub(super) fn share_from_bar_menus(&mut self, chosen: Vec<(TabId, String)>) {
        for (tab, item) in chosen {
            self.request_window_share(tab, item == SHARE_ITEM);
        }
    }

    /// Keep every shareable window's bar-menu item and mark, and stop the
    /// shares whose window went. Runs every frame; the windows are read
    /// again only once the tree changed. Whether a bar changed.
    pub(super) fn sync_window_share_controls(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let now_ms = tilo_clock::unix_now_ms();
        let outlived = {
            let state = chrome.borrow();
            let generation = state.tree_handle().generation();
            if self.window_share_controls.generation == Some(generation) {
                None
            } else {
                self.window_share_controls.generation = Some(generation);
                let tree = state.tree();
                let local = |workspace: &crate::shell::state::Workspace| {
                    state.local_user_for_room(&workspace.room())
                };
                self.window_share_controls.shown = shareable_windows(&tree, &local, now_ms);
                self.window_share_controls.theirs = windows_shared_by_others(&tree, now_ms);
                Some(tree.outlived_window_shares(now_ms))
            }
        };
        if let Some(outlived) = outlived {
            self.end_outlived_window_shares(outlived);
        }
        self.offer_window_share_items();
        let mut changed = false;
        let shown = &self.window_share_controls.shown;
        let theirs = &self.window_share_controls.theirs;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for id in ids {
                    let mark = shown.get(&id).and_then(|shown| shown.mark.clone());
                    changed |= group.set_tab_shared(id, mark);
                    changed |= group.set_tab_shared_by(id, theirs.get(&id).cloned());
                }
            }
        }
        changed
    }

    /// Put each shareable window's item in its bar's menu, and take it out
    /// of the menus of the windows that are no longer shareable. A Tilo
    /// app's window may also be shared live into a conversation: its menu
    /// keeps that item ([`super::live_shares`], set first each frame) and
    /// gets this one after it.
    fn offer_window_share_items(&mut self) {
        let controls = &mut self.window_share_controls;
        let now: HashSet<TabId> = controls.shown.keys().copied().collect();
        for tab in &now {
            if let Some(shown) = controls.shown.get(tab) {
                let mut items: Vec<_> = if self.live_shares.offers(*tab) {
                    self.window_toolbars
                        .host_items(*tab)
                        .iter()
                        .filter(|item| !is_share_item(&item.id))
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                items.push(shown.item.clone());
                if self.window_toolbars.host_items(*tab) != items.as_slice() {
                    self.window_toolbars.set_host_items(*tab, items);
                }
            }
        }
        for gone in controls.offered.difference(&now) {
            if self.live_shares.offers(*gone) {
                // Its conversation item stands alone again.
                let kept: Vec<_> = self
                    .window_toolbars
                    .host_items(*gone)
                    .iter()
                    .filter(|item| !is_share_item(&item.id))
                    .cloned()
                    .collect();
                self.window_toolbars.set_host_items(*gone, kept);
            } else {
                self.window_toolbars.set_host_items(*gone, Vec::new());
            }
        }
        controls.offered = now;
    }

    /// Delete the workspace `room`, which shares this device's windows: on
    /// the network worker, which stops the shares at the room's host before
    /// the room goes, as leaving does. Without a network no host can be
    /// asked, and the room goes as it would without shares.
    pub(super) fn delete_workspace(
        &mut self,
        room: crate::shell::state::RoomId,
        request_id: Option<u64>,
    ) {
        if self.peer_sync.is_running() {
            self.run_room_request(PeerSyncRequest::DeleteWorkspace(room), request_id);
            return;
        }
        if let Some(chrome) = self.chrome.as_ref() {
            let tree = chrome.borrow().tree_handle();
            if let Err(error) =
                crate::shell::native_workspace_owner::release_workspace_room(&tree, room)
            {
                tracing::warn!(%room, %error, "the workspace was not deleted");
            }
        }
        self.complete_peer_sync_action(request_id, Ok(serde_json::Value::Null));
    }

    /// Stop each share in `outlived` (by room), the shares whose window this
    /// device no longer shows: through the room's outbox, which asks the
    /// room's host until the room's record no longer holds the share, again
    /// on the host's next event (its session coming up, the room changing)
    /// when it refused or could not be reached. A refusal is said once.
    fn end_outlived_window_shares(
        &self,
        outlived: Vec<(
            crate::shell::state::RoomId,
            tilo_desktop_model::window_share::WindowShare,
        )>,
    ) {
        if outlived.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let state = chrome.borrow();
        let new: Vec<_> = {
            let tree = state.tree();
            outlived
                .into_iter()
                .filter(|(room, share)| {
                    !tree.room_outbox.iter().any(|entry| {
                        entry.room == *room
                            && matches!(&entry.change, crate::shell::state::RoomChange::WindowUnshare { tab, .. } if *tab == share.tab)
                    })
                })
                .collect()
        };
        if new.is_empty() {
            return;
        }
        let mut tree = state.tree_mut();
        for (room, share) in new {
            tree.stop_outlived_share(room, share.tab, share.app_id);
        }
    }
}
