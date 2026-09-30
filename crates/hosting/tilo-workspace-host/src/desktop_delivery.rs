//! Low-latency delivery of an already durable desktop commit. Retained
//! publication remains the recovery path; this path never executes a command.
use crate::SessionOwner;
use std::collections::{BTreeMap, BTreeSet};
use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction, MAX_PRIVATE_BYTES};
use tilo_peer_state::{
    NeighbourMsg,
    desktop_command::DesktopCommandError,
    owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest},
    transport::Retention,
};
use ui::automerge::{AutoCommit, transaction::Transactable};

type Route = (String, String);

fn delivery_time() -> web_time::SystemTime {
    web_time::SystemTime::now()
}

fn visible_requirements(state: &tilo_peer_state::StatePeer) -> Vec<Route> {
    let Some(room) = tilo_desktop_model::RoomId::parse(state.room()) else {
        return vec![];
    };
    let Some(doc) = state.admitted_room() else {
        return vec![];
    };
    let mut tree = tilo_desktop_model::DesktopTree::default();
    if tilo_desktop_model::projection::apply_admitted_room(&mut tree, room, doc).is_err() {
        return vec![];
    }
    let routes = tilo_desktop_model::apps::record_routes(doc);
    let rows = state.rows();
    let mut required = Vec::new();
    for tab in tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        let Some(tab_app) = tilo_desktop_model::apps::canonical_app_id(&tab.tile) else {
            continue;
        };
        for row in &rows {
            let Some(definition) = crate::desktop_commands::checked_application(&row.app_id) else {
                continue;
            };
            if row.app_id != tab_app {
                continue;
            }
            let record = routes.get(&tab.id).cloned().unwrap_or_else(|| {
                let checkout = ui::record::RecordCheckout::for_component_tab(
                    &row.app_id,
                    tab.id,
                    definition.record_kind.clone(),
                );
                if checkout.id.is_package_primary() {
                    String::new()
                } else {
                    checkout.id.as_str().to_owned()
                }
            });
            if row.record_id != record || row.heads.is_empty() {
                continue;
            }
            let route = (row.app_id.clone(), row.record_id.clone());
            if !required.contains(&route) {
                required.push(route);
            }
        }
    }
    required
}

fn is_current_snapshot(
    state: &tilo_peer_state::StatePeer,
    route: &Route,
    message: &NeighbourMsg,
    now: web_time::SystemTime,
) -> bool {
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope,
    } = message
    else {
        return false;
    };
    if (app_id, record_id) != (&route.0, &route.1)
        || cluster_id != state.room()
        || *peer != tilo_peer_state::peer_handle(&state.authority().device_public_key)
        || envelope.author.as_bytes() != &state.authority().device_public_key
        || !tilo_peer_state::outbox::fresh(message, now)
        || !state
            .durable_covers_current(app_id, record_id)
            .is_ok_and(|current| current)
    {
        return false;
    }
    let Ok(Some(receipt)) = state.durable_coverage(app_id, record_id) else {
        return false;
    };
    let coverage = &receipt.coverage;
    if coverage.room != state.room()
        || coverage.app != *app_id
        || coverage.record != *record_id
        || coverage.author != state.authority().device_public_key
        || coverage.sequence != envelope.seq
        || coverage.payload_hash != *blake3::hash(&envelope.payload).as_bytes()
        || coverage
            .watermarks
            .get(&hex::encode(state.authority().device_public_key))
            .copied()
            != Some(envelope.seq)
    {
        return false;
    }
    let Ok(collab_types::WholeDocumentPayload::Cleartext(
        collab_types::DocumentMutation::Snapshot(bytes),
    )) = collab_types::validate_whole_document_payload_route(
        app_id, cluster_id, record_id, envelope,
    )
    else {
        return false;
    };
    let Ok(snapshot) = AutoCommit::load(&bytes) else {
        return false;
    };
    snapshot
        .base_heads()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        == coverage.heads
}

