//! The direct carrier for a native room: gossip between devices where no
//! relay holds the room.
//!
//! A room this device governs itself, and a room joined through another
//! interactive device rather than a workspace host, exchanges the same
//! `StatePeer` messages a relay session carries: the admitted room
//! document, and the signed app documents the owner holds in outgoing
//! custody. What makes an input admissible is the transport identity the
//! carrier authenticated, bound to the exact certificate the room's
//! governance currently admits (`Input::direct`); a message on a gossip
//! topic is never authority on its own. A workspace host's rooms keep
//! their relay carrier, so a room is carried one way, never both.

use super::{NeighbourMsg, RoomId, ShellNetwork, SystemTime, room_topic_id};
use crate::network::actor::native_ingress::Input;
use crate::shell::native_workspace_owner::{NativeCommittedView, NativeWorkspaceOwner};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// How often an idle direct room asks its peers for what this device lacks.
/// A room change publishes at once; this repairs an answer lost with the
/// peer that sent it.
const ASK_EVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct State {
    /// The owner generation last announced to each room's peers. A room
    /// whose generation is unchanged since then has nothing new to send.
    sent: Mutex<HashMap<RoomId, u64>>,
    /// When this device last asked each room's peers to catch it up.
    asked: Mutex<HashMap<RoomId, Instant>>,
}

/// Whether `view` is carried directly between devices rather than through a
/// workspace host: a room this device governs, or one whose enrolled host
/// is an interactive device that does not act as a workspace host.
#[must_use]
pub(super) fn is_direct(view: &NativeCommittedView) -> bool {
    if view.governed_here() {
        return true;
    }
    match view.host() {
        Some(host) => !host.capabilities.can_act_as_host,
        // A joined room with no separately pinned host is carried by the
        // device that governs it, not by a relay.
        None => true,
    }
}

impl ShellNetwork {
    /// Whether `ws` names a native room carried directly between devices.
    pub(super) fn direct_native_topic(&self, ws: u64) -> bool {
        let Some(room) = self.unique_room_for_topic(ws) else {
            return false;
        };
        let Some(owner) = self.opened_native_owner() else {
            return false;
        };
        owner
            .with_current(room, |view| Ok(is_direct(view)))
            .unwrap_or(false)
    }

    /// Announce every direct room whose admitted state moved since this
    /// device last announced it, and ask the room's peers for what this
    /// device lacks. Called from the frame tick through
    /// [`super::ShellNetwork::tick`], never waiting for the owner.
    pub(super) fn drive_direct_carriers(&self) {
        let Some(owner) = self.opened_native_owner() else {
            return;
        };
        let Ok(tree) = self.replica.lock().map(|replica| replica.tree.clone()) else {
            return;
        };
        let rooms = tree.read().rooms();
        let now = Instant::now();
        for room in rooms {
            let Ok(Some(generation)) =
                owner.with_current(room, |view| Ok(is_direct(view).then(|| view.generation())))
            else {
                if let Ok(mut sent) = self.direct_carrier.sent.lock() {
                    sent.remove(&room);
                }
                continue;
            };
            self.join_topic(room_topic_id(room));
            let changed = self
                .direct_carrier
                .sent
                .lock()
                .is_ok_and(|sent| sent.get(&room) != Some(&generation));
            if changed {
                let mut messages = Vec::new();
                if let Ok(Some(room_message)) =
                    owner.with_current(room, |view| Ok(Some(view.peer().room_message())))
                {
                    messages.push(room_message);
                }
                messages.extend(direct_outgoing(&owner, room));
                for message in messages {
                    let _ = self.publish_message(message);
                }
                if let Ok(mut sent) = self.direct_carrier.sent.lock() {
                    sent.insert(room, generation);
                }
            }
            let due = self.direct_carrier.asked.lock().is_ok_and(|mut asked| {
                let due = asked
                    .get(&room)
                    .is_none_or(|at| now.saturating_duration_since(*at) >= ASK_EVERY);
                if due {
                    asked.insert(room, now);
                }
                due
            });
            if due {
                let since = owner
                    .with_current(room, |view| {
                        Ok(view
                            .peer()
                            .admitted_room()
                            .map_or_else(Vec::new, |doc| doc.clone().get_heads()))
                    })
                    .unwrap_or_default();
                let since = serde_json::to_vec(&since).unwrap_or_default();
                let _ = self.publish_message(NeighbourMsg::SyncReq {
                    since,
                    room: room.to_string(),
                    invite_id: None,
                    nonce: fresh_request_nonce(),
                });
            }
        }
    }

