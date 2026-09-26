//! Inspection: the shell reading an application's semantic tree.
//!
//! The same shape as an invocation. The session addresses one application
//! connection with an [`InspectRequest`](ui::inspect::InspectRequest), the
//! application answers one [`InspectReply`] on `completeInspection`, and the
//! caller waits on the job's receiver. Before anything reaches the app the
//! broker checks the caller may `Read` the room the app is attached to, and
//! it sets the answer's audience itself; the reply is redacted for that
//! audience again on the way back, whatever the app did.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use capnp::capability::Promise;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use ui::inspect::{InspectQuery, InspectReply, InspectRequest};

use collab_types::{CapDecision, ResourceRef, Verb};
use runtime_collab::raw::inspection_stream;

use super::CapnpBrokerState;
use crate::shell::collab_broker::CrdtRoute;
use crate::shell::state::WorkspaceAuthority as _;

/// How long a caller waits for an application to answer before the broker
/// gives up on it. Inspection is a read of a retained tree; an app that
/// cannot answer in this time is not answering.
pub const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Who is asking, as the shell asserts it: the person the local session acts
/// for, and who will read the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectCaller {
    /// The person id the room's capabilities are checked against.
    pub person: String,
    /// Who reads the answer. The local session's own chrome and its recorder
    /// before review may see personal content; anything bound off the device
    /// asks for [`Audience::BeyondDevice`](ui::a11y::Audience::BeyondDevice).
    pub audience: ui::a11y::Audience,
}

/// One application connection that answers inspection requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectableApp {
    pub peer_id: u64,
    pub app_id: String,
    /// The room the connection is attached to.
    pub room: String,
    /// The record the connection edits, empty for a whole-app document.
    pub record: String,
}

/// An admitted inspection. [`wait`](Self::wait) blocks for the answer.
pub struct InspectionJob {
    pub id: u64,
    reply: std::sync::mpsc::Receiver<Result<InspectReply, String>>,
}

impl InspectionJob {
    /// The answer if it has arrived, without blocking: for chrome that polls
    /// once per frame. `None` while the application has not answered.
    ///
    /// # Errors
    ///
    /// Inside the `Some`, as for [`wait`](Self::wait).
    pub fn try_answer(&self) -> Option<Result<InspectReply, String>> {
        match self.reply.try_recv() {
            Ok(answer) => Some(answer.and_then(accept)),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("the application did not answer the inspection".into()))
            }
        }
    }

    /// Block until the application answers, at most `timeout`.
    ///
    /// # Errors
    ///
    /// When the application refused, disconnected, or did not answer in time.
    pub fn wait(self, timeout: Duration) -> Result<InspectReply, String> {
        let reply = self
            .reply
            .recv_timeout(timeout)
            .map_err(|_| "the application did not answer the inspection".to_string())??;
        accept(reply)
    }
}

fn accept(reply: InspectReply) -> Result<InspectReply, String> {
    match reply.refused {
        Some(reason) => Err(format!("the application refused the inspection: {reason}")),
        None => Ok(reply),
    }
}

pub(super) struct InspectionEnvelope {
    id: u64,
    request_json: String,
}

struct InspectionSubscription {
    route: CrdtRoute,
    tx: mpsc::UnboundedSender<InspectionEnvelope>,
}

struct PendingInspection {
    peer_id: u64,
    audience: ui::a11y::Audience,
    tx: std::sync::mpsc::Sender<Result<InspectReply, String>>,
}

/// The broker's inspection state: who answers, and what is outstanding.
pub(super) struct InspectionRegistry {
    subscribers: Mutex<HashMap<u64, InspectionSubscription>>,
    pending: Mutex<HashMap<u64, PendingInspection>>,
    next_id: AtomicU64,
}

