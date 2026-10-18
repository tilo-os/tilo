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
async fn progress_parent(
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
