#[tokio::test(flavor = "current_thread")]
async fn reliable_resolution_binding_uses_exact_captured_signed_worker() {
    use std::sync::atomic::AtomicUsize;
    struct CurrentHost {
        route: CrdtRoute,
        certificate: Mutex<Option<tilo_user_identity::DeviceCert>>,
        checks: AtomicUsize,
        time: crate::peer_actions::PolicyTime,
    }
    impl crate::peer_actions::CertificateAuthority for CurrentHost {
        fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
            Ok(Some(self.time.clone()))
        }
        fn authorize(
            &self,
            room: uuid::Uuid,
            cert: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            if room.to_string() != self.route.cluster_id
                || self.certificate.lock().unwrap().as_ref() != Some(cert)
            {
                return Err("host certificate changed or revoked".into());
            }
            Ok(())
        }
        // A permissive ordinary provider still cannot create descriptor provenance.
        fn authorize_join(
            &self,
            room: uuid::Uuid,
            cert: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            self.authorize(room, cert)
        }
        fn authorize_host_worker(
            &self,
            route: &CrdtRoute,
            cert: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            if route != &self.route || !cert.capabilities.can_act_as_host {
                return Err("wrong host worker route or role".into());
            }
            self.authorize(route.cluster_id.parse().unwrap(), cert)
        }
    }
    struct HeldHostDirectory {
        entered: mpsc::UnboundedSender<&'static str>,
        release: Rc<tokio::sync::Notify>,
        row: runtime_collab::ActionDirectoryRow,
    }
    impl action_directory::Server for HeldHostDirectory {
        fn query(
            self: Rc<Self>,
            _params: action_directory::QueryParams,
            mut results: action_directory::QueryResults,
        ) -> Promise<(), capnp::Error> {
            Promise::from_future(async move {
                self.entered.send("query").unwrap();
                self.release.notified().await;
                write_directory_page(
                    results.get().init_page(),
                    &runtime_collab::ActionDirectoryPage {
                        revision: "directory-r1".into(),
                        rows: vec![self.row.clone()],
                        next_cursor: vec![],
                        exhausted: true,
                        stale_cursor: false,
                        budget_exhausted: false,
                    },
                )
            })
        }
        fn resolve(
            self: Rc<Self>,
            _params: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            Promise::from_future(async move {
                self.entered.send("resolve").unwrap();
                self.release.notified().await;
                write_directory_row(results.get().init_resolution().init_found(), &self.row);
                Ok(())
            })
        }
    }
    fn presentation(
        room: &str,
        cert: &tilo_user_identity::DeviceCert,
        key: &SigningKey,
    ) -> Vec<u8> {
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.into(),
            device_certificate: serde_json::to_vec(cert).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
    fn rpc_pair(
        state: Arc<CapnpBrokerState>,
        peer: u64,
        server: std::os::unix::net::UnixStream,
        client: std::os::unix::net::UnixStream,
        worker: Option<WorkerBinding>,
        directories: Rc<LocalActionDirectoryRegistry>,
    ) -> (
        runtime_collab::CollabClient,
        tokio::task::JoinHandle<Result<(), capnp::Error>>,
        tokio::task::JoinHandle<()>,
    ) {
        server.set_nonblocking(true).unwrap();
        client.set_nonblocking(true).unwrap();
        let server_task = tokio::task::spawn_local(drive_connection(
            tokio::net::UnixStream::from_std(server).unwrap(),
            state,
            directories,
            peer,
            worker,
        ));
        let (r, w) = tokio::net::UnixStream::from_std(client)
            .unwrap()
            .into_split();
        let network = Box::new(twoparty::VatNetwork::new(
            futures::io::BufReader::new(r.compat()),
            futures::io::BufWriter::new(w.compat_write()),
            rpc_twoparty_capnp::Side::Client,
            capnp::message::ReaderOptions::default(),
        ));
        let mut rpc = RpcSystem::new(network, None);
        let client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
        let client_task = tokio::task::spawn_local(async move {
            let _ = rpc.await;
        });
        (
            runtime_collab::CollabClient::from_raw(client),
            server_task,
            client_task,
        )
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            for scenario in ["no-journal", "clock", "origin"] {
                let journal_dir = tempfile::tempdir().unwrap();
                let root = SigningKey::from_bytes(&[83; 32]);
                let key = SigningKey::from_bytes(&[84; 32]);
                let mut cert = tilo_user_identity::DeviceCert {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: "original worker".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::host(),
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
                let (clock, time_control) =
                    crate::peer_actions::OwnerPolicyClock::for_test(50_000_000_000);
                let origin = clock.new_origin();
                let time = origin.capture().unwrap();
                let mut workspace = crate::test_support::Workspace::test_default(
                    crate::test_support::WorkspaceId(83),
                    "Original room",
                );
                workspace.set_owner(&principal_id_for_root(cert.user_public_key));
                workspace.owner = None;
                workspace.capabilities = workspace
                    .capabilities
                    .into_iter()
                    .map(|cap| cap.with_expiry(100))
                    .collect();
                let route = CrdtRoute::record(
                    "dev.qos.word-processor",
                    workspace.room.to_string(),
                    "document-1",
                );
                let mut tree = WorkspaceTree::default();
                tree.workspaces.push(workspace);
                if scenario != "no-journal" {
                    tree.persist_path = Some(journal_dir.path().join("workspace.json"));
                }
                let membership = tree.shared();
                let fanout = Arc::new(FanoutState::new());
                let (owner_handle, _owner_mailbox) =
                    qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(8);
                let publisher = PeerPublisher::with_owner_commit(
                    Arc::new(LifecycleSink),
                    Arc::new(owner_handle),
                );
                let (state, _) = CapnpBrokerState::new(
                    &fanout,
                    Arc::new(Mutex::new(BrokerChannels::default())),
                    publisher,
                    Some(membership.clone()),
                    None,
                );
                let authority = Arc::new(CurrentHost {
                    route: route.clone(),
                    certificate: Mutex::new(Some(cert.clone())),
                    checks: AtomicUsize::new(0),
                    time,
                });
                assert!(state.certificate_authority.set(authority.clone()).is_ok());
                let bytes = presentation(&route.cluster_id, &cert, &key);
                let (socket, authorize) = state
                    .connect_local_worker_with_authorizer(route.clone(), &cert)
                    .unwrap();
                let pending = state.workers.lock().unwrap().pop().unwrap();
                let (_, directory_rx) = std::sync::mpsc::channel();
                let directories = Rc::new(LocalActionDirectoryRegistry::new(directory_rx));
                let (rpc, server, client) = rpc_pair(
                    state.clone(),
                    903,
                    pending.stream,
                    socket,
                    Some(pending.binding),
                    directories.clone(),
                );
                let handle = rpc
                    .join_channel(&route.channel_key(), &bytes)
                    .await
                    .unwrap();
                let directory_state = Rc::new(RefCell::new(TestActionDirectory {
                    revision: "original-r1".into(),
                    resolve_mode: DirectoryResolveMode::Found,
                    result: tilo_actions::ResultSpec::None,
                    confirmation_required: false,
                    query_count: 0,
                    resolve_count: 0,
                }));
                let directory = capnp_rpc::new_client(TestActionDirectoryServer {
                    state: directory_state.clone(),
                });
                handle
                    .publish_action_directory(&route.app_id, directory)
                    .await
                    .unwrap();

                assert!(matches!(
                    handle
                        .resolve_published_action(903, "original-r1", "workspace:atlas", "open")
                        .await
                        .unwrap(),
                    runtime_collab::ActionDirectoryResolution::Found(_)
                ));
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_err(),
                    "no stable UUID stream means no reliable binding"
                );
                let _invocations = handle.subscribe_stable_invocations().await.unwrap();
                if scenario == "no-journal" {
                    assert!(
                        handle
                            .resolve_published_action_for_invocation(
                                903,
                                "original-r1",
                                "workspace:atlas",
                                "open"
                            )
                            .await
                            .is_err(),
                        "read-only browsing cannot manufacture a durable journal binding"
                    );
                    assert!(matches!(
                        handle
                            .resolve_published_action(903, "original-r1", "workspace:atlas", "open")
                            .await
                            .unwrap(),
                        runtime_collab::ActionDirectoryResolution::Found(_)
                    ));
                    server.abort();
                    client.abort();
                    continue;
                }
                let (row, binding) = handle
                    .resolve_published_action_for_invocation(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                    )
                    .await
                    .expect("same captured real directory and worker provide reliable binding");
                assert_eq!(row.resource, "workspace:atlas");
                assert_eq!(binding.peer_id, 903);
                assert_eq!(binding.host, *state.invocation_host().unwrap().as_bytes());
                assert_eq!(
                    (&binding.app_id, &binding.record_id),
                    (&route.app_id, &route.record_id)
                );
                assert_eq!(binding.root, cert.user_public_key);
                assert_eq!(binding.device, cert.device_public_key);
                assert_ne!(binding.target_binding, [0; 32]);
                // Drop only the returned remote stream capability. The signed
                // channel and actual private worker remain live.
                drop(_invocations);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                loop {
                    if state
                        .invocation_subscribers
                        .lock()
                        .unwrap()
                        .get(&903)
                        .is_some_and(|sender| sender.is_closed())
                    {
                        break;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "remote stream capability release must close its receiver"
                    );
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                assert!(state.host_workers.lock().unwrap().contains_key(&903));
                assert!(matches!(
                    handle
                        .resolve_published_action(903, "original-r1", "workspace:atlas", "open")
                        .await
                        .unwrap(),
                    runtime_collab::ActionDirectoryResolution::Found(_)
                ));
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_err(),
                    "closed actual stream cannot qualify via a stale stable flag"
                );
                let legacy_stream = handle.subscribe_invocations().await.unwrap();
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_err(),
                    "live legacy replacement cannot qualify as a stable stream"
                );
                let _current_stream = handle.subscribe_stable_invocations().await.unwrap();
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_ok()
                );
                drop(legacy_stream);
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_ok(),
                    "dropping an older stream must not invalidate the current stable stream"
                );

                let directory = capnp_rpc::new_client(TestActionDirectoryServer {
                    state: directory_state.clone(),
                });
                handle
                    .publish_action_directory(&route.app_id, directory)
                    .await
                    .unwrap();
                let (_, replacement) = handle
                    .resolve_published_action_for_invocation(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                    )
                    .await
                    .unwrap();
                assert_ne!(
                    binding.target_binding, replacement.target_binding,
                    "same route/certificate directory replacement changes exact target binding"
                );
                for mode in ["replace", "revoke"] {
                    let (entered, mut entered_rx) = mpsc::unbounded_channel();
                    let release = Rc::new(tokio::sync::Notify::new());
                    let directory = capnp_rpc::new_client(HeldHostDirectory {
                        entered,
                        release: release.clone(),
                        row: directory_state.borrow().row(),
                    });
                    handle
                        .publish_action_directory(&route.app_id, directory)
                        .await
                        .unwrap();
                    let c = handle.clone();
                    let request = tokio::task::spawn_local(async move {
                        c.resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open",
                        )
                        .await
                    });
                    assert_eq!(entered_rx.recv().await, Some("resolve"));
                    if mode == "replace" {
                        let directory = capnp_rpc::new_client(TestActionDirectoryServer {
                            state: directory_state.clone(),
                        });
                        handle
                            .publish_action_directory(&route.app_id, directory)
                            .await
                            .unwrap();
                    } else {
                        *authority.certificate.lock().unwrap() = None;
                    }
                    release.notify_one();
                    assert!(
                        request.await.unwrap().is_err(),
                        "old captured resolution refused after {mode}"
                    );
                    *authority.certificate.lock().unwrap() = Some(cert.clone());
                }
                let directory = capnp_rpc::new_client(TestActionDirectoryServer {
                    state: directory_state.clone(),
                });
                handle
                    .publish_action_directory(&route.app_id, directory)
                    .await
                    .unwrap();
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_ok()
                );
                let (entered, mut entered_rx) = mpsc::unbounded_channel();
                let release = Rc::new(tokio::sync::Notify::new());
                handle
                    .publish_action_directory(
                        &route.app_id,
                        capnp_rpc::new_client(HeldHostDirectory {
                            entered,
                            release: release.clone(),
                            row: directory_state.borrow().row(),
                        }),
                    )
                    .await
                    .unwrap();
                let c = handle.clone();
                let pending = tokio::task::spawn_local(async move {
                    c.resolve_published_action_for_invocation(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                    )
                    .await
                });
                assert_eq!(entered_rx.recv().await, Some("resolve"));
                if scenario == "origin" {
                    origin.invalidate();
                } else {
                    time_control.set_wall_ns(10_000_000_000);
                    time_control.advance_monotonic(Duration::from_secs(80));
                }
                release.notify_one();
                assert!(
                    pending.await.unwrap().is_err(),
                    "the original retained clock/authority must survive resolution await"
                );
                assert!(
                    state.pending_invocations.lock().unwrap().is_empty(),
                    "read-only bindings admit no invocation"
                );
                assert_eq!(
                    state
                        .journal
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .next_wire_id(),
                    1
                );
                let _ = time_control;
                let _ = authorize;
                server.abort();
                client.abort();
            }
        })
        .await;
}
