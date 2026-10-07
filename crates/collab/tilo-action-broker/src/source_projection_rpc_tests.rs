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
    room: &signed_worker::OriginalRoom,
    descriptors: &[runtime_collab::ExportedAction],
) -> Arc<ProjectionReadProbe> {
    Arc::new(ProjectionReadProbe {
        binding: SourceBinding {
            app_id: room.route.app_id.clone(),
            workspace: room.route.cluster_id.clone(),
            record: room.route.record_id.clone(),
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
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let descriptors = projection_descriptors();
            let probe = projection_probe(&room, &descriptors);
            let (state, connection, directories) =
                projection_native_connection(&room, probe.clone())
                    .await
                    .unwrap();
            let (stream, request, parent) =
                projection_parent(&connection, &room, &descriptors, 997).await;
            let read = async {
                let child = stream.read().await.unwrap();
                assert_ne!(child.id, parent.id);
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
            let (result, ()) = tilo_test_env::within(
                "native source projection must return actual typed read",
                async { tokio::join!(execute, read) },
            )
            .await;
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("source admission only")
            );
            let observed = probe.observed.lock().unwrap().take().unwrap().unwrap();
            assert_eq!(observed["output"]["text"], "edited source");
            assert_eq!(observed["parent_invocation"], request.invocation);
            assert_ne!(observed["invocation"], request.invocation);
            state.remove_connection(997);
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
            let probe = projection_probe(&room, &descriptors);
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
            let probe = projection_probe(&room, &descriptors);
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