impl Default for InspectionRegistry {
    fn default() -> Self {
        Self {
            subscribers: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl InspectionRegistry {
    /// Register `peer_id`'s inspection stream on `route`, replacing any
    /// earlier one, and hand back the capability the app reads from.
    pub(super) fn subscribe(
        &self,
        peer_id: u64,
        route: CrdtRoute,
    ) -> Result<InspectionStreamServer, String> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .map_err(|_| "inspection registry unavailable".to_string())?
            .insert(peer_id, InspectionSubscription { route, tx });
        Ok(InspectionStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        })
    }

    /// Resolve an answer from `peer_id`. Only the connection that was asked
    /// may answer, and only once.
    pub(super) fn complete(&self, peer_id: u64, id: u64, reply_json: &str) -> Result<(), String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "inspection registry unavailable".to_string())?;
        if pending.get(&id).map(|job| job.peer_id) != Some(peer_id) {
            return Err("an application may answer only its own inspection".into());
        }
        let job = pending
            .remove(&id)
            .ok_or_else(|| "unknown inspection id".to_string())?;
        drop(pending);
        let answer = serde_json::from_str::<InspectReply>(reply_json)
            .map(|reply| reply.redacted_for(job.audience))
            .map_err(|error| format!("the application's inspection answer is malformed: {error}"));
        let malformed = answer.as_ref().err().cloned();
        let _ = job.tx.send(answer);
        malformed.map_or(Ok(()), Err)
    }

    /// Forget `peer_id`: its stream closes and whatever it was asked fails.
    pub(super) fn remove_peer(&self, peer_id: u64) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|_, job| {
                if job.peer_id == peer_id {
                    let _ = job
                        .tx
                        .send(Err("the application disconnected before answering".into()));
                    false
                } else {
                    true
                }
            });
        }
    }

    pub(super) fn apps(&self) -> Vec<InspectableApp> {
        let mut apps = self
            .subscribers
            .lock()
            .map(|subscribers| {
                subscribers
                    .iter()
                    .map(|(peer_id, subscription)| InspectableApp {
                        peer_id: *peer_id,
                        app_id: subscription.route.app_id.clone(),
                        room: subscription.route.cluster_id.clone(),
                        record: subscription.route.record_id.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        apps.sort_by_key(|app| app.peer_id);
        apps
    }
}

impl CapnpBrokerState {
    /// Every application connection that currently answers inspection.
    pub fn inspectable_apps(&self) -> Vec<InspectableApp> {
        self.inspections.apps()
    }

    /// Ask application connection `peer_id` what its semantic tree holds.
    ///
    /// # Errors
    ///
    /// When the connection does not answer inspection, its room is not hosted
    /// here, the caller may not read that room, or the request cannot be
    /// delivered.
    pub fn start_inspection(
        &self,
        peer_id: u64,
        query: InspectQuery,
        caller: &InspectCaller,
    ) -> Result<InspectionJob, String> {
        let (route, sender) = self
            .inspections
            .subscribers
            .lock()
            .map_err(|_| "inspection registry unavailable".to_string())?
            .get(&peer_id)
            .map(|subscription| (subscription.route.clone(), subscription.tx.clone()))
            .ok_or_else(|| "that application connection does not answer inspection".to_string())?;
        if route.cluster_id.is_empty() {
            return Err("the application is not attached to a workspace".into());
        }
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the broker has no workspace authority".to_string())?;
        {
            let tree = membership.read();
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == route.cluster_id)
                .ok_or_else(|| "the application's workspace is no longer hosted".to_string())?;
            let actor = workspace.actor_for_person(&caller.person);
            if workspace.decide(&actor, &ResourceRef::Workspace, Verb::Read) != CapDecision::Allow {
                return Err("the caller may not read this room".into());
            }
        }
        let request = InspectRequest::new(query, caller.audience);
        let request_json = serde_json::to_string(&request)
            .map_err(|error| format!("the inspection request cannot be encoded: {error}"))?;
        let id = self.inspections.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, reply) = std::sync::mpsc::channel();
        self.inspections
            .pending
            .lock()
            .map_err(|_| "inspection registry unavailable".to_string())?
            .insert(
                id,
                PendingInspection {
                    peer_id,
                    audience: caller.audience,
                    tx,
                },
            );
        if sender
            .send(InspectionEnvelope { id, request_json })
            .is_err()
        {
            if let Ok(mut pending) = self.inspections.pending.lock() {
                pending.remove(&id);
            }
            return Err("the application's inspection stream closed".into());
        }
        Ok(InspectionJob { id, reply })
    }
}

pub(super) struct InspectionStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<InspectionEnvelope>>>,
}

impl inspection_stream::Server for InspectionStreamServer {
    fn read(
        self: Rc<Self>,
        _params: inspection_stream::ReadParams,
        mut results: inspection_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let envelope = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("application inspection stream closed".into())
            })?;
            drop(guard);
            let mut result = results.get();
            result.set_id(envelope.id);
            result.set_request_json(&envelope.request_json);
            Ok(())
        })
    }
}
