//! A Tilo app's window shared with a workspace where each person lays out
//! their own windows follows the item its sharer's window is at
//! ([`crate::shell::state::WorkspaceTree::shared_window_opening`]).
//!
//! - **The sharer's device** keeps the share saying what the window shows:
//!   when the window moves to another item (its bar anchors another), it
//!   shares what it shows again under the same share id, which the room's
//!   host writes over the one before. The share is the only channel.
//! - **Every other device** opens its window of the share at that item,
//!   through the window's own `open` (the framework's, which a search hit
//!   at the item runs): as soon as the window publishes it, and again each
//!   time the share names another item, until the person there moves the
//!   window elsewhere themselves.

use std::collections::HashMap;

use tilo_desktop_model::window_share::WindowShows;

use super::ShellOverlayHost;
use super::peer_sync_dispatch::{FollowUp, Job};
use crate::shell::peer_sync_overlay::PeerSyncRequest;
use crate::shell::search::providers::app_content::{OpenDoor, open_in_window};
use crate::shell::state::{RoomId, TabId};

/// What the shell keeps to keep shared windows at their sharer's item.
#[derive(Default)]
pub(crate) struct SharedWindowFollow {
    /// The tree generation the windows below were read at.
    generation: Option<u64>,
    /// This device's own Tilo app windows shared with a room, with what the
    /// room's record says each shows.
    own: Vec<OwnShare>,
    /// What this device last asked each of its shares to say its window
    /// shows, as a node: asked once per change.
    said: HashMap<TabId, String>,
    /// The Tilo app windows others shared that this device shows.
    theirs: Vec<(RoomId, TabId, String)>,
    /// Each such window as last looked at: where its bar said it was and
    /// what its share said, and, while an opening waits for the window to
    /// publish its `open`, the broker's surface generation it waited at.
    seen: HashMap<TabId, Seen>,
    /// Where an opening goes: absent until the session's broker attaches.
    door: Option<OpenDoor>,
}

/// One of this device's own shared Tilo app windows.
struct OwnShare {
    room: RoomId,
    tab: TabId,
    app_id: String,
    recorded: Option<WindowShows>,
}

/// A shared window of someone else's, as last looked at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seen {
    at: Option<String>,
    item: Option<String>,
    waiting_at: Option<u64>,
}

impl SharedWindowFollow {
    /// The session's broker's door, once it attached.
    pub(crate) fn door(&self) -> Option<OpenDoor> {
        self.door.clone()
    }

    /// Open shared windows through `door`, the session's broker.
    pub(crate) fn attach(&mut self, door: OpenDoor) {
        self.door = Some(door);
    }
}

impl ShellOverlayHost {
    /// Keep this device's shares saying where their windows are, and the
    /// windows others share that this device shows where theirs are. Runs
    /// every frame; the windows are listed again only once the tree
    /// changed, and each is looked at in what its bar and its share say.
    pub(super) fn follow_shared_windows(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        {
            let state = chrome.borrow();
            let generation = state.tree_handle().generation();
            if self.shared_window_follow.generation != Some(generation) {
                self.shared_window_follow.generation = Some(generation);
                let tree = state.tree();
                let now_ms = tilo_clock::unix_now_ms();
                let local = tree.local_salt();
                let follow = &mut self.shared_window_follow;
                follow.own.clear();
                follow.theirs.clear();
                for workspace in &tree.desktop.workspaces {
                    let room = workspace.room();
                    for share in tree.shared_windows(room, now_ms) {
                        if share.shows.is_none() {
                            continue;
                        }
                        if local != 0 && tilo_desktop_model::peer_salt_of(share.tab) == local {
                            follow.own.push(OwnShare {
                                room,
                                tab: share.tab,
                                app_id: share.app_id.clone(),
                                recorded: share.shows.clone(),
                            });
                        } else {
                            follow.theirs.push((room, share.tab, share.app_id.clone()));
                        }
                    }
                }
                let (own, theirs) = (&follow.own, &follow.theirs);
                follow
                    .said
                    .retain(|tab, _| own.iter().any(|share| share.tab == *tab));
                follow
                    .seen
                    .retain(|tab, _| theirs.iter().any(|(_, shown, _)| shown == tab));
            }
        }
        self.say_where_shared_windows_are();
        self.open_shared_windows_where_theirs_are(&chrome);
    }

