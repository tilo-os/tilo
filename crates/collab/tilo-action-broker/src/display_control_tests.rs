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
        self.calls
            .lock()
            .unwrap()
            .push((context.room().into(), context.root(), context.device()));
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
            let (state, bridge) = CapnpBrokerState::new_configured(
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
                join_worker(&state, &directories, &env.route, &env.cert, &env.key, 401).await;
            let settings = worker.handle.settings().await.unwrap();
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
            let ordinary = rpc
                .join_channel(
                    &env.route.channel_key(),
                    &presentation(&env.route.cluster_id, &env.cert, &env.key),
                )
                .await
                .unwrap();
            assert!(
                ordinary
                    .settings()
                    .await
                    .unwrap()
                    .display_control(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost)
                    .await
                    .is_err()
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            let (_, evidence) = member_device_evidence(&env, 96, true);
            for (peer, kind, agent) in [(404, "person", ""), (405, "agent", "foreign-agent")] {
                let (host_socket, client_socket) = std::os::unix::net::UnixStream::pair().unwrap();
                let (foreign_rpc, foreign_server, foreign_client) = rpc_pair(
                    state.clone(),
                    peer,
                    host_socket,
                    client_socket,
                    None,
                    directories.clone(),
                );
                let joined = foreign_rpc
                    .join_channel_as_kind(&env.route.channel_key(), &evidence, agent, kind, "")
                    .await;
                if kind == "agent" {
                    assert!(
                        joined.is_err(),
                        "foreign devices cannot launch a delegated Agent channel"
                    );
                } else {
                    let foreign = joined.expect("a signed foreign person may join read-only");
                    assert!(
                        foreign
                            .settings()
                            .await
                            .unwrap()
                            .display_control(
                                b"{}",
                                runtime_collab::settings::DisplayOrigin::LocalHost
                            )
                            .await
                            .is_err(),
                        "foreign public channels cannot acquire private Settings mutation authority"
                    );
                }
                foreign_server.abort();
                foreign_client.abort();
                let _ = foreign_server.await;
                let _ = foreign_client.await;
            }
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            let identity = state.host_workers.lock().unwrap()[&401].clone();
            let retained = private_display_control(&state, &env.route, &identity).unwrap();
            let mut agent = identity.clone();
            agent.agent_id = Some("foreign-agent".into());
            assert!(private_display_control(&state, &env.route, &agent).is_none());
            let mut other_app = env.route.clone();
            other_app.app_id = "so.tilo.browser".into();
            assert!(private_display_control(&state, &other_app, &identity).is_none());
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
                join_worker(&state, &directories, &env.route, &env.cert, &env.key, 403).await;
            assert!(
                retained(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost).is_err(),
                "an equal certificate on a new connection cannot revive retired registration"
            );
            assert!(
                replacement
                    .handle
                    .settings()
                    .await
                    .unwrap()
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
async fn display_rpc_preserves_original_published_caller_and_refuses_missing_or_cancelled_ids() {
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
                HostServices {
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
            contract.permission = tilo_actions::PermissionRequirement::Admin;
            contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
            contract.result = tilo_actions::ResultSpec::Job;
            rig.worker
                .handle
                .publish_actions(
                    "so.tilo.settings",
                    &[runtime_collab::ExportedAction {
                        resource: "settings.display".into(),
                        semantic_identity: None,
                        action: "identify-displays".into(),
                        resource_kind: "settings".into(),
                        resource_label: "Displays".into(),
                        action_label: "Identify".into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::View,
                        params: Vec::new(),
                        contract,
                        input: runtime_collab::ExportedInput::None,
                        view: None,
                    }],
                )
                .await
                .unwrap();
            let mut caller = rig.caller(&env, 905, None).await;
            let settings = rig.worker.handle.settings().await.unwrap();
            let (_, receipt) = caller
                .handle
                .invoke_published_action(903, "settings.display", "identify-displays", None, "{}")
                .await
                .unwrap();
            let update: tilo_actions::InvocationUpdate = serde_json::from_str(&receipt).unwrap();
            let original = runtime_collab::settings::DisplayOrigin::Published(
                update.invocation().0.into_bytes(),
            );
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(
                delivered.invocation_uuid,
                update.invocation().0.into_bytes()
            );
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
            caller
                .handle
                .cancel_published_invocation(delivered.id)
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
            env.membership.write().workspaces[0].grant_capability(CapOverride::membership(
                Subject::agent("display-agent"),
                env.route.cluster_id.clone(),
                Verb::Admin,
            ));
            env.membership.write().workspaces[0].grant_capability(CapOverride::grant(
                Subject::agent("display-agent"),
                collab_types::Scope::App(env.route.app_id.clone()),
                Verb::Admin,
            ));
            let agent = rig.caller(&env, 906, Some("display-agent")).await;
            let (_, receipt) = agent
                .handle
                .invoke_published_action(903, "settings.display", "identify-displays", None, "{}")
                .await
                .unwrap();
            let update: tilo_actions::InvocationUpdate = serde_json::from_str(&receipt).unwrap();
            if matches!(update, tilo_actions::InvocationUpdate::Accepted { .. }) {
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(
                    delivered.invocation_uuid,
                    update.invocation().0.into_bytes()
                );
                assert!(
                    settings
                        .display_control(
                            br#"{"operation":"identify"}"#,
                            runtime_collab::settings::DisplayOrigin::Published(
                                update.invocation().0.into_bytes()
                            )
                        )
                        .await
                        .is_err()
                );
            } else {
                assert!(matches!(
                    update,
                    tilo_actions::InvocationUpdate::Failed { .. }
                ));
            }
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            // A foreign member device of the same person has workspace Admin but
            // must not obtain the original worker's device certificate.
            let (_, evidence) = member_device_evidence(&env, 97, false);
            let (host, client) = std::os::unix::net::UnixStream::pair().unwrap();
            let (rpc, foreign_server, foreign_client) = rpc_pair(
                rig.state.clone(),
                907,
                host,
                client,
                None,
                rig.directories.clone(),
            );
            let foreign = rpc
                .join_channel(
                    &CrdtRoute::cluster(env.route.cluster_id.clone()).channel_key(),
                    &evidence,
                )
                .await
                .unwrap();
            let (_, receipt) = foreign
                .invoke_published_action(903, "settings.display", "identify-displays", None, "{}")
                .await
                .unwrap();
            let update: tilo_actions::InvocationUpdate = serde_json::from_str(&receipt).unwrap();
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Accepted { .. }),
                "workspace Admin admission is not physical device ownership"
            );
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(
                delivered.invocation_uuid,
                update.invocation().0.into_bytes()
            );
            assert!(
                settings
                    .display_control(
                        br#"{"operation":"identify"}"#,
                        runtime_collab::settings::DisplayOrigin::Published(
                            update.invocation().0.into_bytes()
                        )
                    )
                    .await
                    .is_err()
            );
            assert_eq!(delegate.calls.lock().unwrap().len(), 1);
            foreign_server.abort();
            foreign_client.abort();
            let _ = foreign_server.await;
            let _ = foreign_client.await;
            rig.state.remove_connection(903);
            let (mut replacement, _) = join_worker(
                &rig.state,
                &rig.directories,
                &env.route,
                &env.cert,
                &env.key,
                908,
            )
            .await;
            assert!(
                replacement
                    .handle
                    .settings()
                    .await
                    .unwrap()
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

struct NativePersonFixture(Arc<CurrentHost>);
impl crate::services::LocalPersonServices for NativePersonFixture {
    fn certificate(
        &self,
        room: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        let certificate = self
            .0
            .certificate
            .lock()
            .unwrap()
            .clone()
            .ok_or("native identity retired")?;
        if room != self.0.route.cluster_id
            || person != principal_id_for_root(certificate.user_public_key)
        {
            return Err("native person mismatch".into());
        }
        Ok(certificate)
    }
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
                HostServices {
                    local_person: Some(Arc::new(NativePersonFixture(env.authority.clone()))),
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
            contract.permission = tilo_actions::PermissionRequirement::Admin;
            contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
            contract.result = tilo_actions::ResultSpec::Job;
            rig.worker
                .handle
                .publish_actions(
                    "so.tilo.settings",
                    &[runtime_collab::ExportedAction {
                        resource: "settings.display".into(),
                        semantic_identity: None,
                        action: "identify-displays".into(),
                        resource_kind: "settings".into(),
                        resource_label: "Displays".into(),
                        action_label: "Identify".into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::View,
                        params: Vec::new(),
                        contract,
                        input: runtime_collab::ExportedInput::None,
                        view: None,
                    }],
                )
                .await
                .unwrap();
            let person = tilo_actions::remote::Caller::local_person(principal_id_for_root(
                env.cert.user_public_key,
            ))
            .in_room(env.route.cluster_id.clone());
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
            };
            // The previous normal host door really admitted this action but
            // supplied no original custody. The generic door still cannot
            // borrow native-owner identity just from a local-person label.
            let mut generic = request.clone();
            generic.invocation = tilo_actions::InvocationId::new();
            rig.state
                .action_invoke(&person, None, None, &generic)
                .await
                .unwrap()
                .unwrap();
            let generic_delivery = rig.stream.as_ref().unwrap().read().await.unwrap();
            let settings = rig.worker.handle.settings().await.unwrap();
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
            let settings = rig.worker.handle.settings().await.unwrap();
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
            let agent = tilo_actions::remote::Caller::local_agent("native-origin-agent")
                .in_room(env.route.cluster_id.clone());
            assert!(
                rig.state
                    .action_invoke_local_person(&agent, &request)
                    .await
                    .unwrap()
                    .is_err()
            );
            let forged = tilo_actions::remote::Caller::local_person("other-person")
                .in_room(env.route.cluster_id.clone());
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
