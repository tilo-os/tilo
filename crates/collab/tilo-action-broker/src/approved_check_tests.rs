// Native launcher-authenticated dispatch for informational progress tests.
struct ProgressFixtureLaunch {
    app: String,
    room: String,
}
impl crate::services::LauncherServices for ProgressFixtureLaunch {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("fixture never launches apps".into())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        pid == std::process::id() && app == self.app && room == self.room
    }
}
fn progress_descriptors() -> Vec<runtime_collab::ExportedAction> {
    let directory = signed_worker::directory_state("reported-check-r1");
    let mut check = directory.borrow().row().descriptor.unwrap();
    check.action = runtime_collab::approved_checks::ACTION_ID.into();
    check.operation_kind = tilo_actions::OperationKind::Effect;
    check.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
        kind: check.resource_kind.clone(),
        key: "check".into(),
    });
    check.contract = tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        result: tilo_actions::ResultSpec::Job,
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        ..Default::default()
    };
    vec![check]
}
async fn progress_native_connection(
    room: &signed_worker::OriginalRoom,
    probe: Arc<dyn ApprovedCheckServices>,
) -> Result<
    (
        Arc<CapnpBrokerState>,
        signed_worker::Connection,
        Rc<LocalActionDirectoryRegistry>,
    ),
    String,
> {
    // The replacement broker reuses the room's journal, which has one writer.
    drop(room.state.journal.lock().unwrap().as_mut().unwrap().take());
    let (state, bridge) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(room.membership.authority()),
        None,
        None,
        HostServices {
            launcher: Some(Arc::new(ProgressFixtureLaunch {
                app: room.route.app_id.clone(),
                room: room.route.cluster_id.clone(),
            })),
            approved_checks: Some(probe),
            ..Default::default()
        },
    );
    assert!(
        state
            .certificate_authority
            .set(room.authority.clone())
            .is_ok()
    );
    let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
    let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
    std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC).unwrap();
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol":launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id":room.route.app_id,"workspace_id":room.route.cluster_id,
    }))
    .unwrap();
    std::io::Write::write_all(
        &mut client,
        &launcher::services::COLLAB_LAUNCH_FRAMING
            .encode(&handoff)
            .unwrap(),
    )
    .unwrap();
    let (rpc, server, client) = signed_worker::rpc_pair(
        state.clone(),
        997,
        server,
        client,
        None,
        directories.clone(),
    );
    let handle = rpc
        .join_channel(
            &room.route.channel_key(),
            &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok((
        state,
        signed_worker::Connection {
            handle,
            server,
            client,
        },
        directories,
    ))
}
/// The host's own person on their own device, as the shell's certified
/// session presents them.
fn person(room: &signed_worker::OriginalRoom) -> (tilo_actions::remote::Caller, Vec<u8>) {
    (
        tilo_actions::remote::Caller::authenticated(
            room.cert.user_public_key,
            room.cert.device_public_key,
        )
        .in_room(&room.route.cluster_id)
        .in_new_execution(),
        signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
    )
}

/// Drive `pending` while the broker's turn resolves directories, as its
/// accept loop does.
#[allow(
    clippy::panic,
    reason = "test code included into the broker module: clippy does not see the cfg(test) on its include!, so it reads the test's checks as production code"
)]
async fn on_broker_turn<T>(
    (state, directories): (&Arc<CapnpBrokerState>, &Rc<LocalActionDirectoryRegistry>),
    mut pending: tilo_actions::remote::Pending<T>,
) -> T {
    tilo_test_env::within("the broker answers the person", async {
        loop {
            state.drain_action_directory_bridge(directories);
            match pending.poll_ready() {
                tilo_actions::remote::PendingPoll::Ready(answer) => return answer,
                tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
                tilo_actions::remote::PendingPoll::Abandoned => panic!("the request was dropped"),
            }
        }
    })
    .await
}

/// The person's certified session with the host: what they ask and
/// cancel there is theirs.
struct PersonSession {
    caller: tilo_actions::remote::Caller,
    host: Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
}