    /// Share again what each of this device's shared Tilo app windows shows,
    /// once it shows something else than the room's record says: the
    /// request the share was made with, which the host writes over the
    /// share of what it showed before.
    fn say_where_shared_windows_are(&mut self) {
        let mut asks = Vec::new();
        for share in &self.shared_window_follow.own {
            let (shows, kind) = self.window_shows_now(share.tab);
            // A window whose app has not said what it shows (yet, or again
            // after a restart) leaves its share as it was.
            if kind.is_empty() || share.recorded.as_ref() == Some(&shows) {
                continue;
            }
            let node = tilo_desktop_model::window_share::shows_node(
                &share.room.to_string(),
                &share.app_id,
                share.tab,
                &kind,
                &shows,
            )
            .to_string();
            if self.shared_window_follow.said.get(&share.tab) == Some(&node) {
                continue;
            }
            asks.push((share.room, share.tab, share.app_id.clone(), node));
        }
        for (room, tab, app_id, node) in asks {
            self.shared_window_follow.said.insert(tab, node.clone());
            let request = PeerSyncRequest::ShareWindow {
                room,
                tab,
                app_id,
                share: true,
                shows: Some(node),
            };
            self.dispatch_peer_sync(
                Job {
                    request: request.clone(),
                    require_pending: false,
                    workspace: None,
                },
                FollowUp::Directory { request },
            );
        }
    }

    /// Open each window others shared that this device shows at the item
    /// its sharer's window is at, while it follows it. A window is looked
    /// at again only when its bar or its share say something new, or, while
    /// it has not published its `open` yet, when the broker's windows
    /// changed.
    fn open_shared_windows_where_theirs_are(
        &mut self,
        chrome: &crate::shell::chrome_state::Chrome,
    ) {
        if self.shared_window_follow.theirs.is_empty() {
            return;
        }
        let Some(door) = self.shared_window_follow.door.clone() else {
            return;
        };
        let theirs = self.shared_window_follow.theirs.clone();
        let mut surfaces = None;
        for (room, tab, app_id) in theirs {
            let at = self
                .window_toolbars
                .toolbar(tab)
                .and_then(|toolbar| toolbar.location.as_ref())
                .and_then(|location| location.anchor.clone())
                .filter(|anchor| !anchor.is_empty());
            let item = {
                let state = chrome.borrow();
                let tree = state.tree();
                tree.shared_windows(room, tilo_clock::unix_now_ms())
                    .find(|share| share.tab == tab)
                    .and_then(|share| share.shows.as_ref()?.item.clone())
            };
            let seen = self.shared_window_follow.seen.entry(tab).or_default();
            let same = seen.at == at && seen.item == item;
            if same {
                let Some(waited) = seen.waiting_at else {
                    continue;
                };
                let now = *surfaces.get_or_insert_with(|| door.surfaces_generation());
                if now == waited {
                    continue;
                }
            }
            seen.at.clone_from(&at);
            seen.item.clone_from(&item);
            seen.waiting_at = None;
            let opening = chrome
                .borrow()
                .tree_mut()
                .shared_window_opening(tab, at.as_deref());
            let instance = chrome
                .borrow()
                .tree_mut()
                .local_app_window(tab)
                .map(|(_, _, instance)| instance);
            let Some(item) = opening else {
                continue;
            };
            let room_text = room.to_string();
            let opened = match instance {
                Some(instance) => {
                    open_in_window(&door, chrome, (&app_id, &room_text), instance, &item)
                }
                None => Ok(false),
            };
            match opened {
                Ok(true) => {
                    chrome.borrow().tree_mut().shared_window_opened(tab, &item);
                }
                Ok(false) => {
                    let now = *surfaces.get_or_insert_with(|| door.surfaces_generation());
                    if let Some(seen) = self.shared_window_follow.seen.get_mut(&tab) {
                        seen.waiting_at = Some(now);
                    }
                }
                Err(error) => {
                    tracing::info!(%room, tab, ?error, "a shared window could not be opened at its sharer's item");
                    chrome.borrow().tree_mut().shared_window_opened(tab, &item);
                }
            }
        }
    }
}
