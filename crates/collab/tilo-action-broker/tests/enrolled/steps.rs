//! Genuine signed worker target; no application document history is fabricated.
use super::*;
use std::sync::atomic::AtomicUsize;
use tilo_action_broker::route::CrdtRoute;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub(super) fn configured_broker(
    root: &Path,
    native: &Arc<native::Native>,
    mode: &str,
    key: &ed25519_dalek::SigningKey,
) -> Qualification<Broker> {
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    let clock = tilo_action_broker::peer_actions::OwnerPolicyClock::new()?.new_origin();
    let authority = Arc::new(ClockAuthority {
        native: native.clone(),
        clock,
    });
    let fanout = Arc::new(FanoutState::new());
    let configured = tilo_action_broker::collab_broker::spawn_with_services(
        root.join("control.sock"),
        Some(native.clone()),
        PeerPublisher::with_optional_owner_commit(
            None,
            super::source_projection::owner_commit(native, key, mode)?,
        ),
        Some(&tilo_action_broker::peer_actions::NativePeerBrokerConfig {
            journal_root: root.join("invocation-journal"),
            certificate_authority: authority,
        }),
        HostServices {
            workspace_memberships: Some(Arc::new(AckLoss {
                native: native.clone(),
                phase: match mode {
                    "steps-prepare-ack-lost" => Loss::Prepare,
                    "steps-final-admission-cancelled" => Loss::PrepareCancel,
                    "steps-commit-ack-lost" => Loss::Commit,
                    _ => Loss::None,
                },
                failed: Arc::new(AtomicBool::new(false)),
            })),
            native_certificates: Some(native.clone()),
            approved_checks: super::source_projection::service(root, native, mode),
            ..Default::default()
        },
        Arc::new(Mutex::new(BrokerChannels::default())),
        fanout.clone(),
    )?;
    Ok(Broker {
        configured: Some(configured),
        quit: Arc::new(AtomicBool::new(false)),
        thread: None,
        fanout,
    })
}

#[derive(Clone, Copy)]
enum Loss {
    None,
    Prepare,
    PrepareCancel,
    Commit,
}
struct AckLoss {
    native: Arc<native::Native>,
    phase: Loss,
    failed: Arc<AtomicBool>,
}
impl tilo_action_broker::services::WorkspaceMemberships for AckLoss {
    fn requires_room(&self, room: &str) -> bool {
        tilo_action_broker::services::WorkspaceMemberships::requires_room(
            self.native.as_ref(),
            room,
        )
    }
    fn health(&self) -> Result<(), String> {
        self.native.owner.health()
    }
    fn task_snapshot(
        &self,
        room: &str,
    ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String> {
        tilo_action_broker::services::WorkspaceMemberships::task_snapshot(
            self.native.as_ref(),
            room,
        )
    }
    fn task_agent_authority(
        &self,
        invocation: &tilo_action_broker::services::WorkspaceTaskAgentInvocation,
    ) -> Result<WorkspaceTaskAuthority, String> {
        tilo_action_broker::services::WorkspaceMemberships::task_agent_authority(
            self.native.as_ref(),
            invocation,
        )
    }
    fn execute(&self, request: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
        let selected = matches!(
            (&self.phase, &request.operation),
            (
                Loss::Prepare | Loss::PrepareCancel,
                CommitOperation::WorkspaceTaskCommand {
                    command: WorkspaceTaskCommand::PrepareStep { .. },
                    ..
                },
            ) | (
                Loss::Commit,
                CommitOperation::WorkspaceTaskCommand {
                    command: WorkspaceTaskCommand::CommitStep { .. },
                    ..
                },
            )
        );
        let native = self.native.clone();
        let failed = self.failed.clone();
        let phase = self.phase;
        let original = request.clone();
        Box::pin(async move {
            let receipt = tilo_action_broker::services::WorkspaceMemberships::execute(
                native.as_ref(),
                request,
            )
            .await?;
            if selected && !failed.swap(true, Ordering::SeqCst) {
                if matches!(phase, Loss::PrepareCancel) {
                    cancel_prepared(&native, &original)?;
                    return Ok(receipt);
                }
                return Err(
                    "injected lost acknowledgement after genuine durable step commit".into(),
                );
            }
            Ok(receipt)
        })
    }
}

fn cancel_prepared(native: &native::Native, prepared: &OwnerCommitRequest) -> Result<(), String> {
    let CommitOperation::WorkspaceTaskCommand {
        command: WorkspaceTaskCommand::PrepareStep {
            task_id, run_epoch, ..
        },
        ..
    } = &prepared.operation
    else {
        return Err("cancellation fixture did not observe genuine prepared step".into());
    };
    let state = native.owner.workspace_task_snapshot(native.room)?;
    let after_revision = state
        .controls
        .get(task_id)
        .ok_or("prepared task control absent")?
        .directions
        .last()
        .map_or(0, |row| row.revision);
    let caller = native
        .owner
        .with_current(native.room, |view| Ok(view.device().clone()))?;
    let cancel = OwnerCommitRequest {
        room: native.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person { caller },
            command: WorkspaceTaskCommand::CancelForRun {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                after_revision,
            },
        },
    };
    native
        .owner
        .commit_workspace_task_command(cancel.clone(), SystemTime::now())?
        .validate_for(&cancel)?;
    Ok(())
}