fn action_packet_budget(
    invocation: tilo_actions::InvocationId,
    opened: Option<tilo_delivery_protocol::peer_actions::OpenedDesktopTab>,
) -> Option<usize> {
    let empty = serde_json::to_vec(&ActionMessage::CommittedDesktop {
        invocation,
        packets: vec![],
        opened,
    })
    .ok()?;
    MAX_PRIVATE_BYTES.checked_sub(empty.len())
}

fn push_packet(
    encoder: &mut tilo_peer_state::desktop_delivery::Encoder,
    remaining_action_bytes: &mut usize,
    bytes: &[u8],
) -> Result<(), String> {
    encoder.preflight_raw(bytes)?;
    let packet = tilo_peer_state::desktop_delivery::encode_packet(bytes)?;
    let action_bytes = packet.wire_bytes() + if encoder.packet_count() == 0 { 2 } else { 3 };
    if action_bytes > *remaining_action_bytes {
        return Err("committed desktop exceeds private action budget".into());
    }
    encoder.push(packet)?;
    *remaining_action_bytes -= action_bytes;
    Ok(())
}

impl SessionOwner {
    pub fn commit_desktop_command_with_delivery(
        &mut self,
        request: &OwnerCommitRequest,
    ) -> Result<(OwnerCommitReceipt, Option<DirectedAction>), DesktopCommandError> {
        self.commit_desktop_command_with_delivery_current(request, None)
    }
    pub(crate) fn commit_desktop_command_with_delivery_policy_time(
        &mut self,
        request: &OwnerCommitRequest,
        time: &tilo_action_broker::peer_actions::PolicyTime,
    ) -> Result<(OwnerCommitReceipt, Option<DirectedAction>), DesktopCommandError> {
        self.commit_desktop_command_with_delivery_current(request, Some(time))
    }
    fn commit_desktop_command_with_delivery_current(
        &mut self,
        request: &OwnerCommitRequest,
        time: Option<&tilo_action_broker::peer_actions::PolicyTime>,
    ) -> Result<(OwnerCommitReceipt, Option<DirectedAction>), DesktopCommandError> {
        self.require_owner_resource_scope()
            .map_err(|e| DesktopCommandError::Rejected(e.to_string()))?;
        let previous_tabs = tab_locations(self.peer());
        let previous_rows = self.peer().rows();
        let baseline = self.publication_baseline().0;
        let start = self.outgoing().len();
        let receipt = match time {
            Some(time) => self.commit_desktop_command_with_policy_time(request, time)?,
            None => self.commit_desktop_command(request)?,
        };
        // Construction/delivery failure cannot turn a durable success into a
        // rejection. The ordinary publication is still retained and retried.
        let delivery = (|| -> Option<DirectedAction> {
            let CommitOperation::DesktopCommand {
                invocation,
                caller,
                command,
            } = &request.operation
            else {
                return None;
            };
            let mut room = self.peer().admitted_room()?.clone();
            let heads = command
                .source
                .heads
                .iter()
                .copied()
                .map(ui::automerge::ChangeHash)
                .collect::<Vec<_>>();
            let changes = room
                .get_changes(&heads)
                .into_iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect();
            let added = tab_locations(self.peer())
                .into_iter()
                .filter(|route| !previous_tabs.iter().any(|old| old.tab == route.tab))
                .collect::<Vec<_>>();
            let opened = matches!(
                command.command,
                tilo_peer_state::desktop_command::DesktopCommand::OpenTab { .. }
                    | tilo_peer_state::desktop_command::DesktopCommand::OpenApp { .. }
            )
            .then(|| added.first().filter(|_| added.len() == 1).cloned())
            .flatten();
            let mut remaining_action_bytes =
                action_packet_budget(tilo_actions::InvocationId(*invocation), opened.clone())?;
            let mut packets = tilo_peer_state::desktop_delivery::Encoder::new();
            let room_packet = postcard::to_stdvec(&NeighbourMsg::SyncResp {
                room: request.room.clone(),
                changes,
            })
            .ok()?;
            push_packet(&mut packets, &mut remaining_action_bytes, &room_packet).ok()?;
            // Compaction replaces the tail with a complete committed baseline.
            let start = if baseline == self.publication_baseline().0 {
                start
            } else {
                0
            };
            let mut mandatory = BTreeSet::new();
            for publication in self.outgoing().iter().skip(start) {
                let bytes = publication.bytes;
                if let NeighbourMsg::AppDocument {
                    app_id, record_id, ..
                } = postcard::from_bytes::<NeighbourMsg>(bytes).ok()?
                {
                    let unchanged = previous_rows
                        .iter()
                        .find(|r| r.app_id == app_id && r.record_id == record_id)
                        .is_some_and(|before| {
                            self.peer().rows().iter().any(|after| after == before)
                        });
                    if !unchanged {
                        mandatory.insert((app_id, record_id));
                        push_packet(&mut packets, &mut remaining_action_bytes, bytes).ok()?;
                    }
                }
            }
            let requirements = visible_requirements(self.peer());
            let missing = requirements
                .iter()
                .filter(|route| !mandatory.contains(*route))
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut snapshots = BTreeMap::new();
            let now = match time {
                Some(time) => crate::owner::policy_fence::sample(Some(time)).ok()?,
                None => delivery_time(),
            };
            let baseline_len = self.publication_baseline().1;
            for (index, publication) in self.outgoing().iter().enumerate().rev() {
                let bytes = publication.bytes;
                let kind = publication.kind;
                if snapshots.len() == missing.len() {
                    break;
                }
                if kind != Retention::Checkpoint || (index == 0 && baseline_len != 0) {
                    continue;
                }
                let Ok(message) = postcard::from_bytes::<NeighbourMsg>(bytes) else {
                    continue;
                };
                let NeighbourMsg::AppDocument {
                    app_id, record_id, ..
                } = &message
                else {
                    continue;
                };
                let route = (app_id.clone(), record_id.clone());
                if missing.contains(&route)
                    && !snapshots.contains_key(&route)
                    && is_current_snapshot(self.peer(), &route, &message, now)
                {
                    snapshots.insert(route, bytes);
                }
            }
            for route in requirements {
                if let Some(bytes) = snapshots.remove(&route) {
                    let _ = push_packet(&mut packets, &mut remaining_action_bytes, bytes);
                }
            }
            let packets = packets.into_packets().ok()?;
            let action = DirectedAction {
                room: request.room.parse().ok()?,
                request_id: *invocation,
                source: self.peer().authority().device_public_key,
                target: caller.device_public_key,
                message: ActionMessage::CommittedDesktop {
                    invocation: tilo_actions::InvocationId(*invocation),
                    packets,
                    opened,
                },
            };
            action.validate().then_some(action)
        })();
        Ok((receipt, delivery))
    }
}

fn tab_locations(
    state: &tilo_peer_state::StatePeer,
) -> Vec<tilo_delivery_protocol::peer_actions::OpenedDesktopTab> {
    let mut tree = tilo_desktop_model::DesktopTree::default();
    let Some(room) = tilo_desktop_model::RoomId::parse(state.room()) else {
        return vec![];
    };
    let Some(doc) = state.admitted_room() else {
        return vec![];
    };
    if tilo_desktop_model::projection::apply_admitted_room(&mut tree, room, doc).is_err() {
        return vec![];
    }
    tree.workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|s| {
            s.window_groups.iter().flat_map(move |g| {
                g.tabs.iter().map(
                    move |t| tilo_delivery_protocol::peer_actions::OpenedDesktopTab {
                        strip: s.id.0.to_string(),
                        group: g.id.to_string(),
                        tab: t.id.to_string(),
                    },
                )
            })
        })
        .collect()
}
