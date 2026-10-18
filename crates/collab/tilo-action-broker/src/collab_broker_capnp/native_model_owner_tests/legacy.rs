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
    let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    launcher::services::write_collab_handoff(&mut client, "so.tilo.agent", &room().to_string())
        .unwrap();
    client.set_nonblocking(true).unwrap();
    server.set_nonblocking(true).unwrap();
    let server = tokio::net::UnixStream::from_std(server).unwrap();
    let task = tokio::task::spawn_local(drive_connection(
        server,
        f.state.clone(),
        f.directories.clone(),
        peer,
        None,
    ));
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
        p.set_identity(&f.signed_presentation());
        p.set_participant_handle("scribe");
        p.set_participant_kind("agent");
        p.set_record_kind("");
    }
    let reply = join.send().promise.await.unwrap();
    (Connection(task), reply.get().unwrap().get_handle().unwrap())
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
/// unlock the following DirectionsApplied report.
pub(in super::super) async fn successful_context(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    listener: Arc<tokio::net::TcpListener>,
) {
    let observed = observed_control(channel, run).await;
    let id = rpc(
        channel,
        serde_json::json!({
            "command": "claim", "run_id": run.run_id, "lease": run.lease,
        }),
    )
    .await;
    let peer = receipt::respond(listener, true);
    let reply = rpc(
        channel,
        serde_json::json!({
            "command": "execute",
            "request": receipt::complete(id, &observed.directions, &observed.messages),
        }),
    )
    .await;
    assert_eq!(reply["response"]["ok"], true, "{reply}");
    let body = tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    let posted = body.to_string();
    for direction in observed.directions {
        assert!(
            posted.contains(&direction.text),
            "the real POST must carry owner context"
        );
    }
}
