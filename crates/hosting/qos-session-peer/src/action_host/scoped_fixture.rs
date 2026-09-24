//! Actual owner, private RPC and mounted apps for scoped-effect regressions.
//! Final native document publication remains closed until its bounded witness exists.
use super::*;
use crate::host_signing::SignatureMailbox;
use crate::owner_commit::{OwnerCommitMailbox, PendingOwnerCommit};
use ui::collaboration::{CollabEvent, CollabRequest};

mod deferred;
mod dispatch_spec;
mod notes;
pub(super) fn test_definition(
    app: &str,
) -> Option<&'static ui::application::ApplicationDefinition> {
    deferred::definition(app).or_else(|| dispatch_spec::definition(app))
}
pub(super) fn observe_authorizer(
    app: &str,
    actual: ui::invocation_scope::PublishedInvocationAuthorizer,
) -> ui::invocation_scope::PublishedInvocationAuthorizer {
    deferred::observe_authorizer(app, dispatch_spec::observe_authorizer(app, actual))
}
mod reopened;
mod writer;
pub(crate) use reopened::ReopenedAppFixture;

thread_local! {
    static LOCAL_FACTORY_EDIT: RefCell<Option<String>> = const { RefCell::new(None) };
}
// Negative fixture only: keep the genuine prepared RecordStore/real factory,
// but introduce local bytes never committed by SessionOwner. This supplies no
// scope, receipt, signature or canonical authority.
pub(super) fn edit_factory_input(app: &str, records: &ui::record::RecordStore, record: &str) {
    if !LOCAL_FACTORY_EDIT.with(|value| value.borrow().as_deref() == Some(app)) {
        return;
    }
    let id = if record.is_empty() {
        RecordId::package_primary()
    } else {
        RecordId::parse(record).unwrap()
    };
    records
        .with(&id, |document| {
            use ui::automerge::transaction::Transactable;
            document
                .put(
                    ui::automerge::ROOT,
                    "uncommitted_factory_probe",
                    "local change",
                )
                .unwrap();
            document.commit();
        })
        .expect("actual prepared factory record must exist");
}

