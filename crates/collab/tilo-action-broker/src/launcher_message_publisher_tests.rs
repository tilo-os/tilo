mod launcher_message_publisher_tests {
    use super::*;

    struct ActualPeerLaunch {
        room: String,
    }
    impl crate::services::LauncherServices for ActualPeerLaunch {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
            Err("fixture launches nothing".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            pid == std::process::id() && app == "so.tilo.chat" && room == self.room
        }
    }

    fn ordinary_rpc(
        room: &signed_worker::OriginalRoom,
        peer: u64,
        launcher_handoff: bool,
    ) -> (
        runtime_collab::CollabClient,
        tokio::task::JoinHandle<Result<(), capnp::Error>>,
        tokio::task::JoinHandle<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("chat.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();
        let server = tokio::net::UnixStream::from_std(server).unwrap();
        assert_eq!(
            server
                .peer_cred()
                .unwrap()
                .pid()
                .map(|pid| u32::try_from(pid).unwrap()),
            Some(std::process::id())
        );
        let server = server.into_std().unwrap();
        let handoff = serde_json::to_vec(&serde_json::json!({
            "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
            "app_id": room.route.app_id, "workspace_id": room.route.cluster_id,
        }))
        .unwrap();
        if launcher_handoff {
            std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC)
                .unwrap();
            std::io::Write::write_all(
                &mut client,
                &launcher::services::COLLAB_LAUNCH_FRAMING
                    .encode(&handoff)
                    .unwrap(),
            )
            .unwrap();
        }
        signed_worker::rpc_pair(
            room.state.clone(),
            peer,
            server,
            client,
            None,
            room.directories.clone(),
        )
    }

    async fn ordinary_chat(
        room: &signed_worker::OriginalRoom,
        peer: u64,
    ) -> (signed_worker::Connection, runtime_collab::CollabClient) {
        let (client, server, rpc) = ordinary_rpc(room, peer, true);
        let handle = client
            .join_channel(
                &room.route.channel_key(),
                &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
            )
            .await
            .unwrap();
        (
            signed_worker::Connection {
                handle,
                server,
                client: rpc,
            },
            client,
        )
    }

    fn room_fixture() -> (signed_worker::OriginalRoom, Arc<MessagePublisherOwner>) {
        let owner = MessagePublisherOwner::enrolled();
        let mut room = signed_worker::OriginalRoom::with_message_service(owner.clone());
        Arc::get_mut(&mut room.state).unwrap().services.launcher =
            Some(Arc::new(ActualPeerLaunch {
                room: room.route.cluster_id.clone(),
            }));
        Arc::get_mut(&mut room.state)
            .unwrap()
            .durable_owner_required = true;
        (room, owner)
    }

    fn enroll_delegatable_agent(room: &signed_worker::OriginalRoom) {
        let mut tree = room.membership.write();
        let workspace = tree.workspaces.first_mut().unwrap();
        workspace.owner = Some(principal_id_for_root(room.cert.user_public_key));
        workspace.grant_capability(CapOverride::membership(
            Subject::agent("scribe"),
            room.route.cluster_id.clone(),
            Verb::Comment,
        ));
    }

    async fn addressed_source(
        room: &signed_worker::OriginalRoom,
        connection: &signed_worker::Connection,
    ) -> (
        runtime_collab::InvocationStream,
        runtime_collab::document_contribution::DocumentContributionSource,
    ) {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        connection
            .handle
            .publish_action_directory(&room.route.app_id, native_message_test_directory())
            .await
            .unwrap();
        let stream = connection
            .handle
            .subscribe_stable_invocations()
            .await
            .unwrap();
        let caller = canonical_caller(room);
        let host = room
            .state
            .certified_local_action_host(
                &caller,
                &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
            )
            .unwrap();
        let page = canonical_pending(room, host.query(&caller, canonical_query(room)))
            .await
            .unwrap();
        assert_eq!(page.providers.len(), 1);
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: page.providers.first().unwrap().provider.clone(),
                address: tilo_actions::ActionAddress::new("opaque-agent-resource", "message"),
            },
            argument: InvokeArgument::Json(
                serde_json::json!({"value":"Original input"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            context: None,
            confirmation: None,
            revision: Some("message-r1".into()),
            fresh_binding: None,
            on_behalf_of: None,
            wait_ms: None,
        };
        let receipt = canonical_pending(room, host.invoke(&caller, request.clone()))
            .await
            .unwrap();
        assert_eq!(receipt.invocation, request.invocation);
        assert!(
            matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ),
            "actual canonical Message must be admitted"
        );
        let delivered = tokio::time::timeout(
            tilo_test_env::hang_bound(),
            stream.read_with_document_source(),
        )
        .await
        .expect("actual canonical Message delivery must finish")
        .unwrap();
        assert_eq!(delivered.invocation_uuid, *request.invocation.0.as_bytes());
        let source = delivered
            .document_source
            .expect("real native source custody");
        // Keep the actual RPC stream alive for source-bound upload assertions.
        // The channel cannot substitute another stream for this generation.
        let retained = room.state.message_publisher_stream(98).unwrap();
        assert!(retained.lease().is_ok());
        // Return the actual stream too: dropping it retires the source.
        // Its lifetime is retained by this bounded test's caller below.
        (stream, source)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_chat_installs_actual_owner_message_publisher() {
        tokio::task::LocalSet::new().run_until(async {
            let (room, owner) = room_fixture();
            assert!(!room.cert.capabilities.can_act_as_host);
            let (connection, client) = ordinary_chat(&room, 98).await;
            let original = room.state.host_workers.lock().unwrap().get(&98).unwrap().worker_registration.clone().unwrap();
            let _repeated = client.join_channel(&room.route.channel_key(), &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key)).await.unwrap();
            assert_eq!(room.state.host_workers.lock().unwrap().get(&98).unwrap().worker_registration.as_ref(), Some(&original));
            let _stream = connection.handle.subscribe_stable_invocations().await.unwrap();
            let directory = signed_worker::directory_state("directory-r1");
            connection.handle.publish_action_directory(&room.route.app_id, signed_worker::directory_server(&directory)).await.unwrap();
            let publisher = room.state.message_publisher_stream(98).unwrap();
            assert!(publisher.lease().is_ok(), "a real launcher-attested certified Person Chat publisher requires owner custody");
            assert_eq!(owner.registrations.lock().unwrap().len(), 1);
            let (_message_stream, source) = addressed_source(&room, &connection).await;
            let mut upload = connection.handle.open_document_contribution(&source, 4).await.unwrap();
            upload.write(b"null").await.unwrap();
            assert!(upload.reference().is_ok(), "bound upload is not an owner publication receipt");
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_chat_refuses_sibling_and_agent_without_replacing_root() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (room, owner) = room_fixture();
                let (connection, client) = ordinary_chat(&room, 98).await;
                let (_stream, _source) = addressed_source(&room, &connection).await;
                let original = room
                    .state
                    .host_workers
                    .lock()
                    .unwrap()
                    .get(&98)
                    .unwrap()
                    .worker_registration
                    .clone()
                    .unwrap();
                let presentation =
                    signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key);
                let mut sibling = room.route.clone();
                sibling.record_id = "sibling".into();
                assert!(
                    client
                        .join_channel(&sibling.channel_key(), &presentation)
                        .await
                        .is_err()
                );
                assert!(
                    client
                        .join_channel_as_kind(
                            &room.route.channel_key(),
                            &presentation,
                            "scribe",
                            "agent",
                            ""
                        )
                        .await
                        .is_err()
                );
                let mut other_app = room.route.clone();
                other_app.app_id = "so.tilo.notes".into();
                assert!(
                    client
                        .join_channel(&other_app.channel_key(), &presentation)
                        .await
                        .is_err()
                );
                let mut other_room = room.route.clone();
                other_room.cluster_id = uuid::Uuid::new_v4().to_string();
                assert!(
                    client
                        .join_channel(
                            &other_room.channel_key(),
                            &signed_worker::presentation(
                                &other_room.cluster_id,
                                &room.cert,
                                &room.key
                            )
                        )
                        .await
                        .is_err()
                );
                assert_eq!(
                    room.state
                        .host_workers
                        .lock()
                        .unwrap()
                        .get(&98)
                        .unwrap()
                        .worker_registration
                        .as_ref(),
                    Some(&original)
                );
                assert!(
                    room.state
                        .message_publisher_stream(98)
                        .unwrap()
                        .lease()
                        .is_ok()
                );
                assert_eq!(owner.registrations.lock().unwrap().len(), 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_chat_source_refuses_after_certificate_revocation() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (room, _) = room_fixture();
                let (connection, _) = ordinary_chat(&room, 98).await;
                let (_stream, source) = addressed_source(&room, &connection).await;
                let mut upload = connection
                    .handle
                    .open_document_contribution(&source, 4)
                    .await
                    .unwrap();
                *room.authority.certificate.lock().unwrap() = None;
                assert!(upload.write(b"null").await.is_err());
                assert!(
                    connection
                        .handle
                        .open_document_contribution(&source, 4)
                        .await
                        .is_err()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_chat_disconnect_retires_actual_source_and_lease() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (room, _) = room_fixture();
                let (connection, client) = ordinary_chat(&room, 98).await;
                let (_stream, source) = addressed_source(&room, &connection).await;
                let publisher = room.state.message_publisher_stream(98).unwrap();
                let original = room
                    .state
                    .host_workers
                    .lock()
                    .unwrap()
                    .get(&98)
                    .unwrap()
                    .worker_registration
                    .clone()
                    .unwrap();
                drop(connection);
                drop(client);
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }
                assert!(!original.current());
                assert!(publisher.lease().is_err());
                assert!(
                    room.state
                        .reserve_native_contribution(98, &room.route, *source.token(), 4)
                        .is_err()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_raw_person_cannot_claim_launcher_message_publisher() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (room, owner) = room_fixture();
                let (client, server, rpc) = ordinary_rpc(&room, 98, false);
                assert!(
                    client
                        .join_channel(
                            &room.route.channel_key(),
                            &signed_worker::presentation(
                                &room.route.cluster_id,
                                &room.cert,
                                &room.key
                            )
                        )
                        .await
                        .is_err()
                );
                assert!(!room.state.host_workers.lock().unwrap().contains_key(&98));
                assert!(room.state.message_publisher_stream(98).is_none());
                assert!(owner.registrations.lock().unwrap().is_empty());
                server.abort();
                rpc.abort();
                assert!(server.await.unwrap_err().is_cancelled());
                assert!(rpc.await.unwrap_err().is_cancelled());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_chat_non_durable_agent_handle_cannot_take_person_source() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut room, _) = room_fixture();
                Arc::get_mut(&mut room.state)
                    .unwrap()
                    .durable_owner_required = false;
                enroll_delegatable_agent(&room);
                let (connection, client) = ordinary_chat(&room, 98).await;
                let (_stream, source) = addressed_source(&room, &connection).await;
                let retained = room.state.message_publisher_stream(98).unwrap();
                let presentation =
                    signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key);
                let mut sibling = room.route.clone();
                sibling.record_id = "sibling".into();
                for route in [&room.route, &sibling] {
                    assert!(
                        client
                            .join_channel_as_kind(
                                &route.channel_key(),
                                &presentation,
                                "scribe",
                                "agent",
                                ""
                            )
                            .await
                            .is_err(),
                        "launcher Person socket must not create an Agent channel"
                    );
                }
                let mut other_app = room.route.clone();
                other_app.app_id = "so.tilo.notes".into();
                for kind in ["person", "agent"] {
                    assert!(
                        client
                            .join_channel_as_kind(
                                &other_app.channel_key(),
                                &presentation,
                                "scribe",
                                kind,
                                ""
                            )
                            .await
                            .is_err()
                    );
                }
                let mut other_room = room.route.clone();
                other_room.cluster_id = uuid::Uuid::new_v4().to_string();
                for kind in ["person", "agent"] {
                    assert!(
                        client
                            .join_channel_as_kind(
                                &other_room.channel_key(),
                                &signed_worker::presentation(
                                    &other_room.cluster_id,
                                    &room.cert,
                                    &room.key
                                ),
                                "scribe",
                                kind,
                                ""
                            )
                            .await
                            .is_err()
                    );
                }
                assert!(Arc::ptr_eq(
                    &retained,
                    &room.state.message_publisher_stream(98).unwrap()
                ));
                assert!(retained.lease().is_ok());
                let mut upload = connection
                    .handle
                    .open_document_contribution(&source, 4)
                    .await
                    .unwrap();
                upload.write(b"null").await.unwrap();
                assert!(upload.reference().is_ok());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_chat_non_durable_agent_first_is_refused() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut room, owner) = room_fixture();
                Arc::get_mut(&mut room.state)
                    .unwrap()
                    .durable_owner_required = false;
                enroll_delegatable_agent(&room);
                let (client, server, rpc) = ordinary_rpc(&room, 98, true);
                assert!(
                    client
                        .join_channel_as_kind(
                            &room.route.channel_key(),
                            &signed_worker::presentation(
                                &room.route.cluster_id,
                                &room.cert,
                                &room.key
                            ),
                            "scribe",
                            "agent",
                            ""
                        )
                        .await
                        .is_err()
                );
                assert!(!room.state.host_workers.lock().unwrap().contains_key(&98));
                assert!(owner.registrations.lock().unwrap().is_empty());
                server.abort();
                rpc.abort();
                assert!(server.await.unwrap_err().is_cancelled());
                assert!(rpc.await.unwrap_err().is_cancelled());
            })
            .await;
    }
}
