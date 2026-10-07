//! Signed Agent source admission only; no guest, model inference or grade is fabricated.
use super::*;
use tilo_action_broker::collab_broker_capnp::{
    ApprovedCheckContext, ApprovedCheckServices, SourceBinding,
};

struct SourceOwner {
    native: Arc<native::Native>,
    key: ed25519_dalek::SigningKey,
    governor: tilo_user_identity::DeviceCert,
}
impl tilo_peer_state::owner_commit::OwnerCommitService for SourceOwner {
    fn requires_room(&self, room: &str) -> bool {
        room == self.native.room.to_string()
    }
    fn health(&self) -> Result<(), String> {
        self.native.owner.health()
    }
    fn commit(&self, request: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
        let native = self.native.clone();
        let key = self.key.clone();
        let governor = self.governor.clone();
        Box::pin(async move {
            if matches!(request.operation, CommitOperation::EffectReceipt { .. }) {
                native
                    .owner
                    .commit_effect_receipt(&request, &governor, &key, SystemTime::now())
            } else {
                native
                    .owner
                    .commit_workspace_task_command(request, SystemTime::now())
            }
        })
    }
}
pub(super) fn owner_commit(
    native: &Arc<native::Native>,
    key: &ed25519_dalek::SigningKey,
    mode: &str,
) -> Qualification<Arc<dyn tilo_peer_state::owner_commit::OwnerCommitService>> {
    if !mode.starts_with("steps-source") {
        return Ok(native.clone());
    }
    let governor = native
        .owner
        .with_current(native.room, |view| Ok(view.device().clone()))?;
    Ok(Arc::new(SourceOwner {
        native: native.clone(),
        key: key.clone(),
        governor,
    }))
}
struct Probe {
    root: std::path::PathBuf,
    binding: SourceBinding,
}
impl ApprovedCheckServices for Probe {
    fn run(
        &self,
        context: &ApprovedCheckContext,
    ) -> Result<runtime_collab::approved_checks::Receipt, String> {
        let result = match context.read_source(&self.binding, &serde_json::json!({})) {
            Ok(observed) => {
                std::fs::write(self.root.join("source-observed"), b"actual child terminal")
                    .map_err(|error| error.to_string())?;
                let began = std::time::Instant::now();
                while !self.root.join("source-release").exists() {
                    if began.elapsed() > tilo_test_env::hang_bound() {
                        return Err("owner fixture never released source recheck".into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                observed.recheck().and_then(|()| {
                    serde_json::to_value(observed).map_err(|error| error.to_string())
                })
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            eprintln!("genuine source probe refused: {error}");
        }
        std::fs::write(
            self.root.join("source-result.json"),
            serde_json::to_vec(&result).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Err("source probe executed no guest check".into())
    }
}
pub(super) fn service(
    root: &Path,
    native: &native::Native,
    mode: &str,
) -> Option<Arc<dyn ApprovedCheckServices>> {
    mode.starts_with("steps-source").then(|| {
        let route = tilo_action_broker::route::CrdtRoute::app(
            "so.tilo.step-fixture-reader",
            native.room.to_string(),
        );
        Arc::new(Probe {
            root: root.to_owned(),
            binding: SourceBinding {
                app_id: route.app_id,
                workspace: route.cluster_id,
                record: route.record_id,
                resource_identity: tilo_actions::ResourceSemanticIdentity {
                    kind: "step_fixture.reader".into(),
                    key: "immutable-fixture".into(),
                },
                action: "inspect".into(),
            },
        }) as Arc<dyn ApprovedCheckServices>
    })
}
pub(super) fn descriptor() -> Qualification<runtime_collab::ExportedAction> {
    let contract = tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Job,
        ..Default::default()
    };
    Ok(runtime_collab::ExportedAction {
        resource: "check:fixture".into(),
        semantic_identity: Some(runtime_collab::ExportedResourceSemanticIdentity {
            kind: "step_fixture.check".into(),
            key: "approved-source".into(),
        }),
        action: runtime_collab::approved_checks::ACTION_ID.into(),
        resource_kind: "step_fixture.check".into(),
        resource_label: "Signed check".into(),
        action_label: "Check".into(),
        description: None,
        operation_kind: "effect".into(),
        params_json: "[]".into(),
        contract_json: serde_json::to_string(&contract)?,
        view: None,
        input: runtime_collab::ExportedInput::None,
    })
}
pub(super) fn check_reply<'a>(
    channel: &'a runtime_collab::ChannelHandle,
    invocation: &'a runtime_collab::Invocation,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<tilo_actions::InvocationUpdate, String>> + 'a>,
> {
    Box::pin(async move {
        let error = channel
            .approved_check(&runtime_collab::approved_checks::Request {
                version: 1,
                invocation: uuid::Uuid::from_bytes(invocation.invocation_uuid).to_string(),
                check_id: "regression".into(),
            })
            .await
            .err()
            .ok_or("source admission fixture unexpectedly claimed an executed check")?;
        eprintln!("actual approved-check RPC refused: {error}");
        Ok(tilo_actions::InvocationUpdate::Failed {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                invocation.invocation_uuid,
            )),
            error: tilo_actions::ActionFailure {
                code: "source-probe-only".into(),
                message: error.to_string(),
                details: None,
                retryable: false,
            },
        })
    })
}

fn grant_check(fixture: &RunningFixture) -> Qualification<()> {
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    let now = SystemTime::now();
    let messages = fixture
        .native
        .owner
        .with_current(fixture.native.room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            candidate.put_governed_capabilities(
                &[CapOverride::grant(
                    Subject::agent("fixture-agent"),
                    Scope::Action {
                        app: "so.tilo.step-fixture-check".into(),
                        action: runtime_collab::approved_checks::ACTION_ID.into(),
                    },
                    Verb::Write,
                )
                .with_id("fixture-source-check")],
                &fixture.key,
                now,
            )?;
            candidate.durable_mutation_messages(
                &before,
                &fixture.key,
                now,
                view.generation()
                    .checked_add(1)
                    .ok_or("generation exhausted")?,
            )
        })?;
    for message in messages {
        fixture.native.owner.receive(
            fixture.native.room,
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                fixture.certificate.device_public_key,
            ),
            message,
            now,
        )?;
    }
    Ok(())
}
fn wait_file(path: &Path) -> Qualification<()> {
    let began = Instant::now();
    while !path.exists() {
        if let Some(parent) = path.parent() {
            let early = parent.join("source-result.json");
            if path
                .file_name()
                .is_some_and(|name| name == "source-observed")
                && early.exists()
            {
                return Err(format!(
                    "source read ended before the observed stage: {}",
                    std::fs::read_to_string(early)?
                )
                .into());
            }
        }
        require(
            began.elapsed() < tilo_test_env::hang_bound(),
            "genuine source stage never observed",
        )?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
fn cancel(fixture: &RunningFixture) -> Qualification<()> {
    let request = OwnerCommitRequest {
        room: fixture.native.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: fixture.certificate.clone(),
            },
            command: WorkspaceTaskCommand::CancelForRun {
                task_id: fixture.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            },
        },
    };
    fixture
        .native
        .owner
        .commit_workspace_task_command(request.clone(), SystemTime::now())?
        .validate_for(&request)?;
    Ok(())
}
fn exercise(mode: &str) -> Qualification<()> {
    let mut fixture = running_fixture_options(false, false, Some(mode))?;
    let mut reader = steps::Target::source_check(&fixture, false)?;
    let mut check = steps::Target::source_check(&fixture, true)?;
    steps::grant_reader(&fixture, true)?;
    grant_check(&fixture)?;
    fixture.accept_and_progress()?;
    steps::write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":check.peer}),
    )?;
    let root = fixture.root.path();
    wait_file(&root.join("source-observed"))?;
    require(
        reader.count.load(Ordering::SeqCst) == 1,
        "one actual independent source child",
    )?;
    if mode.ends_with("cancelled") {
        cancel(&fixture)?;
    }
    if mode.ends_with("revoked") {
        steps::grant_reader(&fixture, false)?;
    }
    std::fs::write(root.join("source-release"), b"owner released recheck")?;
    wait_file(&root.join("source-result.json"))?;
    let result: Result<serde_json::Value, String> =
        serde_json::from_slice(&std::fs::read(root.join("source-result.json"))?)?;
    if mode == "steps-source-read" {
        let observed = result?;
        require(
            observed.get("output") == Some(&serde_json::json!("retained private tool result")),
            "actual source typed terminal differs",
        )?;
        require(
            observed.pointer("/binding/app_id")
                == Some(&serde_json::json!("so.tilo.step-fixture-reader")),
            "source target app differs",
        )?;
        let invocation = observed
            .get("invocation")
            .ok_or("child invocation absent")?;
        let parent = observed
            .get("parent_invocation")
            .ok_or("parent invocation absent")?;
        require(
            invocation != parent,
            "source child reused its parent invocation",
        )?;
    } else {
        let error = result
            .err()
            .ok_or("revoked source read unexpectedly succeeded")?;
        let expected = if mode.ends_with("cancelled") {
            "cancel"
        } else {
            "caller no longer authorized for invocation"
        };
        if !error.contains(expected) {
            return Err(error.into());
        }
    }
    read_frame(&mut fixture.model_peer, "source-action")?;
    fixture.model_peer.write_all(b"F")?;
    read_frame(
        &mut fixture.model_peer,
        if mode.ends_with("cancelled") {
            "cancelled"
        } else {
            "finished"
        },
    )?;
    fixture.model_peer.write_all(b"E")?;
    fixture.launcher.shutdown()?;
    reader.stop()?;
    check.stop()?;
    drop(check);
    drop(reader);
    drop(fixture);
    Ok(())
}
#[test]
fn signed_agent_source_read_retains_actual_accepted_lease_and_cross_app_attribution()
-> Qualification<()> {
    exercise("steps-source-read")
}
#[test]
fn signed_agent_source_observation_refuses_revoked_read_authority() -> Qualification<()> {
    exercise("steps-source-permission-revoked")
}
#[test]
fn signed_agent_source_observation_refuses_owner_cancelled_run() -> Qualification<()> {
    exercise("steps-source-cancelled")
}