struct ClockAuthority {
    native: Arc<native::Native>,
    clock: tilo_action_broker::peer_actions::PolicyOrigin,
}
impl tilo_action_broker::peer_actions::CertificateAuthority for ClockAuthority {
    fn policy_time(&self) -> Result<Option<tilo_action_broker::peer_actions::PolicyTime>, String> {
        self.clock
            .capture()
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
        time: &tilo_action_broker::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        if !self.clock.owns(time) {
            return Err("installed policy origin changed".into());
        }
        time.now_ns().map_err(|error| error.to_string())?;
        self.authorize(room, certificate)
    }
    fn authorize(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_action_broker::peer_actions::CertificateAuthority::authorize(
            self.native.as_ref(),
            room,
            certificate,
        )
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_action_broker::peer_actions::CertificateAuthority::authorize_join(
            self.native.as_ref(),
            room,
            certificate,
        )
    }
}

fn descriptor() -> Qualification<runtime_collab::ExportedAction> {
    let mut contract = tilo_actions::ActionContract::view();
    contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type":"string"}),
    };
    Ok(runtime_collab::ExportedAction {
        resource: "reader:fixture".into(),
        semantic_identity: Some(runtime_collab::ExportedResourceSemanticIdentity {
            kind: "step_fixture.reader".into(),
            key: "immutable-fixture".into(),
        }),
        action: "inspect".into(),
        resource_kind: "step_fixture.reader".into(),
        resource_label: "Signed reader".into(),
        action_label: "Inspect".into(),
        description: None,
        operation_kind: "view".into(),
        params_json: "[]".into(),
        contract_json: serde_json::to_string(&contract)?,
        view: None,
        input: runtime_collab::ExportedInput::None,
    })
}

pub(super) struct Target {
    pub(super) peer: u64,
    quit: Arc<AtomicBool>,
    pub(super) count: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}
