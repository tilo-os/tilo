//! Protected HTTP context turns for the existing native task acceptance journeys.
use super::*;

/// Own the connection task so a failed assertion cannot leave an RPC driver behind.
pub(in super::super) struct Connection(
    pub(in super::super) tokio::task::JoinHandle<Result<(), capnp::Error>>,
);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Use the production handoff authenticator and RPC driver against the same
/// broker state as the journey's app workers; no receipt crosses broker/peer IDs.
pub(in super::super) async fn channel(
    f: &Fixture,
    peer: u64,
) -> (Connection, channel_handle::Client) {
    let identity = f
        .state
        .services
        .launcher
        .as_ref()
        .unwrap()
        .authenticated_launch(std::process::id(), "so.tilo.agent", &room().to_string())
        .unwrap()
        .unwrap()
        .managed_identity
        .unwrap();
    let (certificate, _, proof) = identity.identity().unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room().to_string(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: proof.to_vec(),
    }
    .encode()
    .unwrap();
    let route = CrdtRoute::record("so.tilo.agent", room().to_string(), "main");
    let (mut client, _authorize) = f
        .state
        .connect_local_worker_with_authorizer(route, &certificate)
        .unwrap();
    let pending = f.state.workers.lock().unwrap().pop().unwrap();
    launcher::services::write_collab_handoff(&mut client, "so.tilo.agent", &room().to_string())
        .unwrap();
    let mut server = tokio::net::UnixStream::from_std(pending.stream).unwrap();
    let state = f.state.clone();
    let directories = f.directories.clone();
    let task = tokio::task::spawn_local(async move {
        let handoff = authenticate_launcher_handoff(&mut server, &state.services)
            .await
            .map_err(|r| r.error)?;
        let _exit = ConnectionExit {
            state: state.clone(),
            directories: directories.clone(),
            peer_id: peer,
        };
        let _monitor =
            native_media::ProcessMonitor::start(handoff.local_media.clone(), &state, peer);
        let service: collab_service::Client = capnp_rpc::new_client(CapnpCollabService {
            worker: Some(pending.binding),
            lease: Rc::new(ConnectionLease {
                local_media: handoff.local_media,
                state: state.clone(),
                directories: directories.clone(),
                peer_id: peer,
            }),
            state,
            directories,
            peer_id: peer,
        });
        runtime_portal::serve_connected_io(server, service.client).await
    });
    client.set_nonblocking(true).unwrap();
    let client = tokio::net::UnixStream::from_std(client).unwrap();
    let root: collab_service::Client = runtime_portal::CapnpClient::from_connected_io(
        client,
        capnp_rpc::rpc_twoparty_capnp::Side::Client,
    )
    .bootstrap();
    let mut join = root.join_channel_request();
    {
        let mut p = join.get();
        p.set_channel_id(
            CrdtRoute::record("so.tilo.agent", room().to_string(), "main").channel_key(),
        );
        p.set_identity(&presentation);
        p.set_participant_handle("scribe");
        p.set_participant_kind("agent");
        p.set_record_kind("");
    }
    let reply = join.send().promise.await.unwrap();
    (Connection(task), reply.get().unwrap().get_handle().unwrap())
}

/// `QueueInstruction` tasks own their MODEL source at the task root, not at an
/// unrelated Chat window. Freeze the person's separate model-use consent there.
pub(in super::super) fn configure_task_model(
    f: &Fixture,
    endpoint: String,
) -> tilo_test_env::ScopedEnv {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", f.directory.path().join("config"));
    let mut profile = agent_provider_config::ProviderProfile::local_vllm();
    profile.id = "owner-loopback".into();
    profile.endpoint = endpoint;
    profile.models = vec![agent_provider_config::ModelDescriptor::new("selected")];
    profile.default_model = Some("selected".into());
    let config = agent_provider_config::ProviderConfig {
        default_profile: Some(profile.id.clone()),
        profiles: vec![profile],
    };
    assert!(config.validation_errors().is_empty());
    config.save().unwrap();
    f.models.task_windows.store(true, Ordering::SeqCst);
    let profile = config.active_profile().unwrap();
    let contract =
        combined::task_contract(profile, profile.default_model.as_deref().unwrap(), &f.task);
    let person = hex::encode(f.native.certificate.user_public_key);
    for subject in [Subject::person(person.clone()), Subject::agent("scribe")] {
        put_grant(
            f,
            Grant::new(
                format!("journey-model-{subject:?}"),
                subject,
                contract.request_model.clone(),
                KindSet::OUTSIDE,
                Answer::Allow,
                Subject::person(person.clone()),
            ),
        );
    }
    env
}

async fn rpc(channel: &channel_handle::Client, command: serde_json::Value) -> serde_json::Value {
    let mut request = channel.native_model_request();
    request
        .get()
        .set_command(&serde_json::to_vec(&command).unwrap());
    let reply = request.send().promise.await.unwrap();
    serde_json::from_slice(reply.get().unwrap().get_reply().unwrap()).unwrap()
}

/// Only a successful native complete carrying the exact owner directions may
/// unlock the following `DirectionsApplied` report.
pub(in super::super) async fn successful_context(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    listener: Arc<tokio::net::TcpListener>,
) {
    let frontier = observed_control(channel, run).await;
    let mut applied = run.clone();
    let (base, _) = runtime_collab::native_task_revision(&run.lease).unwrap();
    applied.lease = format!("{base}:{}", frontier.applied_revision);
    let observed = observed_control(channel, &applied).await;
    let (_, prepared_revision) = runtime_collab::native_task_revision(&run.lease).unwrap();
    // A reconnected run can already have applied its latest direction; an
    // empty pending page still belongs to that exact prepared revision.
    assert_eq!(
        prepared_revision,
        observed
            .directions
            .last()
            .map_or(observed.applied_revision, |d| d.revision),
        "claim must freeze the explicitly prepared direction frontier"
    );
    let id = rpc(
        channel,
        serde_json::json!({
            "command": "claim", "run_id": run.run_id, "lease": run.lease,
        }),
    )
    .await;
    assert!(
        id.is_string(),
        "claim must return a native invocation: {id}"
    );
    let mut request: Request = serde_json::from_value(receipt::complete(
        id,
        &observed.directions,
        &observed.messages,
    ))
    .unwrap();
    let Request::NativeComplete {
        request: context, ..
    } = &mut request
    else {
        panic!("expected an approved native task command")
    };
    context.messages.insert(
        0,
        agent_provider_config::protocol::Message::text(
            agent_provider_config::protocol::Role::User,
            run.instruction(),
        ),
    );
    let peer = receipt::respond(listener, true);
    let reply = rpc(
        channel,
        serde_json::json!({
            "command": "execute",
            "request": request,
        }),
    )
    .await;
    if reply["response"]["ok"] != true {
        peer.abort();
    }
    assert_eq!(reply["response"]["ok"], true, "{reply}");
    let body = tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    let posted = body.to_string();
    let original = serde_json::to_string(run.instruction()).unwrap();
    assert!(
        posted.contains(
            original
                .strip_prefix('"')
                .unwrap()
                .strip_suffix('"')
                .unwrap()
        ),
        "POST must retain the original task context"
    );
    for direction in observed.directions {
        assert!(
            posted.contains(&direction.text),
            "the real POST must carry owner context"
        );
    }
}
