/// Publish through the actual Cap'n Proto channel with current signed admission
/// and the host-retained launcher association, pinned to a real kernel socket.
async fn checkout_channel(
    room: &signed_worker::OriginalRoom,
    peer: u64,
    attested: bool,
) -> (
    channel_handle::Client,
    Option<Arc<native_media::LocalAppConnection>>,
    signed_worker::Connection,
) {
    let (worker, _) = room.join_worker(peer).await;
    let identity = room
        .state
        .host_workers
        .lock()
        .unwrap()
        .get(&peer)
        .unwrap()
        .clone();
    // The actual joined channel supplied the verified identity. Its transport
    // is retained below independently from this launcher-admitted test channel.
    let (socket, _peer_socket) = tokio::net::UnixStream::pair().unwrap();
    let local = attested.then(|| {
        native_media::LocalAppConnection::attested(
            room.route.app_id.clone(),
            room.route.cluster_id.clone(),
            native_media::PeerProcess::from_socket(&socket).unwrap(),
            crate::services::AuthenticatedApplicationLaunch {
                creation: None,
                continuation_instance: Some(73),
                launch_ticket: None,
            },
        )
        .unwrap()
    });
    let channel: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
        person_chat_views: RefCell::default(),
        committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
        state: room.state.clone(),
        directories: room.directories.clone(),
        channel_id: room.route.channel_key(),
        peer_id: peer,
        verified_identity: identity,
        record_kind: None,
        route_access: RouteAccess::Own,
        lease: Some(Rc::new(ConnectionLease {
            local_media: local.clone(),
            state: room.state.clone(),
            directories: room.directories.clone(),
            peer_id: peer,
        })),
    });
    let mut request = channel.publish_action_directory_request();
    request.get().set_app_id(&room.route.app_id);
    request.get().set_directory(signed_worker::directory_server(
        &signed_worker::directory_state("checkout-r1"),
    ));
    request.send().promise.await.unwrap();
    (channel, local, worker)
}

fn checkout_process() -> (u32, u64) {
    let pid = std::process::id();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    (pid, start)
}

#[tokio::test(flavor = "current_thread")]
async fn certified_native_checkout_uses_actual_registered_route_and_rejects_wrong_lifetimes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (channel, local, _transport) = checkout_channel(&room, 71, true).await;
            let (pid, start) = checkout_process();
            let root = room
                .state
                .native_checkout(&room.route.app_id, &room.route.cluster_id, 73, pid, start)
                .unwrap();
            assert_eq!(root.record_id, room.route.record_id);
            assert_ne!(root.record_id, "derived:default@tab:0000000000000049");
            for (app, cluster, instance, process, began) in [
                ("foreign", room.route.cluster_id.as_str(), 73, pid, start),
                (room.route.app_id.as_str(), "foreign", 73, pid, start),
                (
                    room.route.app_id.as_str(),
                    room.route.cluster_id.as_str(),
                    74,
                    pid,
                    start,
                ),
                (
                    room.route.app_id.as_str(),
                    room.route.cluster_id.as_str(),
                    73,
                    u32::MAX,
                    start,
                ),
                (
                    room.route.app_id.as_str(),
                    room.route.cluster_id.as_str(),
                    73,
                    pid,
                    0,
                ),
            ] {
                assert!(
                    room.state
                        .native_checkout(app, cluster, instance, process, began)
                        .is_none()
                );
            }
            local.unwrap().stop();
            assert!(
                room.state
                    .native_checkout(&room.route.app_id, &room.route.cluster_id, 73, pid, start)
                    .is_none()
            );
            drop(channel);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_native_checkout_refuses_unattested_revoked_and_ambiguous_channels() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for mode in [
                "unattested",
                "revoked",
                "ambiguous",
                "missing-directory",
                "replaced-record",
            ] {
                let room = signed_worker::OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let (channel, _, _transport) =
                    checkout_channel(&room, 71, mode != "unattested").await;
                let second = if mode == "ambiguous" {
                    Some(checkout_channel(&room, 72, true).await)
                } else {
                    None
                };
                if mode == "revoked" {
                    *room.authority.certificate.lock().unwrap() = None;
                }
                if mode == "missing-directory" {
                    room.state
                        .action_directory_roots
                        .lock()
                        .unwrap()
                        .remove(&71);
                }
                if mode == "replaced-record" {
                    room.state
                        .action_directory_roots
                        .lock()
                        .unwrap()
                        .get_mut(&71)
                        .unwrap()
                        .record_id = "different-current-record".into();
                }
                let (pid, start) = checkout_process();
                assert!(
                    room.state
                        .native_checkout(&room.route.app_id, &room.route.cluster_id, 73, pid, start)
                        .is_none(),
                    "{mode} cannot select checkout custody"
                );
                drop(second);
                drop(channel);
                assert!(room.state.native_checkouts.lock().unwrap().is_empty());
            }
        })
        .await;
}

/// A channel an app holds only to read another app's record changes nothing
/// on it: neither the document nor what is published for the route. The
/// route is another app's, so no write reaches it from this launch.
#[tokio::test(flavor = "current_thread")]
async fn a_channel_held_to_read_another_apps_record_refuses_every_change() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let peer = 72;
            let (_worker, _) = room.join_worker(peer).await;
            let identity = room
                .state
                .host_workers
                .lock()
                .unwrap()
                .get(&peer)
                .unwrap()
                .clone();
            let channel: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                person_chat_views: RefCell::default(),
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state: room.state.clone(),
                directories: room.directories.clone(),
                channel_id: room.route.channel_key(),
                peer_id: peer,
                verified_identity: identity,
                record_kind: None,
                route_access: RouteAccess::ReadOnly,
                lease: None,
            });
            let mut send = channel.send_request();
            send.get().set_op(b"any op");
            let refused = send.send().promise.await.err().expect("a write is refused");
            assert!(refused.to_string().contains("changes nothing"), "{refused}");
            let mut publish = channel.publish_action_directory_request();
            publish.get().set_app_id(&room.route.app_id);
            publish.get().set_directory(signed_worker::directory_server(
                &signed_worker::directory_state("read-only-r1"),
            ));
            let refused = publish
                .send()
                .promise
                .await
                .err()
                .expect("a publication is refused");
            assert!(refused.to_string().contains("changes nothing"), "{refused}");
        })
        .await;
}
