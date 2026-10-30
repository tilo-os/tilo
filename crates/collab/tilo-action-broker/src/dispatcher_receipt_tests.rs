/// Publish the fixture's plain read as the rig application's directory.
async fn publish_receipt_read(rig: &Rig) {
    let directory = directory_state("original-r1");
    rig.worker
        .handle
        .publish_action_directory(super::signed_worker::directory_server(&directory))
        .await
        .unwrap();
}

fn receipt_record(rig: &Rig, wire: u64) -> DurableInvocation {
    let key = rig
        .state
        .pending_invocations
        .lock()
        .unwrap()
        .get(&wire)
        .expect("invocation remains pending")
        .journal_key
        .clone()
        .expect("automatic journal admission");
    rig.state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(&key)
        .unwrap()
        .expect("admission retained")
}

fn receipt_barriers(rig: &Rig) -> invocation_journal::Barriers {
    rig.state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .barriers()
}

#[tokio::test(flavor = "current_thread")]
async fn actor_only_local_dispatch_never_receives_certified_evidence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let row = directory_state("original-r1").borrow().row();
            rig.worker
                .handle
                .publish_actions(&[row.descriptor.unwrap()])
                .await
                .unwrap();
            let actor = principal_id_for_root(env.cert.user_public_key);
            let job = rig
                .state
                .start_invocation_confirmed(903, "workspace:atlas", "open", None, &actor, None)
                .expect("legacy local read remains supported");
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            let record = receipt_record(&rig, delivered.id);
            assert!(
                record.admission.is_none(),
                "an actor string is not authenticated custody"
            );
            assert!(record.terminal.is_none());
            drop(job);
            rig.stop().await;
        })
        .await;
}

/// The app's own published read, as it asks it over its channel: what the
/// host answers.
async fn ask_own_read(
    rig: &Rig,
    request: tilo_actions::remote::InvokeRequest,
) -> tilo_actions::remote::InvocationReceipt {
    super::signed_worker::receipt(
        super::signed_worker::ask(
            &rig.worker.handle,
            tilo_actions::remote::RemoteActionMessage::Invoke(request),
        )
        .await
        .unwrap(),
    )
}

fn own_read(argument: &str) -> tilo_actions::remote::InvokeRequest {
    let mut request = local_host_request(true);
    request.argument = tilo_actions::remote::InvokeArgument::Text(argument.into());
    request
}

/// Dispatch the app's own read and accept the delivery as its worker does:
/// its wire id and admitted journal row.
async fn dispatch_receipt_read(rig: &Rig) -> (u64, DurableInvocation) {
    let receipt = ask_own_read(rig, own_read("private argument")).await;
    assert!(
        matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Accepted
        ),
        "{receipt:?}"
    );
    let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
    assert_eq!(delivered.invocation_uuid, *receipt.invocation.0.as_bytes());
    let lease = (rig.authorize)(&ui::invocation_scope::PublishedInvocationRequest {
        wire_id: delivered.id,
        invocation: receipt.invocation,
        address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
        argument: delivered.argument,
        actor: delivered.actor,
        participant: None,
    })
    .expect("the worker accepts the dispatcher's original custody");
    lease.check().unwrap();
    (delivered.id, receipt_record(rig, delivered.id))
}

/// The row the host's journal keeps for `key`.
fn retained(
    rig: &Rig,
    key: &crate::collab_broker_capnp::invocation_journal::InvocationKey,
) -> DurableInvocation {
    rig.state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(key)
        .unwrap()
        .expect("the journal keeps the row")
}

