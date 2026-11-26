type DisplayCall = (String, [u8; 32], [u8; 32]);
struct DisplayDelegate {
    calls: Mutex<Vec<DisplayCall>>,
}
impl crate::services::DisplayControlServices for DisplayDelegate {
    fn command(
        &self,
        context: &crate::services::DisplayControlContext,
        _: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.calls.lock().unwrap().push((
            context.workspace().into(),
            context.root(),
            context.device(),
        ));
        Ok(b"{}".to_vec())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn private_settings_display_rpc_requires_exact_registration_and_live_certificate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.settings".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let delegate = Arc::new(DisplayDelegate {
                calls: Mutex::new(vec![]),
            });
            let mut services = env.membership.services();
            services.settings = Some(Arc::new(settings_store::HostDocument::new(
                serde_json::Map::default(),
                |_| Ok(()),
            )));
            services.display_control = Some(delegate.clone());
            let fanout = Arc::new(FanoutState::new());
            let (state, bridge) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.authority()),
                None,
                None,
                services,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
            let (worker, _authorizer) =
                join_worker(&state, &directories, &env.route, &env.cert, 401).await;
            let settings = worker.handle.settings();
            assert_eq!(
                settings
                    .display_control(
                        b"{\"operation\":\"inspect\"}",
                        runtime_collab::settings::DisplayOrigin::LocalHost
                    )
                    .await
                    .unwrap(),
                b"{}"
            );
            assert_eq!(
                *delegate.calls.lock().unwrap(),
                vec![(
                    env.route.cluster_id.clone(),
                    env.cert.user_public_key,
                    env.cert.device_public_key
                )]
            );

            // A real descriptor-bearing worker without the private SDK registration
            // cannot borrow another worker's display capability, even with the same cert.
            let socket = state
                .connect_local_worker(env.route.clone(), &env.cert)
                .unwrap();
            let pending = state.workers.lock().unwrap().pop().unwrap();
            let (rpc, server, client) = rpc_pair(
                state.clone(),
                402,
                pending.stream,
                socket,
                Some(pending.binding),
                directories.clone(),
            );
            let ordinary = opened(&rpc, &env.route.channel_key()).await.unwrap();
            assert!(
                ordinary
                    .settings()
                    .display_control(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost)
                    .await
                    .is_err()
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            let foreign_device = member_device_certificate(&env, 96, true);
            for (peer, agent) in [(404, None), (405, Some("foreign-agent"))] {
                let (foreign_rpc, _server, _client) =
                    certified_rpc(&state, &directories, peer, foreign_device.clone(), agent);
                let joined = opened(&foreign_rpc, &env.route.channel_key()).await;
                if agent.is_some() {
                    assert!(
                        joined.is_err(),
                        "foreign devices cannot launch a delegated Agent channel"
                    );
                } else {
                    let foreign = joined.expect("a signed foreign person may join read-only");
                    assert!(
                        foreign
                            .settings()
                            .display_control(
                                b"{}",
                                runtime_collab::settings::DisplayOrigin::LocalHost
                            )
                            .await
                            .is_err(),
                        "foreign public channels cannot acquire private Settings mutation authority"
                    );
                }
            }
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            let identity = state
                .host_workers
                .lock()
                .unwrap()
                .only(401)
                .unwrap()
                .clone();
            let retained = display_authority(&state, &env.route, &identity).unwrap();
            let mut agent = identity.clone();
            agent.agent_id = Some("foreign-agent".into());
            assert!(display_authority(&state, &env.route, &agent).is_none());
            let mut other_app = env.route.clone();
            other_app.app_id = "so.tilo.browser".into();
            assert!(display_authority(&state, &other_app, &identity).is_none());
            *env.authority.certificate.lock().unwrap() = None;
            assert!(
                settings
                    .display_control(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost)
                    .await
                    .is_err()
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            *env.authority.certificate.lock().unwrap() = Some(env.cert.clone());
            state.remove_connection(401);
            assert!(retained(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost).is_err());
            let (replacement, _) =
                join_worker(&state, &directories, &env.route, &env.cert, 403).await;
            assert!(
                retained(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost).is_err(),
                "an equal certificate on a new connection cannot revive retired registration"
            );
            assert!(
                replacement
                    .handle
                    .settings()
                    .display_control(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost)
                    .await
                    .is_ok()
            );
            replacement.server.abort();
            replacement.client.abort();
            worker.server.abort();
            worker.client.abort();
            server.abort();
            client.abort();
            let _ = server.await;
            let _ = client.await;
            state.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_person_display_rpc_keeps_original_owner_and_rejects_agent_continuation_and_replacement()
 {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.settings".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let delegate = Arc::new(DisplayDelegate {
                calls: Mutex::new(vec![]),
            });
            let rig = Rig::with_services(
                &env,
                ProcessServices {
                    local_person: Some(Arc::new(super::signed_worker::NativePerson(
                        env.authority.clone(),
                    ))),
                    settings: Some(Arc::new(settings_store::HostDocument::new(
                        serde_json::Map::default(),
                        |_| Ok(()),
                    ))),
                    display_control: Some(delegate.clone()),
                    ..env.membership.services()
                },
            )
            .await;
            let mut contract = tilo_actions::ActionContract::view();
            contract.kind = tilo_actions::ActionKind::Outside;
            contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
            contract.result = tilo_actions::ResultSpec::Job;
            rig.worker
                .handle
                .publish_actions(&[runtime_collab::ExportedAction {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    resource: "settings.display".into(),
                    semantic_identity: None,
                    action: "identify-displays".into(),
                    resource_kind: "settings".into(),
                    resource_label: "Displays".into(),
                    action_label: "Identify".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    operation_kind: tilo_actions::OperationKind::View,
                    params: Vec::new(),
                    contract,
                    input: runtime_collab::ExportedInput::None,
                    view: None,
                }])
                .await
                .unwrap();
            let person = tilo_actions::remote::Caller::local_person(person_id_for_root(
                env.cert.user_public_key,
            ))
            .in_workspace(env.route.cluster_id.clone());
            let request = tilo_actions::remote::InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                on_behalf_of: None,
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: "903".into(),
                    address: tilo_actions::ActionAddress::new(
                        "settings.display",
                        "identify-displays",
                    ),
                },
                argument: tilo_actions::remote::InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            // The previous normal host api really admitted this action but
            // supplied no original ownership. The generic API still cannot
            // borrow native-owner identity just from a local-person label.
            let mut generic = request.clone();
            generic.invocation = tilo_actions::InvocationId::new();
            rig.state
                .action_invoke(&person, None, None, &generic)
                .await
                .unwrap()
                .unwrap();
            let generic_delivery = rig.stream.as_ref().unwrap().read().await.unwrap();
            let settings = rig.worker.handle.settings();
            assert!(
                settings
                    .display_control(
                        br#"{"operation":"identify"}"#,
                        runtime_collab::settings::DisplayOrigin::Published(
                            generic_delivery.invocation_uuid
                        )
                    )
                    .await
                    .is_err()
            );
            assert!(delegate.calls.lock().unwrap().is_empty());
            let admitted = rig.state.action_invoke_local_person(&person, &request);
            let receipt = admitted.await.unwrap().unwrap();
            assert!(matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ));
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, request.invocation.0.into_bytes());
            let origin =
                runtime_collab::settings::DisplayOrigin::Published(delivered.invocation_uuid);
            let settings = rig.worker.handle.settings();
            settings
                .display_control(br#"{"operation":"identify"}"#, origin)
                .await
                .unwrap();
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            let mut continuation = request.clone();
            continuation.invocation = tilo_actions::InvocationId::new();
            continuation.on_behalf_of = Some(request.invocation);
            assert!(
                rig.state
                    .action_invoke_local_person(&person, &continuation)
                    .await
                    .unwrap()
                    .is_err()
            );
            let agent =
                tilo_actions::remote::Caller::local_agent("native-origin-agent", "other-person")
                    .in_workspace(env.route.cluster_id.clone());
            assert!(
                rig.state
                    .action_invoke_local_person(&agent, &request)
                    .await
                    .unwrap()
                    .is_err()
            );
            let forged = tilo_actions::remote::Caller::local_person("other-person")
                .in_workspace(env.route.cluster_id.clone());
            assert!(
                rig.state
                    .action_invoke_local_person(&forged, &request)
                    .await
                    .unwrap()
                    .is_err()
            );
            assert!(
                rig.state
                    .continuing_caller(903, request.invocation)
                    .is_some()
            );
            *env.authority.certificate.lock().unwrap() = None;
            assert!(
                rig.state
                    .continuing_caller(903, request.invocation)
                    .is_none()
            );
            assert!(
                settings
                    .display_control(br#"{"operation":"identify"}"#, origin)
                    .await
                    .is_err()
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn display_control_keeps_the_original_published_caller_and_refuses_unknown_or_cancelled_ids()
{
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.settings".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let delegate = Arc::new(DisplayDelegate {
                calls: Mutex::new(vec![]),
            });
            let rig = Rig::with_services(
                &env,
                ProcessServices {
                    settings: Some(Arc::new(settings_store::HostDocument::new(
                        serde_json::Map::default(),
                        |_| Ok(()),
                    ))),
                    display_control: Some(delegate.clone()),
                    ..env.membership.services()
                },
            )
            .await;
            let mut contract = tilo_actions::ActionContract::view();
            contract.kind = tilo_actions::ActionKind::Outside;
            contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
            contract.result = tilo_actions::ResultSpec::Job;
            rig.worker
                .handle
                .publish_actions(&[runtime_collab::ExportedAction {
                    did: None,
                    resource: "settings.display".into(),
                    semantic_identity: None,
                    action: "identify-displays".into(),
                    resource_kind: "settings".into(),
                    resource_label: "Displays".into(),
                    action_label: "Identify".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    state: tilo_actions::ActionState::default(),
                    operation_kind: tilo_actions::OperationKind::View,
                    params: Vec::new(),
                    contract,
                    input: runtime_collab::ExportedInput::None,
                    view: None,
                }])
                .await
                .unwrap();
            // The application that launched this connection is the one whose
            // actions these are: its own api reaches them.
            let mut caller = rig.app_caller(&env, 905, "so.tilo.settings", None).await;
            let settings = rig.worker.handle.settings();
            let receipt = identify_displays(&caller.handle).await;
            let original = runtime_collab::settings::DisplayOrigin::Published(
                receipt.invocation.0.into_bytes(),
            );
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, receipt.invocation.0.into_bytes());
            settings
                .display_control(br#"{"operation":"identify"}"#, original)
                .await
                .unwrap();
            assert_eq!(
                *delegate.calls.lock().unwrap(),
                vec![(
                    env.route.cluster_id.clone(),
                    env.cert.user_public_key,
                    env.cert.device_public_key
                )]
            );
            assert!(
                settings
                    .display_control(
                        br#"{"operation":"identify"}"#,
                        runtime_collab::settings::DisplayOrigin::Published(
                            uuid::Uuid::new_v4().into_bytes()
                        )
                    )
                    .await
                    .is_err()
            );
            super::signed_worker::cancel(&caller.handle, receipt.invocation)
                .await
                .unwrap();
            assert!(
                settings
                    .display_control(br#"{"operation":"identify"}"#, original)
                    .await
                    .is_err(),
                "cancelled original authority cannot become local intent"
            );
            // A real published Agent invocation retains its original identity through
            // this Settings worker; it never borrows the worker's person certificate.
            {
                use crate::test_support::KindSet;
                env.agent_may("display-agent", KindSet::ALL, &[&env.route.app_id]);
            }
            let agent = rig
                .app_caller(&env, 906, "so.tilo.settings", Some("display-agent"))
                .await;
            let receipt = identify_displays(&agent.handle).await;
            if matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ) {
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(delivered.invocation_uuid, receipt.invocation.0.into_bytes());
                assert!(
                    settings
                        .display_control(
                            br#"{"operation":"identify"}"#,
                            runtime_collab::settings::DisplayOrigin::Published(
                                receipt.invocation.0.into_bytes()
                            )
                        )
                        .await
                        .is_err()
                );
            } else {
                assert!(matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Rejected { .. }
                        | tilo_actions::remote::ReceiptStatus::Failed { .. }
                ));
            }
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            // A foreign member device of the same person has workspace Admin but
            // must not obtain the original worker's device certificate.
            let (rpc, _server, _client) = certified_rpc(
                &rig.state,
                &rig.directories,
                907,
                member_device_certificate(&env, 97, false),
                None,
            );
            let foreign = opened(
                &rpc,
                &CrdtRoute::cluster(env.route.cluster_id.clone()).channel_key(),
            )
            .await
            .unwrap();
            // Whether or not the other device's call is admitted, it never
            // carries this worker's device into the display transaction.
            let receipt = identify_displays(&foreign).await;
            if matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ) {
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(delivered.invocation_uuid, receipt.invocation.0.into_bytes());
            }
            assert!(
                settings
                    .display_control(
                        br#"{"operation":"identify"}"#,
                        runtime_collab::settings::DisplayOrigin::Published(
                            receipt.invocation.0.into_bytes()
                        )
                    )
                    .await
                    .is_err(),
                "workspace Admin admission is not physical device ownership"
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            rig.state.remove_connection(903);
            let (mut replacement, _) =
                join_worker(&rig.state, &rig.directories, &env.route, &env.cert, 908).await;
            assert!(
                replacement
                    .handle
                    .settings()
                    .display_control(br#"{"operation":"identify"}"#, original)
                    .await
                    .is_err(),
                "a new private channel cannot reinterpret an old UUID as local intent"
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            replacement.server.abort();
            replacement.client.abort();
            let _ = (&mut replacement.server).await;
            let _ = (&mut replacement.client).await;
            caller.server.abort();
            caller.client.abort();
            let _ = (&mut caller.server).await;
            let _ = (&mut caller.client).await;
            rig.stop().await;
        })
        .await;
}