    /// Whether `msg` belongs to a direct room's carrier, and if it does,
    /// handle it: a state input from an authenticated member goes through
    /// the same ordered admission a relay input takes, and a request is
    /// answered with the room's admitted changes. Answers whether the
    /// message was this carrier's.
    pub(super) fn dispatch_direct_carrier(
        &self,
        ws: u64,
        peer: iroh::EndpointId,
        msg: NeighbourMsg,
    ) -> bool {
        let Some(room) = self.unique_room_for_topic(ws) else {
            return false;
        };
        let Some(owner) = self.opened_native_owner() else {
            return false;
        };
        let direct = owner
            .with_current(room, |view| Ok(is_direct(view)))
            .unwrap_or(false);
        if !direct {
            return false;
        }
        // A read request is served only to a device the room currently
        // admits; the answer itself carries the room's admitted history.
        let Some(identity) = self.governed_identity(ws, peer) else {
            tracing::warn!(%peer, ws, "direct room message from a device the room does not admit");
            return true;
        };
        match msg {
            NeighbourMsg::SyncReq { since, .. } => {
                self.answer_direct_request(owner, room, since);
            }
            msg @ (NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }) => {
                if let NeighbourMsg::AppDocument {
                    peer: publisher, ..
                } = &msg
                    && *publisher != crate::network::behaviour::peer_liveness_handle(&peer)
                {
                    tracing::warn!(
                        %peer,
                        publisher,
                        "direct app document does not bind its authenticated transport peer"
                    );
                    return true;
                }
                self.dispatch_native_input_off_loop(Input::direct(
                    owner,
                    room,
                    identity.device_cert,
                    msg,
                ));
            }
            _ => return false,
        }
        true
    }

    /// Answer `since` for `room` with the changes the peer lacks, and the app
    /// documents still in this device's outgoing custody, so a device that
    /// just joined the direct room receives its records too.
    fn answer_direct_request(&self, owner: NativeWorkspaceOwner, room: RoomId, since: Vec<u8>) {
        let wants_records = since.is_empty();
        let since: Vec<automerge::ChangeHash> = serde_json::from_slice(&since).unwrap_or_default();
        let changes = owner
            .with_current(room, |view| {
                let mut doc = view
                    .peer()
                    .admitted_room()
                    .ok_or("direct room has no admitted document")?
                    .clone();
                let base = if since
                    .iter()
                    .all(|head| doc.get_change_by_hash(head).is_some())
                {
                    since
                } else {
                    Vec::new()
                };
                Ok(doc
                    .get_changes(&base)
                    .iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap_or_default();
        if !changes.is_empty() {
            let _ = self.publish_message(NeighbourMsg::SyncResp {
                changes,
                room: room.to_string(),
            });
        }
        // Only a device with no admitted state needs the signed app
        // documents too; one that already holds the room receives what it
        // lacks through its own pull and the owner's outgoing custody.
        if wants_records {
            for message in direct_outgoing(&owner, room) {
                let _ = self.publish_message(message);
            }
        }
    }
}

/// The signed app documents `owner` still holds in outgoing custody for
/// `room`, as the messages a direct peer admits exactly as a relay would.
fn direct_outgoing(owner: &NativeWorkspaceOwner, room: RoomId) -> Vec<NeighbourMsg> {
    owner
        .with_outgoing(room, SystemTime::now(), |_, outgoing| {
            Ok(outgoing
                .entries()
                .filter_map(|entry| postcard::from_bytes(entry.bytes()).ok())
                .collect::<Vec<_>>())
        })
        .unwrap_or_default()
}

/// A fresh nonce for one anti-entropy request: gossip deduplicates by
/// content, so a repeated request for unchanged heads needs a new value.
fn fresh_request_nonce() -> u64 {
    let mut bytes = [0; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    }
    u64::from_le_bytes(bytes)
}
