//! Production ordinary application host; SessionOwner remains the sole durable writer.
use crate::{SessionOwner, host_signing::HostSigning};
use anyhow::{Context, Result};
use ed25519_dalek::Signer;
use qos_action_broker::{
    collab_broker::{BrokerHandle, CrdtRoute},
    collab_outbound::{PeerPublishSink, PeerPublisher},
    peer_actions::CertificateAuthority,
    policy::WorkspacePolicy,
    services::WorkspaceAuthority,
};
use qos_peer_state::{NeighbourMsg, StatePeer, owner_commit::OwnerCommitService};
use std::{
    cell::RefCell,
    os::fd::IntoRawFd,
    rc::Rc,
    sync::{Arc, Mutex},
};
use ui::{
    application::{HostContext, LaunchTarget, RestoreInput},
    collaboration::{CertifiedEndpointIdentity, CollaborationManager, RouteSpec},
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners},
};

#[derive(Clone, Default)]
pub(crate) struct Authority(pub Arc<Mutex<Option<StatePeer>>>);
impl CertificateAuthority for Authority {
    fn authorize(
        &self,
        room: uuid::Uuid,
        cert: &qos_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let guard = self.0.lock().map_err(|_| "owner authority unavailable")?;
        let peer = guard.as_ref().ok_or("owner authority unavailable")?;
        if peer.room() != room.to_string() || !peer.certificates().contains(cert) {
            return Err("certificate not admitted by current owner".into());
        }
        peer.permissions(cert.device_public_key, web_time::SystemTime::now())
            .map(|_| ())
    }
}
impl WorkspaceAuthority for Authority {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        let guard = self.0.lock().map_err(|_| "owner authority unavailable")?;
        project(guard.as_ref().ok_or("owner authority unavailable")?)
    }
}
fn project(peer: &StatePeer) -> Result<qos_desktop::WorkspaceTree, String> {
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
    app: ui::App,
    permissions: Rc<RefCell<ui::permissions::PermissionContext>>,
}
pub(crate) struct ActionHost {
    pub broker: BrokerHandle,
    workers: Vec<Worker>,
    recovery: Vec<ui::application::ComponentCloseReport>,
    signing: HostSigning,
    principal: String,
    cache: PortableStorage,
    owners: RecordOwners,
    views: Rc<ui::application_view::InstanceViewStore>,
}
impl ActionHost {
    pub fn start(
        owner: &SessionOwner,
        authority: Authority,
        signing: HostSigning,
        sender: tokio::sync::mpsc::Sender<NeighbourMsg>,
    ) -> Result<Self> {
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
        let broker = qos_action_broker::collab_broker::spawn_native_peer(
            state.join("broker.sock"),
            Arc::new(authority.clone()),
            publisher,
            qos_action_broker::peer_actions::NativePeerBrokerConfig {
                journal_root: state.join("journal"),
                certificate_authority: Arc::new(authority),
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
            workers: vec![],
            recovery: vec![],
            signing,
            principal,
            cache,
            owners,
            views,
        };
        this.reconcile(owner)?;
        Ok(this)
    }
    fn reconcile(&mut self, owner: &SessionOwner) -> Result<()> {
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
                _ => continue,
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
                .open(definition.app_id, &room.to_string())
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
            let context = HostContext::prepare(
                definition,
                records,
                self.views.clone(),
                hex::encode(cert.device_public_key),
                instance,
                permissions.clone(),
                ui::permissions::UserId(self.principal.clone()),
                qos_actions::ActionContext::default(),
                &target,
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let component = definition
                .instantiate(context, target, restore)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let route = CrdtRoute::record(&row.app_id, room.to_string(), &row.record_id);
            let spec = if row.record_id.is_empty() {
                RouteSpec::app(&row.app_id, room)
            } else {
                RouteSpec::record(&row.app_id, room, &row.record_id)
            };
            let stream = self
                .broker
                .connect_local_worker(route, cert)
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
            let app = ui::App::from_component_with_collaboration(
                component,
                ui::application::ComponentRunnerInputs {
                    theme: ui::Theme::dark(),
                    text_renderer: ui::TextRenderer::new(),
                },
                manager,
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            self.workers.push(Worker {
                app_id: row.app_id,
                record_id: row.record_id,
                app,
                permissions,
            });
        }
        Ok(())
    }

    pub fn pump(&mut self, owner: &SessionOwner) -> Result<(), String> {
        self.signing.health()?;
        self.reconcile(owner)
            .map_err(|e| self.signing.fail(e.to_string()))?;
        let tree = project(owner.peer())?;
        let workspace = &tree.workspaces[0];
        for worker in &mut self.workers {
            *worker.permissions.borrow_mut() =
                ui::permissions::PermissionContext::from_capabilities(
                    workspace.actor_for_person(&self.principal),
                    workspace.capabilities.clone(),
                );
            worker.app.pump_collab_presence();
            if !worker.app.collaboration_is_connected() {
                return Err(self
                    .signing
                    .fail("application collaboration transport failed; outcome unknown"));
            }
        }
        self.acknowledge_committed(owner)?;
        Ok(())
    }
    pub fn ready(&self) -> bool {
        self.signing.health().is_ok()
            && !self.workers.is_empty()
            && self.broker.published_action_surfaces().len() >= self.workers.len()
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
                let mut document =
                    ui::automerge::AutoCommit::load(&committed).map_err(|e| e.to_string())?;
                let mut pending = ui::automerge::AutoCommit::load(&write.value.bytes)
                    .map_err(|e| e.to_string())?;
                let heads = pending.get_heads();
                if heads
                    .iter()
                    .all(|head| document.get_change_by_hash(head).is_some())
                    && owner.permits_publication()
                {
                    self.cache.acknowledge(&write, Ok(()))?;
                }
            }
        }
        Ok(())
    }
    pub fn committed(&self, owner: &SessionOwner) -> Result<(), String> {
        self.acknowledge_committed(owner)?;
        for bytes in owner.outgoing() {
            if let NeighbourMsg::AppDocument {
                app_id,
                cluster_id,
                record_id,
                envelope,
                ..
            } = postcard::from_bytes(bytes).map_err(|e| e.to_string())?
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
        }
        Ok(())
    }
    pub fn close(&mut self, owner: &SessionOwner) -> Result<(), String> {
        for worker in &mut self.workers {
            worker.app.close_components();
            self.recovery
                .extend(worker.app.take_component_close_reports());
        }
        let result: Result<(), String> = (|| {
            self.acknowledge_committed(owner)?;
            for report in &mut self.recovery {
                report.retry_record()?;
                report.retry_view().map_err(|e| e.to_string())?;
            }
            Ok(())
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
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn actual_worker_lost_authority_after_precommit_preserves_uncertain_effect() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let authority = Authority(Arc::new(Mutex::new(Some(owner.peer().clone()))));
        let (handle, mut commits) = crate::owner_commit::OwnerCommitMailbox::bounded(8);
        let (signing, mut signatures) = HostSigning::new(handle);
        let (sender, mut network) = tokio::sync::mpsc::channel(64);
        let mut runtime =
            ActionHost::start(&owner, authority.clone(), signing.clone(), sender).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !runtime.ready() {
            runtime.pump(&owner).unwrap();
            tokio::select! {
                Some(pending) = signatures.recv() => {
                    let mut guard = authority.0.lock().unwrap();
                    let (complete, result) = pending.apply(&mut owner); result.unwrap();
                    *guard = Some(owner.peer().clone()); runtime.committed(&owner).unwrap(); complete();
                },
                Some(pending) = commits.recv() => {
                    let mut guard = authority.0.lock().unwrap(); pending.apply(&mut owner).unwrap(); *guard = Some(owner.peer().clone());
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
                    let mut guard = authority.0.lock().unwrap();
                    let (complete, result) = pending.apply(&mut owner); result.unwrap();
                    runtime.committed(&owner).unwrap();
                    // The exact owner barrier already happened. Retire current
                    // authority before the SDK can send its broker document RPC.
                    *guard = None; retired = true; complete();
                },
                Some(pending) = commits.recv() => {
                    let mut guard = authority.0.lock().unwrap(); pending.apply(&mut owner).unwrap(); *guard = Some(owner.peer().clone());
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
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
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
        let authority = Authority(Arc::new(Mutex::new(Some(owner.peer().clone()))));
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
