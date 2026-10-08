struct ProjectionReadProbe {
    binding: SourceBinding,
    observed: Mutex<Option<Result<serde_json::Value, String>>>,
}
impl ApprovedCheckServices for ProjectionReadProbe {
    fn run(
        &self,
        context: &ApprovedCheckContext,
    ) -> Result<runtime_collab::approved_checks::Receipt, String> {
        let result = context
            .read_source(&self.binding, &serde_json::json!({"file":"fixture"}))
            .and_then(|observation| {
                observation.recheck()?;
                serde_json::to_value(&observation).map_err(|error| error.to_string())
            });
        *self.observed.lock().unwrap() = Some(result);
        Err("probe observes source admission only; no check was executed".into())
    }
}

struct ProjectionFixtureLaunch {
    app: String,
    room: String,
}
impl crate::services::LauncherServices for ProjectionFixtureLaunch {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
        Err("fixture cannot launch apps".into())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        pid == std::process::id() && app == self.app && room == self.room
    }
}

fn projection_descriptors() -> Vec<runtime_collab::ExportedAction> {
    let directory = signed_worker::directory_state("source-projection-r1");
    let template = directory.borrow().row().descriptor.unwrap();
    let mut check = template.clone();
    check.action = runtime_collab::approved_checks::ACTION_ID.into();
    check.operation_kind = "effect".into();
    check.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
        kind: check.resource_kind.clone(),
        key: "check".into(),
    });
    check.contract_json = serde_json::to_string(&tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
        result: tilo_actions::ResultSpec::Job,
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        ..Default::default()
    })
    .unwrap();
    let mut read = template;
    read.resource = "source:opaque-native-resource".into();
    read.action = "committed-source-snapshot".into();
    read.operation_kind = "effect".into();
    read.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
        kind: read.resource_kind.clone(),
        key: "source-file".into(),
    });
    read.contract_json = serde_json::to_string(&tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Read,
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type":"object"}),
        },
        tags: vec![tilo_actions::DOCUMENT_READ_TAG.into()],
        ..Default::default()
    })
    .unwrap();
    vec![check, read]
}

