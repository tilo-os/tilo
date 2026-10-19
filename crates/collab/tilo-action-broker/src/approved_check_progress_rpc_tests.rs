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
async fn approved_check_reported_progress_rpc_preserves_original_custody_replay_and_gated_delivery()
{
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
            let (_state, connection, _directories) =
                progress_native_connection(&room, probe.clone())
                    .await
                    .unwrap();
            let (_invocations, request, _parent) =
                progress_parent(&connection, &room, &descriptors, 997).await;
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
async fn approved_check_reported_progress_rpc_refuses_cancelled_original_before_late_delivery() {
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
            let (state, connection, _directories) =
                progress_native_connection(&room, probe.clone())
                    .await
                    .unwrap();
            let (_invocations, request, parent) =
                progress_parent(&connection, &room, &descriptors, 997).await;
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
            assert!(
                connection
                    .handle
                    .cancel_published_invocation(parent.id)
                    .await
                    .is_err(),
                "certificate-owned jobs reject legacy actor-only cancellation"
            );
            let host = state
                .journal
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .host();
            let cancelled = connection
                .handle
                .cancel_durable_published_invocation(host.as_bytes(), &parent.invocation_uuid)
                .await
                .unwrap();
            assert!(cancelled.result.unwrap().cancellation_requested);
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
async fn approved_check_reported_progress_rpc_preserves_terminal_receipt_after_disabled_lane_and_host_panic()
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
                let (_state, connection, _directories) =
                    progress_native_connection(&room, probe.clone())
                        .await
                        .unwrap();
                let (_invocations, request, _parent) =
                    progress_parent(&connection, &room, &descriptors, 997).await;
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
