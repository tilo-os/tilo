// The actual signed/native RPC path verifies scoped host dispatch. The launcher
// observer is not a recipient app and does not establish file opening.
struct ContentScopeLauncher {
    app: String,
    room: String,
    opened: Mutex<Vec<(String, crate::services::LaunchScope)>>,
}
impl crate::services::LauncherServices for ContentScopeLauncher {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        vec![crate::services::LaunchableApplication {
            package_id: "so.tilo.photos".into(),
            display_name: "Photos".into(),
            unconfined: false,
        }]
    }
    fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
        Err("content must not become an empty launch".into())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        pid == std::process::id() && app == self.app && room == self.room
    }
    fn launch_content(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        content: crate::services::LaunchContent<'_>,
    ) -> Result<(), String> {
        if app != "so.tilo.photos" || workspace.room.to_string() != self.room {
            return Err("wrong recipient or workspace".into());
        }
        let scope = content.caller.ok_or("missing authenticated scope")?;
        self.opened
            .lock()
            .unwrap()
            .push((content.hash.into(), scope.clone()));
        Ok(())
    }
}

struct NativeContentFixture {
    state: Arc<CapnpBrokerState>,
    connection: signed_worker::Connection,
    rpc: runtime_collab::CollabClient,
    launcher: Arc<ContentScopeLauncher>,
    _fanout: Arc<FanoutState>,
}

async fn native_content_connection(
    room: &signed_worker::OriginalRoom,
    authenticated: bool,
) -> NativeContentFixture {
    let launcher = Arc::new(ContentScopeLauncher {
        app: room.route.app_id.clone(),
        room: room.route.cluster_id.clone(),
        opened: Mutex::new(Vec::new()),
    });
    let fanout = Arc::new(FanoutState::new());
    let (state, bridge) = CapnpBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(room.membership.authority()),
        None,
        None,
        HostServices {
            launcher: Some(launcher.clone()),
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
    if authenticated {
        std::io::Write::write_all(
            &mut client,
            &collab_types::local_transport::COLLAB_LAUNCH_MAGIC,
        )
        .unwrap();
        let handoff = serde_json::to_vec(&serde_json::json!({
            "protocol": collab_types::local_transport::COLLAB_LAUNCH_PROTOCOL_VERSION,
            "app_id": room.route.app_id, "workspace_id": room.route.cluster_id,
        }))
        .unwrap();
        std::io::Write::write_all(
            &mut client,
            &launcher::services::COLLAB_LAUNCH_FRAMING
                .encode(&handoff)
                .unwrap(),
        )
        .unwrap();
    }
    let (rpc, server, client) =
        signed_worker::rpc_pair(state.clone(), 991, server, client, None, directories);
    let handle = rpc
        .join_channel(
            &room.route.channel_key(),
            &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
        )
        .await
        .unwrap();
    NativeContentFixture {
        state,
        connection: signed_worker::Connection {
            handle,
            server,
            client,
        },
        rpc,
        launcher,
        _fanout: fanout,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_content_launch_requires_live_authenticated_source_scope() {
    use tilo_actions::remote::{
        InvokeArgument, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            for authenticated in [true, false] {
                let room = signed_worker::OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let fixture = native_content_connection(&room, authenticated).await;
                let connection = &fixture.connection;
                let launcher = &fixture.launcher;
                let descriptor = continuation_descriptor();
                connection
                    .handle
                    .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
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
                        991,
                        &descriptor.resource,
                        &descriptor.action,
                        None,
                        "{}",
                    )
                    .await
                    .unwrap();
                let delivered = stream.read().await.unwrap();
                assert_eq!(delivered.id, wire);
                let parent =
                    tilo_actions::InvocationId(uuid::Uuid::from_bytes(delivered.invocation_uuid));
                let mut child = local_host_request(false);
                child.target = tilo_actions::remote::application_open_target(
                    &room.route.cluster_id,
                    "so.tilo.photos",
                );
                let hash = "ab".repeat(32);
                child.argument = InvokeArgument::Text(hash.clone());
                child.on_behalf_of = Some(parent);
                let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone()))
                    .encode()
                    .unwrap();
                let answer = connection.handle.remote_action(&frame).await.unwrap();
                let RemoteActionMessage::Receipt(receipt) =
                    RemoteActionFrame::decode(&answer).unwrap().message
                else {
                    panic!("expected actual host invocation receipt");
                };
                assert_eq!(receipt.invocation, child.invocation);
                if authenticated {
                    assert!(
                        matches!(receipt.status, ReceiptStatus::Completed { .. }),
                        "{:?}",
                        receipt.status
                    );
                    assert_eq!(
                        *launcher.opened.lock().unwrap(),
                        vec![(
                            hash,
                            crate::services::LaunchScope {
                                app_id: room.route.app_id.clone(),
                                workspace_id: room.route.cluster_id.clone()
                            }
                        )]
                    );
                } else {
                    assert!(
                        matches!(receipt.status, ReceiptStatus::Rejected { .. }),
                        "{:?}",
                        receipt.status
                    );
                    assert!(launcher.opened.lock().unwrap().is_empty());
                }
                if authenticated {
                    let foreign_route = CrdtRoute::record(&room.route.app_id, &room.route.cluster_id, "foreign-record");
                    let foreign = fixture.rpc.join_channel(&foreign_route.channel_key(),
                        &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key)).await.unwrap();
                    child.invocation = tilo_actions::InvocationId::new();
                    let foreign_frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone())).encode().unwrap();
                    let reply = foreign.remote_action(&foreign_frame).await.unwrap();
                    assert!(matches!(RemoteActionFrame::decode(&reply).unwrap().message,
                        RemoteActionMessage::Receipt(receipt) if matches!(receipt.status, ReceiptStatus::Rejected { .. })));
                    assert_eq!(launcher.opened.lock().unwrap().len(), 1);
                }
                fixture.state.remove_connection(991);
                child.invocation = tilo_actions::InvocationId::new();
                let retired_frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(child)).encode().unwrap();
                if let Ok(reply) = connection.handle.remote_action(&retired_frame).await {
                    assert!(matches!(RemoteActionFrame::decode(&reply).unwrap().message,
                        RemoteActionMessage::Receipt(receipt) if matches!(receipt.status, ReceiptStatus::Rejected { .. })),
                        "retired native scope must refuse before dispatch");
                }
                assert_eq!(launcher.opened.lock().unwrap().len(), usize::from(authenticated));
            }
        })
        .await;
}
