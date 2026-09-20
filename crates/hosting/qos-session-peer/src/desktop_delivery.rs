//! Low-latency delivery of an already durable desktop commit. Retained
//! publication remains the recovery path; this lane never executes a command.
use crate::SessionOwner;
use qos_peer_state::{NeighbourMsg, desktop_command::DesktopCommandError, owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest}};
use qos_relay::peer_actions::{ActionMessage, DirectedAction};

impl SessionOwner {
    pub fn commit_desktop_command_with_delivery(&mut self, request: &OwnerCommitRequest)
        -> Result<(OwnerCommitReceipt, Option<DirectedAction>), DesktopCommandError>
    {
        let previous_tabs = tab_locations(self.peer());
        let previous_rows = self.peer().rows();
        let baseline = self.publication_baseline().0;
        let start = self.outgoing().len();
        let receipt = self.commit_desktop_command(request)?;
        // Construction/delivery failure cannot turn a durable success into a
        // rejection. The ordinary publication is still retained and retried.
        let delivery = (|| -> Option<DirectedAction> {
            let CommitOperation::DesktopCommand { invocation, caller, command } = &request.operation else { return None };
            let mut room = self.peer().admitted_room()?.clone();
            let heads = command.source.heads.iter().copied().map(ui::automerge::ChangeHash).collect::<Vec<_>>();
            let changes = room.get_changes(&heads).into_iter().map(|c| c.raw_bytes().to_vec()).collect();
            let mut packets = vec![postcard::to_stdvec(&NeighbourMsg::SyncResp { room: request.room.clone(), changes }).ok()?];
            // Compaction replaces the tail with a complete committed baseline.
            let start = if baseline == self.publication_baseline().0 { start } else { 0 };
            for bytes in &self.outgoing()[start..] {
                if let NeighbourMsg::AppDocument { app_id, record_id, .. } = postcard::from_bytes::<NeighbourMsg>(bytes).ok()? {
                    let unchanged = previous_rows.iter().find(|r| r.app_id == app_id && r.record_id == record_id)
                        .is_some_and(|before| self.peer().rows().iter().any(|after| after == before));
                    if !unchanged { packets.push(bytes.clone()); }
                }
            }
            let packets = qos_peer_state::desktop_delivery::encode(&packets).ok()?;
            let added = tab_locations(self.peer()).into_iter().filter(|route| !previous_tabs.iter().any(|old| old.tab == route.tab)).collect::<Vec<_>>();
            let opened = matches!(command.command, qos_peer_state::desktop_command::DesktopCommand::OpenTab { .. })
                .then(|| added.first().filter(|_| added.len() == 1).cloned()).flatten();
            let action = DirectedAction {
                room: request.room.parse().ok()?, request_id: *invocation,
                source: self.peer().authority().device_public_key, target: caller.device_public_key,
                message: ActionMessage::CommittedDesktop { invocation: qos_actions::InvocationId(*invocation), packets, opened },
            };
            action.validate().then_some(action)
        })();
        Ok((receipt, delivery))
    }
}

fn tab_locations(state: &qos_peer_state::StatePeer) -> Vec<qos_relay::peer_actions::OpenedDesktopTab> {
    let mut tree = qos_desktop::WorkspaceTree::default();
    let Some(room) = qos_desktop::RoomId::parse(state.room()) else { return vec![] };
    let Some(doc) = state.admitted_room() else { return vec![] };
    if qos_desktop::projection::RoomProjection::apply_admitted_room(&mut tree, room, doc).is_err() { return vec![]; }
    tree.workspaces.iter().flat_map(|w| &w.strips).flat_map(|s| s.window_groups.iter().flat_map(move |g| g.tabs.iter().map(move |t| qos_relay::peer_actions::OpenedDesktopTab {
        strip: s.id.0.to_string(), group: g.id.to_string(), tab: t.id.to_string(),
    }))).collect()
}
