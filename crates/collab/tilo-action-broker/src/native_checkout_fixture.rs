/// Signed channel publication and actual kernel attestation for compositor tests.
/// The fixture never installs a checkout directly into the production map.
pub struct NativeCheckoutFixture {
    pub broker: crate::collab_broker::AppActionBroker,
    pub route: CrdtRoute,
    channel: channel_handle::Client,
    transport: signed_worker::Connection,
    local: Arc<native_media::LocalAppConnection>,
    room: signed_worker::OriginalRoom,
}
impl NativeCheckoutFixture {
    pub async fn new() -> Self {
        let room = signed_worker::OriginalRoom::new(
            tilo_user_identity::DeviceCapabilities::primary(),
            None,
            None,
        );
        let (transport, _) = room.join_worker(71).await;
        let identity = room
            .state
            .host_workers
            .lock()
            .unwrap()
            .get(&71)
            .unwrap()
            .clone();
        let (socket, _peer_socket) = tokio::net::UnixStream::pair().unwrap();
        let local = native_media::LocalAppConnection::attested(
            room.route.app_id.clone(),
            room.route.cluster_id.clone(),
            native_media::PeerProcess::from_socket(&socket).unwrap(),
            crate::services::AuthenticatedApplicationLaunch {
                creation: None,
                continuation_instance: Some(73),
            },
        )
        .unwrap();
        let channel: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: room.state.clone(),
            directories: room.directories.clone(),
            channel_id: room.route.channel_key(),
            peer_id: 71,
            verified_identity: identity,
            record_kind: None,
            lease: Some(Rc::new(ConnectionLease {
                local_media: Some(local.clone()),
                state: room.state.clone(),
                directories: room.directories.clone(),
                peer_id: 71,
            })),
        });
        let mut request = channel.publish_action_directory_request();
        request.get().set_app_id(&room.route.app_id);
        request.get().set_directory(signed_worker::directory_server(
            &signed_worker::directory_state("checkout-r1"),
        ));
        request.send().promise.await.unwrap();
        Self {
            broker: crate::collab_broker::AppActionBroker::from_state_for_test(room.state.clone()),
            route: room.route.clone(),
            channel,
            transport,
            local,
            room,
        }
    }
    pub fn revoke(&self) {
        *self.room.authority.certificate.lock().unwrap() = None;
    }
    pub fn disconnect(&self) {
        self.local.stop();
    }
}
impl Drop for NativeCheckoutFixture {
    fn drop(&mut self) {
        // Retain both real channel and socket tasks until the observer finishes.
        let _ = (&self.channel, &self.transport);
        self.local.stop();
    }
}