impl PersonSession {
    fn open(state: &Arc<CapnpBrokerState>, room: &signed_worker::OriginalRoom) -> Self {
        let (caller, evidence) = person(room);
        let host = state
            .certified_local_action_host(&caller, &evidence)
            .unwrap();
        Self { caller, host }
    }

    /// Ask `peer`'s approved check on `resource`.
    async fn runs_check(
        &self,
        broker: (&Arc<CapnpBrokerState>, &Rc<LocalActionDirectoryRegistry>),
        peer: u64,
        resource: &str,
    ) -> tilo_actions::remote::InvocationReceipt {
        let request = signed_worker::invoke_request(
            &peer.to_string(),
            resource,
            runtime_collab::approved_checks::ACTION_ID,
            Some(serde_json::json!({"check_id": "regression"})),
        );
        let receipt = on_broker_turn(broker, self.host.invoke(&self.caller, request))
            .await
            .unwrap();
        assert!(
            matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ),
            "{receipt:?}"
        );
        receipt
    }

    /// Cancel the check `invocation` they asked.
    async fn cancels(
        &self,
        broker: (&Arc<CapnpBrokerState>, &Rc<LocalActionDirectoryRegistry>),
        invocation: tilo_actions::InvocationId,
    ) {
        on_broker_turn(broker, self.host.cancel(&self.caller, &invocation))
            .await
            .unwrap();
    }
}

async fn progress_parent(
    (state, directories): (&Arc<CapnpBrokerState>, &Rc<LocalActionDirectoryRegistry>),
    room: &signed_worker::OriginalRoom,
    connection: &signed_worker::Connection,
    descriptors: &[runtime_collab::ExportedAction],
    peer: u64,
) -> (
    runtime_collab::InvocationStream,
    runtime_collab::approved_checks::Request,
    runtime_collab::Invocation,
    PersonSession,
) {
    connection
        .handle
        .publish_actions(descriptors)
        .await
        .unwrap();
    let stream = connection
        .handle
        .subscribe_stable_invocations()
        .await
        .unwrap();
    let session = PersonSession::open(state, room);
    let accepted = session
        .runs_check((state, directories), peer, "workspace:atlas")
        .await;
    let parent = stream.read().await.unwrap();
    assert_eq!(parent.invocation_uuid, *accepted.invocation.0.as_bytes());
    let request = runtime_collab::approved_checks::Request {
        version: 1,
        invocation: uuid::Uuid::from_bytes(parent.invocation_uuid).to_string(),
        check_id: "regression".into(),
    };
    (stream, request, parent, session)
}

