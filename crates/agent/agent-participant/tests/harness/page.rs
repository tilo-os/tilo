//! A page speaking to a host as a browser does: one peer over the host's
//! HTTPS, the records' durable changes it received, and the host's answers.
//!
//! Every request a scenario makes goes through one vocabulary: the host's
//! agent supervisor, the workspace's records, and the workspace itself.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "a fixture: what it cannot set up is the failure it reports"
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, InvocationReceipt, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};

use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::agents::{
    AGENTS_PROVIDER, AgentsStatus, STATUS_ACTION, SUPERVISOR_RESOURCE,
};
use tilo_session_host::session_actions::Heard;
use tilo_session_host::{Host, PeerEvent, PeerEvents, PeerHandle};

use super::hosted::Hosted;
use crate::support::{Device, WORKSPACE, ready, start_peer};

/// A checkpoint a record's change left: its author and its bytes.
pub type Checkpoint = ([u8; 32], Vec<u8>);

/// A page in a host's workspace.
pub struct Page {
    pub peer: PeerHandle,
    pub events: PeerEvents,
    pub host_key: [u8; 32],
    /// Every durable change of the records this page received, in order.
    pub documents: BTreeMap<DocumentRoute, Vec<Checkpoint>>,
    /// The host's answers, taken by [`Page::answer`].
    pub answers: Vec<RemoteActionMessage>,
    /// The correlation the page names its next catalogue request with.
    pub correlation: u64,
    pub workspace: u64,
    pub cluster_id: String,
}

impl Page {
    /// A page joining the workspace as `device`, under `host`'s host.
    pub async fn open(hosted: &Hosted, device: &Device, host: &Device) -> Self {
        Self::open_workspace(hosted, device, host, WORKSPACE).await
    }

    /// A page joining the workspace `cluster_id` names, asking its host to
    /// govern it and admit the page first.
    pub async fn open_workspace(
        hosted: &Hosted,
        device: &Device,
        host: &Device,
        cluster_id: &str,
    ) -> Self {
        let workspace = tilo_relay_protocol::room_workspace_id(cluster_id).unwrap();
        let peer = start_peer(
            Host::at(hosted.relay.node_addr()),
            device.cert.clone(),
            host.cert.clone(),
            device.key.clone(),
        );
        let events = peer.events();
        if cluster_id == WORKSPACE {
            // The page asks its host to govern its person's workspace and
            // admit it, as every page does before joining one.
            peer.join_placed(cluster_id, "Workspace").unwrap();
        } else {
            ready(&[&peer]).await;
            let (reply, answer) = tokio::sync::oneshot::channel();
            peer.rooms_then(
                tilo_relay_protocol::workspaces::WorkspacesRequest::Enter {
                    workspace: cluster_id.into(),
                },
                Box::new(move |result| {
                    let _ = reply.send(result);
                }),
            )
            .unwrap();
            assert!(matches!(
                tilo_test_env::within("the node enters the governed fixture", answer)
                    .await
                    .unwrap()
                    .unwrap(),
                tilo_relay_protocol::workspaces::WorkspacesReply::Workspace(_)
            ));
            peer.join(workspace).unwrap();
        }
        ready(&[&peer]).await;
        Self {
            peer,
            events,
            host_key: host.cert.device_public_key,
            documents: BTreeMap::new(),
            answers: Vec::new(),
            correlation: 0,
            workspace,
            cluster_id: cluster_id.to_owned(),
        }
    }