/// Ask the Settings worker (peer 903) to identify its displays over
/// `handle`'s channel, as an app reaches another's action.
async fn identify_displays(
    handle: &runtime_collab::RecordHandle,
) -> tilo_actions::remote::InvocationReceipt {
    super::signed_worker::receipt(
        super::signed_worker::ask(
            handle,
            tilo_actions::remote::RemoteActionMessage::Invoke(
                super::signed_worker::invoke_request(
                    "903",
                    "settings.display",
                    "identify-displays",
                    None,
                ),
            ),
        )
        .await
        .unwrap(),
    )
}

/// A computer's displays change only from its own Settings: a Settings worker another device of
/// the same person runs is admitted to read them, and every change it asks is refused before the
/// seat hears of it.
#[tokio::test(flavor = "current_thread")]
async fn another_devices_settings_worker_reads_the_displays_and_changes_nothing() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.settings".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let delegate = Arc::new(DisplayDelegate {
                calls: Mutex::new(vec![]),
            });
            let mut services = env.membership.services();
            services.settings = Some(Arc::new(settings_store::HostDocument::new(
                serde_json::Map::default(),
                |_| Ok(()),
            )));
            services.display_control = Some(delegate.clone());
            let fanout = Arc::new(FanoutState::new());
            let (state, bridge) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.authority()),
                None,
                None,
                services,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
            // Another computer of the same person, admitted to the workspace, runs Settings.
            let elsewhere = member_device_certificate(&env, 97, false);
            let (worker, _authorizer) =
                join_worker(&state, &directories, &env.route, &elsewhere, 411).await;
            let settings = worker.handle.settings();
            settings
                .display_control(
                    br#"{"operation":"inspect"}"#,
                    runtime_collab::settings::DisplayOrigin::LocalHost,
                )
                .await
                .expect("another device's Settings reads this computer's displays");
            assert_eq!(
                *delegate.calls.lock().unwrap(),
                vec![(
                    env.route.cluster_id.clone(),
                    elsewhere.user_public_key,
                    elsewhere.device_public_key
                )]
            );
            for change in [
                br#"{"operation":"identify"}"#.as_slice(),
                br#"{"operation":"keep","transaction":1}"#.as_slice(),
                b"not a display command".as_slice(),
            ] {
                let refused = settings
                    .display_control(change, runtime_collab::settings::DisplayOrigin::LocalHost)
                    .await
                    .expect_err("another device's Settings changes nothing here");
                assert!(
                    format!("{refused:?}").contains("own Settings"),
                    "the refusal says whose Settings may: {refused:?}"
                );
            }
            assert_eq!(
                delegate.calls.lock().unwrap().len(),
                1,
                "no refused change reached the seat"
            );

            // This computer's own Settings changes them.
            let (own, _own_authorizer) =
                join_worker(&state, &directories, &env.route, &env.cert, 412).await;
            own.handle
                .settings()
                .display_control(
                    br#"{"operation":"identify"}"#,
                    runtime_collab::settings::DisplayOrigin::LocalHost,
                )
                .await
                .expect("this computer's own Settings identifies its displays");
            assert_eq!(delegate.calls.lock().unwrap().len(), 2);
            for connection in [worker, own] {
                connection.server.abort();
                connection.client.abort();
            }
            state.stop();
        })
        .await;
}
