//! The channels' workspace transport over the host session: the channel
//! governance this page says waits here until the host takes it for a
//! workspace, and goes out wrapped as the page's own retained frame.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use tilo_server_client::CoeditPayload;
use tilo_server_protocol::Envelope;
use tilo_server_session::ServerClient;

use super::Task;
use tilo_server_protocol::WorkspaceId;

/// What a message waits for before the host session carries it: the workspace
/// whose transport carries the group it speaks for.
struct Waiting {
    workspace: WorkspaceId,
    message: neighbour_wire::NeighbourMsg,
}

/// The channels' workspace transport over the host session: what they say
/// waits here for the session to send it, until the host admitted the session
/// to the workspace that carries the group: the newest access request of each
/// group, and every frame of a call's media group in the order it was written.
#[derive(Default)]
pub(super) struct HostGovernance {
    pub(super) inbound: Mutex<Vec<tilo_workspace_keys::InboundGovernance>>,
    outbound: Mutex<BTreeMap<WorkspaceId, Waiting>>,
    frames: Mutex<Vec<Waiting>>,
}

impl HostGovernance {
    /// What waits for the workspaces `admitted` names, each with the workspace
    /// whose transport carries it: the message's own group for the workspace's
    /// group, the workspace's for a document's or a reader set's.
    pub(super) fn take_for(
        &self,
        admitted: impl Fn(WorkspaceId) -> bool,
    ) -> Vec<(WorkspaceId, neighbour_wire::NeighbourMsg)> {
        let outbound = std::mem::take(&mut *self.outbound.lock_recover());
        let mut due = Vec::new();
        let mut waiting = BTreeMap::new();
        for (group, entry) in outbound {
            if admitted(entry.workspace) {
                due.push((entry.workspace, entry.message));
            } else {
                waiting.insert(group, entry);
            }
        }
        self.outbound.lock_recover().extend(waiting);
        let (frames, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut *self.frames.lock_recover())
            .into_iter()
            .partition(|entry| admitted(entry.workspace));
        *self.frames.lock_recover() = waiting;
        due.extend(
            frames
                .into_iter()
                .map(|entry| (entry.workspace, entry.message)),
        );
        due
    }
}

impl tilo_workspace_keys::GovernanceTransport for HostGovernance {
    fn publish(&self, message: neighbour_wire::NeighbourMsg) -> bool {
        use neighbour_wire::NeighbourMsg;
        // The group a message is for is not always the workspace that carries
        // it: a document's or a reader set's group asks on its workspace's
        // transport, and the host takes it there. A page is admitted to the
        // workspace, never to the group, so a message kept until the group is
        // sent would never leave.
        let Some(workspace) = message.governance_workspace().and_then(WorkspaceId::parse) else {
            return false;
        };
        match &message {
            NeighbourMsg::MlsAccessRequest { request, .. } => {
                let group = request.workspace;
                self.outbound
                    .lock_recover()
                    .insert(group, Waiting { workspace, message });
                true
            }
            // A page founds and commits a call's media group, and no other
            // ([`tilo_workspace_keys::KeyRole::Follower`]).
            NeighbourMsg::MlsGovernance {
                workspace: scope, ..
            } if neighbour_wire::KeyScope::parse(scope)
                .is_some_and(|scope| tilo_workspace_keys::is_call_media(&scope)) =>
            {
                self.frames
                    .lock_recover()
                    .push(Waiting { workspace, message });
                true
            }
            _ => false,
        }
    }