    /// Take one event: a record's change is kept, everything else is the
    /// caller's to read.
    pub fn take(&mut self, event: PeerEvent) -> Option<PeerEvent> {
        if let PeerEvent::Document(document) = &event {
            if let Some(mutation) = document.mutation() {
                let bytes = match mutation {
                    collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes) => bytes,
                };
                self.documents
                    .entry(document.route.clone())
                    .or_default()
                    .push((document.author, bytes));
            }
            return None;
        }
        Some(event)
    }

    /// Take everything the page received.
    pub fn pump(&mut self) {
        for event in self.events.drain() {
            match self.take(event) {
                Some(PeerEvent::Remote(Heard::Action { from, frame, .. }))
                    if from == self.host_key =>
                {
                    self.answers
                        .push(RemoteActionFrame::decode(&frame).unwrap().message);
                }
                Some(PeerEvent::Remote(Heard::Undeliverable { reason, .. })) => {
                    panic!("undeliverable: {reason}")
                }
                Some(_) | None => {}
            }
        }
    }

    /// Send `message` to the host.
    pub fn send(&self, message: RemoteActionMessage) {
        let frame = RemoteActionFrame::new(message).encode().unwrap();
        self.peer
            .send_remote_action(self.workspace, self.host_key, frame)
            .unwrap();
    }

    /// Wait for the first answer `pick` accepts, removing it.
    pub async fn answer<T>(
        &mut self,
        what: &str,
        pick: impl Fn(&RemoteActionMessage) -> Option<T>,
    ) -> T {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.pump();
            if let Some(index) = self
                .answers
                .iter()
                .position(|answer| pick(answer).is_some())
            {
                let answer = self.answers.remove(index);
                return pick(&answer).unwrap();
            }
            assert!(Instant::now() < deadline, "no answer: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The host's catalogue of the workspace's records and their actions.
    pub async fn catalogue(
        &mut self,
        query: CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        self.correlation = self.correlation.checked_add(1).unwrap();
        let correlation = self.correlation;
        self.send(RemoteActionMessage::CatalogueRequest { correlation, query });
        self.answer("catalogue", |answer| match answer {
            RemoteActionMessage::CatalogueResponse {
                correlation: got,
                result,
            } if *got == correlation => Some(result.clone()),
            _ => None,
        })
        .await
    }

    /// The receipt of `invocation`.
    pub async fn receipt(&mut self, invocation: InvocationId) -> InvocationReceipt {
        self.answer("receipt", |answer| match answer {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                Some(receipt.clone())
            }
            _ => None,
        })
        .await
    }

    /// Run `request`, answering its receipt.
    pub async fn invoke(&mut self, request: InvokeRequest) -> InvocationReceipt {
        let invocation = request.invocation;
        self.send(RemoteActionMessage::Invoke(request));
        self.receipt(invocation).await
    }

    /// Run one action on the host's agent supervisor.
    pub async fn call(
        &mut self,
        resource: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> Result<serde_json::Value, String> {
        self.invoke_on(
            AGENTS_PROVIDER,
            ActionAddress::new(resource, action),
            argument,
        )
        .await
        .map(|output| output.value.unwrap_or_default())
    }

    /// Run one action on the record `provider` serves.
    pub async fn invoke_on(
        &mut self,
        provider: &str,
        address: ActionAddress,
        argument: InvokeArgument,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let invocation = InvocationId::new();
        self.send(RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: provider.into(),
                address,
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }));
        match self.receipt(invocation).await.status {
            ReceiptStatus::Completed { output } => Ok(output),
            other => Err(format!("{other:?}")),
        }
    }

    /// The host's status of its agents.
    pub async fn status(&mut self) -> AgentsStatus {
        serde_json::from_value(
            self.call(SUPERVISOR_RESOURCE, STATUS_ACTION, InvokeArgument::None)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// Wait until everything this page published is durable on the host.
    pub async fn durable(&self) {
        let receipt = self.peer.barrier().unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match receipt.try_recv() {
                Ok(result) => return result.unwrap(),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(error) => panic!("the barrier ended: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "the page's edits never became durable"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait until `done` holds for the supervisor's status.
    pub async fn until(
        &mut self,
        what: &str,
        done: impl Fn(&AgentsStatus) -> bool,
    ) -> AgentsStatus {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let status = self.status().await;
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what}: {status:#?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// How many changes of `route` the page received.
    pub fn heard(&self, route: &DocumentRoute) -> usize {
        self.documents.get(route).map_or(0, Vec::len)
    }

    /// Wait until the page has received more than `past` changes of `route`.
    pub async fn until_heard(&mut self, route: &DocumentRoute, past: usize) {
        let deadline = tilo_test_env::hang_deadline();
        while self.heard(route) <= past {
            assert!(Instant::now() < deadline, "no change of {route:?} arrived");
            self.pump();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
