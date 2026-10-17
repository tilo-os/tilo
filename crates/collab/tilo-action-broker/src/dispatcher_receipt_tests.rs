// Included in durable_local_matrix: exercise the signed RPC and real dispatcher,
// not the receipt minting helper or an explicitly durable invocation endpoint.
async fn publish_receipt_read(rig: &Rig, env: &Environment) {
    let directory = directory_state("original-r1");
    rig.worker
        .handle
        .publish_action_directory(
            &env.route.app_id,
            super::signed_worker::directory_server(&directory),
        )
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
        .barriers
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
    let mut forged = receipt.clone();
    forged.intent_commitment[0] ^= 1;
    assert!(crate::action_evidence::verify(&forged, expected).is_err());
}

async fn assert_receipt_observation(
    rig: &Rig,
    env: &Environment,
    expected: &crate::action_evidence::AdmissionFacts,
    after: invocation_journal::Barriers,
    evidence: &crate::action_evidence::AdmissionEvidence,
) {
    let invocation = expected.invocation;
    // The ordinary path's automatic journal row is observable only by its
    // original authenticated owner through the retained-observation door.
    let observed = rig
        .worker
        .handle
        .published_invocation_observation(expected.host.as_bytes(), invocation.0.as_bytes())
        .await
        .unwrap();
    let result = observed
        .result
        .as_ref()
        .expect("original owner sees admission");
    let decoded: crate::action_evidence::AdmissionEvidence =
        serde_json::from_str(result.admission_json.as_deref().expect("private evidence")).unwrap();
    assert_eq!(
        &decoded, evidence,
        "observation preserves the exact private evidence"
    );
    assert_eq!(decoded.public_receipt(), evidence.public_receipt());
    decoded.validate(expected).unwrap();
    assert_eq!(
        serde_json::to_value(decoded.opening_for_owner(&expected.actor).unwrap()).unwrap(),
        serde_json::to_value(evidence.opening_for_owner(&expected.actor).unwrap()).unwrap()
    );
    let mut stranger_actor = expected.actor.clone();
    if let crate::action_evidence::VerifiedActor::AuthenticatedLocal { agent, .. } =
        &mut stranger_actor
    {
        *agent = Some("receipt-observer".into());
    }
    assert!(decoded.opening_for_owner(&stranger_actor).is_err());
    assert_eq!(
        observed,
        rig.worker
            .handle
            .published_invocation_observation(expected.host.as_bytes(), invocation.0.as_bytes(),)
            .await
            .unwrap()
    );
    assert_eq!(
        receipt_barriers(rig),
        after,
        "observation replay neither appends nor dispatches"
    );
    use crate::test_support::{Grants as _, KindSet};
    env.membership
        .write()
        .workspaces
        .get_mut(0)
        .expect("workspace exists")
        .admit_agent("receipt-observer", KindSet::READ);
    env.membership
        .write()
        .workspaces
        .get_mut(0)
        .expect("workspace exists")
        .allow_app(
            tilo_action_policy::Subject::agent("receipt-observer"),
            &env.route.app_id,
            KindSet::ALL,
        );
    let stranger = rig.caller(env, 904, Some("receipt-observer")).await;
    assert!(
        stranger
            .handle
            .published_invocation_observation(expected.host.as_bytes(), invocation.0.as_bytes(),)
            .await
            .unwrap()
            .result
            .is_none(),
        "another authenticated actor cannot read the row or opening"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn nondurable_authenticated_read_dispatch_has_private_signed_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig, &env).await;
            let before = receipt_barriers(&rig);
            let secret = "private read argument not for public evidence";
            let (wire, _) = rig
                .worker
                .handle
                .invoke_resolved_published_action(
                    903,
                    "original-r1",
                    "workspace:atlas",
                    "open",
                    Some(secret),
                    "{}",
                )
                .await
                .expect("ordinary nondurable authenticated read");
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.id, wire);
            let invocation =
                tilo_actions::InvocationId(uuid::Uuid::from_bytes(delivered.invocation_uuid));
            let lease = (rig.authorize)(&ui::invocation_scope::PublishedInvocationRequest {
                wire_id: wire,
                invocation,
                address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
                argument: delivered.argument,
                actor: delivered.actor,
                participant: None,
            })
            .expect("real worker accepts the dispatcher's original custody");
            lease.check().unwrap();
            let record = receipt_record(&rig, wire);
            let contract: tilo_actions::ActionContract =
                serde_json::from_value(record.intent["contract"].clone()).unwrap();
            assert_eq!(contract.kind, tilo_actions::ActionKind::Read);
            let evidence = record
                .admission
                .as_ref()
                .expect("dispatcher mints signed admission");
            let receipt = evidence.public_receipt();
            let expected = crate::action_evidence::AdmissionFacts {
                host: rig.state.invocation_host().unwrap(),
                room: env.route.cluster_id.clone(),
                invocation,
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
            crate::action_evidence::verify(receipt, &expected).unwrap();
            let opening = evidence.opening_for_owner(&expected.actor).unwrap();
            evidence.validate_retained(&record.intent).unwrap();
            let mut exact_intent = record.intent.clone();
            exact_intent["request"] = serde_json::json!({
                "peer_id": 903,
                "app_id": env.route.app_id,
                "record_id": env.route.record_id,
                "resource": "workspace:atlas",
                "action": "open",
                "argument": secret,
                "original_request": null,
            });
            opening.verify(receipt, &exact_intent).unwrap();
            assert!(
                opening
                    .verify(receipt, &serde_json::json!({"argument": "substituted"}))
                    .is_err()
            );
            let public = serde_json::to_string(receipt).unwrap();
            for private in [
                secret,
                "opening",
                "nonce",
                "intent_digest",
                "request",
                "context",
            ] {
                assert!(!public.contains(private), "public receipt leaks {private}");
            }
            let after = receipt_barriers(&rig);
            assert_eq!(after.writes - before.writes, 1);
            assert_eq!(
                after.file - before.file,
                1,
                "signing shares admission's data barrier"
            );
            assert_eq!(
                after.directory, before.directory,
                "no separate evidence file"
            );

            assert_receipt_substitution_refused(&env, evidence, &expected);
            // A different endpoint cannot reuse the automatically admitted UUID
            // with a substituted request, even though actor and target match.
            assert!(
                rig.worker
                    .handle
                    .invoke_resolved_published_action_durable(
                        invocation.0.as_bytes(),
                        &rig.binding,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                        Some("substituted retry argument"),
                        "{}",
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                receipt_barriers(&rig),
                after,
                "rejected replay adds no journal barrier"
            );

            assert_receipt_observation(&rig, &env, &expected, after, evidence).await;
            // A read with no result or mutation still completes through the real
            // worker lifecycle, rather than leaving an admitted job behind.
            rig.worker
                .handle
                .complete_invocation(wire, "", "")
                .await
                .unwrap();
            no_delivery(&rig).await;
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn nondurable_authenticated_dispatch_without_host_identity_fails_closed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig, &env).await;
            let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
            std::fs::remove_file(paths.device_privkey).unwrap();
            let before = receipt_barriers(&rig);
            let error = rig
                .worker
                .handle
                .invoke_resolved_published_action(
                    903,
                    "original-r1",
                    "workspace:atlas",
                    "open",
                    None,
                    "{}",
                )
                .await
                .expect_err("authenticated dispatch requires a real host signer");
            assert!(
                error.to_string().contains("action_evidence_unavailable"),
                "{error}"
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

async fn dispatch_receipt_read(rig: &Rig) -> (u64, DurableInvocation) {
    let (wire, _) = rig
        .worker
        .handle
        .invoke_resolved_published_action(
            903,
            "original-r1",
            "workspace:atlas",
            "open",
            Some("private argument"),
            "{}",
        )
        .await
        .unwrap();
    let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
    assert_eq!(delivered.id, wire);
    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_bytes(delivered.invocation_uuid));
    let lease = (rig.authorize)(&ui::invocation_scope::PublishedInvocationRequest {
        wire_id: wire,
        invocation,
        address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
        argument: delivered.argument,
        actor: delivered.actor,
        participant: None,
    })
    .unwrap();
    lease.check().unwrap();
    (wire, receipt_record(rig, wire))
}

#[tokio::test(flavor = "current_thread")]
async fn dispatcher_terminal_receipts_attest_only_the_observed_outcome() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::action_evidence::{TerminalCategory, TerminalReceipt, verify_terminal};
            for category in [
                TerminalCategory::Succeeded,
                TerminalCategory::Failed,
                TerminalCategory::Cancelled,
            ] {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                if category == TerminalCategory::Cancelled {
                    // Cancellation is a declared capability, not a valid outcome
                    // for the ordinary noncancellable read used by other cases.
                    let directory = DurableDirectory {
                        state: directory_state("original-r1"),
                    };
                    rig.worker
                        .handle
                        .publish_action_directory(
                            &env.route.app_id,
                            capnp_rpc::new_client(directory),
                        )
                        .await
                        .unwrap();
                } else {
                    publish_receipt_read(&rig, &env).await;
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
                    "terminal signature shares the outcome barrier"
                );
                assert_eq!(after.directory, before.directory);
                let observed = rig
                    .worker
                    .handle
                    .published_invocation_observation(
                        admitted.key.host.as_bytes(),
                        invocation.0.as_bytes(),
                    )
                    .await
                    .unwrap();
                let result = observed.result.as_ref().unwrap();
                assert!(!result.uncertain);
                assert_eq!(
                    serde_json::from_str::<tilo_actions::InvocationUpdate>(&result.update_json)
                        .unwrap(),
                    update
                );
                let terminal: TerminalReceipt =
                    serde_json::from_str(result.terminal_json.as_deref().unwrap()).unwrap();
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
                assert_receipt_observation(&rig, &env, &evidence.receipt.facts, after, evidence)
                    .await;
                no_delivery(&rig).await;
                rig.stop().await;
                let restarted = Rig::new(&env).await;
                let before = receipt_barriers(&restarted);
                assert_eq!(
                    observed,
                    restarted
                        .worker
                        .handle
                        .published_invocation_observation(
                            admitted.key.host.as_bytes(),
                            invocation.0.as_bytes(),
                        )
                        .await
                        .unwrap(),
                    "recovery preserves the original terminal and admission"
                );
                assert_eq!(receipt_barriers(&restarted), before);
                no_delivery(&restarted).await;
                restarted.stop().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dispatcher_retained_evidence_survives_uncertain_recovery_and_rejects_intent_tampering() {
    tokio::task::LocalSet::new().run_until(async {
        let env = Environment::new();
        let rig = Rig::new(&env).await;
        publish_receipt_read(&rig, &env).await;
        let (_, admitted) = dispatch_receipt_read(&rig).await;
        let evidence = admitted.admission.as_ref().unwrap();
        // Retire the broker before disconnecting the worker: an orderly worker
        // disconnect persists a signed uncertain outcome, whereas this case
        // must recover an admission that never received a terminal transition.
        rig.state.stop();
        rig.stop().await;
        let restarted = Rig::new(&env).await;
        let before = receipt_barriers(&restarted);
        let observed = restarted.worker.handle.published_invocation_observation(
            admitted.key.host.as_bytes(), admitted.key.invocation.0.as_bytes(),
        ).await.unwrap();
        let result = observed.result.as_ref().unwrap();
        assert!(result.uncertain);
        assert!(matches!(serde_json::from_str::<tilo_actions::InvocationUpdate>(&result.update_json).unwrap(),
            tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable));
        assert!(result.terminal_json.is_none(), "recovery must not invent a signed completion");
        assert_receipt_observation(&restarted, &env, &evidence.receipt.facts, before, evidence).await;
        no_delivery(&restarted).await;
        restarted.stop().await;

        // Recovery compacted the real dispatch journal into a snapshot. Change
        // retained fields to exercise both the signed retained projection and
        // the public receipt's actor fact comparison.
        let root = env.root.path().join("workspace.json.invocations");
        let path = root.join("records.json");
        let bytes = std::fs::read(&path).unwrap();
        let snapshot: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for field in ["context", "contract", "operation_kind", "actor", "resource_kind"] {
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
            let expected_error = if field == "actor" {
                "admission facts do not match"
            } else {
                "retained admission intent differs from signed intent"
            };
            assert_eq!(error, expected_error, "tampered {field}");
        }
        std::fs::write(&path, bytes).unwrap();
        let restored = InvocationJournal::open(&root).unwrap();
        let recovered = restored.lookup(&admitted.key).unwrap().unwrap();
        assert_eq!(recovered.admission.as_ref().unwrap(), evidence);
        assert_eq!(recovered.intent, admitted.intent);
        evidence.validate_retained(&recovered.intent).unwrap();
        assert!(recovered.terminal.is_none());
        assert!(recovered.uncertain);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dispatcher_terminal_without_signer_retains_outcome_without_fabricating_receipt() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            publish_receipt_read(&rig, &env).await;
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
            let observed = rig
                .worker
                .handle
                .published_invocation_observation(
                    admitted.key.host.as_bytes(),
                    admitted.key.invocation.0.as_bytes(),
                )
                .await
                .unwrap();
            let result = observed.result.as_ref().unwrap();
            assert!(!result.uncertain);
            assert!(result.terminal_json.is_none());
            assert_eq!(
                serde_json::from_str::<tilo_actions::InvocationUpdate>(&result.update_json)
                    .unwrap(),
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: admitted.key.invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            );
            no_delivery(&rig).await;
            rig.stop().await;
            let restarted = Rig::new(&env).await;
            assert_eq!(
                observed,
                restarted
                    .worker
                    .handle
                    .published_invocation_observation(
                        admitted.key.host.as_bytes(),
                        admitted.key.invocation.0.as_bytes(),
                    )
                    .await
                    .unwrap(),
                "a retained unsigned outcome is not retroactively certified"
            );
            no_delivery(&restarted).await;
            restarted.stop().await;
        })
        .await;
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
                .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
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