struct ProjectionNativePerson(Arc<signed_worker::CurrentHost>);
impl crate::services::LocalPersonServices for ProjectionNativePerson {
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

fn projection_broker(
    room: &signed_worker::OriginalRoom,
    probe: Arc<ProjectionReadProbe>,
) -> (Arc<CapnpBrokerState>, Rc<LocalActionDirectoryRegistry>) {
    let (state, bridge) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        // This Person fixture proves current admission and typed read terminals,
        // not owner-journal persistence. Private Worker registration requires the
        // existing mailbox-backed publisher, but is refused before a source read.
        if room.cert.capabilities.can_act_as_host {
            room.state.publisher.clone()
        } else {
            PeerPublisher::disabled()
        },
        Some(room.membership.authority()),
        None,
        None,
        HostServices {
            launcher: Some(Arc::new(ProjectionFixtureLaunch {
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
    (state, directories)
}

async fn projection_native_connection(
    room: &signed_worker::OriginalRoom,
    probe: Arc<ProjectionReadProbe>,
) -> Result<
    (
        Arc<CapnpBrokerState>,
        signed_worker::Connection,
        Rc<LocalActionDirectoryRegistry>,
    ),
    String,
> {
    let (state, directories) = projection_broker(room, probe);
    let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
    std::io::Write::write_all(
        &mut client,
        &collab_types::local_transport::COLLAB_LAUNCH_MAGIC,
    )
    .unwrap();
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol":collab_types::local_transport::COLLAB_LAUNCH_PROTOCOL_VERSION,
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

fn projection_probe(
    route: &CrdtRoute,
    descriptors: &[runtime_collab::ExportedAction],
) -> Arc<ProjectionReadProbe> {
    Arc::new(ProjectionReadProbe {
        binding: SourceBinding {
            app_id: route.app_id.clone(),
            workspace: route.cluster_id.clone(),
            record: route.record_id.clone(),
            resource_identity: tilo_actions::ResourceSemanticIdentity {
                kind: descriptors.get(1).unwrap().resource_kind.clone(),
                key: "source-file".into(),
            },
            action: descriptors.get(1).unwrap().action.clone(),
        },
        observed: Mutex::new(None),
    })
}
async fn projection_parent(
    connection: &signed_worker::Connection,
    room: &signed_worker::OriginalRoom,
    descriptors: &[runtime_collab::ExportedAction],
    peer: u64,
) -> (
    runtime_collab::InvocationStream,
    runtime_collab::approved_checks::Request,
    runtime_collab::Invocation,
) {
    connection
        .handle
        .publish_actions(&room.route.app_id, descriptors)
        .await
        .unwrap();
    let stream = connection
        .handle
        .subscribe_stable_invocations()
        .await
        .unwrap();
    let (wire, _) = connection
        .handle
        .invoke_published_action(
            peer,
            "workspace:atlas",
            runtime_collab::approved_checks::ACTION_ID,
            Some(r#"{"check_id":"regression"}"#),
            "{}",
        )
        .await
        .unwrap();
    let parent = stream.read().await.unwrap();
    assert_eq!(parent.id, wire);
    let request = runtime_collab::approved_checks::Request {
        version: 1,
        invocation: uuid::Uuid::from_bytes(parent.invocation_uuid).to_string(),
        check_id: "regression".into(),
    };
    (stream, request, parent)
}

#[tokio::test(flavor = "current_thread")]
async fn source_projection_person_reads_through_actual_native_published_dispatch() {
    projection_read_through_native_dispatch(false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn source_projection_native_seat_reads_and_refuses_retired_original_identity() {
    projection_read_through_native_dispatch(true, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn source_projection_continued_native_seat_keeps_actual_original_origin() {
    projection_read_through_native_dispatch(true, true).await;
}

async fn projection_continued_parent(
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    connection: &signed_worker::Connection,
    stream: &runtime_collab::InvocationStream,
    parent: &runtime_collab::Invocation,
) -> (
    runtime_collab::approved_checks::Request,
    runtime_collab::Invocation,
) {
    let child = tilo_actions::InvocationId::new();
    let original = tilo_actions::InvocationId(uuid::Uuid::from_bytes(parent.invocation_uuid));
    let request = tilo_actions::remote::InvokeRequest {
        invocation: child,
        on_behalf_of: Some(original),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "903".into(),
            address: tilo_actions::ActionAddress::new(
                "workspace:atlas",
                runtime_collab::approved_checks::ACTION_ID,
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Text(r#"{"check_id":"regression"}"#.into()),
        context: Some(tilo_actions::ActionContext {
            purpose: Some("untrusted child context".into()),
            ..Default::default()
        }),
        confirmation: None,
        revision: None,
    };
    for invalid in [original, tilo_actions::InvocationId(uuid::Uuid::nil())] {
        let mut refused = request.clone();
        refused.invocation = invalid;
        let response = projection_continued_reply(state, directories, connection, refused).await;
        assert!(
            matches!(response, tilo_actions::remote::RemoteActionMessage::Receipt(ref receipt)
            if receipt.invocation == invalid && matches!(receipt.status,
                tilo_actions::remote::ReceiptStatus::Rejected { error: tilo_actions::remote::ActionHostError::NotPermitted })),
            "{response:?}"
        );
    }
    let response = projection_continued_reply(state, directories, connection, request).await;
    assert!(
        matches!(response, tilo_actions::remote::RemoteActionMessage::Receipt(ref receipt)
        if receipt.invocation == child && receipt.status == tilo_actions::remote::ReceiptStatus::Accepted),
        "{response:?}"
    );
    let delivered = stream.read().await.unwrap();
    assert_ne!(delivered.id, parent.id);
    assert_eq!(delivered.invocation_uuid, child.0.into_bytes());
    assert_eq!(delivered.actor, parent.actor);
    let context: tilo_actions::ActionContext =
        serde_json::from_str(&delivered.context_json).unwrap();
    let mut parent_context: tilo_actions::ActionContext =
        serde_json::from_str(&parent.context_json).unwrap();
    // The admitted dispatcher adds the canonical target labels. Retained
    // original attribution and custody remain exact, not child-supplied.
    parent_context.action_label = "Open Atlas".into();
    parent_context.app = Some(tilo_actions::ActionApp {
        id: "so.tilo.word-processor".into(),
        label: "so.tilo.word-processor".into(),
    });
    parent_context.resource = Some(tilo_actions::ActionResource {
        id: "workspace:atlas".into(),
        kind: "workspace".into(),
        label: "Atlas".into(),
    });
    assert_eq!(context, parent_context);
    assert_ne!(context.purpose.as_deref(), Some("untrusted child context"));
    (
        runtime_collab::approved_checks::Request {
            version: 1,
            invocation: child.0.to_string(),
            check_id: "regression".into(),
        },
        delivered,
    )
}

async fn projection_continued_reply(
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    connection: &signed_worker::Connection,
    request: tilo_actions::remote::InvokeRequest,
) -> tilo_actions::remote::RemoteActionMessage {
    let frame = tilo_actions::remote::RemoteActionFrame::new(
        tilo_actions::remote::RemoteActionMessage::Invoke(request),
    )
    .encode()
    .unwrap();
    let request = connection.handle.remote_action(&frame);
    tokio::pin!(request);
    let bytes = tilo_test_env::within(
        "continued native seat needs the production owner turn",
        async {
            loop {
                state.drain_action_directory_bridge(directories);
                tokio::select! {
                    result = &mut request => break result.unwrap(),
                    () = tokio::task::yield_now() => {}
                }
            }
        },
    )
    .await;
    tilo_actions::remote::RemoteActionFrame::decode(&bytes)
        .unwrap()
        .message
}

async fn projection_read_through_native_dispatch(native_seat: bool, continued: bool) {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(), None, None,
            );
            let native = native_seat.then(durable_local_matrix::Environment::new);
            let route = native.as_ref().map_or(&room.route, |env| &env.route);
            let certificate = native.as_ref().map_or(&room.cert, |env| &env.cert);
            let authority = native.as_ref().map_or(&room.authority, |env| &env.authority);
            let peer = if native_seat { 903 } else { 997 };
            let descriptors = projection_descriptors();
            let probe = projection_probe(route, &descriptors);
            let (state, connection, directories) = if let Some(env) = &native {
                let mut services = env.membership.services();
                services.approved_checks = Some(probe.clone());
                services.local_person = Some(Arc::new(ProjectionNativePerson(env.authority.clone())));
                let rig = durable_local_matrix::Rig::with_services(env, services).await;
                (rig.state, rig.worker, rig.directories)
            } else {
                projection_native_connection(&room, probe.clone()).await.unwrap()
            };
            let (stream, request, parent) = if native_seat {
                connection
                    .handle
                    .publish_actions(&route.app_id, &descriptors)
                    .await
                    .unwrap();
                let stream = connection
                    .handle
                    .subscribe_stable_invocations()
                    .await
                    .unwrap();
                let person = tilo_actions::remote::Caller::local_person(principal_id_for_root(
                    certificate.user_public_key,
                ))
                .in_room(route.cluster_id.clone());
                let invoke = tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    on_behalf_of: None,
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: peer.to_string(),
                        address: tilo_actions::ActionAddress::new(
                            "workspace:atlas",
                            runtime_collab::approved_checks::ACTION_ID,
                        ),
                    },
                    argument: tilo_actions::remote::InvokeArgument::Text(
                        r#"{"check_id":"regression"}"#.into(),
                    ),
                    context: continued.then(|| tilo_actions::ActionContext {
                        purpose: Some("original admitted context".into()),
                        ..Default::default()
                    }),
                    confirmation: None,
                    revision: None,
                };
                let admission = state.action_invoke_local_person(&person, &invoke);
                tokio::pin!(admission);
                let receipt = tilo_test_env::within(
                    "native seat parent needs the normal directory owner turn",
                    async {
                        loop {
                            state.drain_action_directory_bridge(&directories);
                            tokio::select! {
                                result = &mut admission => break result.unwrap().unwrap(),
                                () = tokio::task::yield_now() => {}
                            }
                        }
                    },
                )
                .await;
                assert!(matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Accepted
                ));
                let parent = stream.read().await.unwrap();
                assert_eq!(parent.invocation_uuid, invoke.invocation.0.into_bytes());
                let request = runtime_collab::approved_checks::Request {
                    version: 1,
                    invocation: invoke.invocation.0.to_string(),
                    check_id: "regression".into(),
                };
                (stream, request, parent)
            } else {
                projection_parent(&connection, &room, &descriptors, 997).await
            };
            let (request, parent) = if continued {
                projection_continued_parent(&state, &directories, &connection, &stream, &parent).await
            } else { (request, parent) };
            let result = {
            let read = async {
                let child = stream.read().await.unwrap();
                assert_ne!(child.id, parent.id);
                assert_ne!(child.invocation_uuid, parent.invocation_uuid);
                assert_eq!(child.action, descriptors.get(1).unwrap().action);
                let encoded = child.argument.as_deref().expect("typed source arguments");
                let json = encoded
                    .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                    .expect("published source receives the ordinary typed argument slot");
                let supplied: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(json).unwrap();
                let arguments = tilo_actions::ActionArgs::bind_json(
                    &[tilo_actions::ActionParam::required(
                        "file",
                        tilo_actions::ParamType::Text,
                        "File",
                    )],
                    &supplied,
                )
                .unwrap();
                assert_eq!(arguments.text("file"), Some("fixture"));
                let update = tilo_actions::InvocationUpdate::Succeeded {
                    invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                        child.invocation_uuid,
                    )),
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"text":"edited source"})),
                        ..Default::default()
                    },
                };
                connection
                    .handle
                    .update_invocation(child.id, &serde_json::to_string(&update).unwrap())
                    .await
                    .unwrap();
            };
            let execute = connection.handle.approved_check(&request);
            tokio::pin!(execute, read);
            tilo_test_env::within(
                "native source projection must return actual typed read",
                async {
                    tokio::select! {
                        biased;
                        () = &mut read => execute.await,
                        result = &mut execute => panic!("check ended before its source child was observed: {result:?}; source observation: {:?}", probe.observed.lock().unwrap().as_ref()),
                    }
                },
            )
            .await
            };
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("source admission only")
            );
            let observed = probe.observed.lock().unwrap().take().unwrap().unwrap();
            assert_eq!(observed.get("output").and_then(|output| output.get("text")).and_then(serde_json::Value::as_str).expect("typed source text"), "edited source");
            assert_eq!(observed.get("parent_invocation").and_then(serde_json::Value::as_str).expect("typed parent identity"), request.invocation);
            assert_ne!(observed.get("invocation").and_then(serde_json::Value::as_str).expect("typed child identity"), request.invocation);
            if native_seat {
                // Keep the target's existing host certificate admitted while
                // withdrawing only the native-seat service's current identity.
                authority.additional_certificates.lock().unwrap().push(certificate.clone());
                *authority.certificate.lock().unwrap() = None;
                let retired = connection
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err();
                assert!(
                    retired.to_string().contains("native identity retired"),
                    "{retired}"
                );
                assert!(
                    probe.observed.lock().unwrap().is_none(),
                    "retired original identity must not reach the source service"
                );
            }
            state.remove_connection(peer);
            drop(connection);
            drop(directories);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn source_projection_refuses_a_host_without_private_worker_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::host(),
                None,
                None,
            );
            let descriptors = projection_descriptors();
            let probe = projection_probe(&room.route, &descriptors);
            match projection_native_connection(&room, probe.clone()).await {
                Ok(_) => panic!(
                    "ordinary native transport must not admit a Host without its private descriptor"
                ),
                Err(error) => assert!(
                    error.contains("host certificate requires a private host worker descriptor"),
                    "{error}"
                ),
            }
            assert!(
                probe.observed.lock().unwrap().is_none(),
                "Host never reaches the source service"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn source_projection_refuses_a_registered_worker_origin_before_dispatching_source() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::host(),
                None,
                None,
            );
            let descriptors = projection_descriptors();
            let probe = projection_probe(&room.route, &descriptors);
            let (state, directories) = projection_broker(&room, probe.clone());
            let (connection, _) = signed_worker::join_worker(
                &state,
                &directories,
                &room.route,
                &room.cert,
                &room.key,
                998,
            )
            .await;
            let (_stream, request, _parent) =
                projection_parent(&connection, &room, &descriptors, 998).await;
            assert!(connection.handle.approved_check(&request).await.is_err());
            let error = probe.observed.lock().unwrap().take().unwrap().unwrap_err();
            assert!(error.contains("Person or accepted native Agent"), "{error}");
            assert_eq!(
                state.pending_invocations.lock().unwrap().len(),
                1,
                "refused Worker authority must not dispatch a source child"
            );
            assert!(room.authority.checks.load(Ordering::SeqCst) > 0);
            state.remove_connection(998);
            drop(connection);
            drop(directories);
        })
        .await;
}
