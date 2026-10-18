//! Source regression coverage for durable physical-host MODEL enrollment.
use super::*;
use crate::task_admission::model::{
    FrozenNativeModelSelection, NativeModelAuthority, NativeModelContext, NativeModelContract,
    NativeModelPlanning, TypedNativeModelRequirement,
};
use crate::task_admission::model_host::{NativeModelConnection, NativeModelHostRegistration};
use std::time::{Duration, UNIX_EPOCH};

struct HostAuthority(NativeModelContext);
impl NativeModelAuthority for HostAuthority {
    fn current_model_context(
        &self,
        _: &NativeCommittedView,
        _: SystemTime,
    ) -> Result<NativeModelContext, String> {
        Ok(self.0.clone())
    }
}

fn enrollment(fixture: &Fixture, binding: &NativeInvocationBinding) -> NativeModelHostRegistration {
    let window = tilo_action_policy::TreePath::room(binding.room.to_string())
        .app("so.tilo.chat")
        .resource("host-window");
    let authority = HostAuthority(NativeModelContext {
        actor: binding.agent.clone(),
        task: binding.task.clone(),
        epoch: binding.epoch,
        revision: binding.revision,
        planning: NativeModelPlanning::Model,
        private_run_digest: [7; 32],
        contract: NativeModelContract {
            request_model: window.clone().action("requestModel"),
            window,
            kind: tilo_action_policy::ActionKind::Outside,
            selection: FrozenNativeModelSelection {
                app: "so.tilo.agent".into(),
                provider: "local".into(),
                model: "fixture".into(),
                contract: serde_json::json!({"endpoint": "http://localhost"}),
            },
        },
    });
    NativeModelHostRegistration::capture(
        &fixture.owner,
        binding.room,
        NativeModelConnection {
            connection: uuid::Uuid::new_v4(),
            lease: uuid::Uuid::new_v4(),
        },
        &authority,
        &fixture.key,
    )
    .unwrap()
}

fn requirement(registered: &NativeModelHostRegistration) -> TypedNativeModelRequirement {
    let context = registered.context();
    TypedNativeModelRequirement::freeze(
        TypedNativeModelRequirement {
            room: registered.room,
            run: crate::task_policy::NativeTaskBinding {
                task_id: context.task.clone(),
                agent_id: context.actor.clone(),
                epoch: context.epoch,
                revision: context.revision,
                starter: Some("governor".into()),
            },
            invocation: uuid::Uuid::new_v4(),
            request: uuid::Uuid::new_v4(),
            host: registered.host,
            private_run_digest: context.private_run_digest,
            modelselection: context.contract.clone(),
            requestdigest: [0; 32],
        },
        b"exact outbound request",
    )
    .unwrap()
}

// Construct independently signed wire statements to test expiry and renewal
// without sleeping or substituting a clock for reserve/claim's real wall clock.
fn resign(
    mut wire: serde_json::Value,
    key: &ed25519_dalek::SigningKey,
) -> NativeModelHostRegistration {
    #[derive(serde::Deserialize)]
    struct Statement {
        id: uuid::Uuid,
        room: RoomId,
        host: [u8; 32],
        connection: NativeModelConnection,
        context: NativeModelContext,
        issued_at_ns: u64,
        expires_at_ns: u64,
    }
    let statement: Statement = serde_json::from_value(wire.clone()).unwrap();
    let signature = crate::task_admission::sign(
        "tilo-native-model-host-v1",
        &(
            statement.id,
            statement.room,
            statement.host,
            statement.connection,
            statement.context,
            statement.issued_at_ns,
            statement.expires_at_ns,
        ),
        key,
    )
    .unwrap();
    wire["signature"] = serde_json::to_value(signature).unwrap();
    serde_json::from_value(wire).unwrap()
}