pub(crate) struct ScopedAppFixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) owner: SessionOwner,
    pub(crate) host: ActionHost,
    pub(crate) app: String,
    pub(crate) record: String,
    caller: CollaborationManager,
    expire: Box<dyn Fn()>,
    signatures: SignatureMailbox,
    commits: OwnerCommitMailbox,
    network: tokio::sync::mpsc::Receiver<NeighbourMsg>,
    request: u64,
}
impl ScopedAppFixture {
    pub(crate) fn with_uncommitted_factory_data(app: &str) -> Self {
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                LOCAL_FACTORY_EDIT.with(|value| *value.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(LOCAL_FACTORY_EDIT.with(|value| value.replace(Some(app.into()))));
        Self::new(app)
    }
    pub(crate) fn new(app: &str) -> Self {
        Self::new_seeded(app, None, false)
    }
    /// Select the V5 delivery writer before the host starts, as the installed
    /// HTTPS data routes do, so browser-style state items can be admitted.
    pub(crate) fn with_delivery(app: &str) -> Self {
        Self::new_seeded(app, None, true)
    }
    /// The seed is signed and committed by the actual owner before any factory
    /// can observe it. It is never installed directly in the SDK RecordStore.
    pub(crate) fn with_record_seed(
        app: &str,
        seed: impl FnOnce(&mut ui::automerge::AutoCommit) + 'static,
    ) -> Self {
        Self::new_seeded(app, Some(Box::new(seed)), false)
    }
    fn new_seeded(
        app: &str,
        seed: Option<Box<dyn FnOnce(&mut ui::automerge::AutoCommit)>>,
        delivery: bool,
    ) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("qos_action_broker=debug,tilo_ui=debug")
            .with_test_writer()
            .try_init();
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        if !owner.peer().rows().iter().any(|row| row.app_id == app) {
            use qos_peer_state::{
                desktop_command::{BoundCommand, CommandSource, DesktopCommand, TabPlacement},
                owner_commit::{CommitOperation, OwnerCommitRequest},
            };
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
                                    .map(|h| h.0)
                                    .collect(),
                            },
                            command: DesktopCommand::OpenTab {
                                app: app.into(),
                                placement: TabPlacement::NewGroup { strip, after: None },
                            },
                        },
                    },
                })
                .unwrap();
        }
        let record = owner
            .peer()
            .rows()
            .into_iter()
            .find(|row| row.app_id == app)
            .unwrap()
            .record_id;
        if let Some(seed) = seed {
            crate::owner::mutate_owned_record_for_test(&mut owner, app, &record, seed).unwrap();
        }
        let now = tilo_relay::clock::now_ns();
        let (certificate, key) = admit_caller(&mut owner, now);
        if delivery {
            owner.activate_delivery().unwrap();
        }
        let (clock, control) = OwnerPolicyClock::for_test(now);
        let authority = Authority::new(CommittedAuthority::new_with_test_clock(&owner, &clock));
        authority.set_ready(true);
        let (handle, commits) = OwnerCommitMailbox::bounded(8);
        let (signing, signatures) = HostSigning::new(handle);
        let (sender, network) = tokio::sync::mpsc::channel(64);
        let host = ActionHost::start_inner(&owner, authority, signing, sender, None, true).unwrap();
        let room: uuid::Uuid = owner.peer().room().parse().unwrap();
        let route = CrdtRoute::record(app, room.to_string(), &record);
        let stream = host
            .broker
            .connect_local_worker(route, &certificate)
            .unwrap();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_string(),
            device_certificate: serde_json::to_vec(&certificate).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &room.to_string(),
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        let caller = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            app,
            room,
            (!record.is_empty()).then(|| record.clone()),
            None,
            CertifiedEndpointIdentity::new(presentation, key),
        )
        .unwrap();
        Self {
            root,
            owner,
            host,
            app: app.into(),
            record,
            caller,
            signatures,
            commits,
            network,
            request: 0,
            expire: Box::new(move || {
                control.set_wall_ns(now.saturating_sub(1_000_000_000));
                control.advance_monotonic(std::time::Duration::from_secs(120));
            }),
        }
    }
    pub(crate) fn expire_original_caller(&self) {
        (self.expire)();
    }
    pub(crate) fn retire_policy_origin(&self) {
        self.host.authority.committed.invalidate();
    }
    pub(crate) fn signing(&self) -> HostSigning {
        self.host.signing.clone()
    }
    pub(crate) async fn next_startup_signature(&mut self) -> crate::host_signing::PendingSignature {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            self.host.pump(&self.owner).unwrap();
            if let Ok(pending) = self.signatures.try_recv() {
                return pending;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "real mounted app did not enqueue scoped startup"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
    pub(crate) async fn next_signature_for_selected_record(
        &mut self,
    ) -> crate::host_signing::PendingSignature {
        loop {
            let pending = self.next_startup_signature().await;
            if pending.matches_route(&self.app, &self.record) {
                return pending;
            }
            let update = self.host.authority.committed.begin(&self.owner).unwrap();
            let (complete, result) = pending.apply(&mut self.owner);
            update.publish(&self.owner).unwrap();
            complete();
            result.unwrap();
        }
    }
    pub(crate) fn invocation_status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
        self.host
            .broker
            .app_action_broker()
            .and_then(|broker| broker.status(id))
    }
    /// Drives genuine UI/model continuations and existing owner queues only.
    pub(crate) async fn pump(&mut self) -> Result<(), String> {
        self.host.pump(&self.owner)?;
        while let Ok(pending) = self.signatures.try_recv() {
            let update = self
                .host
                .authority
                .committed
                .begin(&self.owner)
                .map_err(|error| error.to_string())?;
            let (complete, result) = pending.apply(&mut self.owner);
            match result {
                Ok(()) => {
                    update
                        .publish(&self.owner)
                        .map_err(|error| error.to_string())?;
                    complete();
                }
                Err(error) => {
                    drop(update);
                    complete();
                    return Err(error);
                }
            }
        }
        while let Ok(pending) = self.commits.try_recv() {
            self.commit(pending)?;
        }
        while self.network.try_recv().is_ok() {}
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Ok(())
    }
    fn commit(&mut self, pending: PendingOwnerCommit) -> Result<(), String> {
        let update = self
            .host
            .authority
            .committed
            .begin(&self.owner)
            .map_err(|error| error.to_string())?;
        let complete = pending.apply_deferred(&mut self.owner);
        update
            .publish(&self.owner)
            .map_err(|error| error.to_string())?;
        complete()?;
        Ok(())
    }
    pub(crate) async fn wait_action(&mut self, action: &str) -> (u64, String) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(found) = self
                .host
                .broker
                .published_action_surfaces()
                .iter()
                .filter(|surface| {
                    surface.live
                        && surface.app_id == self.app
                        && surface.route.record_id == self.record
                })
                .find_map(|surface| {
                    surface
                        .actions
                        .iter()
                        .find(|entry| entry.action == action)
                        .map(|entry| (surface.peer_id, entry.resource.clone()))
                })
            {
                return found;
            }
            self.pump().await.unwrap();
            assert!(
                tokio::time::Instant::now() < deadline,
                "mounted app never published {action}; actual surfaces: {:?}",
                self.host
                    .broker
                    .published_action_surfaces()
                    .iter()
                    .map(|surface| (
                        &surface.app_id,
                        &surface.route.record_id,
                        surface.live,
                        surface
                            .actions
                            .iter()
                            .map(|a| (&a.resource, &a.action))
                            .collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
    pub(crate) async fn invoke(
        &mut self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> u64 {
        self.invoke_with_document_turns(action, arguments, serde_json::json!({}), true)
            .await
    }
    pub(crate) async fn invoke_confirmed(
        &mut self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> u64 {
        self.invoke_with_document_turns(
            action,
            arguments,
            serde_json::json!({"confirm":true}),
            true,
        )
        .await
    }
    pub(crate) async fn invoke_holding_documents(
        &mut self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> u64 {
        self.invoke_with_document_turns(action, arguments, serde_json::json!({}), false)
            .await
    }
    async fn invoke_with_document_turns(
        &mut self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        confirmation: serde_json::Value,
        apply_documents: bool,
    ) -> u64 {
        let (peer, resource) = self.wait_action(action).await;
        self.request += 1;
        assert!(self.caller.send(CollabRequest::InvokePublishedAction {
            request_id: self.request,
            peer_id: peer,
            resource,
            action: action.into(),
            arguments,
            confirmation
        }));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for event in self.caller.poll() {
                if let CollabEvent::PublishedInvocationResult { request_id, result } = event {
                    if request_id == self.request {
                        return result.expect("genuine original caller RPC admission").0;
                    }
                }
            }
            if apply_documents {
                self.pump().await.unwrap();
            } else {
                self.host.pump(&self.owner).unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "original caller RPC did not admit"
            );
        }
    }
    pub(crate) async fn query_directory(
        &mut self,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<runtime_collab::ActionDirectoryPage, String> {
        self.request += 1;
        let request_id = self.request;
        if !self
            .caller
            .send(CollabRequest::QueryActionDirectory { request_id, query })
        {
            return Err("actual caller queue refused directory query".into());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for event in self.caller.poll() {
                if let CollabEvent::ActionDirectoryPage {
                    request_id: id,
                    result,
                } = event
                {
                    if id == request_id {
                        return result;
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("actual directory query timed out".into());
            }
            self.pump().await?;
        }
    }
    pub(crate) async fn resolve_directory(
        &mut self,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<runtime_collab::ActionDirectoryResolution, String> {
        self.request += 1;
        let request_id = self.request;
        if !self.caller.send(CollabRequest::ResolvePublishedAction {
            request_id,
            peer_id,
            revision: revision.into(),
            resource: resource.into(),
            action: action.into(),
        }) {
            return Err("actual caller queue refused directory resolution".into());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for event in self.caller.poll() {
                if let CollabEvent::PublishedActionResolution {
                    request_id: id,
                    result,
                } = event
                {
                    if id == request_id {
                        return result;
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("actual directory resolution timed out".into());
            }
            self.pump().await?;
        }
    }
    pub(crate) async fn invoke_resolved(
        &mut self,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, String> {
        self.request += 1;
        let request_id = self.request;
        if !self
            .caller
            .send(CollabRequest::InvokeResolvedPublishedAction {
                request_id,
                peer_id,
                revision: revision.into(),
                resource: resource.into(),
                action: action.into(),
                arguments,
                confirmation: serde_json::json!({}),
            })
        {
            return Err("actual caller queue refused resolved invocation".into());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for event in self.caller.poll() {
                if let CollabEvent::PublishedInvocationResult {
                    request_id: id,
                    result,
                } = event
                {
                    if id == request_id {
                        return result.map(|(id, _)| id);
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("actual resolved invocation timed out".into());
            }
            self.pump().await?;
        }
    }
    /// Browser-style state upload: the admitted editor's signed incremental
    /// enters the native delivery admission the HTTPS data route uses, then the
    /// committed publication fans out to the mounted workers.
    pub(crate) fn upload_remote_edit(&mut self, delivery: u8, field: &str, value: &str) {
        let (field, value) = (field.to_owned(), value.to_owned());
        self.upload_remote_change(delivery, move |document| {
            use ui::automerge::transaction::Transactable;
            document
                .put(ui::automerge::ROOT, field.as_str(), value.as_str())
                .unwrap();
        });
    }
    /// Same browser-style upload for an arbitrary application-level change.
    pub(crate) fn upload_remote_change(
        &mut self,
        delivery: u8,
        change: impl FnOnce(&mut ui::automerge::AutoCommit),
    ) {
        use crate::owner::delivery::{AdmissionOutcome, CompleteStateItem};
        let key = ed25519_dalek::SigningKey::from_bytes(&[104; 32]);
        let bytes = self.owner.peer().document(&self.app, &self.record).unwrap();
        let mut document = ui::automerge::AutoCommit::load(&bytes).unwrap();
        let heads = document.get_heads();
        change(&mut document);
        document.commit();
        let now = web_time::SystemTime::now();
        let message = self
            .owner
            .peer()
            .clone()
            .publish_incremental(
                &self.app,
                &self.record,
                document.save_after(&heads),
                &key,
                now,
            )
            .unwrap();
        let canonical = postcard::to_stdvec(&message).unwrap();
        let update = self.host.authority.committed.begin(&self.owner).unwrap();
        let outcome = self
            .owner
            .receive_delivery(
                qos_peer_state::TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                CompleteStateItem {
                    delivery_id: [delivery; 16],
                    canonical: &canonical,
                },
                now,
            )
            .unwrap();
        update.publish(&self.owner).unwrap();
        assert!(
            matches!(outcome, AdmissionOutcome::Durable(_)),
            "browser-style upload not durable: {outcome:?}"
        );
        self.host.committed(&self.owner).unwrap();
    }
    pub(crate) fn pending_worker_writes(&self) -> usize {
        self.host.cache.pending_writes().len()
    }
    pub(crate) fn records(&mut self) -> ui::record::RecordStore {
        self.host
            .owners
            .open(&self.app, self.owner.peer().room())
            .unwrap()
    }
    pub(crate) fn record_id(&self) -> RecordId {
        if self.record.is_empty() {
            RecordId::package_primary()
        } else {
            RecordId::parse(&self.record).unwrap()
        }
    }
}
fn admit_caller(
    owner: &mut SessionOwner,
    now: u64,
) -> (tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey) {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
    };
    let root = ed25519_dalek::SigningKey::from_bytes(&[103; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[104; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Scoped app caller".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now - 1,
        expires_at_ns: Some(now + 60_000_000_000),
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let service = CollaborationService::new(vec![]);
    let invitation = service
        .issue(
            owner,
            owner.peer().authority().device_public_key,
            IssueInvitationRequest {
                request_id: [103; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some("Scoped caller".into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            owner,
            cert.device_public_key,
            JoinChallengeRequest {
                signed_invitation: invitation.signed_invitation,
                device_cert: cert.clone(),
                request_id: [104; 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    service
        .complete(
            owner,
            cert.device_public_key,
            JoinCompleteRequest {
                proof: sign_join_proof(challenge, &key).unwrap(),
            },
            now + 2,
        )
        .unwrap();
    (cert, key)
}
#[tokio::test(flavor = "current_thread")]
async fn scoped_app_fixture_reaches_real_writer_published_action() {
    let mut fixture = ScopedAppFixture::new("dev.qos.word-processor");
    // Read-only diagnostic: compare actual mounted bytes with retained signed
    // publication bytes. This creates no receipt and authorizes no replay.
    let id = fixture.record_id();
    let snapshot = fixture
        .records()
        .read(&id, |doc| doc.clone().save())
        .unwrap();
    let encoded = collab_types::DocumentMutation::Snapshot(snapshot.clone())
        .encode()
        .unwrap();
    let retained = fixture
        .owner
        .outgoing()
        .iter()
        .filter_map(|publication| {
            let message = postcard::from_bytes::<NeighbourMsg>(publication.bytes).ok()?;
            let NeighbourMsg::AppDocument {
                app_id,
                cluster_id,
                record_id,
                envelope,
                ..
            } = message
            else {
                return None;
            };
            if app_id != fixture.app
                || record_id != fixture.record
                || cluster_id != fixture.owner.peer().room()
            {
                return None;
            }
            Some((
                envelope.op_class,
                envelope.payload == encoded,
                envelope.seq,
                envelope.payload.len(),
            ))
        })
        .collect::<Vec<_>>();
    eprintln!(
        "read-only baseline diagnostic: requested_bytes={} retained={retained:?} durable_receipt={:?}",
        encoded.len(),
        fixture
            .owner
            .peer()
            .durable_coverage(&fixture.app, &fixture.record)
            .unwrap()
    );
    fixture.wait_action("save-as").await;
}
