#[tokio::test(flavor = "current_thread")]
async fn installed_launch_requires_signed_admission_and_durable_terminal() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::collab_broker_capnp::invocation_journal::{
                BarrierFailure, InvocationKey, InvocationOwner,
            };
            for scenario in [
                "ready",
                "timeout",
                "observer_lost",
                "spawn_failed",
                "timeout_terminal",
                "file",
                "directory",
                "after_admission",
                "no_journal",
                "terminal",
                "revoked_at_effect",
            ] {
                let env = Environment::new();
                let (started, started_rx) = std::sync::mpsc::channel();
                let (adopt, receiver) = tokio::sync::oneshot::channel();
                let launcher = Arc::new(ObservedLauncher {
                    started,
                    receiver: Mutex::new(Some(receiver)),
                    requests: Mutex::new(Vec::new()),
                    adoption: Mutex::new(None),
                    original: Mutex::new(None),
                });
                let rig = Rig::with_services(
                    &env,
                    HostServices {
                        launcher: Some(launcher.clone()),
                        ..env.membership.services()
                    },
                )
                .await;
                let caller = rig.caller(&env, 905, None).await;
                {
                    let mut guard = rig.state.journal.lock().unwrap();
                    if scenario == "no_journal" {
                        *guard = Ok(None);
                    } else {
                        guard.as_mut().unwrap().as_mut().unwrap().fail_at = match scenario {
                            "file" => Some(BarrierFailure::File),
                            "directory" => Some(BarrierFailure::Directory),
                            "after_admission" => Some(BarrierFailure::AfterAdmission),
                            _ => None,
                        };
                    }
                }
                if scenario == "revoked_at_effect" {
                    let membership = env.membership.clone();
                    rig.state.set_observed_launch_effect_hook(move || {
                        membership.write().workspaces[0].revoke_grants_matching(|_| true);
                    });
                }
                let handle = caller.handle.clone();
                let root = format!("tapp:{}", "34".repeat(32));
                let selected = root.clone();
                let mut pending = tokio::task::spawn_local(async move {
                    handle
                        .launch_installed_application(
                            "so.tilo.word-processor",
                            None,
                            Some(&selected),
                        )
                        .await
                });
                if !matches!(
                    scenario,
                    "ready"
                        | "terminal"
                        | "timeout"
                        | "observer_lost"
                        | "spawn_failed"
                        | "timeout_terminal"
                ) {
                    let result = pending.await.unwrap();
                    if scenario == "revoked_at_effect" {
                        let (_, json) =
                            result.expect("retained admission receives a signed failure");
                        let update: tilo_actions::InvocationUpdate =
                            serde_json::from_str(&json).unwrap();
                        assert!(matches!(
                            update,
                            tilo_actions::InvocationUpdate::Failed { .. }
                        ));
                    } else {
                        assert!(result.is_err(), "{scenario}");
                    }
                    assert!(launcher.requests.lock().unwrap().is_empty(), "{scenario}");
                    assert!(started_rx.try_recv().is_err(), "{scenario}");
                    rig.stop().await;
                    continue;
                }
                tokio::select! {
                    () = certified_launch_wait(&started_rx) => {},
                    result = &mut pending => panic!("launch refused before readiness: {result:?}"),
                }
                assert!(!pending.is_finished(), "admission is not readiness");
                let owner = InvocationOwner::AuthenticatedLocal {
                    root: env.cert.user_public_key,
                    device: env.cert.device_public_key,
                    agent: None,
                };
                let statuses = rig.state.invocation_statuses_for_owner(&owner);
                let (wire, accepted) = statuses
                    .iter()
                    .find(|(_, update)| {
                        matches!(update, tilo_actions::InvocationUpdate::Accepted { .. })
                    })
                    .expect("signed owner can observe admission while startup waits");
                let key = {
                    let guard = rig.state.journal.lock().unwrap();
                    let journal = guard.as_ref().unwrap().as_ref().unwrap();
                    let key = InvocationKey {
                        room: env.route.cluster_id.clone(),
                        host: journal.host(),
                        owner: InvocationOwner::AuthenticatedLocal {
                            root: env.cert.user_public_key,
                            device: env.cert.device_public_key,
                            agent: None,
                        },
                        invocation: accepted.invocation(),
                    };
                    let record = journal
                        .lookup(&key)
                        .unwrap()
                        .expect("admitted before launch");
                    assert!(
                        record.admission.is_some(),
                        "certificate-backed signed admission"
                    );
                    assert!(record.terminal.is_none());
                    assert_eq!(record.intent["host_start"]["package_root"], root);
                    assert_eq!(
                        record.intent["request"]["on_behalf_of"],
                        serde_json::Value::Null
                    );
                    key
                };
                if matches!(scenario, "terminal" | "timeout_terminal") {
                    rig.state
                        .journal
                        .lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .fail_at = Some(BarrierFailure::File);
                }
                match scenario {
                    "timeout" | "timeout_terminal" => {
                        tokio::time::pause();
                        tokio::time::advance(crate::services::APPLICATION_STARTUP_TIMEOUT).await;
                    }
                    "observer_lost" => drop(adopt),
                    "spawn_failed" => {
                        adopt
                            .send(Err("signed worker spawn refused".into()))
                            .unwrap();
                    }
                    _ => adopt.send(Ok(())).unwrap(),
                }
                let result = pending.await.unwrap();
                if matches!(scenario, "timeout" | "timeout_terminal") {
                    tokio::time::resume();
                }
                if matches!(scenario, "terminal" | "timeout_terminal") {
                    assert!(
                        result.is_err(),
                        "a failed terminal barrier cannot return success"
                    );
                    assert!(!matches!(
                        rig.state.invocation_status(*wire),
                        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                    ));
                } else {
                    let (id, json) = result.unwrap();
                    assert_eq!(id, *wire);
                    let update: tilo_actions::InvocationUpdate =
                        serde_json::from_str(&json).unwrap();
                    assert_eq!(update.invocation(), key.invocation);
                    let successful = scenario == "ready";
                    assert_eq!(
                        matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                        successful,
                        "{scenario}: {update:?}"
                    );
                    if !successful {
                        assert!(matches!(
                            update,
                            tilo_actions::InvocationUpdate::Failed { .. }
                        ));
                    }
                    assert_eq!(
                        caller.handle.published_invocation_status(id).await.unwrap(),
                        Some(json)
                    );
                    let guard = rig.state.journal.lock().unwrap();
                    let record = guard
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .lookup(&key)
                        .unwrap()
                        .unwrap();
                    use crate::action_evidence::{TerminalCategory, verify_terminal};
                    let terminal = record
                        .terminal
                        .as_ref()
                        .expect("completion is durably signed");
                    let expected = match scenario {
                        "timeout" | "observer_lost" => TerminalCategory::Uncertain,
                        "spawn_failed" => TerminalCategory::Failed,
                        _ => TerminalCategory::Succeeded,
                    };
                    assert_eq!(terminal.category, expected, "{scenario}");
                    verify_terminal(
                        terminal,
                        record.admission.as_ref().unwrap().public_receipt(),
                    )
                    .unwrap();
                    assert_eq!(record.update, update);
                }
                rig.stop().await;
            }
        })
        .await;
}