#[test]
fn revoked_model_connection_does_not_stop_task_but_stop_still_fences_host_after_reopen() {
    let (fixture, binding) = running_with_model(true);
    let registered = enrollment(&fixture, &binding);
    let need = requirement(&registered);
    fixture
        .owner
        .with_current(binding.room, |view| {
            let mut ledger = TaskAdmissionLedger::default();
            {
                let mut admission = NativeGovernorAdmission {
                    room: binding.room,
                    peer: view.peer(),
                    ledger: &mut ledger,
                    continuity: view.continuity(),
                    key: &fixture.key,
                };
                admission.register_model_host(
                    &registered,
                    &fixture.certificate,
                    SystemTime::now(),
                )?;
                admission.revoke_model_host(
                    registered.id(),
                    &fixture.certificate,
                    SystemTime::now(),
                )?;
            }
            assert_eq!(
                ledger.state(&binding.task, binding.epoch),
                NativeTaskAdmissionState::Open
            );
            let mut reopened: TaskAdmissionLedger =
                serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
            reopened.verify_retained(binding.room, view.governor().device_public_key)?;
            assert!(
                reopened
                    .check_model_host(registered.id(), &need, &fixture.certificate)
                    .is_err()
            );
            let mut admission = NativeGovernorAdmission {
                room: binding.room,
                peer: view.peer(),
                ledger: &mut reopened,
                continuity: view.continuity(),
                key: &fixture.key,
            };
            assert!(
                admission
                    .register_model_host(&registered, &fixture.certificate, SystemTime::now())
                    .is_err()
            );
            let mut wire = serde_json::to_value(&registered).unwrap();
            wire["id"] = serde_json::json!(uuid::Uuid::new_v4());
            let replacement = resign(wire, &fixture.key);
            assert!(
                admission
                    .register_model_host(&replacement, &fixture.certificate, SystemTime::now())
                    .is_err()
            );
            let fence = admission.stop(&binding.task, binding.epoch)?;
            assert_eq!(fence.hosts, BTreeSet::from([binding.host]));
            assert_eq!(reopened.pending_host_fences(binding.host), vec![fence]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn signed_expiry_is_checked_on_registration_reservation_and_claim() {
    let (fixture, binding) = running_with_model(true);
    let registered = enrollment(&fixture, &binding);
    let need = requirement(&registered);
    let mut tampered = serde_json::to_value(&registered).unwrap();
    tampered["issued_at_ns"] = serde_json::json!(1_000_000_000_u64);
    tampered["expires_at_ns"] = serde_json::json!(121_000_000_000_u64);
    let tampered: NativeModelHostRegistration = serde_json::from_value(tampered).unwrap();
    assert!(
        tampered.verify().is_err(),
        "the deadline must be host-signed"
    );
    let mut wire = serde_json::to_value(&registered).unwrap();
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        - 1;
    wire["issued_at_ns"] = serde_json::json!(expiry - 120_000_000_000);
    wire["expires_at_ns"] = serde_json::json!(expiry);
    let expired = resign(wire, &fixture.key);
    expired.verify().unwrap();
    fixture
        .owner
        .with_current(binding.room, |view| {
            let mut ledger = TaskAdmissionLedger::default();
            let mut admission = NativeGovernorAdmission {
                room: binding.room,
                peer: view.peer(),
                ledger: &mut ledger,
                continuity: view.continuity(),
                key: &fixture.key,
            };
            assert!(
                admission
                    .register_model_host(&expired, &fixture.certificate, SystemTime::now())
                    .is_err()
            );
            admission.register_model_host(&registered, &fixture.certificate, SystemTime::now())?;
            let reservation = admission.reserve(
                need.binding()?,
                &fixture.certificate,
                SystemTime::now(),
                |_, _| Ok(()),
            )?;
            // Model an enrollment accepted earlier and retained through expiry.
            admission
                .ledger
                .model_hosts
                .insert(expired.id(), expired.clone());
            let mut reopened: TaskAdmissionLedger =
                serde_json::from_slice(&serde_json::to_vec(admission.ledger).unwrap()).unwrap();
            reopened.verify_retained(binding.room, view.governor().device_public_key)?;
            assert!(
                reopened
                    .check_model_host(expired.id(), &need, &fixture.certificate)
                    .is_err()
            );
            let mut admission = NativeGovernorAdmission {
                room: binding.room,
                peer: view.peer(),
                ledger: &mut reopened,
                continuity: view.continuity(),
                key: &fixture.key,
            };
            let current =
                admission
                    .ledger
                    .check_model_host(expired.id(), &need, &fixture.certificate);
            assert!(
                admission
                    .reserve(
                        need.binding()?,
                        &fixture.certificate,
                        SystemTime::now(),
                        |_, _| { current.map(|_| ()) }
                    )
                    .is_err()
            );
            // Stale enrollment cannot authorize dispatch even when the reservation
            // itself was made while the host was live.
            let current =
                admission
                    .ledger
                    .check_model_host(expired.id(), &need, &fixture.certificate);
            assert!(
                admission
                    .claim(
                        &reservation,
                        &fixture.certificate,
                        SystemTime::now(),
                        |_, _| { current.map(|_| ()) }
                    )
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn prepared_host_enrolls_desired_not_applied_and_keeps_exact_revision_and_digest() {
    let (fixture, mut binding) = running_with_model(true);
    fixture
        .owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: binding.room.to_string(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::new_v4(),
                    authority: WorkspaceTaskAuthority::Person {
                        caller: fixture.certificate.clone(),
                    },
                    command: WorkspaceTaskCommand::DirectionForRun {
                        task_id: binding.task.clone(),
                        run_epoch: binding.epoch,
                        after_revision: 0,
                        text: "Review the compatibility section first".into(),
                    },
                },
            },
            fixture.now,
        )
        .unwrap();
    binding.revision = 1;
    let registered = enrollment(&fixture, &binding);
    let need = requirement(&registered);
    fixture
        .owner
        .with_current(binding.room, |view| {
            let state =
                tilo_peer_state::workspace_task::snapshot(view.peer().admitted_room().unwrap())?;
            assert_eq!(
                state.controls[&binding.task]
                    .active_run
                    .as_ref()
                    .unwrap()
                    .applied_direction_revision,
                0
            );
            let mut ledger = TaskAdmissionLedger::default();
            let mut admission = NativeGovernorAdmission {
                room: binding.room,
                peer: view.peer(),
                ledger: &mut ledger,
                continuity: view.continuity(),
                key: &fixture.key,
            };
            admission.register_model_host(&registered, &fixture.certificate, SystemTime::now())?;
            assert_eq!(
                admission
                    .ledger
                    .check_model_host(registered.id(), &need, &fixture.certificate)?,
                *registered.context()
            );
            for revision in [0, 2] {
                let mut wire = serde_json::to_value(&registered).unwrap();
                wire["id"] = serde_json::json!(uuid::Uuid::new_v4());
                wire["context"]["revision"] = serde_json::json!(revision);
                let wrong = resign(wire, &fixture.key);
                assert!(
                    admission
                        .register_model_host(&wrong, &fixture.certificate, SystemTime::now())
                        .is_err()
                );
                let mut wrong = need.clone();
                wrong.run.revision = revision;
                assert!(
                    admission
                        .ledger
                        .check_model_host(registered.id(), &wrong, &fixture.certificate)
                        .is_err()
                );
            }
            let mut wrong = need.clone();
            wrong.private_run_digest = [8; 32];
            assert!(
                admission
                    .ledger
                    .check_model_host(registered.id(), &wrong, &fixture.certificate)
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn enrollment_rejects_stale_frontier_profile_contract_and_forged_agent_host() {
    let (fixture, binding) = running_with_model(true);
    let registered = enrollment(&fixture, &binding);
    let need = requirement(&registered);
    fixture
        .owner
        .with_current(binding.room, |view| {
            let mut ledger = TaskAdmissionLedger::default();
            let mut admission = NativeGovernorAdmission {
                room: binding.room,
                peer: view.peer(),
                ledger: &mut ledger,
                continuity: view.continuity(),
                key: &fixture.key,
            };
            admission.register_model_host(&registered, &fixture.certificate, SystemTime::now())?;
            for field in ["revision", "epoch"] {
                let mut wire = serde_json::to_value(&registered).unwrap();
                wire["id"] = serde_json::json!(uuid::Uuid::new_v4());
                wire["context"][field] = serde_json::json!(99);
                let stale = resign(wire, &fixture.key);
                assert!(
                    admission
                        .register_model_host(&stale, &fixture.certificate, SystemTime::now())
                        .is_err()
                );
            }
            for field in ["model", "provider"] {
                let mut wrong = need.clone();
                if field == "model" {
                    wrong.modelselection.selection.model = "different".into();
                } else {
                    wrong.modelselection.selection.provider = "different".into();
                }
                assert!(
                    admission
                        .ledger
                        .check_model_host(registered.id(), &wrong, &fixture.certificate)
                        .is_err()
                );
            }
            let mut wrong = need.clone();
            wrong.modelselection.selection.contract =
                serde_json::json!({"endpoint": "http://elsewhere"});
            assert!(
                admission
                    .ledger
                    .check_model_host(registered.id(), &wrong, &fixture.certificate)
                    .is_err()
            );
            let mut forged = fixture.certificate.clone();
            forged.device_public_key = ed25519_dalek::SigningKey::from_bytes(&[39; 32])
                .verifying_key()
                .to_bytes();
            forged.capabilities.can_act_as_host = true;
            assert!(
                admission
                    .register_model_host(&registered, &forged, SystemTime::now())
                    .is_err()
            );
            assert!(
                admission
                    .ledger
                    .check_model_host(registered.id(), &need, &forged)
                    .is_err()
            );
            // Even a correctly root-signed host bit is not room admission or
            // proof of custody for the enrolled physical connection.
            tilo_user_identity::sign_device_cert(
                &mut forged,
                &ed25519_dalek::SigningKey::from_bytes(&[91; 32]),
            )
            .unwrap();
            tilo_user_identity::verify_device_cert(&forged).unwrap();
            let agent_key = ed25519_dalek::SigningKey::from_bytes(&[39; 32]);
            let mut wire = serde_json::to_value(&registered).unwrap();
            wire["host"] = serde_json::json!(forged.device_public_key);
            wire["id"] = serde_json::json!(uuid::Uuid::new_v4());
            let agent_registration = resign(wire, &agent_key);
            agent_registration.verify().unwrap();
            assert!(
                admission
                    .register_model_host(&agent_registration, &forged, SystemTime::now(),)
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn renewal_spends_old_nonce_and_cannot_restart_expired_connection() {
    let (fixture, binding) = running_with_model(true);
    let registered = enrollment(&fixture, &binding);
    let need = requirement(&registered);
    fixture
        .owner
        .with_current(binding.room, |view| {
            let mut ledger = TaskAdmissionLedger::default();
            let mut admission = NativeGovernorAdmission {
                room: binding.room,
                peer: view.peer(),
                ledger: &mut ledger,
                continuity: view.continuity(),
                key: &fixture.key,
            };
            admission.register_model_host(&registered, &fixture.certificate, SystemTime::now())?;
            let mut wire = serde_json::to_value(&registered).unwrap();
            wire["id"] = serde_json::json!(uuid::Uuid::new_v4());
            let issued = wire["issued_at_ns"].as_u64().unwrap();
            wire["issued_at_ns"] = serde_json::json!(issued + 1);
            wire["expires_at_ns"] = serde_json::json!(wire["expires_at_ns"].as_u64().unwrap() + 1);
            let renewed = resign(wire, &fixture.key);
            admission.register_model_host(&renewed, &fixture.certificate, SystemTime::now())?;
            assert!(
                admission
                    .ledger
                    .check_model_host(registered.id(), &need, &fixture.certificate)
                    .is_err()
            );
            assert!(
                admission
                    .ledger
                    .check_model_host(renewed.id(), &need, &fixture.certificate)
                    .is_ok()
            );
            assert!(
                admission
                    .register_model_host(&registered, &fixture.certificate, SystemTime::now())
                    .is_err()
            );
            let too_late = registered.expires_at() + Duration::from_secs(1);
            let mut wire = serde_json::to_value(&renewed).unwrap();
            wire["id"] = serde_json::json!(uuid::Uuid::new_v4());
            let issued = too_late.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
            wire["issued_at_ns"] = serde_json::json!(issued);
            wire["expires_at_ns"] = serde_json::json!(issued + 120_000_000_000);
            let late_renewal = resign(wire, &fixture.key);
            assert!(
                admission
                    .register_model_host(&late_renewal, &fixture.certificate, too_late)
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}
