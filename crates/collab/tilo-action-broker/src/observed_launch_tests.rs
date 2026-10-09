// Real certified channel and installed-launch RPC; only desktop adoption is
// controlled so a receipt can be observed before and after the actual signal.
type ObservedLaunch = (String, Option<String>, String, Option<String>);

#[tokio::test(flavor = "current_thread")]
async fn observed_person_launch_retains_exact_original_scope_and_cancellation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let (started, started_rx) = std::sync::mpsc::channel();
            let (adopt, receiver) = tokio::sync::oneshot::channel();
            let launcher = Arc::new(ObservedLauncher {
                started,
                receiver: Mutex::new(Some(receiver)),
                requests: Mutex::new(Vec::new()),
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
            let (person, evidence) = local_host_evidence(&env);
            let facade = crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade.certified_local_action_host(&person, &evidence).unwrap();
            let original = id(0xc529);
            let mut request = local_host_request(true);
            request.invocation = original;
            let admitted = host.invoke(&person, request);
            rig.state.drain_action_directory_bridge(&rig.directories);
            local_host_answer(admitted).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *original.0.as_bytes());
            let handle = rig.worker.handle.clone();
            let root = format!("tapp:{}", "12".repeat(32));
            let selected = root.clone();
            let mut pending = tokio::task::spawn_local(async move {
                handle
                    .launch_installed_application_for(
                        "so.tilo.word-processor",
                        None,
                        Some(&selected),
                        Some(*original.0.as_bytes()),
                    )
                    .await
            });
            tokio::select! {
                () = certified_launch_wait(&started_rx) => {},
                result = &mut pending => panic!("original launch refused before host admission: {result:?}"),
            }
            let custody = launcher
                .original
                .lock()
                .unwrap()
                .clone()
                .expect("real original is retained");
            let workspace = env.membership.read().workspaces[0].clone();
            assert!(custody.matches("so.tilo.word-processor", Some(&root), &workspace));
            assert!(!custody.matches("so.tilo.chat", Some(&root), &workspace));
            assert!(!custody.matches("so.tilo.word-processor", None, &workspace));
            assert_eq!(custody.certificate().unwrap(),
                *env.authority.additional_certificates.lock().unwrap().last().unwrap());
            assert!(!pending.is_finished(), "custody is not actual adoption");
            let grants = std::mem::take(&mut env.membership.write().workspaces[0].capabilities);
            assert!(custody.certificate().is_err(), "fresh rights, not the launch snapshot, govern admission");
            env.membership.write().workspaces[0].capabilities = grants;
            assert!(custody.certificate().is_ok());
            host.cancel(&person, &original).unwrap();
            assert!(
                custody.certificate().is_err(),
                "cancelled original cannot be reused"
            );
            assert!(custody.delegate_authorship().is_err());
            adopt.send(Ok(())).unwrap();
            let (_, json) = pending.await.unwrap().unwrap();
            assert!(matches!(
                serde_json::from_str::<tilo_actions::InvocationUpdate>(&json).unwrap(),
                tilo_actions::InvocationUpdate::Failed { .. }
            ));
        })
        .await;
}

