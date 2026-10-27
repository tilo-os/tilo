/// A launch's committed reads of the records every app of its room opens
/// (`collab_types::WORKSPACE_RECORDS`), through the actual channel handle
/// and its live launcher custody: any app's window of the room reads and
/// changes them, as the room's grants on their owner decide, and nothing
/// else of their owner's.
mod workspace_record_custody {
    use super::*;

    const LAUNCHED: &str = "so.tilo.notes";

    fn emoji() -> &'static collab_types::WorkspaceRecord {
        collab_types::WORKSPACE_RECORDS
            .first()
            .expect("the workspace's emoji")
    }

    /// A channel on `route` held by a connection the launcher attested for
    /// `LAUNCHED` in the fixture's room, launched from its signed original.
    fn launched_channel(
        room: &signed_worker::OriginalRoom,
        identity: &AdmittedJoinIdentity,
        route: &CrdtRoute,
        socket: &tokio::net::UnixStream,
    ) -> CapnpChannelHandle {
        let local = native_media::LocalAppConnection::attested(
            LAUNCHED.into(),
            room.route.cluster_id.clone(),
            native_media::PeerProcess::from_socket(socket).unwrap(),
            crate::services::AuthenticatedApplicationLaunch {
                creation: Some(crate::services::ApplicationCreationEvidence {
                    package_hash: "ab".repeat(32),
                    declared_kinds: vec!["notes.notebook".into()],
                }),
                continuation_instance: Some(91),
                launch_ticket: None,
            },
        )
        .unwrap();
        CapnpChannelHandle {
            person_chat_views: RefCell::default(),
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: room.state.clone(),
            directories: room.directories.clone(),
            channel_id: route.channel_key(),
            peer_id: 91,
            connection_id: 91,
            verified_identity: identity.clone(),
            record_kind: route_record_kind(route, None).unwrap(),
            lease: Some(Rc::new(ConnectionLease {
                local_media: Some(local),
                state: room.state.clone(),
                directories: room.directories.clone(),
                peer_id: 91,
            })),
            _hold: None,
        }
    }

    /// The certified person the room admitted, and the connection that
    /// keeps them admitted.
    async fn admitted(
        room: &signed_worker::OriginalRoom,
    ) -> (AdmittedJoinIdentity, signed_worker::Connection) {
        let (connection, _) = room.join_worker(90).await;
        let mut identity = room
            .state
            .host_workers
            .lock()
            .unwrap()
            .only(90)
            .unwrap()
            .clone();
        identity.worker_registration = None;
        (identity, connection)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn any_app_of_the_room_reads_its_workspace_records_and_no_other_record_of_their_owner() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = signed_worker::OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let (identity, _connection) = admitted(&room).await;
                let (socket, _peer) = tokio::net::UnixStream::pair().unwrap();
                let cluster = room.route.cluster_id.clone();
                let record = collab_types::workspace_record_id(emoji().kind);

                let workspace = CrdtRoute::record(emoji().app, &cluster, &record);
                let channel = launched_channel(&room, &identity, &workspace, &socket);
                let context = channel
                    .document_read_context()
                    .expect("a window of the room reads the workspace's record");
                assert_eq!(context.record_kind(), Some(emoji().kind));
                assert_eq!(
                    context
                        .application_creation()
                        .map(|evidence| evidence.declared_kinds.clone()),
                    Some(vec![emoji().kind.to_owned()]),
                    "the record's kind is the platform's, not the launching package's"
                );

                for (route, why) in [
                    (
                        CrdtRoute::record(emoji().app, &cluster, "tab:0000000000000001"),
                        "the owner's other records stay its own",
                    ),
                    (
                        CrdtRoute::record(emoji().app, &cluster, ""),
                        "the owner's primary record stays its own",
                    ),
                    (
                        CrdtRoute::record("so.tilo.code-editor", &cluster, &record),
                        "a record of the kind kept by an app that does not own it",
                    ),
                    (
                        CrdtRoute::record(
                            emoji().app,
                            tilo_desktop_model::RoomId::new().to_string(),
                            &record,
                        ),
                        "the workspace record of another room",
                    ),
                ] {
                    let channel = launched_channel(&room, &identity, &route, &socket);
                    let refused = channel.document_read_context().err();
                    assert!(refused.is_some(), "{why}");
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_owners_grants_decide_a_change_to_a_workspace_record_from_another_app() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = signed_worker::OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let (identity, _connection) = admitted(&room).await;
                let (socket, _peer) = tokio::net::UnixStream::pair().unwrap();
                let cluster = room.route.cluster_id.clone();
                let record = collab_types::workspace_record_id(emoji().kind);
                let route = CrdtRoute::record(emoji().app, &cluster, &record);
                let add = {
                    let mut op = SignedOp::new_unsigned(
                        DocumentMutation::Snapshot(automerge::AutoCommit::new().save())
                            .encode()
                            .unwrap(),
                        document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
                        Ed25519PublicKey::from_bytes(room.key.verifying_key().to_bytes()),
                        document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
                        collab_types::OpKind::Change,
                        SystemTime::now(),
                        1,
                    );
                    sign_op(&mut op, &room.key).unwrap();
                    op
                };
                room.state
                    .authorize_certified_document(&route, &identity, &add)
                    .expect("someone the room lets change it changes it from any app");

                let principal = principal_id_for_root(room.cert.user_public_key);
                {
                    let mut tree = room.membership.write();
                    let workspace = &mut tree.workspaces[0];
                    workspace.revoke_grants_matching(|grant| grant.id.0 == "changes-for-a-while");
                    workspace.set_owner("someone-else");
                    workspace.admit(principal, crate::test_support::Role::Viewer);
                }
                let viewer = room
                    .state
                    .revalidate_admission(&route, &identity)
                    .expect("a viewer is still in the room");
                let channel = launched_channel(&room, &viewer, &route, &socket);
                channel
                    .document_read_context()
                    .expect("a viewer reads the workspace's record");
                let Err(refused) = room
                    .state
                    .authorize_certified_document(&route, &viewer, &add)
                else {
                    panic!("a viewer adds nothing");
                };
                assert!(
                    refused.to_string().contains("grants deny"),
                    "refused by the grants, not elsewhere: {refused}"
                );
                let client: channel_handle::Client = capnp_rpc::new_client(channel);
                let mut send = client.send_request();
                send.get().set_op(&postcard::to_stdvec(&add).unwrap());
                assert!(
                    send.send().promise.await.is_err(),
                    "the channel refuses the viewer's change"
                );
            })
            .await;
    }

    #[test]
    fn a_workspace_record_route_is_joined_as_its_own_kind() {
        let room = tilo_desktop_model::RoomId::new().to_string();
        let record = collab_types::workspace_record_id(emoji().kind);
        let route = CrdtRoute::record(emoji().app, &room, &record);
        assert_eq!(
            route_record_kind(&route, None).unwrap().as_deref(),
            Some(emoji().kind),
            "an owner route joined without a kind holds the workspace record's"
        );
        assert_eq!(
            route_record_kind(&route, Some(emoji().kind.into()))
                .unwrap()
                .as_deref(),
            Some(emoji().kind)
        );
        assert!(route_record_kind(&route, Some("chat.workspace".into())).is_err());
        let other = CrdtRoute::record(emoji().app, &room, "tab:0000000000000001");
        assert_eq!(route_record_kind(&other, None).unwrap(), None);
        assert_eq!(
            route_record_kind(&other, Some("chat.workspace".into()))
                .unwrap()
                .as_deref(),
            Some("chat.workspace")
        );
    }
}
