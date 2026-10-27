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
//!   ([`ui::widget::SharedWith`]).
//! - **The end.** A share whose window this device no longer shows in that
//!   workspace (closed, or moved to another one) is stopped. Leaving the
//!   workspace stops its shares on the way out
//!   ([`crate::shell::network::ShellNetwork::release_room`]).
//!
//! Every door asks the one request `tab.share` / `tab.unshare` make
//! ([`desktop_windows::share_request`]): the room's host records the share,
//! or its end, as a governance change attributed to the person.

use std::collections::{HashMap, HashSet};

use ui::widget::{GroupRow, SharedWith};
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
    /// Shares this device asked to stop because their window went, until
    /// the room's record no longer holds them.
    ending: HashSet<TabId>,
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

impl ShellOverlayHost {
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
    pub(super) fn window_share_answered(
        &mut self,
        request: &PeerSyncRequest,
        refused: Option<&str>,
    ) {
        let PeerSyncRequest::ShareWindow { tab, share, .. } = request else {
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
        } else if self.window_share_controls.ending.contains(tab) {
            t("window-share-end-failed")
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
                Some(tree.outlived_window_shares(now_ms))
            }
        };
        if let Some(outlived) = outlived {
            self.end_outlived_window_shares(outlived);
        }
        self.offer_window_share_items();
        let mut changed = false;
        let shown = &self.window_share_controls.shown;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for id in ids {
                    let mark = shown.get(&id).and_then(|shown| shown.mark.clone());
                    changed |= group.set_tab_shared(id, mark);
                }
            }
        }
        changed
    }

    /// Put each shareable window's item in its bar's menu, and take it out
    /// of the menus of the windows that are no longer shareable. A window
    /// shared live into a conversation keeps that item instead: the two
    /// never meet, since only a host window is shared with a workspace and
    /// only a Tilo app's window is shared into a conversation.
    fn offer_window_share_items(&mut self) {
        let controls = &mut self.window_share_controls;
        let now: HashSet<TabId> = controls
            .shown
            .keys()
            .copied()
            .filter(|tab| !self.live_shares.offers(*tab))
            .collect();
        for tab in &now {
            if let Some(shown) = controls.shown.get(tab) {
                self.window_toolbars
                    .set_host_items(*tab, vec![shown.item.clone()]);
            }
        }
        for gone in controls.offered.difference(&now) {
            self.window_toolbars.set_host_items(*gone, Vec::new());
        }
        controls.offered = now;
    }

    /// Stop each share in `outlived` (by room), the shares whose window this
    /// device no longer shows, once: the same request `tab.unshare` makes.
    /// A share asked to stop is not asked again while the room's record
    /// still holds it.
    fn end_outlived_window_shares(
        &mut self,
        outlived: Vec<(
            crate::shell::state::RoomId,
            tilo_desktop_model::window_share::WindowShare,
        )>,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if outlived.is_empty() && self.window_share_controls.ending.is_empty() {
            return;
        }
        let still: HashSet<TabId> = outlived.iter().map(|(_, share)| share.tab).collect();
        self.window_share_controls
            .ending
            .retain(|tab| still.contains(tab));
        for (room, share) in outlived {
            if !self.window_share_controls.ending.insert(share.tab) {
                continue;
            }
            let workspace = chrome
                .borrow()
                .tree()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.id);
            let request = PeerSyncRequest::ShareWindow {
                room,
                tab: share.tab,
                app_id: share.app_id,
                share: false,
            };
            self.dispatch_peer_sync(
                Job {
                    request: request.clone(),
                    require_pending: false,
                    workspace,
                },
                FollowUp::Directory { request },
            );
        }
    }
}