struct ObservedLauncher {
    started: std::sync::mpsc::Sender<()>,
    receiver: Mutex<Option<tokio::sync::oneshot::Receiver<Result<(), String>>>>,
    requests: Mutex<Vec<ObservedLaunch>>,
    original: Mutex<Option<crate::services::ObservedApplicationLaunch>>,
}
impl crate::services::LauncherServices for ObservedLauncher {
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
        Err("unobserved launches are not a readiness receipt".into())
    }
    fn begin_launch(
        &self,
        app: &str,
        root: Option<&str>,
        workspace: &tilo_desktop_model::Workspace,
        author: Option<&str>,
    ) -> Result<crate::services::LaunchReadiness, String> {
        self.requests.lock().unwrap().push((
            app.into(),
            root.map(str::to_owned),
            workspace.room.to_string(),
            author.map(str::to_owned),
        ));
        self.started.send(()).unwrap();
        Ok(crate::services::LaunchReadiness::new(
            self.receiver.lock().unwrap().take().unwrap(),
            || {},
        ))
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
    fn begin_launch_from_original(
        &self,
        app: &str,
        root: Option<&str>,
        workspace: &tilo_desktop_model::Workspace,
        author: Option<&str>,
        original: Option<crate::services::ObservedApplicationLaunch>,
    ) -> Result<crate::services::LaunchReadiness, String> {
        *self.original.lock().unwrap() = original;
        self.begin_launch(app, root, workspace, author)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn installed_launch_rpc_waits_for_adoption_preserves_signed_root_and_revalidates_actor() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for scenario in ["ready", "spawn_failed", "revoked", "expired"] {
                let env = Environment::new();
                let (started, started_rx) = std::sync::mpsc::channel();
                let (adopt, receiver) = tokio::sync::oneshot::channel();
                let launcher = Arc::new(ObservedLauncher {
                    started,
                    receiver: Mutex::new(Some(receiver)),
                    requests: Mutex::new(Vec::new()),
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
                let handle = caller.handle.clone();
                let root = format!("tapp:{}", "12".repeat(32));
                let selected = root.clone();
                let pending = tokio::task::spawn_local(async move {
                    handle
                        .launch_installed_application(
                            "so.tilo.word-processor",
                            None,
                            Some(&selected),
                        )
                        .await
                });
                certified_launch_wait(&started_rx).await;
                assert!(
                    !pending.is_finished(),
                    "worker admission is not actual startup"
                );
                assert_eq!(
                    *launcher.requests.lock().unwrap(),
                    vec![(
                        "so.tilo.word-processor".into(),
                        Some(root),
                        env.route.cluster_id.clone(),
                        Some(principal_id_for_root(env.cert.user_public_key))
                    )]
                );
                if scenario == "revoked" {
                    let mut tree = env.membership.write();
                    let grants = tree.workspaces[0].capabilities.clone();
                    for grant in grants {
                        tree.workspaces[0].revoke_capability(&grant.id);
                    }
                } else if scenario == "expired" {
                    env.control.advance_monotonic(Duration::from_secs(80));
                }
                adopt
                    .send(if scenario == "spawn_failed" {
                        Err("signed worker spawn refused".into())
                    } else {
                        Ok(())
                    })
                    .unwrap();
                let (id, encoded) = pending.await.unwrap().unwrap();
                let update: tilo_actions::InvocationUpdate =
                    serde_json::from_str(&encoded).unwrap();
                if scenario == "ready" {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Succeeded { .. }
                    ));
                    assert_eq!(
                        caller.handle.published_invocation_status(id).await.unwrap(),
                        Some(encoded)
                    );
                } else {
                    assert!(
                        matches!(update, tilo_actions::InvocationUpdate::Failed { .. }),
                        "{update:?}"
                    );
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn installed_launch_rpc_rejects_a_noncanonical_package_root_before_host_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let (started, started_rx) = std::sync::mpsc::channel();
            let (_adopt, receiver) = tokio::sync::oneshot::channel();
            let launcher = Arc::new(ObservedLauncher {
                started,
                receiver: Mutex::new(Some(receiver)),
                requests: Mutex::new(Vec::new()),
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
            assert!(
                caller
                    .handle
                    .launch_installed_application(
                        "so.tilo.word-processor",
                        None,
                        Some("../../payload")
                    )
                    .await
                    .is_err()
            );
            assert!(launcher.requests.lock().unwrap().is_empty());
            assert!(started_rx.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn installed_launch_continuation_keeps_the_original_agents_authority() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for scenario in ["denied", "ready", "revoked"] {
                let env = Environment::new();
                let (started, started_rx) = std::sync::mpsc::channel();
                let (adopt, receiver) = tokio::sync::oneshot::channel();
                let launcher = Arc::new(ObservedLauncher {
                    started,
                    receiver: Mutex::new(Some(receiver)),
                    requests: Mutex::new(Vec::new()),
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
                let room = env.route.cluster_id.clone();
                let target = "host:org.example.App.desktop";
                let target_grant = CapOverride::grant(
                    Subject::agent("scribe"),
                    collab_types::Scope::App(target.into()),
                    Verb::Write,
                );
                for cap in [
                    CapOverride::membership(Subject::agent("scribe"), room, Verb::Write),
                    CapOverride::grant(
                        Subject::agent("scribe"),
                        collab_types::Scope::App(env.route.app_id.clone()),
                        Verb::Write,
                    ),
                ] {
                    env.membership.write().workspaces[0].grant_capability(cap);
                }
                let target_grant_id = (scenario != "denied")
                    .then(|| env.membership.write().workspaces[0].grant_capability(target_grant));
                let agent = rig.caller(&env, 905, Some("scribe")).await;
                let original = id(0xc510);
                invoke(&agent.handle, &rig.binding, original).await.unwrap();
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(delivered.actor, "agent:scribe");
                // A different certified connection cannot borrow the worker's parent.
                assert!(
                    agent
                        .handle
                        .launch_installed_application_for(
                            target,
                            None,
                            None,
                            Some(*original.0.as_bytes())
                        )
                        .await
                        .is_err()
                );
                let handle = rig.worker.handle.clone();
                let pending = tokio::task::spawn_local(async move {
                    handle
                        .launch_installed_application_for(
                            target,
                            None,
                            None,
                            Some(*original.0.as_bytes()),
                        )
                        .await
                });
                if scenario == "denied" {
                    assert!(pending.await.unwrap().is_err());
                    assert!(started_rx.try_recv().is_err());
                    assert!(launcher.requests.lock().unwrap().is_empty());
                    continue;
                }
                certified_launch_wait(&started_rx).await;
                assert!(!pending.is_finished());
                assert_eq!(
                    launcher.requests.lock().unwrap()[0].3,
                    None,
                    "an agent does not become the Store owner's launch author"
                );
                if scenario == "revoked" {
                    assert!(
                        env.membership.write().workspaces[0]
                            .revoke_capability(target_grant_id.as_ref().unwrap())
                    );
                }
                adopt.send(Ok(())).unwrap();
                let (launch_id, json) = pending.await.unwrap().unwrap();
                let update: tilo_actions::InvocationUpdate = serde_json::from_str(&json).unwrap();
                assert_eq!(
                    matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                    scenario == "ready",
                    "{update:?}"
                );
                assert!(
                    rig.state
                        .invocation_statuses("agent:scribe")
                        .iter()
                        .any(|(id, _)| *id == launch_id)
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_person_installed_launch_keeps_host_origin_until_real_adoption() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for scenario in ["ready", "revoked"] {
                let env = Environment::new();
                let (started, started_rx) = std::sync::mpsc::channel();
                let (adopt, receiver) = tokio::sync::oneshot::channel();
                let launcher = Arc::new(ObservedLauncher {
                    started,
                    receiver: Mutex::new(Some(receiver)),
                    requests: Mutex::new(vec![]),
                    original: Mutex::new(None),
                });
                let rig = Rig::with_services(
                    &env,
                    HostServices {
                        local_person: Some(Arc::new(NativePersonFixture(env.authority.clone()))),
                        launcher: Some(launcher.clone()),
                        ..env.membership.services()
                    },
                )
                .await;
                let person = principal_id_for_root(env.cert.user_public_key);
                let caller = tilo_actions::remote::Caller::local_person(&person)
                    .in_room(env.route.cluster_id.clone());
                let original = tilo_actions::InvocationId::new();
                let pending_admission = rig.state.action_invoke_local_person(
                    &caller,
                    &tilo_actions::remote::InvokeRequest {
                        invocation: original,
                        on_behalf_of: None,
                        target: tilo_actions::remote::RemoteActionTarget {
                            provider: "903".into(),
                            address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                        },
                        argument: tilo_actions::remote::InvokeArgument::None,
                        context: None,
                        confirmation: None,
                        revision: Some("original-r1".into()),
                        wait_ms: None,
                    },
                );
                rig.state.drain_action_directory_bridge(&rig.directories);
                let admitted = local_host_answer(pending_admission).await.unwrap();
                assert_eq!(
                    admitted.status,
                    tilo_actions::remote::ReceiptStatus::Accepted
                );
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(delivered.actor, person);
                assert_eq!(delivered.invocation_uuid, original.0.into_bytes());
                let handle = rig.worker.handle.clone();
                let pending = tokio::task::spawn_local(async move {
                    handle
                        .launch_installed_application_for(
                            "host:org.example.App.desktop",
                            None,
                            None,
                            Some(original.0.into_bytes()),
                        )
                        .await
                });
                certified_launch_wait(&started_rx).await;
                assert!(
                    !pending.is_finished(),
                    "host origin is not a startup receipt"
                );
                assert_eq!(
                    launcher.requests.lock().unwrap()[0].3.as_deref(),
                    Some(person.as_str())
                );
                if scenario == "revoked" {
                    *env.authority.certificate.lock().unwrap() = None;
                }
                adopt.send(Ok(())).unwrap();
                let (_, json) = pending.await.unwrap().unwrap();
                let update: tilo_actions::InvocationUpdate = serde_json::from_str(&json).unwrap();
                assert_eq!(
                    matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                    scenario == "ready",
                    "{update:?}"
                );
                rig.stop().await;
            }
        })
        .await;
}
