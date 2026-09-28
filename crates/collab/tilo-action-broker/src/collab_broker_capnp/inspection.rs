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
use crate::collab_broker::CrdtRoute;
use crate::services::LaunchInstance;

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
    /// The supervised launch the connection belongs to, which is what ties
    /// it to that launch's windows. `None` for a process the shell did not
    /// launch.
    pub launch: Option<LaunchInstance>,
    /// How many times the app has reported that its semantic tree changed.
    /// Zero until the first report.
    pub semantic_generation: u64,
}

/// An application's report that its semantic tree changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticChange {
    pub peer_id: u64,
    /// Monotonic per connection; a watcher that missed reports compares it
    /// with the last generation it inspected.
    pub generation: u64,
}

/// How many detached inspections (started by a caller that polls rather
/// than blocks) are kept before the oldest is dropped.
const MAX_DETACHED_INSPECTIONS: usize = 64;

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
    launch: Option<LaunchInstance>,
    generation: u64,
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
    watchers: Mutex<Vec<std::sync::mpsc::Sender<SemanticChange>>>,
    detached: Mutex<std::collections::VecDeque<InspectionJob>>,
}

impl Default for InspectionRegistry {
    fn default() -> Self {
        Self {
            subscribers: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            watchers: Mutex::new(Vec::new()),
            detached: Mutex::new(std::collections::VecDeque::new()),
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
        launch: Option<LaunchInstance>,
    ) -> Result<InspectionStreamServer, String> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .map_err(|_| "inspection registry unavailable".to_string())?
            .insert(
                peer_id,
                InspectionSubscription {
                    route,
                    tx,
                    launch,
                    generation: 0,
                },
            );
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

    /// Record that `peer_id`'s semantic tree changed and tell every watcher.
    /// A generation that does not advance is ignored, so a replayed or
    /// reordered report cannot make a watcher re-inspect for nothing.
    pub(super) fn report_change(&self, peer_id: u64, generation: u64) -> Result<(), String> {
        {
            let mut subscribers = self
                .subscribers
                .lock()
                .map_err(|_| "inspection registry unavailable".to_string())?;
            let subscription = subscribers
                .get_mut(&peer_id)
                .ok_or_else(|| "subscribe to inspections before reporting changes".to_string())?;
            if generation <= subscription.generation {
                return Ok(());
            }
            subscription.generation = generation;
            drop(subscribers);
        }
        let change = SemanticChange {
            peer_id,
            generation,
        };
        if let Ok(mut watchers) = self.watchers.lock() {
            watchers.retain(|watcher| watcher.send(change.clone()).is_ok());
        }
        Ok(())
    }

    fn watch(&self) -> std::sync::mpsc::Receiver<SemanticChange> {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(mut watchers) = self.watchers.lock() {
            watchers.push(tx);
        }
        rx
    }

    fn keep_detached(&self, job: InspectionJob) -> u64 {
        let id = job.id;
        if let Ok(mut detached) = self.detached.lock() {
            while detached.len() >= MAX_DETACHED_INSPECTIONS {
                detached.pop_front();
            }
            detached.push_back(job);
        }
        id
    }

    fn detached_answer(&self, id: u64) -> Result<Option<InspectReply>, String> {
        let mut detached = self
            .detached
            .lock()
            .map_err(|_| "inspection registry unavailable".to_string())?;
        let index = detached
            .iter()
            .position(|job| job.id == id)
            .ok_or_else(|| format!("no inspection {id} is outstanding"))?;
        let answer = detached.get(index).and_then(InspectionJob::try_answer);
        if answer.is_some() {
            detached.remove(index);
        }
        drop(detached);
        answer.transpose()
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
                        launch: subscription.launch,
                        semantic_generation: subscription.generation,
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

    /// The application connection that answers for the window `surface_id`:
    /// the one descended from the same supervised launch. When a launch
    /// holds several connections (a document route and a room index), the
    /// one that has reported semantic changes speaks for its tree, then the
    /// earliest.
    pub fn inspectable_app_for_surface(&self, surface_id: u64) -> Option<InspectableApp> {
        let launch = self
            .services
            .launcher
            .as_ref()?
            .launch_instance_for_surface(surface_id)?;
        self.inspections
            .apps()
            .into_iter()
            .filter(|app| app.launch == Some(launch))
            .min_by_key(|app| (app.semantic_generation == 0, app.peer_id))
    }

    /// Every later report of a semantic tree change, from any application.
    /// The receiver is dropped from the watch list once it hangs up.
    pub fn watch_semantic_changes(&self) -> std::sync::mpsc::Receiver<SemanticChange> {
        self.inspections.watch()
    }

    /// Start an inspection whose answer is collected later with
    /// [`inspection_answer`](Self::inspection_answer), for a caller that
    /// must not block (the control socket, a frame loop).
    ///
    /// # Errors
    ///
    /// As [`start_inspection`](Self::start_inspection).
    pub fn start_detached_inspection(
        &self,
        peer_id: u64,
        query: InspectQuery,
        caller: &InspectCaller,
    ) -> Result<u64, String> {
        let job = self.start_inspection(peer_id, query, caller)?;
        Ok(self.inspections.keep_detached(job))
    }

    /// The answer to detached inspection `id`: `None` while the application
    /// has not answered. Once returned, the answer is forgotten.
    ///
    /// # Errors
    ///
    /// When `id` is unknown or already collected, or the application refused
    /// or disconnected.
    pub fn inspection_answer(&self, id: u64) -> Result<Option<InspectReply>, String> {
        self.inspections.detached_answer(id)
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
        let now = self.policy_now_ns()? / 1_000_000_000;
        let decision = {
            let tree = membership.snapshot()?;
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == route.cluster_id)
                .ok_or_else(|| "the application's workspace is no longer hosted".to_string())?;
            let actor = workspace.actor_for_person_at(&caller.person, now);
            workspace.decide_at(&actor, &ResourceRef::Workspace, Verb::Read, now)
        };
        if decision != CapDecision::Allow {
            return Err("the caller may not read this room".into());
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

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each scenario holds its broker fixture for the whole body"
)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A host whose launch registry is a table the test writes: which
    /// supervised launch owns each window.
    #[derive(Default)]
    struct SurfaceLaunches {
        surfaces: Mutex<HashMap<u64, LaunchInstance>>,
    }

    impl crate::services::LauncherServices for SurfaceLaunches {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
            Err("this host launches nothing".into())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
        fn launch_instance_for_surface(&self, surface_id: u64) -> Option<LaunchInstance> {
            self.surfaces
                .lock()
                .ok()
                .and_then(|surfaces| surfaces.get(&surface_id).copied())
        }
    }

    fn route(record: &str) -> CrdtRoute {
        CrdtRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: "room".into(),
            record_id: record.into(),
        }
    }

    #[test]
    fn a_window_resolves_to_the_connection_of_its_own_launch() {
        let launcher = Arc::new(SurfaceLaunches::default());
        let broker = CapnpBrokerState::test_broker_with_authority(
            crate::test_support::WorkspaceTree::default()
                .shared()
                .authority(),
            crate::services::HostServices {
                launcher: Some(launcher.clone()),
                ..Default::default()
            },
        );
        let state = broker.state();
        let launch = LaunchInstance {
            pid: 4_000_001,
            start_time: 77,
        };
        let other = LaunchInstance {
            pid: 4_000_002,
            start_time: 78,
        };
        let _index = state
            .inspections
            .subscribe(10, route("room-index"), Some(launch))
            .expect("subscribe");
        let _document = state
            .inspections
            .subscribe(11, route("doc"), Some(launch))
            .expect("subscribe");
        let _stranger = state
            .inspections
            .subscribe(12, route("doc"), Some(other))
            .expect("subscribe");
        let _unlaunched = state
            .inspections
            .subscribe(13, route("doc"), None)
            .expect("subscribe");
        launcher
            .surfaces
            .lock()
            .expect("launch table")
            .insert(900_001, launch);
        assert_eq!(
            state
                .inspectable_app_for_surface(900_001)
                .map(|app| app.peer_id),
            Some(10),
            "the launch's earliest connection answers until one reports changes"
        );
        state.inspections.report_change(11, 1).expect("report");
        assert_eq!(
            state
                .inspectable_app_for_surface(900_001)
                .map(|app| app.peer_id),
            Some(11),
            "the connection that describes its tree speaks for the window"
        );
        assert_eq!(state.inspectable_app_for_surface(900_404), None);
        launcher
            .surfaces
            .lock()
            .expect("launch table")
            .remove(&900_001);
        assert_eq!(state.inspectable_app_for_surface(900_001), None);
    }

    #[test]
    fn semantic_changes_reach_watchers_once_per_advancing_generation() {
        let broker = CapnpBrokerState::test_broker_with_idle_directory_queue();
        let state = broker.state();
        let watcher = state.watch_semantic_changes();
        assert!(
            state.inspections.report_change(5, 1).is_err(),
            "a connection reports only after it answers inspection"
        );
        let _stream = state
            .inspections
            .subscribe(5, route("doc"), None)
            .expect("subscribe");
        state.inspections.report_change(5, 1).expect("report");
        state
            .inspections
            .report_change(5, 1)
            .expect("replayed report");
        state.inspections.report_change(5, 3).expect("report");
        let changes: Vec<_> = watcher.try_iter().collect();
        assert_eq!(
            changes,
            [
                SemanticChange {
                    peer_id: 5,
                    generation: 1
                },
                SemanticChange {
                    peer_id: 5,
                    generation: 3
                },
            ]
        );
        assert_eq!(state.inspectable_apps()[0].semantic_generation, 3);
    }

    #[test]
    fn a_detached_inspection_is_answered_later_and_collected_once() {
        let broker = CapnpBrokerState::test_broker_with_idle_directory_queue();
        let state = broker.state();
        let (tx, reply) = std::sync::mpsc::channel();
        let id = state
            .inspections
            .keep_detached(InspectionJob { id: 42, reply });
        assert_eq!(id, 42);
        assert_eq!(state.inspection_answer(42), Ok(None), "pending");
        tx.send(Ok(InspectReply::empty())).expect("answer");
        assert_eq!(state.inspection_answer(42), Ok(Some(InspectReply::empty())));
        assert!(state.inspection_answer(42).is_err(), "collected once");
    }
}
