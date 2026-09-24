//! Ordinary certified Cloud workers. Store is the only durable document owner;
//! the shared broker journal retains generic invocation uncertainty only.
use crate::agent_runtime_adapter::RuntimeOwner;
use collab_types::{DocumentMutation, caps::Actor};
use ed25519_dalek::Signer;
use qos_action_broker::{
    collab_broker::{AppActionBroker, BrokerHandle, CrdtRoute},
    collab_broker_capnp::{PublishedInput, PublishedSurface},
    collab_outbound::{PeerPublishSink, PeerPublisher},
    host_documents::{HostDocuments, PendingHostDocument},
};
use qos_agent_control_protocol::GrantAction;
use qos_peer_state::{NeighbourMsg, StatePeer, owner_commit::OwnerCommitService};
use qos_user_identity::DeviceCert;
use std::{
    cell::RefCell,
    os::{fd::IntoRawFd, unix::fs::DirBuilderExt},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use tokio::sync::mpsc;
use ui::{
    application::{ComponentCloseReport, HostContext, LaunchTarget, RestoreInput},
    collaboration::{CertifiedEndpointIdentity, CollaborationManager, RouteSpec},
    host_worker::HostWorker,
    permissions::PermissionContext,
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners, RecordStore},
};

struct NetworkSink {
    sender: mpsc::Sender<NeighbourMsg>,
    documents: HostDocuments,
    owner: Arc<RuntimeOwner>,
    peer: u64,
}
impl NetworkSink {
    fn send(&self, message: NeighbourMsg) {
        if self.sender.try_send(message).is_err() {
            self.owner.stop_readiness();
            self.documents.fail("worker outbound queue unavailable");
        }
    }
}
impl PeerPublishSink for NetworkSink {
    fn app_document(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {
        self.owner.stop_readiness();
        self.documents
            .fail("unexpected legacy worker document publication");
    }
    fn app_presence(
        &self,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        self.send(NeighbourMsg::AppPresenceV2 {
            peer: self.peer,
            app_id,
            cluster_id,
            presence,
        });
    }
    fn scrollback_request(&self, artifact: collab_types::ArtifactId, before_seq: u64, max: u16) {
        self.send(NeighbourMsg::ScrollbackRequest {
            artifact,
            before_seq,
            max,
            requester: self.peer,
        });
    }
}
struct SocketDirectory(PathBuf);
impl SocketDirectory {
    fn new() -> Result<Self, String> {
        // Keep Unix socket paths short even when the durable Store path is long.
        let path = Path::new("/tmp").join(format!("qos-cloud-worker-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Worker {
    app: String,
    record: String,
    checkout: RecordCheckout,
    records: RecordStore,
    runner: HostWorker,
    heads: Vec<ui::automerge::ChangeHash>,
    #[cfg(test)]
    authorizer: ui::host_worker::PublishedInvocationAuthorizer,
}
struct Snapshot {
    generation: u64,
    peer: StatePeer,
}
/// Inert publication facts only. The lifecycle combines these with real
/// managed-runtime, gateway, transport and explicitly selected schema readiness.
pub(crate) struct WorkerPublications {
    pub(crate) room: String,
    pub(crate) host_device: [u8; 32],
    pub(crate) governance_revision: String,
    pub(crate) publication_revision: String,
    pub(crate) grants: Vec<GrantAction>,
    pub(crate) ready: bool,
    pub(crate) native_v5_ready: bool,
}
pub(crate) struct AgentWorkers {
    // Broker joins its owned thread before the private socket directory drops.
    broker: BrokerHandle,
    owner: Arc<RuntimeOwner>,
    documents: HostDocuments,
    root: PathBuf,
    certificate: DeviceCert,
    presentation: Vec<u8>,
    room: String,
    principal: String,
    cache: PortableStorage,
    owners: RecordOwners,
    views: Rc<ui::application_view::InstanceViewStore>,
    workers: Vec<Worker>,
    recovery: Vec<ComponentCloseReport>,
    generation: Option<u64>,
    transport_ready: bool,
    closed: bool,
    /// Managed sender for enrolled task children (None when unavailable).
    steps: Option<crate::agent_sender::AgentSteps>,
    #[cfg(test)]
    step_service: Arc<crate::agent_sender::CloudStepService>,
    _socket: SocketDirectory,
}
impl AgentWorkers {
    pub(crate) fn start(
        root: &Path,
        owner: Arc<RuntimeOwner>,
        outbound: mpsc::Sender<NeighbourMsg>,
    ) -> Result<(Self, mpsc::Receiver<PendingHostDocument>), String> {
        owner.policy_context().mark_started();
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let (snapshot, certificate, presentation) = {
            let store = owner.store()?;
            // Source preserves the configured spelling (for example /var), while
            // this worker uses a canonical path. Compare the actual directory to
            // the already-admitted held Store descriptor, not those spellings.
            let worker_directory = std::fs::File::open(&root).map_err(|e| e.to_string())?;
            let worker_identity =
                atomic_file::FileIdentity::of(&worker_directory).map_err(|e| e.to_string())?;
            let owner_identity =
                atomic_file::FileIdentity::of(&store.directory).map_err(|e| e.to_string())?;
            if worker_identity != owner_identity {
                return Err("worker root differs from Store owner".into());
            }
            let peer = store.peer().map_err(|e| e.to_string())?;
            check_peer(&peer, store.policy_now().map_err(|e| e.to_string())?)?;
            let certificate = peer.authority().clone();
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: peer.room().into(),
                device_certificate: serde_json::to_vec(&certificate).map_err(|e| e.to_string())?,
                proof: store
                    .key()
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        peer.room(),
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .map_err(|e| e.to_string())?;
            (
                Snapshot {
                    generation: store.state.generation,
                    peer,
                },
                certificate,
                presentation,
            )
        }; // No Store lock may span shared owner health, broker or worker calls.
        let actions = root.join("actions");
        private_directory(&actions)?;
        if actions
            .join("recovery.json")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            return Err("unresolved application recovery; explicit recovery required".into());
        }
        let socket = SocketDirectory::new()?;
        let (documents, requests) = HostDocuments::new(Arc::new(owner.ordinary()));
        let (step_service, step_requests) = crate::agent_sender::CloudStepService::channel();
        let step_service = Arc::new(step_service);
        let publisher = PeerPublisher::with_owner_commit(
            Arc::new(NetworkSink {
                sender: outbound,
                documents: documents.clone(),
                owner: owner.clone(),
                peer: qos_peer_state::peer_handle(&certificate.device_public_key),
            }),
            Arc::new(documents.clone()),
        );
        let broker = qos_action_broker::collab_broker::spawn_native_peer_with_services(
            socket.0.join("broker.sock"),
            owner.clone(),
            publisher,
            qos_action_broker::peer_actions::NativePeerBrokerConfig {
                journal_root: actions.join("journal"),
                certificate_authority: owner.clone(),
            },
            qos_action_broker::services::HostServices {
                enrolled_agents: Some(owner.clone()),
                enrolled_steps: Some(step_service.clone()),
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        let room = snapshot.peer.room().to_owned();
        let (_, _, principal) = snapshot.peer.permissions(
            certificate.device_public_key,
            crate::now_time(owner.policy_time()?.now_ns().map_err(|e| e.to_string())?),
        )?;
        let cache = PortableStorage::new(
            "cloud-agent-workers",
            &hex::encode(certificate.device_public_key),
        )?;
        let views = Rc::new(
            ui::application_view::InstanceViewStore::portable(cache.clone(), &room)
                .map_err(|e| e.to_string())?,
        );
        let owners = RecordOwners::portable(cache.clone());
        let mut this = Self {
            broker,
            owner,
            documents,
            root,
            certificate,
            presentation,
            room,
            principal,
            cache,
            owners,
            views,
            workers: vec![],
            recovery: vec![],
            generation: None,
            transport_ready: false,
            closed: false,
            steps: None,
            #[cfg(test)]
            step_service,
            _socket: socket,
        };
        if let Err(error) = this.reconcile(&snapshot) {
            return Err(match this.close() {
                Ok(()) => error,
                Err(recovery) => format!("{error}; {recovery}"),
            });
        }
        // Install the managed sender and reconcile every retained child with
        // its durable parent step before any new step can be accepted. An
        // unavailable sender leaves children retained for explicit recovery;
        // step requests are then refused, never re-executed.
        match this.app_broker().and_then(|broker| {
            crate::agent_sender::AgentSteps::install(
                &this.owner,
                &this.root,
                &broker,
                step_requests,
            )
        }) {
            Ok(steps) => this.steps = Some(steps),
            Err(error) => {
                tracing::warn!(%error, "managed sender unavailable; enrolled steps refused")
            }
        }
        Ok((this, requests))
    }
    pub(crate) fn broker(&self) -> &BrokerHandle {
        &self.broker
    }
    pub(crate) fn app_broker(&self) -> Result<AppActionBroker, String> {
        self.broker
            .app_action_broker()
            .ok_or_else(|| "worker broker unavailable".into())
    }
    /// Only the lifecycle's acknowledged live relay connection can set true.
    /// Queued signing work waits for reconnection; withdrawing readiness grants
    /// no new admission or dispatch and does not replay an existing effect.
    pub(crate) fn set_transport_ready(&mut self, ready: bool) {
        self.transport_ready = ready && !self.closed;
        if !self.transport_ready {
            self.owner.stop_readiness();
        }
    }
    pub(crate) fn fail(&self, reason: impl Into<String>) -> String {
        self.owner.stop_readiness();
        self.documents.fail(reason)
    }
    fn snapshot(&self) -> Result<Snapshot, String> {
        let store = self.owner.store()?;
        let peer = store.peer().map_err(|e| e.to_string())?;
        check_peer(&peer, store.policy_now().map_err(|e| e.to_string())?)?;
        if peer.room() != self.room || peer.authority() != &self.certificate {
            return Err("worker owner scope changed".into());
        }
        Ok(Snapshot {
            generation: store.state.generation,
            peer,
        })
    }
    fn reconcile(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        if self.generation == Some(snapshot.generation) {
            return Ok(());
        }
        // Validate every canonical record before changing any live owner.
        let validation = PortableStorage::new("cloud-worker-validation", &self.room)?;
        let mut validators = RecordOwners::portable(validation.clone());
        let mut plans = vec![];
        for row in snapshot.peer.rows() {
            let Some(definition) = [word_processor::definition(), chat::definition()]
                .into_iter()
                .find(|definition| {
                    ui::application::lookup_compatible_first_party_app_ids(
                        &row.app_id,
                        definition.app_id,
                    )
                })
            else {
                continue;
            };
            let checkout = checkout(definition, &row.record_id)?;
            if definition.app_id == chat::definition().app_id && !checkout.id.is_package_primary() {
                return Err("Chat requires its canonical primary route".into());
            }
            let bytes = snapshot
                .peer
                .document(&row.app_id, &row.record_id)
                .ok_or("canonical worker bytes missing")?;
            validation.hydrate(
                storage_key(&row.app_id, &self.room, &row.record_id),
                bytes.clone(),
                Some(self.certificate.device_public_key.to_vec()),
            )?;
            validators
                .open(&row.app_id, &self.room)
                .map_err(|e| e.to_string())?
                .try_open_record(&checkout)
                .map_err(|e| e.to_string())?;
            if let Some(worker) = self
                .workers
                .iter()
                .find(|w| w.app == row.app_id && w.record == row.record_id)
            {
                if !worker.records.can_merge_admitted(&checkout)? {
                    return Err("worker record is busy; committed refresh pending".into());
                }
            }
            plans.push((row.app_id, row.record_id, definition, checkout, bytes));
        }
        if self.workers.iter().any(|worker| {
            !plans
                .iter()
                .any(|(app, record, ..)| app == &worker.app && record == &worker.record)
        }) {
            return Err("mounted worker lost canonical record custody".into());
        }
        for (app, record, definition, checkout, bytes) in plans {
            let heads = snapshot
                .peer
                .document_heads(&app, &record)
                .ok_or("canonical worker heads missing")?;
            if let Some(worker) = self
                .workers
                .iter_mut()
                .find(|w| w.app == app && w.record == record)
            {
                if worker.heads != heads {
                    if worker
                        .records
                        .merge_admitted(&worker.checkout, &DocumentMutation::Snapshot(bytes))?
                        .is_none()
                    {
                        return Err("worker record owner changed during refresh".into());
                    }
                    worker.heads = heads;
                }
                continue;
            }
            self.cache.hydrate(
                storage_key(&app, &self.room, &record),
                bytes,
                Some(self.certificate.device_public_key.to_vec()),
            )?;
            let records = self
                .owners
                .open(&app, &self.room)
                .map_err(|e| e.to_string())?;
            let instance = u64::from_be_bytes(
                blake3::hash(format!("{}/{app}/{record}", self.room).as_bytes()).as_bytes()[..8]
                    .try_into()
                    .expect("fixed hash"),
            );
            let view_path = self
                .root
                .join("actions/views")
                .join(format!("{instance}.bin"));
            if view_path.try_exists().map_err(|e| e.to_string())? {
                if !std::fs::symlink_metadata(&view_path)
                    .map_err(|e| e.to_string())?
                    .is_file()
                {
                    return Err("worker view must be a regular file".into());
                }
                self.cache.hydrate(
                    StorageKey::View {
                        workspace: self.room.clone(),
                        instance,
                    },
                    std::fs::read(view_path).map_err(|e| e.to_string())?,
                    None,
                )?;
            }
            let restore = self
                .views
                .load(instance)
                .map_err(|e| e.to_string())?
                .map_or(RestoreInput::None, RestoreInput::Local);
            let permissions = Rc::new(RefCell::new(permission(
                &self.owner,
                &snapshot.peer,
                &self.certificate,
            )?));
            let target = LaunchTarget::Record {
                checkout: checkout.clone(),
                reveal: None,
            };
            let context = HostContext::prepare(
                definition,
                records.clone(),
                self.views.clone(),
                hex::encode(self.certificate.device_public_key),
                instance,
                permissions.clone(),
                ui::permissions::UserId(self.principal.clone()),
                qos_actions::ActionContext::default(),
                &target,
            )
            .map_err(|e| self.setup_error(e))?;
            let room = self.room.parse().map_err(|_| "invalid room UUID")?;
            let route = if record.is_empty() {
                RouteSpec::app(&app, room)
            } else {
                RouteSpec::record(&app, room, &record)
            };
            let (stream, authorizer) = self.broker.connect_local_worker_with_authorizer(
                CrdtRoute::record(&app, &self.room, &record),
                &self.certificate,
            )?;
            let manager = CollaborationManager::for_route_fd_with_identity(
                stream.into_raw_fd(),
                &app,
                room,
                (!record.is_empty()).then_some(record.clone()),
                None,
                CertifiedEndpointIdentity::with_host_documents(
                    self.presentation.clone(),
                    self.documents.bind(route, self.certificate.clone()),
                ),
            )
            .map_err(|e| format!("worker join: {e:?}"))?;
            let mut runner = HostWorker::mount(
                definition,
                ui::application::PreparedComponentInputs {
                    context,
                    target,
                    restore,
                    runner_inputs: ui::application::ComponentRunnerInputs {
                        theme: ui::Theme::dark(),
                        text_renderer: ui::TextRenderer::new(),
                    },
                },
                manager,
                permissions,
            )
            .map_err(|e| self.setup_error(e))?;
            #[cfg(test)]
            let observed_authorizer = authorizer.clone();
            if let Err(error) = runner.install_published_invocation_authorizer(authorizer) {
                self.recovery.extend(runner.close());
                return Err(error);
            }
            self.workers.push(Worker {
                app,
                record,
                checkout,
                records,
                runner,
                heads,
                #[cfg(test)]
                authorizer: observed_authorizer,
            });
        }
        self.generation = Some(snapshot.generation);
        Ok(())
    }
    fn setup_error(&mut self, error: ui::launch::LaunchError) -> String {
        let message = error.to_string();
        ui::host_worker::retain_setup_error(error, &mut self.recovery);
        message
    }
    /// A definite document refusal leaves the signer healthy; an uncertain
    /// outcome latched by the handoff service fails here.
    pub(crate) fn document_health(&self) -> Result<(), String> {
        if self.closed {
            return Err("workers closed".into());
        }
        self.documents.health()
    }
    pub(crate) fn committed(&mut self) -> Result<(), String> {
        let result = (|| {
            if self.closed {
                return Err("workers closed".into());
            }
            self.documents.health()?;
            let snapshot = self.snapshot()?;
            self.reconcile(&snapshot)?;
            self.acknowledge(&snapshot.peer)
        })();
        result.map_err(|reason| self.fail(reason))
    }
    pub(crate) fn pump(&mut self) -> Result<(), String> {
        let result = (|| {
            if self.closed {
                return Err("workers closed".into());
            }
            self.documents.health()?;
            let snapshot = self.snapshot()?;
            self.reconcile(&snapshot)?;
            for worker in &mut self.workers {
                worker
                    .runner
                    .pump(permission(&self.owner, &snapshot.peer, &self.certificate)?)?;
            }
            self.acknowledge(&snapshot.peer)
        })();
        let result = result.map_err(|reason| self.fail(reason));
        if result.is_ok() {
            if let (Some(steps), Ok(broker)) = (
                self.steps.as_mut(),
                self.broker.app_action_broker().ok_or(()),
            ) {
                steps.pump(&self.owner, &broker);
            }
        }
        result
    }
    fn acknowledge(&self, peer: &StatePeer) -> Result<(), String> {
        for write in self.cache.pending_writes() {
            let route = match &write.key {
                StorageKey::Primary { package, workspace } if workspace == &self.room => {
                    Some((package.as_str(), ""))
                }
                StorageKey::Record {
                    package,
                    workspace,
                    record,
                } if workspace == &self.room => Some((package.as_str(), record.as_str())),
                StorageKey::View {
                    workspace,
                    instance,
                } if workspace == &self.room => {
                    let directory = self.root.join("actions/views");
                    private_directory(&directory)?;
                    atomic_file::write_with_mode(
                        &directory.join(format!("{instance}.bin")),
                        &write.value.bytes,
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                    self.cache.acknowledge(&write, Ok(()))?;
                    None
                }
                _ => return Err("worker persistence crossed owner namespace".into()),
            };
            if let Some((app, record)) = route {
                let committed = peer
                    .document(app, record)
                    .ok_or("worker lost committed record")?;
                ui::host_worker::acknowledge_document(&self.cache, &write, &committed, true)?;
            }
        }
        Ok(())
    }
    pub(crate) fn publications(&self) -> Result<WorkerPublications, String> {
        self.documents.health().map_err(|e| self.fail(e))?;
        let snapshot = self.snapshot().map_err(|e| self.fail(e))?;
        let mut surfaces: Vec<PublishedSurface> = vec![];
        for surface in self.broker.published_action_surfaces() {
            if surface.live
                && surface.route.cluster_id == self.room
                && surface.app_id == surface.route.app_id
                && self.workers.iter().any(|worker| {
                    worker.app == surface.app_id && worker.record == surface.route.record_id
                })
                && self
                    .broker
                    .is_current_host_worker(surface.peer_id, &surface.route, &self.certificate)
                    .map_err(|reason| self.fail(reason))?
            {
                surfaces.push(surface);
            }
        }
        surfaces.sort_by(|a, b| {
            (&a.app_id, &a.route.record_id, a.peer_id).cmp(&(
                &b.app_id,
                &b.route.record_id,
                b.peer_id,
            ))
        });
        let mut grants = vec![];
        let mut revision = vec![];
        for surface in &surfaces {
            let mut actions: Vec<_> = surface
                .actions
                .iter()
                .filter(|a| a.operation_kind == qos_actions::OperationKind::Document)
                .collect();
            actions.sort_by(|a, b| (&a.resource, &a.action).cmp(&(&b.resource, &b.action)));
            for action in actions {
                grants.push(GrantAction {
                    app_id: surface.app_id.clone(),
                    record_id: if surface.route.record_id.is_empty() {
                        RecordId::package_primary().as_str().to_owned()
                    } else {
                        surface.route.record_id.clone()
                    },
                    resource: action.resource.clone(),
                    action: action.action.clone(),
                });
                let input = match &action.input {
                    PublishedInput::None => serde_json::Value::Null,
                    PublishedInput::Text {
                        prompt,
                        placeholder,
                        wants_content,
                    } => {
                        serde_json::json!({"prompt":prompt,"placeholder":placeholder,"wants_content":wants_content})
                    }
                };
                revision.push(serde_json::json!({"peer":surface.peer_id,"app":surface.app_id,"record":surface.route.record_id,"resource":action.resource,"action":action.action,"kind":action.resource_kind,"label":action.resource_label,"action_label":action.action_label,"description":action.description,"params":action.params,"contract":action.contract,"input":input}));
            }
        }
        grants.dedup();
        let ready = !self.closed
            && self.transport_ready
            && self.generation == Some(snapshot.generation)
            && !grants.is_empty()
            && self.workers.iter().all(|w| {
                surfaces
                    .iter()
                    .any(|s| s.app_id == w.app && s.route.record_id == w.record)
            });
        if !ready {
            self.owner.stop_readiness();
        }
        Ok(WorkerPublications {
            room: self.room.clone(),
            host_device: self.certificate.device_public_key,
            governance_revision: qos_peer_state::owner_commit::governance_revision(&snapshot.peer)?,
            publication_revision: blake3::hash(
                &serde_json::to_vec(&revision).map_err(|e| e.to_string())?,
            )
            .to_hex()
            .to_string(),
            grants: if ready { grants } else { vec![] },
            ready,
            native_v5_ready: snapshot.peer.admission_v5_selected(),
        })
    }
    pub(crate) fn close(&mut self) -> Result<(), String> {
        // Retained children stay durable in the sender file and task ledger.
        self.steps = None;
        self.set_transport_ready(false);
        self.documents
            .fail("workers closing; prior outcome must be reconciled");
        if !self.closed {
            self.closed = true;
            for worker in &mut self.workers {
                self.recovery.extend(worker.runner.close());
            }
        }
        // Ask every original owner to transfer pending bytes before checking any failure.
        for report in &mut self.recovery {
            let _ = report.retry_record();
            let _ = report.retry_view();
        }
        let result = (|| {
            let snapshot = self.snapshot()?;
            self.acknowledge(&snapshot.peer)?;
            let mut errors = vec![];
            for report in &mut self.recovery {
                let _ = report.retry_record();
                let _ = report.retry_view();
                errors.extend(
                    [
                        &report.record_error,
                        &report.view_error,
                        &report.teardown_error,
                    ]
                    .into_iter()
                    .filter_map(|e| e.clone()),
                );
                if report.record_retry.is_some()
                    || report.record_retry_target.is_some()
                    || report.view_retry.is_some()
                {
                    errors.push("unresolved component retry custody".into());
                }
            }
            if !self.cache.pending_writes().is_empty() {
                errors.push("uncommitted worker bytes".into());
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        })();
        if let Err(reason) = result {
            let preserved = self.preserve_recovery(&reason);
            return Err(match preserved {
                Ok(()) => format!("{reason}; custody preserved in application recovery"),
                Err(error) => format!("{reason}; FAILED to preserve application recovery: {error}"),
            });
        }
        Ok(())
    }
    fn preserve_recovery(&self, reason: &str) -> Result<(), String> {
        let pending = self.cache.pending_writes().into_iter().map(|write| {
            let key = match write.key {
                StorageKey::Primary { package, workspace } => serde_json::json!({"kind":"primary","app":package,"room":workspace}),
                StorageKey::Record { package, workspace, record } => serde_json::json!({"kind":"record","app":package,"room":workspace,"record":record}),
                StorageKey::View { workspace, instance } => serde_json::json!({"kind":"view","room":workspace,"instance":instance}),
                StorageKey::Admission { workspace } => serde_json::json!({"kind":"admission","room":workspace}),
                StorageKey::PrivateActions { workspace } => serde_json::json!({"kind":"private_actions","room":workspace}),
            };
            serde_json::json!({"key":key,"actor":write.value.actor,"bytes":write.value.bytes,"revision":write.revision})
        }).collect::<Vec<_>>();
        let reports = self.recovery.iter().map(|report| serde_json::json!({"view":report.recovery,"view_error":report.view_error,"record_error":report.record_error,"teardown_error":report.teardown_error})).collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"room":self.room,"device":self.certificate.device_public_key,"outcome":"unknown","error":reason,"pending":pending,"reports":reports})).map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("recovery artifact exceeds bounded capacity".into());
        }
        atomic_file::write_with_mode(&self.root.join("actions/recovery.json"), &bytes, 0o600)
            .map_err(|e| e.to_string())
    }
}
fn check_peer(peer: &StatePeer, now: web_time::SystemTime) -> Result<(), String> {
    if !peer.admission_v5_selected() || peer.incomplete_reason().is_some() {
        return Err("complete explicitly selected V5 worker owner required".into());
    }
    peer.permissions(peer.authority().device_public_key, now)
        .map(|_| ())
}
fn permission(
    owner: &Arc<RuntimeOwner>,
    peer: &StatePeer,
    certificate: &DeviceCert,
) -> Result<PermissionContext, String> {
    use qos_action_broker::peer_actions::PrivatePolicyBasis;
    let time = owner.policy_time()?;
    let now = time.now_ns().map_err(|e| e.to_string())?;
    let (role, caps, person) =
        peer.permissions(certificate.device_public_key, crate::now_time(now))?;
    let basis = PrivatePolicyBasis::from_peer(peer).map_err(|e| e.to_string())?;
    let owner = owner.clone();
    let certificate = certificate.clone();
    let retained_role = role;
    let retained_person = person.clone();
    let sampler = Arc::new(move || {
        let store = owner.store_for(&time)?;
        let current = store.peer().map_err(|e| e.to_string())?;
        let now = store.policy_now_ns().map_err(|e| e.to_string())?;
        let (role, _, person) =
            current.permissions(certificate.device_public_key, crate::now_time(now))?;
        if !current.certificates().contains(&certificate)
            || role != retained_role
            || person != retained_person
            || PrivatePolicyBasis::from_peer(&current)
                .map_err(|e| e.to_string())?
                .revision()
                != basis.revision()
        {
            return Err("worker policy changed; rebuild current permission context".into());
        }
        Ok(now / 1_000_000_000)
    });
    Ok(PermissionContext::from_capabilities(
        Actor::person(Some(role), person).in_room(peer.room().to_string()),
        caps,
    )
    .with_policy_time_sampler(sampler))
}

fn checkout(
    definition: &ui::application::ApplicationDefinition,
    record: &str,
) -> Result<RecordCheckout, String> {
    if record.is_empty() {
        Ok(RecordCheckout::package_primary(
            definition.record_kind.clone(),
        ))
    } else {
        Ok(RecordCheckout::new(
            RecordId::parse(record).ok_or("invalid canonical worker record")?,
            definition.record_kind.clone(),
        ))
    }
}
fn storage_key(app: &str, room: &str, record: &str) -> StorageKey {
    if record.is_empty() {
        StorageKey::Primary {
            package: app.into(),
            workspace: room.into(),
        }
    } else {
        StorageKey::Record {
            package: app.into(),
            workspace: room.into(),
            record: record.into(),
        }
    }
}
fn private_directory(path: &Path) -> Result<(), String> {
    atomic_file::provision_directory(path, atomic_file::Privacy::OwnerOnly)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod enrolled_handoff_tests;
#[cfg(test)]
mod tests;
