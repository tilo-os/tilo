//! Actual signed empty native enrollment, owner publication, and journal recovery.
use super::{
    BrokerChannels, CapnpBrokerState, DurableInvocation, FanoutState, HostServices,
    InvocationJournal, InvocationKey, PeerPublisher, VerifiedActionCaller, principal_id_for_root,
};
use ed25519_dalek::{Signer as _, SigningKey};
use std::{
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tilo_peer_state::owner_commit::{CommitFuture, OwnerCommitRequest, OwnerCommitService};

struct NativeEffects {
    owner: Arc<tilo_joined_room::NativeWorkspaceOwner>,
    key: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    held: Arc<tokio::sync::Semaphore>,
    submitted: Arc<tokio::sync::Notify>,
}
impl OwnerCommitService for NativeEffects {
    fn health(&self) -> Result<(), String> {
        let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        self.owner.with_current(room, |_| Ok(()))
    }
    fn requires_room(&self, room: &str) -> bool {
        room == tilo_joined_room::testing::ROOM
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let owner = self.owner.clone();
        let key = self.key.clone();
        let certificate = self.certificate.clone();
        let held = self.held.clone();
        let submitted = self.submitted.clone();
        Box::pin(async move {
            submitted.notify_one();
            let permit = held
                .acquire()
                .await
                .map_err(|_| "native owner test retired")?;
            let receipt =
                owner.commit_effect_receipt(&request, &certificate, &key, SystemTime::now());
            drop(permit);
            receipt
        })
    }
}
struct NativeAuthority(Arc<tilo_joined_room::NativeWorkspaceOwner>);
impl crate::peer_actions::CertificateAuthority for NativeAuthority {
    fn authorize(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let room = tilo_desktop_model::RoomId::parse(&room.to_string()).ok_or("invalid room")?;
        self.0.with_current(room, |view| {
            if !view.peer().certificates().contains(certificate) {
                return Err("native certificate no longer admitted".into());
            }
            view.peer()
                .permissions(certificate.device_public_key, SystemTime::now())
                .map(|_| ())
        })
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    owner: Arc<tilo_joined_room::NativeWorkspaceOwner>,
    state: Arc<CapnpBrokerState>,
    caller: VerifiedActionCaller,
    membership: crate::test_support::WorkspaceTreeHandle,
    held: Arc<tokio::sync::Semaphore>,
    submitted: Arc<tokio::sync::Notify>,
    _fanout: Arc<FanoutState>,
}
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        Arc::new(tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap());
    let key = SigningKey::from_bytes(&[92; 32]);
    let certificate = tilo_joined_room::testing::certificate(
        &SigningKey::from_bytes(&[91; 32]),
        &key,
        "Governor",
    );
    tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
    let room = tilo_joined_room::testing::ROOM;
    let mut workspace =
        crate::test_support::Workspace::new(crate::test_support::WorkspaceId(1), "Native recovery");
    workspace.room = uuid::Uuid::parse_str(room).unwrap().into();
    workspace.set_owner(principal_id_for_root(certificate.user_public_key));
    let mut tree = crate::test_support::WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let membership = tree.shared();
    let held = Arc::new(tokio::sync::Semaphore::new(1));
    let submitted = Arc::new(tokio::sync::Notify::new());
    let publisher = PeerPublisher::with_optional_owner_commit(
        None,
        Arc::new(NativeEffects {
            owner: owner.clone(),
            key: key.clone(),
            certificate: certificate.clone(),
            held: held.clone(),
            submitted: submitted.clone(),
        }),
    );
    let config = crate::peer_actions::NativePeerBrokerConfig {
        journal_root: directory.path().join("journal"),
        certificate_authority: Arc::new(NativeAuthority(owner.clone())),
    };
    let fanout = Arc::new(FanoutState::new());
    let (state, _) = CapnpBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        publisher,
        Some(membership.authority()),
        None,
        Some(&config),
        HostServices::default(),
    );
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.into(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    let caller = state.verify_action_caller(room, &presentation).unwrap();
    Fixture {
        directory,
        owner,
        state,
        caller,
        membership,
        held,
        submitted,
        _fanout: fanout,
    }
}
fn prepare(f: &Fixture) -> DurableInvocation {
    let key = f
        .state
        .caller_key(&f.caller, tilo_actions::InvocationId::new())
        .unwrap();
    let actor = f.caller.admitted.broker_actor().id;
    let context = tilo_actions::ActionContext::default();
    let contract = tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
        ..Default::default()
    };
    let intent = serde_json::json!({"operation_kind":tilo_actions::OperationKind::Effect,"actor":actor,"context":context,"contract":contract,"app_id":"so.tilo.chat","resource":"app.so.tilo.chat","action":"test-effect"});
    let update = tilo_actions::InvocationUpdate::Succeeded {
        invocation: key.invocation,
        output: tilo_actions::ActionOutput::default(),
    };
    let mut receipt = crate::policy::redacted_effect_receipt(
        key.invocation,
        &actor,
        context,
        tilo_actions::EffectReceiptStatus::Succeeded,
    );
    receipt.completed_at = 100;
    let request = OwnerCommitRequest {
        room: key.room.clone(),
        operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
            invocation: key.invocation.0,
            redacted: serde_json::to_vec(&receipt).unwrap(),
        },
    };
    let mut guard = f.state.journal.lock().unwrap();
    let journal = guard.as_mut().unwrap().as_mut().unwrap();
    journal.admit(key.clone(), intent, 1).unwrap();
    journal.prepare_effect(&key, update, &request).unwrap();
    let record = journal.lookup(&key).unwrap().unwrap();
    drop(guard);
    record
}
fn restart_journal(f: &Fixture, key: &InvocationKey) -> DurableInvocation {
    let previous = f
        .state
        .journal
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .take()
        .unwrap();
    drop(previous);
    *f.state.journal.lock().unwrap() = Ok(Some(
        InvocationJournal::open(&f.directory.path().join("journal")).unwrap(),
    ));
    f.state
        .verified_invocation_status(&f.caller, key.invocation)
        .unwrap()
        .unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn prepared_effect_native_owner_crash_gap_keeps_exact_history() {
    let f = fixture();
    let record = prepare(&f);
    let prepared = record.prepared_effect.as_ref().unwrap().clone();
    let first = f
        .state
        .publisher
        .commit_owner(prepared.request(), None)
        .await
        .unwrap();
    let path = f.directory.path().join("native.json");
    let bytes = std::fs::read(&path).unwrap();
    let record = restart_journal(&f, &record.key);
    assert!(record.uncertain);
    let invocation = record.key.invocation;
    let terminal = f
        .state
        .reconcile_prepared_effect(record, || {
            f.state.verified_invocation_status(&f.caller, invocation)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!terminal.uncertain);
    assert_eq!(terminal.update, prepared.update);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "real owner exact retry is byte-identical"
    );
    let key = terminal.key.clone();
    assert_eq!(restart_journal(&f, &key), terminal);
    let room = tilo_desktop_model::RoomId::parse(&key.room).unwrap();
    f.owner
        .with_current(room, |view| {
            let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(
                view.peer().admitted_room().unwrap(),
            );
            assert_eq!(effects.len(), 1);
            assert_eq!(effects[0].completed_at, 100);
            assert_eq!(view.generation(), first.generation);
            Ok(())
        })
        .unwrap();
    assert!(f.state.pending_invocations.lock().unwrap().is_empty());
    drop(f);
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_effect_native_recovery_rechecks_authority_and_failed_custody() {
    for scenario in ["before", "after", "refused"] {
        let f = fixture();
        let record = prepare(&f);
        let key = record.key.clone();
        let record = restart_journal(&f, &key);
        if scenario == "before" {
            f.membership.write().workspaces[0].set_owner("another person");
        }
        if scenario == "refused" {
            f.held.close();
        }
        let permit = if scenario == "after" {
            Some(f.held.acquire().await.unwrap())
        } else {
            None
        };
        let result = {
            let reconciliation = f.state.reconcile_prepared_effect(record, || {
                f.state
                    .verified_invocation_status(&f.caller, key.invocation)?;
                Ok(())
            });
            tokio::pin!(reconciliation);
            if scenario == "after" {
                tokio::select! {
                    _ = &mut reconciliation => panic!("held owner must block terminal disclosure"),
                    () = f.submitted.notified() => {}
                }
                f.membership.write().workspaces[0].set_owner("another person");
            }
            drop(permit);
            reconciliation.await
        };
        if scenario == "refused" {
            assert!(result.unwrap().uncertain);
        } else {
            assert!(
                result.is_err(),
                "current caller authority precedes recovery disclosure"
            );
        }
        let retained = {
            let guard = f.state.journal.lock().unwrap();
            let retained = guard
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .lookup(&key)
                .unwrap()
                .unwrap();
            drop(guard);
            retained
        };
        assert!(retained.uncertain);
        assert!(matches!(
            retained.update,
            tilo_actions::InvocationUpdate::Accepted { .. }
        ));
        assert!(f.state.pending_invocations.lock().unwrap().is_empty());
        if scenario == "before" {
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), f.submitted.notified())
                    .await
                    .is_err(),
                "refused caller must not submit an inert receipt either"
            );
        }
        drop(f);
    }
}