fn assert_receipt_substitution_refused(
    env: &Environment,
    evidence: &crate::action_evidence::AdmissionEvidence,
    expected: &crate::action_evidence::AdmissionFacts,
) {
    let receipt = evidence.public_receipt();
    for field in [
        "host",
        "room",
        "invocation",
        "actor",
        "app",
        "record",
        "resource",
        "action",
        "revision",
    ] {
        let mut altered = expected.clone();
        match field {
            "host" => altered.host = uuid::Uuid::new_v4(),
            "room" => altered.room.push('x'),
            "invocation" => altered.invocation = tilo_actions::InvocationId::new(),
            "actor" => {
                altered.actor = crate::action_evidence::VerifiedActor::AuthenticatedLocal {
                    root: env.cert.user_public_key,
                    device: [0; 32],
                    agent: None,
                }
            }
            "app" => altered.app_id.push('x'),
            "record" => altered.record_id.push('x'),
            "resource" => altered.resource.push('x'),
            "action" => altered.action.push('x'),
            "revision" => altered.revision = None,
            _ => unreachable!(),
        }
        assert!(
            crate::action_evidence::verify(receipt, &altered).is_err(),
            "substituted {field}"
        );
        let mut forged = receipt.clone();
        forged.facts = altered.clone();
        assert!(
            crate::action_evidence::verify(&forged, &altered).is_err(),
            "signature binds {field}"
        );
        if field == "actor" {
            assert!(evidence.opening_for_owner(&altered.actor).is_err());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_authenticated_read_is_admitted_with_private_signed_evidence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig).await;
            let before = receipt_barriers(&rig);
            let secret = "private read argument not for public evidence";
            let request = own_read(secret);
            let receipt = ask_own_read(&rig, request.clone()).await;
            assert!(
                matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Accepted
                ),
                "{receipt:?}"
            );
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            let record = receipt_record(&rig, delivered.id);
            let contract: tilo_actions::ActionContract =
                serde_json::from_value(record.intent["contract"].clone()).unwrap();
            assert_eq!(contract.kind, tilo_actions::ActionKind::Read);
            let evidence = record
                .admission
                .as_ref()
                .expect("the dispatcher signs the admission");
            let public = evidence.public_receipt();
            let expected = crate::action_evidence::AdmissionFacts {
                host: rig.state.invocation_host().unwrap(),
                room: env.route.cluster_id.clone(),
                invocation: request.invocation,
                actor: crate::action_evidence::VerifiedActor::AuthenticatedLocal {
                    root: env.cert.user_public_key,
                    device: env.cert.device_public_key,
                    agent: None,
                },
                actor_id: principal_id_for_root(env.cert.user_public_key),
                app_id: env.route.app_id.clone(),
                record_id: env.route.record_id.clone(),
                resource: "workspace:atlas".into(),
                action: "open".into(),
                revision: Some("original-r1".into()),
            };
            crate::action_evidence::verify(public, &expected).unwrap();
            evidence.validate_retained(&record.intent).unwrap();
            let opening = evidence.opening_for_owner(&expected.actor).unwrap();
            assert!(
                opening
                    .verify(public, &serde_json::json!({"argument": "substituted"}))
                    .is_err()
            );
            let text = serde_json::to_string(public).unwrap();
            for private in [
                secret,
                "opening",
                "nonce",
                "intent_digest",
                "request",
                "context",
            ] {
                assert!(!text.contains(private), "public receipt leaks {private}");
            }
            let after = receipt_barriers(&rig);
            assert_eq!(after.writes - before.writes, 1);
            assert_eq!(
                after.file - before.file,
                1,
                "signing shares admission's barrier"
            );
            assert_eq!(
                after.directory, before.directory,
                "no separate evidence file"
            );
            assert_receipt_substitution_refused(&env, evidence, &expected);

            // The same invocation asked again with another argument is not a
            // second admission.
            let mut substituted = request;
            substituted.argument =
                tilo_actions::remote::InvokeArgument::Text("substituted retry argument".into());
            let replay = ask_own_read(&rig, substituted).await;
            assert!(
                !matches!(replay.status, tilo_actions::remote::ReceiptStatus::Accepted),
                "{replay:?}"
            );
            assert_eq!(
                receipt_barriers(&rig),
                after,
                "a refused replay writes nothing"
            );
            rig.worker
                .handle
                .complete_invocation(delivered.id, "", "")
                .await
                .unwrap();
            no_delivery(&rig).await;
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_authenticated_dispatch_without_a_host_signer_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig).await;
            let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
            std::fs::remove_file(paths.device_privkey).unwrap();
            let before = receipt_barriers(&rig);
            let receipt = ask_own_read(&rig, own_read("unsigned")).await;
            assert!(
                format!("{receipt:?}").contains("action_evidence_unavailable"),
                "{receipt:?}"
            );
            assert_eq!(
                receipt_barriers(&rig),
                before,
                "no unsigned admission persisted"
            );
            assert!(rig.state.pending_invocations.lock().unwrap().is_empty());
            no_delivery(&rig).await;
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dispatcher_terminal_receipts_attest_only_the_observed_outcome() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::action_evidence::{TerminalCategory, verify_terminal};
            for category in [
                TerminalCategory::Succeeded,
                TerminalCategory::Failed,
                TerminalCategory::Cancelled,
            ] {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                // Cancellation is a declared capability, not a valid outcome
                // for the ordinary noncancellable read: that case keeps the
                // rig's cancellable effect.
                if category != TerminalCategory::Cancelled {
                    publish_receipt_read(&rig).await;
                }
                let (wire, admitted) = dispatch_receipt_read(&rig).await;
                assert!(admitted.terminal.is_none(), "admission is not completion");
                let evidence = admitted.admission.as_ref().unwrap();
                let invocation = admitted.key.invocation;
                let update = match category {
                    TerminalCategory::Succeeded => tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                    TerminalCategory::Failed => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "private_failure".into(),
                            message: "private failure detail".into(),
                            details: None,
                            retryable: false,
                        },
                    },
                    TerminalCategory::Cancelled => tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("private cancellation reason".into()),
                    },
                    TerminalCategory::Uncertain => unreachable!(),
                };
                let before = receipt_barriers(&rig);
                rig.worker
                    .handle
                    .update_invocation(wire, &serde_json::to_string(&update).unwrap())
                    .await
                    .unwrap();
                let after = receipt_barriers(&rig);
                // The cancellable effect also prepares its owner-accepted
                // outcome before the terminal write. Signing adds no write.
                let transitions = if category == TerminalCategory::Cancelled {
                    2
                } else {
                    1
                };
                assert_eq!(after.writes - before.writes, transitions);
                assert_eq!(
                    after.file - before.file,
                    transitions,
                    "the terminal signature shares the outcome barrier"
                );
                assert_eq!(after.directory, before.directory);
                let row = retained(&rig, &admitted.key);
                assert!(!row.uncertain);
                assert_eq!(row.update, update);
                let terminal = row.terminal.clone().expect("a signed terminal");
                assert_eq!(terminal.category, category);
                verify_terminal(&terminal, evidence.public_receipt()).unwrap();
                let mut forged = terminal.clone();
                forged.category = TerminalCategory::Uncertain;
                assert!(verify_terminal(&forged, evidence.public_receipt()).is_err());
                let mut different_admission = evidence.public_receipt().clone();
                different_admission.facts.invocation = tilo_actions::InvocationId::new();
                assert!(verify_terminal(&terminal, &different_admission).is_err());
                let public = serde_json::to_string(&terminal).unwrap();
                for private in [
                    "private argument",
                    "private_failure",
                    "private failure detail",
                    "private cancellation reason",
                    "opening",
                    "nonce",
                ] {
                    assert!(!public.contains(private), "terminal leaks {private}");
                }
                no_delivery(&rig).await;
                let restarted = rig.restarted(&env).await;
                let recovered = retained(&restarted, &admitted.key);
                assert_eq!(recovered.update, update, "recovery keeps the outcome");
                assert_eq!(recovered.terminal, Some(terminal));
                assert_eq!(recovered.admission.as_ref(), Some(evidence));
                no_delivery(&restarted).await;
                restarted.stop().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dispatcher_retained_evidence_survives_uncertain_recovery_and_rejects_intent_tampering() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig).await;
            let (_, admitted) = dispatch_receipt_read(&rig).await;
            let evidence = admitted.admission.clone().unwrap();
            // Retire the broker before the worker disconnects: an orderly
            // disconnect records a signed uncertain outcome, and this case
            // recovers an admission that never reached a terminal.
            rig.state.stop();
            let restarted = rig.restarted(&env).await;
            let recovered = retained(&restarted, &admitted.key);
            assert!(recovered.uncertain);
            assert!(matches!(&recovered.update,
                tilo_actions::InvocationUpdate::Failed { error, .. }
                    if error.code == "action_outcome_unknown" && !error.retryable));
            assert!(
                recovered.terminal.is_none(),
                "recovery invents no signed completion"
            );
            assert_eq!(recovered.admission.as_ref(), Some(&evidence));
            no_delivery(&restarted).await;
            let gone = Arc::downgrade(&restarted.state);
            restarted.stop().await;
            super::broker_gone(gone).await;

            // Recovery compacted the journal into a snapshot: a changed
            // retained field no longer matches the signed admission.
            let root = env.root.path().join("workspace.json.invocations");
            let path = root.join("records.json");
            let bytes = std::fs::read(&path).unwrap();
            let snapshot: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            for field in [
                "context",
                "contract",
                "operation_kind",
                "actor",
                "resource_kind",
            ] {
                let mut altered = snapshot.clone();
                altered["records"][0]["intent"][field] = if field == "actor" {
                    serde_json::json!("substituted actor")
                } else {
                    serde_json::json!({"substituted": field})
                };
                std::fs::write(&path, serde_json::to_vec(&altered).unwrap()).unwrap();
                let Err(error) = InvocationJournal::open(&root) else {
                    panic!("tampered retained {field} was accepted");
                };
                let expected = if field == "actor" {
                    "admission facts do not match"
                } else {
                    "retained admission intent differs from signed intent"
                };
                assert_eq!(error, expected, "tampered {field}");
            }
            for digest in [serde_json::json!("00".repeat(32)), serde_json::Value::Null] {
                let mut altered = snapshot.clone();
                if digest.is_null() {
                    altered["records"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("request_digest");
                } else {
                    altered["records"][0]["request_digest"] = digest;
                }
                std::fs::write(&path, serde_json::to_vec(&altered).unwrap()).unwrap();
                let Err(error) = InvocationJournal::open(&root) else {
                    panic!("tampered retry digest was accepted");
                };
                assert_eq!(error, "retained request digest differs from signed request");
            }
            std::fs::write(&path, bytes).unwrap();
            let restored = InvocationJournal::open(&root).unwrap();
            let recovered = restored.lookup(&admitted.key).unwrap().unwrap();
            assert_eq!(recovered.admission.as_ref(), Some(&evidence));
            evidence.validate_retained(&recovered.intent).unwrap();
            assert!(recovered.uncertain);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dispatcher_terminal_without_signer_retains_outcome_without_fabricating_receipt() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig).await;
            let (wire, admitted) = dispatch_receipt_read(&rig).await;
            let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
            std::fs::remove_file(paths.device_privkey).unwrap();
            let before = receipt_barriers(&rig);
            rig.worker
                .handle
                .complete_invocation(wire, "", "")
                .await
                .unwrap();
            let after = receipt_barriers(&rig);
            assert_eq!(after.writes - before.writes, 1);
            assert_eq!(after.file - before.file, 1);
            assert_eq!(after.directory, before.directory);
            let succeeded = tilo_actions::InvocationUpdate::Succeeded {
                invocation: admitted.key.invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            let row = retained(&rig, &admitted.key);
            assert!(!row.uncertain);
            assert!(row.terminal.is_none());
            assert_eq!(row.update, succeeded);
            no_delivery(&rig).await;
            let restarted = rig.restarted(&env).await;
            let recovered = retained(&restarted, &admitted.key);
            assert_eq!(recovered.update, succeeded);
            assert!(
                recovered.terminal.is_none(),
                "a retained unsigned outcome is not certified later"
            );
            no_delivery(&restarted).await;
            restarted.stop().await;
        })
        .await;
}
