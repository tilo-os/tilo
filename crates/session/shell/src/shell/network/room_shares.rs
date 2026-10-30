//! What this device's person shares with a room, and what they may do at a
//! node of it. A share is grants they give: this device writes them into its
//! replica of the room, signed with its own key, and publishes them as any
//! room change; every receiver admits them as their sharer's
//! ([`tilo_peer_state::room::share`]). What they may do at a node is the
//! room's policy, read here.
use std::sync::Arc;

use super::ShellNetwork;
use crate::shell::state::RoomId;
use tilo_action_broker::services::{RoomNodeAsk, RoomNodeOutcome};
use tilo_action_policy::share::ShareAccess;
use tilo_action_policy::{ActionKind, TreePath};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

/// How many grants of `share` `room` holds, as `owner` holds it.
fn share_count(
    owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
    room: RoomId,
    share: &str,
) -> Result<usize, String> {
    owner.with_current(room, |view| {
        let room_doc = view.peer().admitted_room().ok_or("the room is absent")?;
        Ok(
            tilo_desktop_model::projection::room_policy_of(room_doc, &room.to_string())
                .grants
                .iter()
                .filter(|grant| tilo_action_policy::share::is_of_share(grant, share))
                .count(),
        )
    })
}

impl ShellNetwork {
    /// Write `command`, a share or its end, in `room` as this device, and
    /// answer how many grants of `share` the room then holds. Blocks on the
    /// owner's open: for a worker, never the frame loop.
    fn write_share(
        &self,
        room: RoomId,
        share: &str,
        command: WorkspaceMembershipCommand,
    ) -> Result<usize, String> {
        let owner = self.native_owner()?;
        let caller = owner.with_current(room, |view| Ok(view.device().clone()))?;
        self.commit_membership_here(&owner, room, caller, command)?;
        share_count(&owner, room, share)
    }

    /// Share `node` of `room` with its members as `access` gives, under the
    /// share id `share`. Answers how many grants the share holds.
    ///
    /// # Errors
    /// The room refuses the share, or this device holds no replica of it.
    pub(crate) fn share_node(
        &self,
        room: RoomId,
        share: &str,
        node: TreePath,
        access: ShareAccess,
    ) -> Result<usize, String> {
        self.write_share(
            room,
            share,
            WorkspaceMembershipCommand::Share {
                share: share.to_owned(),
                node,
                access,
            },
        )
    }

    /// End this person's share `share` in `room`. Answers how many of its
    /// grants it ended.
    ///
    /// # Errors
    /// The room refuses it, or this device holds no replica of it.
    pub(crate) fn unshare_node(&self, room: RoomId, share: &str) -> Result<usize, String> {
        let owner = self.native_owner()?;
        let before = share_count(&owner, room, share)?;
        let left = self.write_share(
            room,
            share,
            WorkspaceMembershipCommand::Unshare {
                share: share.to_owned(),
            },
        )?;
        Ok(before.saturating_sub(left))
    }

    /// Whether this device's person may read, and make undoable changes
    /// at, `node` of `room`, as the room's policy decides here.
    ///
    /// # Errors
    /// This device holds no replica of the room, or is not admitted to it.
    pub(crate) fn node_permits(
        &self,
        room: RoomId,
        node: &TreePath,
    ) -> Result<(bool, bool), String> {
        if node.room_id() != Some(room.to_string().as_str()) {
            return Err("the node is not in this room".into());
        }
        let owner = self.native_owner()?;
        let now = std::time::SystemTime::now();
        owner.with_current(room, |view| {
            let member = view
                .peer()
                .permissions(view.device().device_public_key, now)?;
            Ok((
                member.allows(node, ActionKind::Read, now),
                member.allows(node, ActionKind::Undoable, now),
            ))
        })
    }

    /// Share this device's window `tab` of `app_id` with `room` (`share`),
    /// or stop sharing it: the room's resource share of the window's node,
    /// which every receiver takes only from the device the tab's topology
    /// namespace is registered to ([`tilo_desktop_model::window_share`]). A
    /// Tilo app's window is shared with what it shows (`shows`, a node),
    /// shared first, so no member ever sees the window as an outside one; a
    /// window share the room then refuses takes that back. Stopping the
    /// window's share ends both. Runs on the peer-sync worker.
    ///
    /// # Errors
    /// The room refused the share, or this device holds no replica of it.
    pub(crate) fn share_window(
        &self,
        room: RoomId,
        tab: crate::shell::state::TabId,
        app_id: &str,
        share: bool,
        shows: Option<&str>,
    ) -> Result<(), String> {
        use tilo_desktop_model::window_share::{
            window_node, window_share_id, window_shows_share_id,
        };
        let stop = window_share_id(tab);
        if !share {
            return self.unshare_node(room, &stop).map(drop);
        }
        if let Some(shows) = shows {
            let node = shows
                .parse()
                .map_err(|_| "what the window shows is not a node of the room")?;
            self.share_node(room, &window_shows_share_id(tab), node, ShareAccess::View)?;
        }
        let window = window_node(&room.to_string(), app_id, tab);
        self.share_node(room, &stop, window, ShareAccess::View)
            .map(drop)
            .inspect_err(|_| {
                if shows.is_some()
                    && let Err(error) = self.unshare_node(room, &stop)
                {
                    tracing::warn!(%room, tab, %error, "what a window shows stayed shared without it");
                }
            })
    }

    /// Answer `ask` of `node` in `room` as this device's person, on a
    /// worker: a share or its end written here, or what they may do there.
    ///
    /// # Errors
    /// No worker runs to answer it.
    pub(crate) fn ask_room_node(
        self: &Arc<Self>,
        room: RoomId,
        node: &str,
        ask: RoomNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<RoomNodeOutcome>, String> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("the room's worker is unavailable")?;
        let network = Arc::clone(self);
        let node = node.to_owned();
        let (completer, answer) = tilo_actions::remote::pending();
        runtime.spawn(async move {
            let outcome =
                tokio::task::spawn_blocking(move || network.answer_room_node(room, &node, ask))
                    .await
                    .map_err(|_| "the room's worker stopped".to_owned())
                    .and_then(std::convert::identity);
            completer.complete(outcome);
        });
        Ok(answer)
    }

    /// What `ask` of `node` in `room` comes to, as the room's member actions
    /// answered it: `grants` a share holds, `revoked` by its end, or `read`
    /// and `undoable` at the node.
    fn answer_room_node(&self, room: RoomId, node: &str, ask: RoomNodeAsk) -> RoomNodeOutcome {
        let parsed = || -> Result<TreePath, String> {
            node.parse()
                .map_err(|_| "`node` is not a node of the room's tree".to_owned())
        };
        Ok(match ask {
            RoomNodeAsk::Share { share, access } => {
                let access: ShareAccess = access
                    .parse()
                    .map_err(|_| "`access` is `view` or `edit`".to_owned())?;
                serde_json::json!({ "grants": self.share_node(room, &share, parsed()?, access)? })
            }
            RoomNodeAsk::Unshare { share } => {
                serde_json::json!({ "revoked": self.unshare_node(room, &share)? })
            }
            RoomNodeAsk::Permits => {
                let (read, undoable) = self.node_permits(room, &parsed()?)?;
                serde_json::json!({ "read": read, "undoable": undoable })
            }
        })
    }
}
