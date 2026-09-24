//! Production ordinary application host; SessionOwner remains the sole durable writer.
use crate::{SessionOwner, committed_authority::CommittedAuthority, host_signing::HostSigning};
use anyhow::{Context, Result};
use ed25519_dalek::Signer;
use qos_action_broker::{
    collab_broker::{BrokerHandle, CrdtRoute},
    collab_outbound::{PeerPublishSink, PeerPublisher},
    peer_actions::{CertificateAuthority, OwnerPolicyClock, PolicyTime},
    policy::WorkspacePolicy,
    services::WorkspaceAuthority,
};
use qos_peer_state::{NeighbourMsg, StatePeer, owner_commit::OwnerCommitService};
use std::{
    cell::RefCell,
    os::{fd::IntoRawFd, unix::fs::PermissionsExt},
    rc::Rc,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use ui::{
    application::{HostContext, LaunchTarget, RestoreInput},
    collaboration::{CertifiedEndpointIdentity, CollaborationManager, RouteSpec},
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners},
};

#[derive(Clone)]
pub(crate) struct Authority {
    committed: CommittedAuthority,
    ready: Arc<AtomicBool>,
    registration: Arc<OnceLock<PolicyTime>>,
}
impl Authority {
    pub(crate) fn new(committed: CommittedAuthority) -> Self {
        Self {
            committed,
            ready: Arc::new(AtomicBool::new(false)),
            registration: Arc::default(),
        }
    }
    pub(crate) fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
    fn current(&self) -> Result<crate::committed_authority::Current<'_>, String> {
        let current = self.committed.read()?;
        if !self.ready.load(Ordering::Acquire) {
            return Err("action transport unavailable".into());
        }
        Ok(current)
    }
    fn register_policy_time(&self) -> Result<PolicyTime, String> {
        let current = self.committed.read()?;
        let time = current.view().policy_time()?;
        let registered = self.registration.get_or_init(|| time.clone());
        if !registered.same_origin(&time) {
            return Err("action runtime belongs to a retired policy origin".into());
        }
        current.view().check_policy_time(registered)?;
        Ok(registered.clone())
    }
}
impl CertificateAuthority for Authority {
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        let Some(time) = self.registration.get() else {
            return Ok(None);
        };
        self.current()?.view().check_policy_time(time)?;
        Ok(Some(time.clone()))
    }
    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
        time: &PolicyTime,
    ) -> Result<(), String> {
        let registered = self
            .registration
            .get()
            .ok_or("action policy time is not registered")?;
        if !registered.same_origin(time) {
            return Err("action policy clock or origin mismatch".into());
        }
        let guard = self.current()?;
        let now = guard.view().check_policy_time(time)?;
        let peer = guard.peer();
        if peer.room() != room.to_string() || !peer.certificates().contains(cert) {
            return Err("certificate not admitted by current owner".into());
        }
        peer.permissions(cert.device_public_key, policy_system_time(now))
            .map(|_| ())
    }
    fn authorize(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if let Some(time) = self.registration.get() {
            return self.authorize_with_policy_time(room, cert, time);
        }
        let guard = self.current()?;
        let peer = guard.peer();
        if peer.room() != room.to_string() || !peer.certificates().contains(cert) {
            return Err("certificate not admitted by current owner".into());
        }
        peer.permissions(cert.device_public_key, web_time::SystemTime::now())
            .map(|_| ())
    }
}
impl WorkspaceAuthority for Authority {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        let guard = self.current()?;
        project(guard.peer())
    }
}
fn project(peer: &StatePeer) -> Result<qos_desktop::WorkspaceTree, String> {
    #[cfg(test)]
    PROJECTION_CALLS.with(|count| count.set(count.get() + 1));
    let doc = peer.admitted_room().ok_or("admitted room missing")?;
    let mut tree = qos_desktop::WorkspaceTree::default();
    let room = uuid::Uuid::parse_str(peer.room()).map_err(|e| e.to_string())?;
    qos_desktop::projection::RoomProjection::apply_admitted_room(&mut tree, room.into(), doc)?;
    let (_, owner, capabilities) = qos_peer_state::room::extract_governed_fields(doc);
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|w| w.room.as_uuid() == room)
        .ok_or("workspace missing")?;
    workspace.owner = owner;
    workspace.capabilities = capabilities;
    workspace.tasks = qos_desktop::projection::workspace_tasks_of(doc);
    workspace.effect_receipts = qos_desktop::projection::workspace_effect_receipts_of(doc);
    Ok(tree)
}
#[cfg(test)]
thread_local! { static PROJECTION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
struct NetworkSink {
    sender: tokio::sync::mpsc::Sender<NeighbourMsg>,
    signing: HostSigning,
    peer: u64,
}
impl PeerPublishSink for NetworkSink {
    fn app_document(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {
        self.signing.fail("unexpected legacy document publication");
    }
    fn app_presence(
        &self,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        if self
            .sender
            .try_send(NeighbourMsg::AppPresenceV2 {
                peer: self.peer,
                app_id,
                cluster_id,
                presence,
            })
            .is_err()
        {
            self.signing.fail("presence publication queue unavailable");
        }
    }
    fn scrollback_request(&self, artifact: collab_types::ArtifactId, before_seq: u64, max: u16) {
        if self
            .sender
            .try_send(NeighbourMsg::ScrollbackRequest {
                artifact,
                before_seq,
                max,
                requester: self.peer,
            })
            .is_err()
        {
            self.signing
                .fail("scrollback publication queue unavailable");
        }
    }
}
struct Worker {
    app_id: String,
    record_id: String,
    runner: ui::host_worker::HostWorker,
}
/// Cache policy data, never its time-sensitive role or permission decision.
struct WorkerPolicy {
    room: String,
    owner: Option<String>,
    capabilities: Vec<collab_types::caps::CapOverride>,
}
impl WorkerPolicy {
    fn context_at(&self, principal: &str, now: u64) -> ui::permissions::PermissionContext {
        let role = qos_peer_state::room::role_of(
            self.owner.as_deref(),
            &self.capabilities,
            principal,
            now,
        );
        ui::permissions::PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(role, principal).in_room(self.room.clone()),
            self.capabilities.clone(),
        )
        .at_unix_time(now)
    }
    fn context(
        &self,
        principal: &str,
        authority: Authority,
        time: PolicyTime,
    ) -> Result<ui::permissions::PermissionContext, String> {
        let now = authority.current()?.view().check_policy_time(&time)? / 1_000_000_000;
        let context = self.context_at(principal, now);
        let role = qos_peer_state::room::role_of(
            self.owner.as_deref(),
            &self.capabilities,
            principal,
            now,
        );
        let principal = principal.to_owned();
        let room = self.room.clone();
        let owner = self.owner.clone();
        let capabilities = self.capabilities.clone();
        Ok(context.with_policy_time_sampler(Arc::new(move || {
            let current = authority.current()?;
            let now = current.view().check_policy_time(&time)? / 1_000_000_000;
            let document = current
                .peer()
                .admitted_room()
                .ok_or("worker governance missing")?;
            let (_, current_owner, current_caps) =
                qos_peer_state::room::extract_governed_fields(document);
            if current.peer().room() != room
                || current_owner != owner
                || current_caps != capabilities
                || qos_peer_state::room::role_of(
                    current_owner.as_deref(),
                    &current_caps,
                    &principal,
                    now,
                ) != role
            {
                return Err("worker policy changed; current pump required".into());
            }
            Ok(now)
        })))
    }
}
fn policy_system_time(now_ns: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(now_ns)
}
pub(crate) struct ActionHost {
    pub broker: BrokerHandle,
    policy_clock: OwnerPolicyClock,
    policy_time: PolicyTime,
    authority: Authority,
    workers: Vec<Worker>,
    #[cfg(test)]
    scoped_fixture: bool,
    recovery: Vec<ui::application::ComponentCloseReport>,
    signing: HostSigning,
    principal: String,
    cache: PortableStorage,
    owners: RecordOwners,
    views: Rc<ui::application_view::InstanceViewStore>,
    publication: RefCell<crate::publication::PublicationCursor>,
    reconciled_policy: Option<(u64, WorkerPolicy)>,
    // Declared after broker so its listener/thread closes before directory cleanup.
    _socket_directory: tempfile::TempDir,
}
impl ActionHost {
    pub(crate) fn registered_policy_time(&self) -> &PolicyTime {
        &self.policy_time
    }
    pub(crate) fn policy_clock(&self) -> Result<OwnerPolicyClock, String> {
        self.policy_clock
            .now_ns()
            .map_err(|error| error.to_string())?;
        Ok(self.policy_clock.clone())
    }

    #[cfg(test)]
    pub fn start(
        owner: &SessionOwner,
        authority: Authority,
        signing: HostSigning,
        sender: tokio::sync::mpsc::Sender<NeighbourMsg>,
    ) -> Result<Self> {
        Self::start_with_collaboration(owner, authority, signing, sender, None)
    }

    pub(crate) fn start_with_collaboration(
        owner: &SessionOwner,
        authority: Authority,
        signing: HostSigning,
        sender: tokio::sync::mpsc::Sender<NeighbourMsg>,
        collaboration_admin: Option<crate::collaboration::CollaborationAdminHandle>,
    ) -> Result<Self> {
        #[cfg(test)]
        {
            Self::start_inner(
                owner,
                authority,
                signing,
                sender,
                collaboration_admin,
                false,
            )
        }
        #[cfg(not(test))]
        {
            Self::start_inner(owner, authority, signing, sender, collaboration_admin)
        }
    }
    fn start_inner(
        owner: &SessionOwner,
        authority: Authority,
        signing: HostSigning,
        sender: tokio::sync::mpsc::Sender<NeighbourMsg>,
        collaboration_admin: Option<crate::collaboration::CollaborationAdminHandle>,
        #[cfg(test)] scoped_fixture: bool,
    ) -> Result<Self> {
        let _timing = crate::action_timing::Timing::new("action_startup");
        let policy_clock = authority
            .committed
            .policy_clock()
            .map_err(anyhow::Error::msg)?;
        let policy_time = authority
            .register_policy_time()
            .map_err(anyhow::Error::msg)?;
        signing
            .register_policy_time(policy_time.clone())
            .map_err(anyhow::Error::msg)?;
        let state = owner.root().join("actions");
        ensure_private(&state)?;
        if state.join("recovery.json").try_exists()? {
            anyhow::bail!(
                "unresolved application recovery at {}; actions require explicit recovery",
                state.join("recovery.json").display()
            );
        }
        let cert = &owner.identity.native_cert;
        let publisher = PeerPublisher::with_owner_commit(
            Arc::new(NetworkSink {
                sender,
                signing: signing.clone(),
                peer: qos_peer_state::peer_handle(&cert.device_public_key),
            }),
            Arc::new(signing.clone()),
        );
        // Unix socket names have a small platform limit, independent of the
        // durable state path. TempDir atomically creates a unique private 0700
        // directory; /tmp keeps even macOS's long per-user TMPDIR out of it.
        let socket_directory = tempfile::Builder::new()
            .prefix("qos-peer-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .context("creating private broker runtime directory")?;
        let broker = qos_action_broker::collab_broker::spawn_native_peer_with_services(
            socket_directory.path().join("broker.sock"),
            Arc::new(authority.clone()),
            publisher,
            qos_action_broker::peer_actions::NativePeerBrokerConfig {
                journal_root: state.join("journal"),
                certificate_authority: Arc::new(authority.clone()),
            },
            qos_action_broker::services::HostServices {
                desktop_commands: Some(signing.desktop_commands(owner.peer().room().into())),
                collaboration_admin: collaboration_admin.map(|service| Arc::new(service) as Arc<_>),
                ..Default::default()
            },
        )?;
        let principal = match collab_types::caps::Principal::person_from_key(
            collab_types::Ed25519PublicKey::from_bytes(cert.user_public_key),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            _ => unreachable!("person principal"),
        };
        let cache = PortableStorage::new(
            "ordinary-session-worker",
            &hex::encode(cert.device_public_key),
        )
        .map_err(anyhow::Error::msg)?;
        let views = Rc::new(
            ui::application_view::InstanceViewStore::portable(cache.clone(), owner.peer().room())
                .map_err(anyhow::Error::msg)?,
        );
        let owners = RecordOwners::portable(cache.clone());
        let mut this = Self {
            broker,
            policy_clock,
            policy_time,
            authority,
            workers: vec![],
            #[cfg(test)]
            scoped_fixture,
            recovery: vec![],
            signing,
            principal,
            cache,
            owners,
            views,
            publication: Default::default(),
            reconciled_policy: None,
            _socket_directory: socket_directory,
        };
        let result = this.reconcile(owner);
        this.finish_start(owner, result)
    }
    fn finish_start(mut self, owner: &SessionOwner, result: Result<()>) -> Result<Self> {
        if let Err(error) = result {
            let cleanup = self.close(owner);
            return Err(match cleanup {
                Ok(()) => error,
                Err(recovery) => anyhow::anyhow!("{error}; {recovery}"),
            });
        }
        Ok(self)
    }
    fn setup_error(&mut self, error: ui::launch::LaunchError) -> anyhow::Error {
        let message = error.to_string();
        self.retain_setup(error);
        anyhow::anyhow!(message)
    }
    fn retain_setup(&mut self, error: ui::launch::LaunchError) {
        ui::host_worker::retain_setup_error(error, &mut self.recovery);
    }
    fn reconcile(&mut self, owner: &SessionOwner) -> Result<()> {
        let generation = owner.committed_generation();
        if self
            .reconciled_policy
            .as_ref()
            .is_some_and(|(cached, _)| *cached == generation)
        {
            return Ok(());
        }
        let _timing = crate::action_timing::Timing::slow("action_reconcile");
        let cert = &owner.identity.native_cert;
        let room: uuid::Uuid = owner.peer().room().parse()?;
        let tree = project(owner.peer()).map_err(anyhow::Error::msg)?;
        let workspace = &tree.workspaces[0];
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_string(),
            device_certificate: serde_json::to_vec(cert)?,
            proof: owner
                .identity
                .native
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &room.to_string(),
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .map_err(anyhow::Error::msg)?;
        for row in owner.peer().rows() {
            if self
                .workers
                .iter()
                .any(|worker| worker.app_id == row.app_id && worker.record_id == row.record_id)
            {
                continue;
            }
            let definition = match row.app_id.as_str() {
                "dev.qos.word-processor" => word_processor::definition(),
                "dev.qos.chat" => chat::definition(),
                "dev.qos.notes" => notes::definition(),
                "dev.qos.calculator" => calculator::definition(),
                _ => continue,
            };
            #[cfg(all(test, feature = "test-support"))]
            let definition = if self.scoped_fixture {
                scoped_fixture::test_definition(&row.app_id).unwrap_or(definition)
            } else {
                definition
            };
            let checkout = if row.record_id.is_empty() {
                RecordCheckout::package_primary(definition.record_kind.clone())
            } else {
                RecordCheckout::new(
                    RecordId::parse(&row.record_id).context("invalid canonical record")?,
                    definition.record_kind.clone(),
                )
            };
            self.cache
                .hydrate(
                    if checkout.id.is_package_primary() {
                        StorageKey::Primary {
                            package: row.app_id.clone(),
                            workspace: room.to_string(),
                        }
                    } else {
                        StorageKey::Record {
                            package: row.app_id.clone(),
                            workspace: room.to_string(),
                            record: checkout.id.as_str().into(),
                        }
                    },
                    owner
                        .peer()
                        .document(&row.app_id, &row.record_id)
                        .context("canonical worker record missing")?,
                    Some(cert.device_public_key.to_vec()),
                )
                .map_err(anyhow::Error::msg)?;
            let records = self
                .owners
                .open(&row.app_id, &room.to_string())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

            let instance = u64::from_be_bytes(
                blake3::hash(format!("{room}/{}/{}", row.app_id, row.record_id).as_bytes())
                    .as_bytes()[..8]
                    .try_into()
                    .expect("fixed digest"),
            );
            let view_key = StorageKey::View {
                workspace: room.to_string(),
                instance,
            };
            let view_path = owner
                .root()
                .join("actions/views")
                .join(format!("{instance}.bin"));
            if view_path.try_exists()? {
                if !std::fs::symlink_metadata(&view_path)?.is_file() {
                    anyhow::bail!("local view must be a regular file");
                }
                self.cache
                    .hydrate(view_key, std::fs::read(view_path)?, None)
                    .map_err(anyhow::Error::msg)?;
            }
            let restore = self
                .views
                .load(instance)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .map_or(RestoreInput::None, RestoreInput::Local);
            let target = LaunchTarget::Record {
                checkout,
                reveal: None,
            };
            let permissions = Rc::new(RefCell::new(
                ui::permissions::PermissionContext::from_capabilities(
                    workspace.actor_for_person(&self.principal),
                    workspace.capabilities.clone(),
                ),
            ));
            #[cfg(all(test, feature = "test-support"))]
            let fixture_records = records.clone();
            let context = HostContext::prepare(
                definition,
                records,
                self.views.clone(),
                hex::encode(cert.device_public_key),
                instance,
                permissions.clone(),
                ui::permissions::UserId(self.principal.clone()),
                tilo_actions::ActionContext::default(),
                &target,
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            #[cfg(all(test, feature = "test-support"))]
            if self.scoped_fixture {
                scoped_fixture::edit_factory_input(&row.app_id, &fixture_records, &row.record_id);
            }
            let route = CrdtRoute::record(&row.app_id, room.to_string(), &row.record_id);
            let spec = if row.record_id.is_empty() {
                RouteSpec::app(&row.app_id, room)
            } else {
                RouteSpec::record(&row.app_id, room, &row.record_id)
            };
            let (stream, authorizer) = self
                .broker
                .connect_local_worker_with_authorizer(route, cert)
                .map_err(anyhow::Error::msg)?;
            let manager = CollaborationManager::for_route_fd_with_identity(
                stream.into_raw_fd(),
                &row.app_id,
                room,
                (!row.record_id.is_empty()).then_some(row.record_id.clone()),
                None,
                CertifiedEndpointIdentity::with_host_documents(
                    presentation.clone(),
                    self.signing.bind(spec, cert.clone()),
                ),
            )
            .map_err(|e| anyhow::anyhow!("worker join: {e:?}"))?;
            let mut runner = ui::host_worker::HostWorker::mount(
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
            #[cfg(all(test, feature = "test-support"))]
            let authorizer = if self.scoped_fixture {
                scoped_fixture::observe_authorizer(&row.app_id, authorizer)
            } else {
                authorizer
            };
            if let Err(error) = runner.install_published_invocation_authorizer(authorizer) {
                self.recovery.extend(runner.close());
                return Err(anyhow::Error::msg(error));
            }
            self.workers.push(Worker {
                app_id: row.app_id,
                record_id: row.record_id,
                runner,
            });
        }
        // A partial setup failure must retry this generation, preserving any
        // workers already installed and their original recovery custody.
        self.reconciled_policy = Some((
            generation,
            WorkerPolicy {
                room: workspace.room.to_string(),
                owner: workspace.owner.clone(),
                capabilities: workspace.capabilities.clone(),
            },
        ));
        Ok(())
    }

    pub fn pump(&mut self, owner: &SessionOwner) -> Result<(), String> {
        let _timing = crate::action_timing::Timing::slow("action_pump");
        self.authority
            .current()?
            .view()
            .check_policy_time(&self.policy_time)?;
        self.signing.health()?;
        self.reconcile(owner)
            .map_err(|e| self.signing.fail(e.to_string()))?;
        let policy = &self
            .reconciled_policy
            .as_ref()
            .expect("successful reconciliation installs policy")
            .1;
        for worker in &mut self.workers {
            let _timing = crate::action_timing::Timing::slow("worker_pump");
            worker
                .runner
                .pump(policy.context(
                    &self.principal,
                    self.authority.clone(),
                    self.policy_time.clone(),
                )?)
                .map_err(|reason| self.signing.fail(reason))?;
        }
        self.acknowledge_committed(owner)?;
        Ok(())
    }
    #[cfg(feature = "test-support")]
    pub fn qualification_heads(&mut self, room: &str) -> serde_json::Value {
        serde_json::Value::Array(self.workers.iter().map(|worker| {
            let records = self.owners.open(&worker.app_id, room).unwrap();
            let id = if worker.record_id.is_empty() { RecordId::package_primary() } else { RecordId::parse(&worker.record_id).unwrap() };
            let heads = records.read(&id, |doc| doc.clone().get_heads().iter().map(ToString::to_string).collect::<Vec<_>>()).unwrap();
            serde_json::json!({"app": worker.app_id, "record": worker.record_id, "heads": heads})
        }).collect())
    }
    pub fn ready(&self) -> bool {
        use qos_peer_state::desktop_command;
        let room = self
            .reconciled_policy
            .as_ref()
            .map(|(_, policy)| policy.room.as_str())
            .unwrap_or_default();
        desktop_service_ready(
            self.signing.health().is_ok(),
            self.broker.has_live_published_action(
                desktop_command::APP_ID,
                room,
                "",
                desktop_command::RESOURCE,
                desktop_command::ACTION,
            ),
        )
    }
    fn acknowledge_committed(&self, owner: &SessionOwner) -> Result<(), String> {
        for write in self.cache.pending_writes() {
            let target = match &write.key {
                StorageKey::Record {
                    package,
                    workspace,
                    record,
                } if workspace == owner.peer().room() => Some((package.as_str(), record.as_str())),
                StorageKey::Primary { package, workspace } if workspace == owner.peer().room() => {
                    Some((package.as_str(), ""))
                }
                StorageKey::View {
                    workspace,
                    instance,
                } if workspace == owner.peer().room() => {
                    let directory = owner.root().join("actions/views");
                    ensure_private(&directory).map_err(|e| e.to_string())?;
                    atomic_file::write_with_mode(
                        &directory.join(format!("{instance}.bin")),
                        &write.value.bytes,
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                    crate::store::sync_dir(&directory).map_err(|e| e.to_string())?;
                    self.cache.acknowledge(&write, Ok(()))?;
                    None
                }
                _ => return Err("worker persistence crossed session scope".into()),
            };
            if let Some((app, record)) = target {
                let committed = owner
                    .peer()
                    .document(app, record)
                    .ok_or("worker record lost committed custody")?;
                ui::host_worker::acknowledge_document(
                    &self.cache,
                    &write,
                    &committed,
                    owner.permits_publication(),
                )?;
            }
        }
        Ok(())
    }
    pub fn committed(&self, owner: &SessionOwner) -> Result<(), String> {
        self.acknowledge_committed(owner)?;
        self.publication
            .borrow_mut()
            .send_committed(owner, |bytes, _| {
                if let NeighbourMsg::AppDocument {
                    app_id,
                    cluster_id,
                    record_id,
                    envelope,
                    ..
                } = postcard::from_bytes(bytes)?
                {
                    self.broker.fanout_event(
                        &ui::collaboration::CollabEvent::Document {
                            envelope,
                            from_user_id: self.principal.clone(),
                            decrypted_payload: None,
                        },
                        None,
                        &CrdtRoute::record(app_id, cluster_id, record_id),
                    );
                }
                Ok(())
            })
            .map_err(|error| error.to_string())
    }
    /// Stop the old runtime without borrowing authority from its replacement.
    /// Original component retry custody stays here until close can settle it.
    pub(crate) fn retire(&mut self) {
        for mut worker in self.workers.drain(..) {
            self.recovery.extend(worker.runner.close());
        }
    }
    pub fn close(&mut self, owner: &SessionOwner) -> Result<(), String> {
        self.retire();
        let result: Result<(), String> = (|| {
            // First request every original owner's pending bytes; one failure
            // must not prevent another report from transferring its custody.
            for report in &mut self.recovery {
                let _ = report.retry_record();
                let _ = report.retry_view();
            }
            self.acknowledge_committed(owner)?;
            let mut errors = Vec::new();
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
                    .filter_map(|error| error.clone()),
                );
                if report.record_retry.is_some()
                    || report.record_retry_target.is_some()
                    || report.view_retry.is_some()
                {
                    errors.push("unresolved component retry custody".into());
                }
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        })();
        if let Err(reason) = result {
            let path = owner.root().join("actions/recovery.json");
            let preserve = (|| -> Result<(), String> {
                let pending = self.cache.pending_writes().into_iter().map(|write| {
                    let key = match write.key {
                        StorageKey::Record { package, workspace, record } => serde_json::json!({"kind":"record","app":package,"room":workspace,"record":record}),
                        StorageKey::Primary { package, workspace } => serde_json::json!({"kind":"primary","app":package,"room":workspace}),
                        StorageKey::View { workspace, instance } => serde_json::json!({"kind":"view","room":workspace,"instance":instance}),
                        StorageKey::Admission { workspace } => serde_json::json!({"kind":"admission","room":workspace}),
                        StorageKey::PrivateActions { workspace } => serde_json::json!({"kind":"private_actions","room":workspace}),
                    };
                    serde_json::json!({"key":key,"actor":write.value.actor,"bytes":write.value.bytes,"revision":write.revision})
                }).collect::<Vec<_>>();
                let reports = self.recovery.iter().map(|report| serde_json::json!({"view":report.recovery,"view_error":report.view_error,"record_error":report.record_error,"teardown_error":report.teardown_error})).collect::<Vec<_>>();
                let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"room":owner.peer().room(),"device":owner.identity.native_cert.device_public_key,"outcome":"unknown","error":reason,"pending":pending,"reports":reports})).map_err(|e| e.to_string())?;
                if bytes.len() > 64 * 1024 * 1024 {
                    return Err("recovery artifact exceeds bounded capacity".into());
                }
                atomic_file::write_with_mode(&path, &bytes, 0o600).map_err(|e| e.to_string())?;
                crate::store::sync_dir(path.parent().ok_or("missing recovery parent")?)
                    .map_err(|e| e.to_string())
            })();
            return Err(match preserve {
                Ok(()) => format!(
                    "{reason}; unresolved application custody preserved at {}",
                    path.display()
                ),
                Err(failure) => {
                    format!("{reason}; FAILED to preserve application recovery: {failure}")
                }
            });
        }
        Ok(())
    }
}

fn desktop_service_ready(signing_healthy: bool, desktop_published: bool) -> bool {
    signing_healthy && desktop_published
}

fn ensure_private(path: &std::path::Path) -> Result<()> {
    if path.try_exists()? {
        crate::store::check_dir(path)
    } else {
        crate::store::private_dir(path)?;
        crate::store::sync_dir(path.parent().context("private state requires parent")?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    fn test_authority(owner: &crate::SessionOwner) -> super::Authority {
        let authority =
            super::Authority::new(crate::committed_authority::CommittedAuthority::new(owner));
        authority.set_ready(true);
        authority
    }

    use super::*;
    // A transport ACK or an older durable snapshot must not clear pending bytes.
    #[test]
    fn shared_worker_ack_requires_committed_heads_and_publication_authority() {
        use ui::automerge::{ROOT, transaction::Transactable};
        let cache = PortableStorage::new("host-worker-test", "device").unwrap();
        let key = StorageKey::Record {
            package: "app".into(),
            workspace: "room".into(),
            record: "record".into(),
        };
        let mut doc = ui::automerge::AutoCommit::new();
        doc.put(ROOT, "value", 1).unwrap();
        let before = doc.save();
        doc.put(ROOT, "value", 2).unwrap();
        let after = doc.save();
        cache.put(key, after.clone(), None).unwrap();
        let pending = cache.pending_writes().remove(0);
        assert!(!ui::host_worker::acknowledge_document(&cache, &pending, &before, true).unwrap());
        assert!(!ui::host_worker::acknowledge_document(&cache, &pending, &after, false).unwrap());
        assert_eq!(cache.pending_writes().len(), 1);
        assert!(ui::host_worker::acknowledge_document(&cache, &pending, &after, true).unwrap());
        assert!(cache.pending_writes().is_empty());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn long_durable_paths_use_private_independent_ephemeral_broker_sockets() {
        use std::os::unix::{fs::PermissionsExt, net::UnixStream};

        let root = tempfile::tempdir().unwrap();
        let state_parent = root.path().join("long-durable-workspace-parent-".repeat(5));
        std::fs::create_dir(&state_parent).unwrap();
        let start = |name: &str| {
            let owner = SessionOwner::initialize_workspace(&state_parent.join(name), name).unwrap();
            let authority = test_authority(&owner);
            let (handle, commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
            let (signing, signatures) = HostSigning::new(handle);
            let (sender, network) = tokio::sync::mpsc::channel(64);
            let host = ActionHost::start(&owner, authority, signing, sender).unwrap();
            (owner, host, commits, signatures, network)
        };
        let first = start("First");
        let second = start("Second");
        let socket = first.1.broker.capnp_socket().to_path_buf();
        let second_socket = second.1.broker.capnp_socket().to_path_buf();
        let directory = socket.parent().unwrap().to_path_buf();
        assert_ne!(socket, second_socket);
        assert!(!socket.starts_with(&state_parent));
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        UnixStream::connect(&socket).unwrap();
        UnixStream::connect(&second_socket).unwrap();
        let journal = first.0.root().join("actions/journal");
        assert!(
            journal.is_dir(),
            "the action journal stays in durable state"
        );
        drop(first);
        assert!(
            !directory.exists(),
            "dropping the host removes its temporary socket directory"
        );
        assert!(
            journal.is_dir(),
            "socket cleanup must retain the durable journal"
        );
        UnixStream::connect(&second_socket).unwrap();
        let second_directory = second_socket.parent().unwrap().to_path_buf();
        drop(second);
        assert!(!second_directory.exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn desktop_service_readiness_does_not_wait_for_document_workers() {
        let (_root, _owner, runtime) = runtime();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let desktop = loop {
            if let Some(surface) = runtime
                .broker
                .published_action_surfaces()
                .into_iter()
                .find(|surface| surface.app_id == qos_peer_state::desktop_command::APP_ID)
            {
                break surface;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        let room = desktop.route.cluster_id.clone();
        let published = runtime.broker.has_live_published_action(
            qos_peer_state::desktop_command::APP_ID,
            &room,
            "",
            qos_peer_state::desktop_command::RESOURCE,
            qos_peer_state::desktop_command::ACTION,
        );
        assert!(
            desktop_service_ready(true, published),
            "unrelated unfinished workers cannot block desktop commands"
        );
        assert!(
            !desktop_service_ready(true, false),
            "the native desktop service is mandatory"
        );
        assert!(
            !desktop_service_ready(false, true),
            "unhealthy durable signing must fail closed"
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn idle_pumps_reuse_projection_and_committed_generations_invalidate_it() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let authority = test_authority(&owner);
        let (handle, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, _signatures) = HostSigning::new(handle);
        let (sender, _network) = tokio::sync::mpsc::channel(64);
        let mut runtime = ActionHost::start(&owner, authority, signing, sender).unwrap();
        let projections = PROJECTION_CALLS.get();
        for _ in 0..5 {
            runtime.pump(&owner).unwrap();
        }
        assert_eq!(
            PROJECTION_CALLS.get(),
            projections,
            "idle pumps must keep collaboration running without reprojecting room history"
        );
        owner.refresh_checkpoint().unwrap();
        runtime.pump(&owner).unwrap();
        assert_eq!(
            PROJECTION_CALLS.get(),
            projections + 1,
            "one fresh projection serves reconciliation and worker permissions"
        );
        runtime.pump(&owner).unwrap();
        assert_eq!(PROJECTION_CALLS.get(), projections + 1);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn changed_generation_admits_new_workers_and_failed_setup_is_not_cached() {
        use qos_peer_state::{
            desktop_command::{BoundCommand, CommandSource, DesktopCommand, TabPlacement},
            owner_commit::{CommitOperation, OwnerCommitRequest},
        };
        for fail_setup in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
            let authority = test_authority(&owner);
            let (handle, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
            let (signing, _signatures) = HostSigning::new(handle);
            let (sender, _network) = tokio::sync::mpsc::channel(64);
            let mut runtime = ActionHost::start(&owner, authority, signing, sender).unwrap();
            let initial_workers = runtime.workers.len();
            let strip = project(owner.peer()).unwrap().workspaces[0].strips[0].id.0;
            owner
                .commit_desktop_command(&OwnerCommitRequest {
                    room: owner.peer().room().into(),
                    operation: CommitOperation::DesktopCommand {
                        invocation: uuid::Uuid::new_v4(),
                        caller: owner.identity.native_cert.clone(),
                        command: BoundCommand {
                            source: CommandSource {
                                room: owner.peer().room().into(),
                                heads: owner
                                    .peer()
                                    .admitted_room()
                                    .unwrap()
                                    .clone()
                                    .get_heads()
                                    .iter()
                                    .map(|head| head.0)
                                    .collect(),
                            },
                            command: DesktopCommand::OpenTab {
                                app: "dev.qos.notes".into(),
                                placement: TabPlacement::NewGroup { strip, after: None },
                            },
                        },
                    },
                })
                .unwrap();
            let row = owner
                .peer()
                .rows()
                .into_iter()
                .find(|row| {
                    !runtime.workers.iter().any(|worker| {
                        worker.app_id == row.app_id && worker.record_id == row.record_id
                    })
                })
                .unwrap();
            let instance = u64::from_be_bytes(
                blake3::hash(
                    format!("{}/{}/{}", owner.peer().room(), row.app_id, row.record_id).as_bytes(),
                )
                .as_bytes()[..8]
                    .try_into()
                    .unwrap(),
            );
            let invalid_view = owner
                .root()
                .join("actions/views")
                .join(format!("{instance}.bin"));
            if fail_setup {
                std::fs::create_dir_all(&invalid_view).unwrap();
                assert!(
                    runtime
                        .reconcile(&owner)
                        .unwrap_err()
                        .to_string()
                        .contains("regular file")
                );
                assert_eq!(runtime.workers.len(), initial_workers);
                let projections = PROJECTION_CALLS.get();
                // Failed setup retains its hydrated custody. A second attempt must
                // still fail, never report a falsely cached successful generation.
                assert!(runtime.reconcile(&owner).is_err());
                assert_eq!(PROJECTION_CALLS.get(), projections + 1);
                continue;
            }
            runtime.reconcile(&owner).unwrap();
            assert!(
                runtime
                    .workers
                    .iter()
                    .any(|worker| worker.app_id == row.app_id && worker.record_id == row.record_id)
            );
            assert_eq!(runtime.workers.len(), initial_workers + 1);
            let projections = PROJECTION_CALLS.get();
            runtime.reconcile(&owner).unwrap();
            assert_eq!(PROJECTION_CALLS.get(), projections);
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn committed_permission_change_refreshes_existing_worker_policy() {
        use collab_types::caps::{CapDecision, CapOverride, ResourceRef, Scope, Subject, Verb};
        use ui::automerge::{ROOT, ReadDoc, transaction::Transactable};
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let authority = test_authority(&owner);
        let (handle, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, _signatures) = HostSigning::new(handle);
        let (sender, _network) = tokio::sync::mpsc::channel(64);
        let mut runtime = ActionHost::start(&owner, authority, signing, sender).unwrap();
        let app = runtime.workers[0].app_id.clone();
        let resource = ResourceRef::App(app.clone());
        assert_eq!(
            runtime.workers[0]
                .runner
                .permissions()
                .borrow()
                .decide(&resource, Verb::Write),
            CapDecision::Allow
        );
        let mut doc = owner.peer().admitted_room().unwrap().clone();
        let before = doc.get_heads();
        let capabilities = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
        let restriction = CapOverride::new(
            Subject::person(&runtime.principal),
            Scope::App(app),
            collab_types::caps::Effect::Restrict,
            Verb::Read,
        );
        doc.put(
            &capabilities,
            "worker-read-only",
            serde_json::to_vec(&restriction).unwrap(),
        )
        .unwrap();
        doc.commit();
        let mut candidate = owner.peer().clone();
        candidate
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(
                    owner.identity.native_cert.device_public_key,
                ),
                NeighbourMsg::SyncResp {
                    room: owner.peer().room().into(),
                    changes: doc
                        .get_changes(&before)
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                },
                web_time::SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
        runtime.pump(&owner).unwrap();
        assert_eq!(
            runtime.workers[0]
                .runner
                .permissions()
                .borrow()
                .decide(&resource, Verb::Write),
            CapDecision::DeniedByRestriction
        );
    }
    #[test]
    fn cached_policy_recomputes_capability_and_membership_expiry() {
        use collab_types::caps::{CapDecision, CapOverride, ResourceRef, Scope, Subject, Verb};
        let policy = WorkerPolicy {
            room: "room".into(),
            owner: Some("owner".into()),
            capabilities: vec![
                CapOverride::grant(Subject::person("member"), Scope::Workspace, Verb::Write)
                    .with_expiry(100),
            ],
        };
        let before = policy.context_at("member", 99);
        assert_eq!(before.role(), Some(collab_types::Role::Editor));
        assert_eq!(
            before.decide(&ResourceRef::Workspace, Verb::Write),
            CapDecision::Allow
        );
        let after = policy.context_at("member", 100);
        assert_eq!(after.role(), None);
        assert_eq!(
            after.decide(&ResourceRef::Workspace, Verb::Write),
            CapDecision::DeniedByRole
        );
    }
    fn runtime() -> (tempfile::TempDir, SessionOwner, ActionHost) {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let authority = test_authority(&owner);
        let (handle, commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, signatures) = HostSigning::new(handle);
        // These tests exercise close custody without pumping asynchronous actions.
        let (sender, network) = tokio::sync::mpsc::channel(64);
        let runtime = ActionHost::start(&owner, authority, signing, sender).unwrap();
        drop((commits, signatures, network));
        (root, owner, runtime)
    }
    #[tokio::test(flavor = "current_thread")]
    async fn committed_fanout_only_delivers_new_publication_and_resets_with_runtime() {
        let (root, mut owner, runtime) = runtime();
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink = delivered.clone();
        let _subscription = runtime.broker.subscribe_rooms(
            vec![owner.peer().room().into()],
            Box::new(move |event, route| {
                if let ui::collaboration::CollabEvent::Document { envelope, .. } = event {
                    sink.lock()
                        .unwrap()
                        .push((route.app_id.clone(), envelope.seq));
                }
            }),
        );
        runtime.committed(&owner).unwrap();
        let baseline = delivered.lock().unwrap().clone();
        assert_eq!(baseline.len(), owner.peer().rows().len());
        runtime.committed(&owner).unwrap();
        assert_eq!(
            *delivered.lock().unwrap(),
            baseline,
            "a repeated committed notification must not fan out history again"
        );
        let mut candidate = owner.peer().clone();
        let app = "dev.qos.word-processor";
        let record = "tab:0000000000000001";
        let mut doc =
            ui::automerge::AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
        use ui::automerge::transaction::Transactable;
        let before = doc.get_heads();
        doc.put(ui::automerge::ROOT, "selective_fanout", 1).unwrap();
        doc.commit();
        candidate
            .publish_incremental(
                app,
                record,
                doc.save_after(&before),
                &owner.identity.native,
                web_time::SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
        runtime.committed(&owner).unwrap();
        let sent = delivered.lock().unwrap().clone();
        assert_eq!(sent.len(), baseline.len() + 1);
        assert_eq!(sent.last().unwrap().0, app);
        owner.refresh_checkpoint().unwrap();
        runtime.committed(&owner).unwrap();
        assert_eq!(
            delivered.lock().unwrap().len(),
            sent.len() + owner.peer().rows().len(),
            "compaction must deliver its complete replacement basis"
        );
        drop(runtime);
        // Broker shutdown is asynchronous. Restore the same canonical aggregate
        // into fresh custody to exercise a new runtime deterministically, without
        // racing the previous invocation-journal writer's teardown.
        let export = root.path().join("export.bin");
        owner.export(&export).unwrap();
        // Supply the original canonical identity paths: secure key loading rejects
        // symlinked ancestors, including the system temporary-directory alias.
        let identity = owner.root().join("identity").canonicalize().unwrap();
        let host_identity = owner.root().join("host-identity").canonicalize().unwrap();
        drop(owner);
        let owner = SessionOwner::import(
            &root.path().join("restored"),
            &export,
            &identity,
            &host_identity,
        )
        .unwrap();
        let authority = test_authority(&owner);
        let (handle, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, _signatures) = HostSigning::new(handle);
        let (sender, _network) = tokio::sync::mpsc::channel(64);
        let before_restart = PROJECTION_CALLS.get();
        let mut restarted = ActionHost::start(&owner, authority, signing, sender).unwrap();
        assert!(
            PROJECTION_CALLS.get() > before_restart,
            "new runtime must reconcile even the same persisted generation"
        );
        assert_eq!(restarted.workers.len(), owner.peer().rows().len());
        let after_restart = PROJECTION_CALLS.get();
        restarted.reconcile(&owner).unwrap();
        assert_eq!(PROJECTION_CALLS.get(), after_restart);
        let fresh = Arc::new(Mutex::new(Vec::new()));
        let sink = fresh.clone();
        let _subscription = restarted.broker.subscribe_rooms(
            vec![owner.peer().room().into()],
            Box::new(move |event, _| {
                if let ui::collaboration::CollabEvent::Document { envelope, .. } = event {
                    sink.lock().unwrap().push(envelope.seq);
                }
            }),
        );
        restarted.committed(&owner).unwrap();
        assert_eq!(fresh.lock().unwrap().len(), owner.peer().rows().len());
    }
    fn saved_report(runtime: &mut ActionHost) -> ui::application::ComponentCloseReport {
        runtime.workers[0].runner.app_mut().close_components();
        runtime.workers[0]
            .runner
            .app_mut()
            .take_component_close_reports()
            .into_iter()
            .find(|report| report.recovery.is_some())
            .unwrap()
    }
    #[tokio::test(flavor = "current_thread")]
    async fn capture_without_retry_and_teardown_failures_preserve_reports() {
        for capture in [true, false] {
            let (_root, owner, mut runtime) = runtime();
            let mut report = saved_report(&mut runtime);
            let saved = serde_json::to_value(&report.recovery).unwrap();
            report.record_error = None;
            report.record_retry = None;
            report.view_error = capture.then(|| "capture failed with fallback".into());
            report.view_retry = None;
            report.teardown_error = (!capture).then(|| "teardown failed".into());
            runtime.recovery.push(report);
            let failure = runtime
                .close(&owner)
                .expect_err("unresolved lifecycle error was discarded");
            assert!(failure.contains("custody preserved"), "{failure}");
            let artifact: serde_json::Value = serde_json::from_slice(
                &std::fs::read(owner.root().join("actions/recovery.json")).unwrap(),
            )
            .unwrap();
            assert!(
                artifact["reports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|report| report["view"] == saved
                        && if capture {
                            report["view_error"] == "capture failed with fallback"
                        } else {
                            report["teardown_error"] == "teardown failed"
                        })
            );
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn structured_setup_failure_preserves_partial_host_and_exact_custody() {
        use ui::automerge::transaction::Transactable;
        let (_root, owner, mut runtime) = runtime();
        let records = runtime
            .owners
            .open("dev.qos.word-processor", owner.peer().room())
            .unwrap();
        let record = RecordId::for_tab(1);
        records
            .with(&record, |doc| {
                doc.put(ui::automerge::ROOT, "setup-dirty", "exact bytes")
                    .unwrap();
                doc.commit();
            })
            .unwrap();
        let dirty = records.read(&record, |doc| doc.clone().save()).unwrap();
        let mut report = saved_report(&mut runtime);
        let saved = serde_json::to_value(&report.recovery).unwrap();
        report.view_error = Some("setup capture failed".into());
        report.view_retry = None;
        let error = runtime.setup_error(ui::launch::LaunchError::ComponentSetup {
            source: Box::new(ui::launch::LaunchError::Operation("mount failed".into())),
            record: RecordCheckout::new(record, word_processor::definition().record_kind.clone()),
            records,
            reports: vec![report],
        });
        assert!(
            !runtime.recovery.is_empty(),
            "structured setup custody was discarded"
        );
        assert!(runtime.finish_start(&owner, Err(error)).is_err());
        let artifact: serde_json::Value = serde_json::from_slice(
            &std::fs::read(owner.root().join("actions/recovery.json")).unwrap(),
        )
        .unwrap();
        assert!(
            artifact["reports"]
                .as_array()
                .unwrap()
                .iter()
                .any(|report| report["view"] == saved
                    && report["view_error"] == "setup capture failed")
        );
        assert!(
            artifact["pending"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["bytes"] == serde_json::json!(dirty)
                    && entry["actor"]
                        == serde_json::json!(owner.identity.native_cert.device_public_key))
        );
        let authority = test_authority(&owner);
        let (handle, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, _signatures) = HostSigning::new(handle);
        let (sender, _network) = tokio::sync::mpsc::channel(64);
        assert!(
            ActionHost::start(&owner, authority, signing, sender)
                .err()
                .unwrap()
                .to_string()
                .contains("unresolved application recovery")
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn actual_worker_lost_authority_after_precommit_preserves_uncertain_effect() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let authority = test_authority(&owner);
        let (handle, mut commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, mut signatures) = HostSigning::new(handle);
        let (sender, mut network) = tokio::sync::mpsc::channel(64);
        let mut runtime =
            ActionHost::start(&owner, authority.clone(), signing.clone(), sender).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let room = owner.peer().room().to_owned();
        let expected_surface =
            |surface: &qos_action_broker::collab_broker_capnp::PublishedSurface| {
                surface.live
                    && surface.app_id == "dev.qos.word-processor"
                    && surface.route.app_id == "dev.qos.word-processor"
                    && surface.route.record_id == "tab:0000000000000001"
                    && surface.route.cluster_id == room
            };
        while !runtime.ready()
            || !runtime
                .broker
                .published_action_surfaces()
                .iter()
                .filter(|surface| expected_surface(surface))
                .any(|surface| {
                    surface
                        .actions
                        .iter()
                        .any(|action| action.action == "insert-table")
                })
        {
            runtime.pump(&owner).unwrap();
            tokio::select! {
                Some(pending) = signatures.recv() => {
                    let guard = authority.committed.begin(&owner).unwrap();
                    let (complete, result) = pending.apply(&mut owner); result.unwrap();
                    guard.publish(&owner).unwrap(); runtime.committed(&owner).unwrap(); complete();
                },
                Some(pending) = commits.recv() => {
                    let guard = authority.committed.begin(&owner).unwrap(); let complete = pending.apply_deferred(&mut owner); guard.publish(&owner).unwrap(); complete().unwrap();
                },
                _ = network.recv() => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {},
            }
            assert!(tokio::time::Instant::now() < deadline);
        }
        let before = owner
            .peer()
            .document("dev.qos.word-processor", "tab:0000000000000001")
            .unwrap();
        let (surface, action) = runtime
            .broker
            .published_action_surfaces()
            .into_iter()
            .filter(|surface| expected_surface(surface))
            .find_map(|surface| {
                surface
                    .actions
                    .iter()
                    .find(|action| action.action == "insert-table")
                    .cloned()
                    .map(|action| (surface, action))
            })
            .unwrap();
        let broker = runtime.broker.app_action_broker().unwrap();
        let job = broker
            .start(
                surface.peer_id,
                &action.resource,
                &action.action,
                None,
                &runtime.principal,
            )
            .unwrap();
        let mut retired = false;
        loop {
            let health = runtime.pump(&owner);
            if health.is_err() {
                assert!(retired);
                break;
            }
            tokio::select! {
                Some(pending) = signatures.recv() => {
                    let guard = authority.committed.begin(&owner).unwrap();
                    let (complete, result) = pending.apply(&mut owner); result.unwrap();
                    runtime.committed(&owner).unwrap();
                    // The exact owner barrier already happened. Retire current
                    // authority before the SDK can send its broker document RPC.
                    drop(guard); retired = true; complete();
                },
                Some(pending) = commits.recv() => {
                    let guard = authority.committed.begin(&owner).unwrap(); let complete = pending.apply_deferred(&mut owner); guard.publish(&owner).unwrap(); complete().unwrap();
                },
                _ = network.recv() => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {},
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "worker did not retire after lost authority"
            );
        }
        assert_ne!(
            owner
                .peer()
                .document("dev.qos.word-processor", "tab:0000000000000001")
                .unwrap(),
            before
        );
        assert!(!runtime.ready());
        assert!(!matches!(
            broker.status(job.id),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(broker.peer_action_attachment().is_err());
        let committed = owner.status();
        runtime.close(&owner).unwrap();
        drop(runtime);
        drop(owner);
        assert_eq!(
            SessionOwner::open(&root.path().join("owner"))
                .unwrap()
                .status(),
            committed
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn failed_worker_close_preserves_dirty_custody_and_blocks_automatic_restart() {
        use ui::automerge::transaction::Transactable;
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let authority = test_authority(&owner);
        let (handle, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, _signatures) = HostSigning::new(handle);
        let (sender, _network) = tokio::sync::mpsc::channel(64);
        let mut runtime =
            ActionHost::start(&owner, authority.clone(), signing.clone(), sender.clone()).unwrap();
        let records = runtime
            .owners
            .open("dev.qos.word-processor", owner.peer().room())
            .unwrap();
        let record = RecordId::for_tab(1);
        records
            .with(&record, |document| {
                document
                    .put(ui::automerge::ROOT, "unsent-close-recovery", "preserve me")
                    .unwrap();
                document.commit();
            })
            .unwrap();
        let dirty = records
            .read(&record, |document| document.clone().save())
            .unwrap();
        let prior = owner.status();
        let failure = runtime.close(&owner).unwrap_err();
        assert!(failure.contains("custody preserved"), "{failure}");
        let recovery: serde_json::Value = serde_json::from_slice(
            &std::fs::read(owner.root().join("actions/recovery.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(recovery["room"], owner.peer().room());
        assert_eq!(recovery["outcome"], "unknown");
        let entry = recovery["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"]["app"] == "dev.qos.word-processor")
            .unwrap();
        assert_eq!(
            serde_json::from_value::<Vec<u8>>(entry["bytes"].clone()).unwrap(),
            dirty
        );
        assert_eq!(
            serde_json::from_value::<Vec<u8>>(entry["actor"].clone()).unwrap(),
            owner.identity.native_cert.device_public_key
        );
        assert_eq!(
            owner.status(),
            prior,
            "inert recovery must not mutate canonical admission"
        );
        drop(runtime);
        assert!(
            ActionHost::start(&owner, authority, signing, sender)
                .err()
                .unwrap()
                .to_string()
                .contains("unresolved application recovery")
        );
    }
}

#[cfg(all(test, feature = "test-support"))]
pub(crate) mod scoped_fixture;

#[cfg(test)]
#[path = "action_host/production_authorizer_tests.rs"]
mod production_authorizer_tests;
