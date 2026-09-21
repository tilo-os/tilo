//! Low-latency delivery of an already durable desktop commit. Retained
//! publication remains the recovery path; this lane never executes a command.
use crate::SessionOwner;
use qos_peer_state::{
    NeighbourMsg,
    desktop_command::DesktopCommandError,
    owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest},
    transport::Retention,
};
use std::collections::{BTreeMap, BTreeSet};
use tilo_relay::peer_actions::{ActionMessage, DirectedAction, MAX_PRIVATE_BYTES};
use ui::automerge::{AutoCommit, transaction::Transactable};

type Route = (String, String);

fn visible_requirements(state: &qos_peer_state::StatePeer) -> Vec<Route> {
    let Some(room) = qos_desktop::RoomId::parse(state.room()) else {
        return vec![];
    };
    let Some(doc) = state.admitted_room() else {
        return vec![];
    };
    let mut tree = qos_desktop::WorkspaceTree::default();
    if qos_desktop::projection::RoomProjection::apply_admitted_room(&mut tree, room, doc).is_err() {
        return vec![];
    }
    let routes = qos_desktop::projection::RoomProjection::record_routes(doc);
    let rows = state.rows();
    let mut required = Vec::new();
    for tab in tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        let Some(tab_app) = qos_desktop::resolver::canonical_app_id(&tab.tile) else {
            continue;
        };
        for row in &rows {
            let Some(definition) = crate::desktop_commands::checked_application(&row.app_id) else {
                continue;
            };
            if !ui::application::lookup_compatible_first_party_app_ids(&row.app_id, &tab_app) {
                continue;
            }
            let record = routes.get(&tab.id).cloned().unwrap_or_else(|| {
                let checkout = qos_desktop::records::component_record(
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
    state: &qos_peer_state::StatePeer,
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
        || *peer != qos_peer_state::peer_handle(&state.authority().device_public_key)
        || envelope.author.as_bytes() != &state.authority().device_public_key
        || !qos_peer_state::outbox::fresh(message, now)
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
    let Ok(collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded)) =
        collab_types::validate_document_payload_route_versioned(
            app_id, cluster_id, record_id, envelope,
        )
    else {
        return false;
    };
    let collab_types::DocumentMutation::Snapshot(bytes) = decoded.mutation else {
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
    opened: Option<tilo_relay::peer_actions::OpenedDesktopTab>,
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
    encoder: &mut qos_peer_state::desktop_delivery::Encoder,
    remaining_action_bytes: &mut usize,
    bytes: &[u8],
) -> Result<(), String> {
    encoder.preflight_raw(bytes)?;
    let packet = qos_peer_state::desktop_delivery::encode_packet(bytes)?;
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
        let previous_tabs = tab_locations(self.peer());
        let previous_rows = self.peer().rows();
        let baseline = self.publication_baseline().0;
        let start = self.outgoing().len();
        let receipt = self.commit_desktop_command(request)?;
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
                qos_peer_state::desktop_command::DesktopCommand::OpenTab { .. }
            )
            .then(|| added.first().filter(|_| added.len() == 1).cloned())
            .flatten();
            let mut remaining_action_bytes =
                action_packet_budget(tilo_actions::InvocationId(*invocation), opened.clone())?;
            let mut packets = qos_peer_state::desktop_delivery::Encoder::new();
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
            for bytes in &self.outgoing()[start..] {
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
            let now = web_time::SystemTime::now();
            let baseline_len = self.publication_baseline().1;
            for (index, (bytes, kind)) in self
                .outgoing()
                .iter()
                .zip(self.outgoing_kinds())
                .enumerate()
                .rev()
            {
                if snapshots.len() == missing.len() {
                    break;
                }
                if *kind != Retention::Checkpoint || (index == 0 && baseline_len != 0) {
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
    state: &qos_peer_state::StatePeer,
) -> Vec<tilo_relay::peer_actions::OpenedDesktopTab> {
    let mut tree = qos_desktop::WorkspaceTree::default();
    let Some(room) = qos_desktop::RoomId::parse(state.room()) else {
        return vec![];
    };
    let Some(doc) = state.admitted_room() else {
        return vec![];
    };
    if qos_desktop::projection::RoomProjection::apply_admitted_room(&mut tree, room, doc).is_err() {
        return vec![];
    }
    tree.workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|s| {
            s.window_groups.iter().flat_map(move |g| {
                g.tabs
                    .iter()
                    .map(move |t| tilo_relay::peer_actions::OpenedDesktopTab {
                        strip: s.id.0.to_string(),
                        group: g.id.to_string(),
                        tab: t.id.to_string(),
                    })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_peer_state::{
        TransportOrigin,
        desktop_command::{BoundCommand, CommandSource, DesktopCommand, TabPlacement},
        owner_commit::{CommitOperation, OwnerCommitRequest},
    };
    use tilo_relay::transport_v2::EnvelopeV2;

    fn request(owner: &SessionOwner, command: DesktopCommand) -> OwnerCommitRequest {
        OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::DesktopCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: owner.peer().authority().clone(),
                command: BoundCommand {
                    source: CommandSource {
                        room: owner.peer().room().into(),
                        heads: owner
                            .peer()
                            .admitted_room()
                            .unwrap()
                            .clone()
                            .get_heads()
                            .into_iter()
                            .map(|head| head.0)
                            .collect(),
                    },
                    command,
                },
            },
        }
    }

    fn strip(owner: &SessionOwner) -> u64 {
        let mut tree = qos_desktop::WorkspaceTree::default();
        qos_desktop::projection::RoomProjection::apply_admitted_room(
            &mut tree,
            qos_desktop::RoomId::parse(owner.peer().room()).unwrap(),
            owner.peer().admitted_room().unwrap(),
        )
        .unwrap();
        tree.workspaces[0].strips[0].id.0
    }

    fn apply_first_publication(
        owner: &SessionOwner,
        browser: &mut qos_peer_state::StatePeer,
        start: usize,
        include_documents: bool,
    ) {
        let source = owner.peer().authority().device_public_key;
        for bytes in &owner.outgoing()[start..] {
            let message = postcard::from_bytes(bytes).unwrap();
            if !include_documents && matches!(message, NeighbourMsg::AppDocument { .. }) {
                continue;
            }
            browser
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(source),
                    message,
                    web_time::SystemTime::now(),
                )
                .unwrap();
        }
    }

    #[test]
    fn stale_visible_prerequisite_does_not_suppress_the_new_direct_open() {
        use web_time::Duration;

        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("stale-direct")).unwrap();
        let mut healthy = owner.peer().clone();
        let mut missing = owner.peer().clone();
        let first = request(
            &owner,
            DesktopCommand::OpenTab {
                app: "dev.qos.notes".into(),
                placement: TabPlacement::NewGroup {
                    strip: strip(&owner),
                    after: None,
                },
            },
        );
        let start = owner.outgoing().len();
        let stale = web_time::SystemTime::now()
            .checked_sub(Duration::from_secs(301))
            .unwrap();
        crate::owner::with_test_publication_time(stale, || {
            owner.commit_desktop_command(&first).unwrap();
        });
        let stale_route = owner.outgoing()[start..]
            .iter()
            .find_map(|bytes| match postcard::from_bytes(bytes).unwrap() {
                NeighbourMsg::AppDocument {
                    app_id, record_id, ..
                } => Some((app_id, record_id)),
                _ => None,
            })
            .expect("first launch publishes its record checkpoint");
        apply_first_publication(&owner, &mut healthy, start, true);
        apply_first_publication(&owner, &mut missing, start, false);

        let second = request(
            &owner,
            DesktopCommand::OpenTab {
                app: "dev.qos.calculator".into(),
                placement: TabPlacement::NewGroup {
                    strip: strip(&owner),
                    after: None,
                },
            },
        );
        let (_, delivery) = owner.commit_desktop_command_with_delivery(&second).unwrap();
        let delivery = delivery.expect("fresh mandatory calculator launch stays direct");
        let target = delivery.target;
        let ActionMessage::CommittedDesktop { packets, .. } = &delivery.message else {
            panic!("desktop command emits a committed desktop action")
        };
        let messages = qos_peer_state::desktop_delivery::decode(packets)
            .unwrap()
            .into_iter()
            .map(|bytes| postcard::from_bytes::<NeighbourMsg>(&bytes).unwrap())
            .collect::<Vec<_>>();
        assert!(messages.iter().any(
            |message| matches!(message, NeighbourMsg::AppDocument { app_id, .. } if app_id == "dev.qos.calculator")
        ));
        assert!(!messages.iter().any(
            |message| matches!(message, NeighbourMsg::AppDocument { app_id, record_id, .. } if (app_id, record_id) == (&stale_route.0, &stale_route.1))
        ));
        let source_user = owner.peer().authority().user_public_key;
        let event = EnvelopeV2::Delivered {
            action: delivery,
            source_user,
        };
        assert!(matches!(
            qos_browser_runtime::committed_desktop::admit(
                &healthy,
                &event,
                target,
                web_time::SystemTime::now(),
            )
            .unwrap(),
            qos_browser_runtime::committed_desktop::Admission::Ready { .. }
        ));
        assert!(matches!(
            qos_browser_runtime::committed_desktop::admit(
                &missing,
                &event,
                target,
                web_time::SystemTime::now(),
            )
            .unwrap(),
            qos_browser_runtime::committed_desktop::Admission::AwaitingRetainedState(_)
        ));
    }

    #[test]
    fn current_checkpoint_after_baseline_replacement_catches_up_a_missing_visible_record() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("checkpoint-direct")).unwrap();
        let mut browser = owner.peer().clone();
        let first = request(
            &owner,
            DesktopCommand::OpenTab {
                app: "dev.qos.notes".into(),
                placement: TabPlacement::NewGroup {
                    strip: strip(&owner),
                    after: None,
                },
            },
        );
        let start = owner.outgoing().len();
        owner.commit_desktop_command(&first).unwrap();
        apply_first_publication(&owner, &mut browser, start, false);

        owner.refresh_checkpoint().unwrap();
        let notes_checkpoint = owner
            .outgoing()
            .iter()
            .find(|bytes| {
                matches!(
                    postcard::from_bytes::<NeighbourMsg>(bytes).unwrap(),
                    NeighbourMsg::AppDocument { ref app_id, .. } if app_id == "dev.qos.notes"
                )
            })
            .cloned()
            .expect("the replacement basis contains the current Notes snapshot");
        apply_first_publication(&owner, &mut browser, 0, false);

        let second = request(
            &owner,
            DesktopCommand::OpenTab {
                app: "dev.qos.calculator".into(),
                placement: TabPlacement::NewGroup {
                    strip: strip(&owner),
                    after: None,
                },
            },
        );
        let (_, delivery) = owner.commit_desktop_command_with_delivery(&second).unwrap();
        let delivery = delivery.expect("the fresh calculator open stays direct");
        let target = delivery.target;
        let ActionMessage::CommittedDesktop { packets, .. } = &delivery.message else {
            panic!("desktop command emits a committed desktop action")
        };
        assert!(
            qos_peer_state::desktop_delivery::decode(packets)
                .unwrap()
                .iter()
                .any(|bytes| bytes == &notes_checkpoint),
            "the low-latency action carries the exact current checkpoint from the replacement basis"
        );
        let source_user = owner.peer().authority().user_public_key;
        let event = EnvelopeV2::Delivered {
            action: delivery,
            source_user,
        };
        assert!(matches!(
            qos_browser_runtime::committed_desktop::admit(
                &browser,
                &event,
                target,
                web_time::SystemTime::now(),
            )
            .unwrap(),
            qos_browser_runtime::committed_desktop::Admission::Ready { .. }
        ));
    }

    #[test]
    fn oversized_visible_prerequisite_does_not_suppress_the_new_direct_open() {
        use ui::automerge::{ROOT, ScalarValue, transaction::Transactable};

        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("oversized-direct")).unwrap();
        let mut healthy = owner.peer().clone();
        let mut missing = owner.peer().clone();
        let first = request(
            &owner,
            DesktopCommand::OpenTab {
                app: "dev.qos.notes".into(),
                placement: TabPlacement::NewGroup {
                    strip: strip(&owner),
                    after: None,
                },
            },
        );
        let first_start = owner.outgoing().len();
        owner.commit_desktop_command(&first).unwrap();
        let notes_route = owner.outgoing()[first_start..]
            .iter()
            .find_map(|bytes| match postcard::from_bytes(bytes).unwrap() {
                NeighbourMsg::AppDocument {
                    app_id, record_id, ..
                } if app_id == "dev.qos.notes" => Some((app_id, record_id)),
                _ => None,
            })
            .expect("opening Notes publishes its owned record");
        apply_first_publication(&owner, &mut healthy, first_start, true);
        apply_first_publication(&owner, &mut missing, first_start, false);

        let mut state = 0x9e37_79b9u32;
        let incompressible = (0..160_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect::<Vec<_>>();
        let mutation_start = owner.outgoing().len();
        crate::owner::mutate_owned_record_for_test(
            &mut owner,
            &notes_route.0,
            &notes_route.1,
            |document| {
                document
                    .put(
                        ROOT,
                        "direct-delivery-overflow",
                        ScalarValue::Bytes(incompressible),
                    )
                    .unwrap();
            },
        )
        .unwrap();
        let oversized_checkpoint = owner.outgoing()[mutation_start..]
            .iter()
            .find(|bytes| {
                matches!(
                    postcard::from_bytes::<NeighbourMsg>(bytes).unwrap(),
                    NeighbourMsg::AppDocument { ref app_id, ref record_id, .. }
                        if (app_id, record_id) == (&notes_route.0, &notes_route.1)
                )
            })
            .expect("the mutation publishes its current Notes snapshot");
        assert!(
            qos_peer_state::desktop_delivery::encode_packet(oversized_checkpoint)
                .unwrap()
                .wire_bytes()
                > qos_peer_state::desktop_delivery::MAX_WIRE_BYTES,
            "the optional persisted snapshot exceeds the low-latency wire budget"
        );
        apply_first_publication(&owner, &mut healthy, mutation_start, true);
        apply_first_publication(&owner, &mut missing, mutation_start, false);

        let second = request(
            &owner,
            DesktopCommand::OpenTab {
                app: "dev.qos.calculator".into(),
                placement: TabPlacement::NewGroup {
                    strip: strip(&owner),
                    after: None,
                },
            },
        );
        let (_, delivery) = owner.commit_desktop_command_with_delivery(&second).unwrap();
        let delivery =
            delivery.expect("an oversized optional checkpoint keeps the fresh open direct");
        let target = delivery.target;
        let ActionMessage::CommittedDesktop { packets, .. } = &delivery.message else {
            panic!("desktop command emits a committed desktop action")
        };
        let messages = qos_peer_state::desktop_delivery::decode(packets)
            .unwrap()
            .into_iter()
            .map(|bytes| postcard::from_bytes::<NeighbourMsg>(&bytes).unwrap())
            .collect::<Vec<_>>();
        assert!(messages.iter().any(
            |message| matches!(message, NeighbourMsg::AppDocument { app_id, .. } if app_id == "dev.qos.calculator")
        ));
        assert!(!messages.iter().any(
            |message| matches!(message, NeighbourMsg::AppDocument { app_id, record_id, .. } if (app_id, record_id) == (&notes_route.0, &notes_route.1))
        ));
        let source_user = owner.peer().authority().user_public_key;
        let event = EnvelopeV2::Delivered {
            action: delivery,
            source_user,
        };
        assert!(matches!(
            qos_browser_runtime::committed_desktop::admit(
                &healthy,
                &event,
                target,
                web_time::SystemTime::now(),
            )
            .unwrap(),
            qos_browser_runtime::committed_desktop::Admission::Ready { .. }
        ));
        assert!(matches!(
            qos_browser_runtime::committed_desktop::admit(
                &missing,
                &event,
                target,
                web_time::SystemTime::now(),
            )
            .unwrap(),
            qos_browser_runtime::committed_desktop::Admission::AwaitingRetainedState(_)
        ));
    }
}