// Real authenticated capability dispatch. The host fixture reports bounded
// observations but deliberately executes no guest and claims no success.
struct GatedProgressProbe {
    gates: Mutex<std::sync::mpsc::Receiver<()>>,
    calls: std::sync::atomic::AtomicUsize,
}
impl ApprovedCheckServices for GatedProgressProbe {
    fn run(
        &self,
        context: &ApprovedCheckContext,
    ) -> Result<runtime_collab::approved_checks::Receipt, String> {
        use runtime_collab::approved_checks::{LaneRefusal, ReportObservation};
        self.calls.fetch_add(1, Ordering::SeqCst);
        for (phase, completed) in [("Synthesis", 1), ("Routing", 2)] {
            context.report(ReportObservation::Reported {
                phase: phase.into(),
                completed: Some(completed),
                total: Some(2),
            })?;
            self.gates
                .lock()
                .unwrap()
                .recv_timeout(tilo_test_env::hang_bound())
                .map_err(|_| "gated progress fixture was not released")?;
            context.recheck()?;
        }
        assert!(
            context
                .report(ReportObservation::Reported {
                    phase: "invalid\nphase".into(),
                    completed: None,
                    total: None
                })
                .is_err()
        );
        context.report(ReportObservation::Disabled {
            reason: LaneRefusal::InvalidReport,
        })?;
        assert!(
            context
                .report(ReportObservation::Reported {
                    phase: "late phase".into(),
                    completed: None,
                    total: None
                })
                .is_err(),
            "disabled informational lane cannot restart"
        );
        Err("gated fixture executed no guest check".into())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn approved_check_reported_progress_preserves_original_custody_replay_and_gated_delivery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (release, gates) = std::sync::mpsc::channel();
            let probe = Arc::new(GatedProgressProbe {
                gates: Mutex::new(gates),
                calls: std::sync::atomic::AtomicUsize::new(0),
            });
            let descriptors = progress_descriptors();
            let (state, connection, directories) = progress_native_connection(&room, probe.clone())
                .await
                .unwrap();
            let (_invocations, request, _parent, _session) = progress_parent(
                (&state, &directories),
                &room,
                &connection,
                &descriptors,
                997,
            )
            .await;
            let mut progress = connection
                .handle
                .approved_check_reported_progress(&request)
                .await
                .unwrap();
            use runtime_collab::approved_checks::{ReportObservation, ReportedProgressUpdate};
            for (sequence, phase) in [(1, "Synthesis"), (2, "Routing")] {
                let update = tilo_test_env::within(
                    "reported phase must arrive before gate release",
                    progress.read(),
                )
                .await
                .unwrap();
                let ReportedProgressUpdate::Report(report) = update else {
                    panic!("gate cannot produce a terminal before release");
                };
                assert_eq!(report.invocation, request.invocation);
                assert_eq!(report.check_id, request.check_id);
                assert_eq!(report.sequence, sequence);
                assert_eq!(
                    report.observation,
                    ReportObservation::Reported {
                        phase: phase.into(),
                        completed: Some(sequence),
                        total: Some(2)
                    }
                );
                assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
                assert!(
                    connection
                        .handle
                        .approved_check(&request)
                        .await
                        .unwrap_err()
                        .to_string()
                        .contains("already executing"),
                    "legacy retry must share the original owned execution"
                );
                release.send(()).unwrap();
            }
            // Coalescing may replace Disabled with the final host refusal. In either
            // case, no fabricated success or late report may reach the caller.
            let mut update =
                tilo_test_env::within("same run must reach observed host refusal", progress.read())
                    .await;
            if let Ok(ReportedProgressUpdate::Report(report)) = &update {
                assert!(matches!(
                    report.observation,
                    ReportObservation::Disabled { .. }
                ));
                update = tilo_test_env::within(
                    "disabled lane retains final observation",
                    progress.read(),
                )
                .await;
            }
            assert!(
                update
                    .unwrap_err()
                    .to_string()
                    .contains("executed no guest check")
            );
            assert!(
                connection
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("executed no guest check")
            );
            assert_eq!(
                probe.calls.load(Ordering::SeqCst),
                1,
                "response loss/refusal never reexecutes"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn approved_check_reported_progress_refuses_cancelled_original_before_late_delivery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (release, gates) = std::sync::mpsc::channel();
            let probe = Arc::new(GatedProgressProbe {
                gates: Mutex::new(gates),
                calls: std::sync::atomic::AtomicUsize::new(0),
            });
            let descriptors = progress_descriptors();
            let (state, connection, directories) = progress_native_connection(&room, probe.clone())
                .await
                .unwrap();
            let (_invocations, request, parent, session) = progress_parent(
                (&state, &directories),
                &room,
                &connection,
                &descriptors,
                997,
            )
            .await;
            let mut progress = connection
                .handle
                .approved_check_reported_progress(&request)
                .await
                .unwrap();
            assert!(matches!(
                tilo_test_env::within("first live report", progress.read())
                    .await
                    .unwrap(),
                runtime_collab::approved_checks::ReportedProgressUpdate::Report(_)
            ));
            let invocation =
                tilo_actions::InvocationId(uuid::Uuid::from_bytes(parent.invocation_uuid));
            assert!(
                signed_worker::cancel(&connection.handle, invocation)
                    .await
                    .is_err(),
                "only the person who asked may cancel their check"
            );
            session.cancels((&state, &directories), invocation).await;
            release.send(()).unwrap();
            assert!(
                tilo_test_env::within(
                    "cancelled custody cannot deliver a late phase",
                    progress.read()
                )
                .await
                .is_err()
            );
            assert!(
                connection
                    .handle
                    .approved_check_reported_progress(&request)
                    .await
                    .is_err()
            );
            state.remove_connection(997);
            assert!(connection.handle.approved_check(&request).await.is_err());
            assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        })
        .await;
}