impl Target {
    fn new(fixture: &RunningFixture) -> Qualification<Self> {
        Self::held(fixture, false)
    }
    fn held(fixture: &RunningFixture, held: bool) -> Qualification<Self> {
        Self::configured(fixture, held, false, false)
    }
    pub(super) fn source_check(fixture: &RunningFixture, check: bool) -> Qualification<Self> {
        Self::configured(fixture, false, check, true)
    }
    fn connection(
        fixture: &RunningFixture,
        route: &CrdtRoute,
        check: bool,
    ) -> Qualification<(UnixStream, Vec<u8>)> {
        let stream = if check {
            // The check host requires the actual retained worker registration.
            // Ordinary reader fixture modes retain their existing descriptor path.
            fixture
                .state
                .connect_local_worker_with_authorizer(route.clone(), &fixture.certificate)?
                .0
        } else {
            fixture
                .state
                .connect_local_worker(route.clone(), &fixture.certificate)?
        };
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: route.cluster_id.clone(),
            device_certificate: serde_json::to_vec(&fixture.certificate)?,
            proof: fixture
                .key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &route.cluster_id,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()?;
        Ok((stream, presentation))
    }
    fn configured(
        fixture: &RunningFixture,
        held: bool,
        check: bool,
        projection: bool,
    ) -> Qualification<Self> {
        let route = CrdtRoute::app(
            if check {
                "so.tilo.step-fixture-check"
            } else {
                "so.tilo.step-fixture-reader"
            },
            fixture.native.room.to_string(),
        );
        let (stream, presentation) = Self::connection(fixture, &route, check)?;
        let quit = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(!held));
        let thread_release = release.clone();
        let state = fixture.state.clone();
        let (ready, result) = std::sync::mpsc::channel();
        let thread_quit = quit.clone();
        let thread_count = count.clone();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            let result = runtime.block_on(tokio::task::LocalSet::new().run_until(async move {
                stream.set_nonblocking(true).map_err(|error| error.to_string())?;
                let stream = tokio::net::UnixStream::from_std(stream).map_err(|error| error.to_string())?;
                let (read, write) = tokio::io::split(stream);
                let network = capnp_rpc::twoparty::VatNetwork::new(read.compat(), write.compat_write(),
                    capnp_rpc::rpc_twoparty_capnp::Side::Client, capnp::message::ReaderOptions::default());
                let mut rpc = capnp_rpc::RpcSystem::new(Box::new(network), None);
                let raw = rpc.bootstrap(capnp_rpc::rpc_twoparty_capnp::Side::Server);
                let driver = tokio::task::spawn_local(rpc);
                let client = runtime_collab::CollabClient::from_raw(raw);
                let channel = client.join_channel(&route.channel_key(), &presentation).await
                    .map_err(|error| error.to_string())?;
                let mut operation = if check { super::source_projection::descriptor() } else { descriptor() }.map_err(|error| error.to_string())?;
                if projection && !check { let mut contract: tilo_actions::ActionContract = serde_json::from_str(&operation.contract_json).map_err(|error| error.to_string())?;
                    contract.tags.push(tilo_actions::DOCUMENT_READ_TAG.into());
                    operation.contract_json = serde_json::to_string(&contract).map_err(|error| error.to_string())?; }
                channel.publish_actions(&route.app_id, &[operation])
                    .await.map_err(|error| error.to_string())?;
                let invocations = channel.subscribe_stable_invocations().await.map_err(|error| error.to_string())?;
                let peer = state.published_surfaces().iter().find(|surface| surface.app_id == route.app_id)
                    .map(|surface| surface.peer_id).ok_or("actual target publication absent")?;
                drop(state);
                ready.send(peer).map_err(|error| error.to_string())?;
                loop {
                    // Keep the same outstanding Capnp read while checking owned
                    // shutdown. Dropping/reissuing it can consume a delivery into
                    // an abandoned request before the fixture handler sees it.
                    let next = invocations.read();
                    tokio::pin!(next);
                    let delivered = loop {
                        tokio::select! {
                            invocation = &mut next => break Some(invocation.map_err(|error| error.to_string())?),
                            () = tokio::time::sleep(Duration::from_millis(20)) => {
                                if thread_quit.load(Ordering::SeqCst) { break None; }
                            }
                        }
                    };
                    let Some(invocation) = delivered else { break; };
                    thread_count.fetch_add(1, Ordering::SeqCst);
                    if invocation.actor != "agent:fixture-agent" {
                        return Err(format!("target received another actor: {}", invocation.actor));
                    }
                    while !thread_release.load(Ordering::SeqCst) {
                        if thread_quit.load(Ordering::SeqCst) {
                            return Err("owned target stopped before settlement".into());
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    let update = if check { super::source_projection::check_reply(&channel, &invocation).await? } else { tilo_actions::InvocationUpdate::Succeeded {
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(invocation.invocation_uuid)),
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!("retained private tool result")),
                            ..Default::default()
                        },
                    }};
                    channel.update_invocation(invocation.id,
                        &serde_json::to_string(&update).map_err(|error| error.to_string())?)
                        .await.map_err(|error| error.to_string())?;
                }
                driver.abort();
                Ok(())
            }));
            if let Err(error) = &result {
                eprintln!("signed target failed: {error}");
            }
            result
        });
        let peer = result.recv_timeout(tilo_test_env::hang_bound())?;
        Ok(Self {
            peer,
            quit,
            count,
            release,
            thread: Some(thread),
        })
    }
    pub(super) fn stop(&mut self) -> Qualification<()> {
        self.quit.store(true, Ordering::SeqCst);
        self.thread
            .take()
            .ok_or("target already stopped")?
            .join()
            .map_err(|_| "target worker panicked")??;
        Ok(())
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _result = thread.join();
        }
    }
}