    fn take_inbound(&self) -> Vec<tilo_workspace_keys::InboundGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

/// `message`, signed governance of `workspace_id`, wrapped for the server.
pub(super) fn governance_envelope(
    device: [u8; 32],
    workspace_id: WorkspaceId,
    message: &neighbour_wire::NeighbourMsg,
) -> Option<Envelope> {
    let (channel, _) = neighbour_wire::retention(message, false)?;
    Some(Envelope::Delta {
        workspace_id,
        author_device_public_key: device,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: CoeditPayload::Checkpoint {
            channel,
            body: postcard::to_allocvec(message).ok()?,
        }
        .encode()
        .ok()?,
        signature: [0; 64],
    })
}

impl Task {
    /// Send the governance the channels said for every workspace the host
    /// admitted this session to or began replaying to it; the rest waits
    /// until it does. Returns `false` when the session could not take an
    /// envelope.
    pub(super) fn send_governance(&self, client: &ServerClient) -> bool {
        // A workspace whose replay has begun takes the page's channel
        // governance: its access requests go out as soon as the replay's
        // governance names the page, so its keys arrive while the rest of the
        // workspace is still on its way, rather than a whole replay late.
        let admitted = |workspace_id: WorkspaceId| {
            self.begun.contains(&workspace_id)
                || self
                    .reports
                    .read(|status| status.workspaces.get(&workspace_id).copied())
                    == Some(true)
        };
        let device = self.config.cert.device_public_key;
        let mut delivered = true;
        let Some(governance) = &self.governance else {
            return true;
        };
        for (workspace_id, message) in governance.take_for(admitted) {
            if let Some(envelope) = governance_envelope(device, workspace_id, &message) {
                delivered &= client.send(envelope).is_ok();
            }
        }
        delivered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{Ed25519PublicKey, MlsAccessRequestV1};
    use neighbour_wire::{KeyScope, NeighbourMsg};
    use tilo_server_protocol::WorkspaceId;

    /// A device asking for the keys of `scope`.
    fn asking(scope: &KeyScope) -> NeighbourMsg {
        NeighbourMsg::MlsAccessRequest {
            workspace: scope.to_string(),
            request: Box::new(MlsAccessRequestV1::unsigned(
                scope.group(),
                Ed25519PublicKey::from_bytes([7; 32]),
                vec![1, 2, 3],
                1,
            )),
            delivery: 0,
        }
    }

    /// A document's group asks on the transport of the workspace that carries it. A page is
    /// admitted to the workspace and never to the group, so a request kept for the group is kept
    /// forever: it goes once the workspace is admitted, and names the workspace, which is the
    /// transport the host takes the request on.
    #[test]
    fn a_documents_group_asks_on_the_workspace_that_carries_it() {
        let workspace = WorkspaceId::new();
        let scope = KeyScope::document(workspace, "so.tilo.mail", "uuid:mailbox", None).unwrap();
        let request = asking(&scope);
        let governance = HostGovernance::default();
        assert!(tilo_workspace_keys::GovernanceTransport::publish(
            &governance,
            request
        ));

        // The page is not in the workspace yet: the request waits, and no other workspace's
        // admission hands it over.
        assert!(governance.take_for(|_| false).is_empty());
        let elsewhere = WorkspaceId::new();
        assert!(governance.take_for(|id| id == elsewhere).is_empty());

        // Admitted to the workspace that carries the document, it goes there at once.
        let sent = governance.take_for(|id| id == workspace);
        let [(carrier, message)] = sent.as_slice() else {
            panic!("one request goes");
        };
        assert_eq!(*carrier, workspace, "it rides the workspace's transport");
        let NeighbourMsg::MlsAccessRequest { request: sent, .. } = message else {
            panic!("an access request goes");
        };
        assert_eq!(sent.workspace, scope.group(), "for the document's group");
    }

    /// The workspace's own group still asks where it always did.
    #[test]
    fn a_workspaces_group_asks_on_its_own_transport() {
        let workspace = WorkspaceId::new();
        let scope = KeyScope::Workspace(workspace);
        let request = asking(&scope);
        let governance = HostGovernance::default();
        assert!(tilo_workspace_keys::GovernanceTransport::publish(
            &governance,
            request
        ));
        assert!(governance.take_for(|_| false).is_empty());
        let sent = governance.take_for(|id| id == workspace);
        assert_eq!(sent.len(), 1, "it goes once the workspace is admitted");
        assert_eq!(sent[0].0, workspace);
    }
}