struct TerminalProgressProbe {
    panic: bool,
    calls: std::sync::atomic::AtomicUsize,
    receipt: Mutex<Option<runtime_collab::approved_checks::Receipt>>,
}
impl ApprovedCheckServices for TerminalProgressProbe {
    fn run(
        &self,
        context: &ApprovedCheckContext,
    ) -> Result<runtime_collab::approved_checks::Receipt, String> {
        use runtime_collab::approved_checks::{
            Binding, Diagnostic, LaneRefusal, Outcome, Receipt, ReportObservation,
        };
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.panic,
            "deliberate host adapter panic before observed execution"
        );
        context.report(ReportObservation::Disabled {
            reason: LaneRefusal::InvalidReport,
        })?;
        // This typed host fixture claims no command execution or cleanup. Its
        // exact failed receipt tests transport finality, not hardware evidence.
        let receipt = Receipt {
            version: 1,
            invocation: context.invocation(),
            check_id: context.check_id().into(),
            binding: Binding {
                app_id: context.app_id().into(),
                workspace: context.workspace().into(),
                host: [1; 32],
                record: context.record().into(),
                resource: context.resource().into(),
            },
            artifact_sha256: "a".repeat(64),
            config_sha256: "b".repeat(64),
            runtime_binary_sha256: "c".repeat(64),
            supervisor_source_sha256: "d".repeat(64),
            outcome: Outcome::Failed,
            exit_code: None,
            signal: None,
            elapsed_ms: 0,
            cleanup_observed: false,
            stdout: Diagnostic {
                preview: String::new(),
                truncated: false,
                sha256: None,
            },
            stderr: Diagnostic {
                preview: String::new(),
                truncated: false,
                sha256: None,
            },
            failure: None,
        };
        *self.receipt.lock().unwrap() = Some(receipt.clone());
        Ok(receipt)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn approved_check_reported_progress_preserves_terminal_receipt_after_disabled_lane_and_host_panic()
 {
    tokio::task::LocalSet::new()
        .run_until(async {
            for panic in [false, true] {
                let room = signed_worker::OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let probe = Arc::new(TerminalProgressProbe {
                    panic,
                    calls: std::sync::atomic::AtomicUsize::new(0),
                    receipt: Mutex::new(None),
                });
                let descriptors = progress_descriptors();
                let (state, connection, directories) =
                    progress_native_connection(&room, probe.clone())
                        .await
                        .unwrap();
                let (_invocations, request, _parent, _session) = progress_parent(
                    (&state, &directories),
                    &room,
                    &connection,
                    &descriptors,
                    997,
                )
                .await;
                let mut progress = connection
                    .handle
                    .approved_check_reported_progress(&request)
                    .await
                    .unwrap();
                let mut update = tilo_test_env::within(
                    "terminal outcome cannot depend on informational lane",
                    progress.read(),
                )
                .await;
                if let Ok(runtime_collab::approved_checks::ReportedProgressUpdate::Report(report)) =
                    &update
                {
                    assert!(matches!(
                        report.observation,
                        runtime_collab::approved_checks::ReportObservation::Disabled { .. }
                    ));
                    update = tilo_test_env::within(
                        "disabled report retains final receipt",
                        progress.read(),
                    )
                    .await;
                }
                if panic {
                    assert!(
                        update
                            .unwrap_err()
                            .to_string()
                            .contains("panicked; outcome unobserved")
                    );
                    assert!(
                        connection
                            .handle
                            .approved_check(&request)
                            .await
                            .unwrap_err()
                            .to_string()
                            .contains("panicked; outcome unobserved")
                    );
                } else {
                    let runtime_collab::approved_checks::ReportedProgressUpdate::Receipt(receipt) =
                        update.unwrap()
                    else {
                        panic!("terminal receipt required");
                    };
                    assert_eq!(
                        Some(receipt.as_ref().clone()),
                        *probe.receipt.lock().unwrap()
                    );
                    assert_eq!(
                        connection.handle.approved_check(&request).await.unwrap(),
                        *receipt
                    );
                    assert!(
                        progress.read().await.is_err(),
                        "client finality freezes further reads"
                    );
                }
                assert_eq!(
                    probe.calls.load(Ordering::SeqCst),
                    1,
                    "terminal observation never reexecutes the host service"
                );
            }
        })
        .await;
}

// A person's check reaches its genuine signed target, whose custody is checked before any execution service.
#[tokio::test(flavor = "current_thread")]
async fn approved_check_requires_exact_retained_target_and_accepted_id() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (target, _) = room.join_worker(981).await;
            let (other, _) = room.join_worker(982).await;
            let state = signed_worker::directory_state("approved-check-r1");
            let mut descriptor = state.borrow().row().descriptor.unwrap();
            descriptor.action = runtime_collab::approved_checks::ACTION_ID.into();
            descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
                kind: descriptor.resource_kind.clone(),
                key: descriptor.resource.clone(),
            });
            descriptor.operation_kind = tilo_actions::OperationKind::Effect;
            descriptor.contract = tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                result: tilo_actions::ResultSpec::Job,
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                ..Default::default()
            };
            target
                .handle
                .publish_actions(std::slice::from_ref(&descriptor))
                .await
                .unwrap();
            let stream = target.handle.subscribe_stable_invocations().await.unwrap();
            let session = PersonSession::open(&room.state, &room);
            let accepted = session
                .runs_check((&room.state, &room.directories), 981, "workspace:atlas")
                .await;
            let delivered = stream.read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *accepted.invocation.0.as_bytes());
            let request = runtime_collab::approved_checks::Request {
                version: 1,
                invocation: uuid::Uuid::from_bytes(delivered.invocation_uuid).to_string(),
                check_id: "regression".into(),
            };
            let error = target
                .handle
                .approved_check(&request)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("unsupported by this host"),
                "genuine accepted target custody must reach the real host-service gate: {error}"
            );
            let mut changed = descriptor.clone();
            changed.semantic_identity.as_mut().unwrap().key = "different-resource-key".into();
            target.handle.publish_actions(&[changed]).await.unwrap();
            assert!(
                target
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("semantic identity changed"),
                "same kind cannot replace the accepted resource key"
            );
            let mut missing = descriptor.clone();
            missing.semantic_identity = None;
            target.handle.publish_actions(&[missing]).await.unwrap();
            assert!(
                target.handle.approved_check(&request).await.is_err(),
                "missing live semantic identity refuses original custody"
            );
            target.handle.publish_actions(&[descriptor]).await.unwrap();
            let wrong = runtime_collab::approved_checks::Request {
                check_id: "other".into(),
                ..request.clone()
            };
            assert!(
                target
                    .handle
                    .approved_check(&wrong)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("differs from accepted action")
            );
            assert!(
                other
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("no retained target invocation")
            );
            let invocation =
                tilo_actions::InvocationId(uuid::Uuid::from_bytes(delivered.invocation_uuid));
            assert!(
                signed_worker::cancel(&target.handle, invocation)
                    .await
                    .is_err(),
                "only the person who asked may cancel their check"
            );
            session
                .cancels((&room.state, &room.directories), invocation)
                .await;
            assert!(
                target
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("retired or cancelled")
            );
            room.state.remove_connection(981);
            assert!(
                target.handle.approved_check(&request).await.is_err(),
                "retired connection cannot recover accepted target custody"
            );
        })
        .await;
}