pub(super) fn grant_reader(fixture: &RunningFixture, allowed: bool) -> Qualification<()> {
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    let now = SystemTime::now();
    let messages = fixture
        .native
        .owner
        .with_current(fixture.native.room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            if allowed {
                candidate.put_governed_capabilities(
                    &[CapOverride::grant(
                        Subject::agent("fixture-agent"),
                        Scope::Action {
                            app: "so.tilo.step-fixture-reader".into(),
                            action: "inspect".into(),
                        },
                        Verb::Read,
                    )
                    .with_id("fixture-reader-inspect")],
                    &fixture.key,
                    now,
                )?;
            } else {
                candidate.remove_governed_capabilities(
                    &["fixture-reader-inspect".into()],
                    &fixture.key,
                    now,
                )?;
            }
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

#[test]
fn signed_native_step_actual_cross_app_handler_runs_once_under_committed_replay() {
    let mut fixture = running_fixture_options(false, false, Some("steps")).unwrap();
    let mut target = Target::new(&fixture).unwrap();
    grant_reader(&fixture, true).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    let receipt = read_frame(&mut fixture.model_peer, "steps");
    if receipt.is_err() {
        eprintln!(
            "step callback count: {}; owner: {:?}",
            target.count.load(Ordering::SeqCst),
            fixture
                .native
                .owner
                .workspace_task_snapshot(fixture.native.room)
        );
    }
    let receipt = receipt.unwrap();
    assert_eq!(receipt["replay_same_terminal"], true);
    assert_eq!(receipt["changed_arguments_refused"], true);
    assert_eq!(receipt["wrong_lease_refused"], true);
    assert_eq!(target.count.load(Ordering::SeqCst), 1);
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    let control = &snapshot.controls[&fixture.task];
    assert_eq!(control.steps.len(), 1);
    assert!(matches!(
        control.steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Committed {
            outcome: tilo_desktop_model::WorkspaceTaskStepOutcome::Succeeded,
            ..
        }
    ));
    fixture.finish().unwrap();
    target.stop().unwrap();
    fixture.stop().unwrap();
}

pub(super) fn write_frame(stream: &mut UnixStream, value: &serde_json::Value) -> Qualification<()> {
    let bytes = serde_json::to_vec(value)?;
    let length = u32::try_from(bytes.len())?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

#[test]
fn signed_native_step_contributor_cannot_read_without_scoped_grant() {
    let mut fixture = running_fixture_options(false, false, Some("steps-denied")).unwrap();
    let mut target = Target::new(&fixture).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    let refused = read_frame(&mut fixture.model_peer, "step-denied").unwrap();
    assert_eq!(refused["scoped_read_refused"], true);
    assert_eq!(target.count.load(Ordering::SeqCst), 0);
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert!(snapshot.controls[&fixture.task].steps.is_empty());
    fixture.finish().unwrap();
    target.stop().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn signed_native_step_revoked_scoped_permission_refuses_new_child_without_dispatch() {
    let mut fixture =
        running_fixture_options(false, false, Some("steps-permission-revoked")).unwrap();
    let mut target = Target::new(&fixture).unwrap();
    grant_reader(&fixture, true).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    read_frame(&mut fixture.model_peer, "steps").unwrap();
    let before = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    grant_reader(&fixture, false).unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"continue":true}),
    )
    .unwrap();
    let refused = read_frame(&mut fixture.model_peer, "step-revoked").unwrap();
    assert_eq!(refused["current_scoped_read_refused"], true);
    assert_eq!(target.count.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .native
            .owner
            .workspace_task_snapshot(fixture.native.room)
            .unwrap(),
        before
    );
    fixture.finish().unwrap();
    target.stop().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn signed_native_step_actual_cancel_allows_admitted_child_settlement_and_refuses_next() {
    let mut fixture = running_fixture_options(false, false, Some("steps-cancelled")).unwrap();
    let mut target = Target::held(&fixture, true).unwrap();
    grant_reader(&fixture, true).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    while target.count.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "actual admitted child never reached target"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let prepared = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert_eq!(prepared.controls[&fixture.task].steps.len(), 1);
    assert_eq!(
        prepared.controls[&fixture.task].steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Prepared
    );
    let cancel = OwnerCommitRequest {
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
        .commit_workspace_task_command(cancel.clone(), SystemTime::now())
        .unwrap()
        .validate_for(&cancel)
        .unwrap();
    target.release.store(true, Ordering::SeqCst);
    read_frame(&mut fixture.model_peer, "steps").unwrap();
    let refused = read_frame(&mut fixture.model_peer, "step-cancelled").unwrap();
    assert_eq!(refused["new_admission_refused"], true);
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert_eq!(
        snapshot.tasks[0].status,
        tilo_desktop_model::WorkspaceTaskStatus::Cancelled
    );
    assert!(snapshot.controls[&fixture.task].active_run.is_none());
    assert!(matches!(
        snapshot.controls[&fixture.task].steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Committed {
            outcome: tilo_desktop_model::WorkspaceTaskStepOutcome::Succeeded,
            ..
        }
    ));
    assert_eq!(target.count.load(Ordering::SeqCst), 1);
    fixture.model_peer.write_all(b"F").unwrap();
    read_frame(&mut fixture.model_peer, "cancelled").unwrap();
    fixture.model_peer.write_all(b"E").unwrap();
    target.stop().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn signed_native_step_lost_prepare_ack_has_no_dispatch_permit_and_replay_refuses_uncertainty() {
    let mut fixture =
        running_fixture_options(false, false, Some("steps-prepare-ack-lost")).unwrap();
    let mut target = Target::new(&fixture).unwrap();
    grant_reader(&fixture, true).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    let proof = read_frame(&mut fixture.model_peer, "step-prepare-ack-lost").unwrap();
    assert_eq!(proof["prepared_without_journal_refused"], true);
    assert_eq!(target.count.load(Ordering::SeqCst), 0);
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert_eq!(snapshot.controls[&fixture.task].steps.len(), 1);
    assert_eq!(
        snapshot.controls[&fixture.task].steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Prepared
    );
    fixture.model_peer.write_all(b"F").unwrap();
    read_frame(&mut fixture.model_peer, "finished").unwrap();
    let stopped = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert_eq!(
        stopped.tasks[0].status,
        tilo_desktop_model::WorkspaceTaskStatus::Waiting
    );
    assert!(stopped.controls[&fixture.task].active_run.is_none());
    assert_eq!(
        stopped.controls[&fixture.task].steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Prepared
    );
    fixture.model_peer.write_all(b"E").unwrap();
    target.stop().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn signed_native_step_lost_terminal_owner_ack_replays_retained_output_without_redispatch() {
    let mut fixture = running_fixture_options(false, false, Some("steps-commit-ack-lost")).unwrap();
    let mut target = Target::new(&fixture).unwrap();
    grant_reader(&fixture, true).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    let proof = read_frame(&mut fixture.model_peer, "steps").unwrap();
    assert_eq!(proof["lost_terminal_ack_recovered"], true);
    assert_eq!(target.count.load(Ordering::SeqCst), 1);
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert_eq!(snapshot.controls[&fixture.task].steps.len(), 1);
    assert!(matches!(
        snapshot.controls[&fixture.task].steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Committed { .. }
    ));
    let commits = snapshot.controls[&fixture.task]
        .receipts
        .iter()
        .filter(|receipt| {
            matches!(
                receipt.result,
                tilo_desktop_model::WorkspaceTaskMutationResult::StepCommitted { .. }
            )
        })
        .count();
    assert_eq!(
        commits, 1,
        "stable owner command id must retain only one settlement receipt"
    );
    fixture.finish().unwrap();
    target.stop().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn signed_native_step_cancel_after_prepare_refuses_final_child_admission() {
    let mut fixture =
        running_fixture_options(false, false, Some("steps-final-admission-cancelled")).unwrap();
    let mut target = Target::new(&fixture).unwrap();
    grant_reader(&fixture, true).unwrap();
    fixture.accept_and_progress().unwrap();
    write_frame(
        &mut fixture.model_peer,
        &serde_json::json!({"peer":target.peer}),
    )
    .unwrap();
    let observed = read_frame(&mut fixture.model_peer, "step-final-admission-cancelled");
    // Clean up every genuinely owned process before checking the result, including baseline refusal failures.
    if observed.is_ok() {
        fixture.model_peer.write_all(b"F").unwrap();
        let cancelled = read_frame(&mut fixture.model_peer, "cancelled").unwrap();
        assert_eq!(cancelled["owner_cancelled_observed"], true);
        fixture.model_peer.write_all(b"E").unwrap();
    }
    target.stop().unwrap();
    fixture.stop().unwrap();
    let observed = observed.unwrap();
    assert_eq!(observed["final_admission_refused"], true);
    assert_eq!(target.count.load(Ordering::SeqCst), 0);
    let state = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    assert_eq!(
        state.tasks[0].status,
        tilo_desktop_model::WorkspaceTaskStatus::Cancelled
    );
    let task = fixture.task.clone();
    drop(fixture);
    assert!(state.controls[&task].active_run.is_none());
    assert_eq!(state.controls[&task].steps.len(), 1);
    assert_eq!(
        state.controls[&task].steps[0].state,
        tilo_desktop_model::WorkspaceTaskStepState::Prepared
    );
}