// Actual kernel process plus launcher handoff exercises the native target branch.
// The fixture admits only its own process/app/room, and executes no guest command.
#[tokio::test(flavor = "current_thread")]
async fn approved_check_native_target_requires_live_launcher_connection() {
    struct ExactFixtureLaunch {
        pid: u32,
        app: String,
        room: String,
    }
    impl crate::services::LauncherServices for ExactFixtureLaunch {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("fixture never launches applications".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            pid == self.pid && app == self.app && room == self.room
        }
    }
    tokio::task::LocalSet::new().run_until(async {
        let room = signed_worker::OriginalRoom::new(tilo_user_identity::DeviceCapabilities::primary(), None, None);
        drop(room.state.journal.lock().unwrap().as_mut().unwrap().take());
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(room.membership.authority()),
            None, None,
            HostServices {
                    launcher: Some(Arc::new(ExactFixtureLaunch { pid: std::process::id(), app: room.route.app_id.clone(), room: room.route.cluster_id.clone() })),
                ..Default::default()
            },
        );
        assert!(state.certificate_authority.set(room.authority.clone()).is_ok());
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC).unwrap();
        let handoff = serde_json::to_vec(&serde_json::json!({"protocol":launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,"app_id":room.route.app_id,"workspace_id":room.route.cluster_id})).unwrap();
        std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_FRAMING.encode(&handoff).unwrap()).unwrap();
        let (rpc, server, client) = signed_worker::rpc_pair(state.clone(), 991, server, client, None, directories.clone());
        let handle = rpc.join_channel(&room.route.channel_key(), &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key)).await.unwrap();
        let connection = signed_worker::Connection { handle, server, client };
        let directory = signed_worker::directory_state("native-check-r1");
        let mut descriptor = directory.borrow().row().descriptor.unwrap();
        descriptor.action = runtime_collab::approved_checks::ACTION_ID.into();
            descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity { kind: descriptor.resource_kind.clone(), key: descriptor.resource.clone() });
        descriptor.operation_kind = tilo_actions::OperationKind::Effect;
        descriptor.contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        };
        connection.handle.publish_actions(std::slice::from_ref(&descriptor)).await.unwrap();
        let stream = connection.handle.subscribe_stable_invocations().await.unwrap();
        let session = PersonSession::open(&state, &room);
        let accepted = session.runs_check((&state, &directories), 991, "workspace:atlas").await;
        let delivered = stream.read().await.unwrap();
        assert_eq!(delivered.invocation_uuid, *accepted.invocation.0.as_bytes());
        let request = runtime_collab::approved_checks::Request { version: 1, invocation: uuid::Uuid::from_bytes(delivered.invocation_uuid).to_string(), check_id: "regression".into() };
        let error = connection.handle.approved_check(&request).await.unwrap_err().to_string();
        assert!(error.contains("unsupported by this host"), "actual native custody must reach the trusted service gate: {error}");
        let mut replacement = descriptor;
        replacement.resource = "fresh-opaque-resource-lifetime".into();
        connection.handle.publish_actions(&[replacement]).await.unwrap();
        assert!(connection.handle.approved_check(&request).await.unwrap_err().to_string().contains("publication retired"),
            "new publication cannot revive the old opaque target invocation");
        let fresh = session.runs_check((&state, &directories), 991, "fresh-opaque-resource-lifetime").await;
        let next = stream.read().await.unwrap();
        assert_eq!(next.invocation_uuid, *fresh.invocation.0.as_bytes());
        let new_request = runtime_collab::approved_checks::Request { invocation: uuid::Uuid::from_bytes(next.invocation_uuid).to_string(), ..request.clone() };
        assert!(connection.handle.approved_check(&new_request).await.unwrap_err().to_string().contains("unsupported by this host"),
            "fresh opaque address retains authenticated stable identity and reaches the real host service gate");
        let foreign_route = CrdtRoute::record(&room.route.app_id, &room.route.cluster_id, "foreign-record");
        let foreign = rpc.join_channel(&foreign_route.channel_key(), &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key)).await.unwrap();
        assert!(foreign.approved_check(&request).await.unwrap_err().to_string().contains("no retained target invocation"), "same process and app cannot send an accepted invocation over a foreign record channel: that record's own hold was never handed it");
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_bytes(delivered.invocation_uuid));
            assert!(
                signed_worker::cancel(&connection.handle, invocation).await.is_err(),
                "only the person who asked may cancel their check"
            );
            session.cancels((&state, &directories), invocation).await;
        assert!(connection.handle.approved_check(&request).await.unwrap_err().to_string().contains("retired or cancelled"));
        state.remove_connection(991);
        assert!(connection.handle.approved_check(&request).await.is_err());
    }).await;
}
