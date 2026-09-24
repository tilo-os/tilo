// Genuine signed RPC and original worker custody for revision-bound lazy invocation.
#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_rpc_retains_actual_original_and_exact_worker() {
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
    async fn scenario(capabilities: tilo_user_identity::DeviceCapabilities, final_case: &str) {
        let root = SigningKey::from_bytes(&[83; 32]);
        let key = SigningKey::from_bytes(&[84; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "original worker".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: (final_case == "certificate").then_some(75_000_000_000),
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let (clock, time_control) = crate::peer_actions::OwnerPolicyClock::for_test(50_000_000_000);
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
            "so.tilo.word-processor",
            workspace.room.to_string(),
            "document-1",
        );
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (owner_handle, _owner_mailbox) =
            qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(8);
        let publisher =
            PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(owner_handle));
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
        assert!(
            handle
                .invoke_published_action(
                    903,
                    "workspace:atlas",
                    "open",
                    Some("original argument"),
                    "{}"
                )
                .await
                .is_err(),
            "the genuine lazy-only action must not enter via eager @21"
        );
        let invocations = handle.subscribe_stable_invocations().await.unwrap();
        let (id, _) = handle
            .invoke_resolved_published_action(
                903,
                "original-r1",
                "workspace:atlas",
                "open",
                Some("original argument"),
                "{}",
            )
            .await
            .unwrap();
        let delivered = invocations.read().await.unwrap();
        assert_eq!(delivered.id, id);
        let request = ui::invocation_scope::PublishedInvocationRequest {
            wire_id: id,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                delivered.invocation_uuid.unwrap(),
            )),
            address: tilo_actions::ActionAddress {
                resource: delivered.resource,
                action: delivered.action,
            },
            argument: delivered.argument,
            actor: delivered.actor,
        };
        let lease = authorize(&request)
            .expect("actual signed worker and original RPC caller must mint the exact lease");
        lease.check().unwrap();
        let mut altered = request.clone();
        altered.argument = Some("substituted".into());
        assert!(authorize(&altered).is_err());
        let retained = state.pending_invocations.lock().unwrap().len();
        for (resource, action, revision) in [
            ("wrong", "open", "original-r1"),
            ("workspace:atlas", "wrong", "original-r1"),
        ] {
            assert!(
                handle
                    .invoke_resolved_published_action(903, revision, resource, action, None, "{}")
                    .await
                    .is_err()
            );
        }
        directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Stale;
        assert!(
            handle
                .invoke_resolved_published_action(
                    903,
                    "stale-r0",
                    "workspace:atlas",
                    "open",
                    None,
                    "{}"
                )
                .await
                .is_err()
        );
        directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Found;
        for case in [
            "directory replacement",
            "revoked certificate",
            "lost membership",
            "oversized reply",
            "timeout",
        ] {
            let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
            let release = Rc::new(tokio::sync::Notify::new());
            let mut row = directory_state.borrow().row();
            if case == "oversized reply" {
                row.descriptor.as_mut().unwrap().action_label = "x".repeat(300 * 1024);
            }
            let held = capnp_rpc::new_client(HeldHostDirectory {
                entered: entered_tx,
                release: release.clone(),
                row,
            });
            handle
                .publish_action_directory(&route.app_id, held)
                .await
                .unwrap();
            let pending_handle = handle.clone();
            let resolving = tokio::task::spawn_local(async move {
                pending_handle
                    .invoke_resolved_published_action(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                        None,
                        "{}",
                    )
                    .await
            });
            assert_eq!(entered_rx.recv().await, Some("resolve"));
            let old_caps = if case == "lost membership" {
                Some(std::mem::take(
                    &mut membership.write().workspaces[0].capabilities,
                ))
            } else {
                None
            };
            if case == "revoked certificate" {
                *authority.certificate.lock().unwrap() = None;
            }
            if case == "directory replacement" {
                handle
                    .publish_action_directory(
                        &route.app_id,
                        capnp_rpc::new_client(TestActionDirectoryServer {
                            state: directory_state.clone(),
                        }),
                    )
                    .await
                    .unwrap();
            }
            if case != "timeout" {
                release.notify_one();
            }
            assert!(
                resolving.await.unwrap().is_err(),
                "{case} accepted after awaited resolution"
            );
            assert_eq!(
                state.pending_invocations.lock().unwrap().len(),
                retained,
                "{case} allocated an invocation"
            );
            if let Some(caps) = old_caps {
                membership.write().workspaces[0].capabilities = caps;
            }
            if case == "revoked certificate" {
                *authority.certificate.lock().unwrap() = Some(cert.clone());
            }
            handle
                .publish_action_directory(
                    &route.app_id,
                    capnp_rpc::new_client(TestActionDirectoryServer {
                        state: directory_state.clone(),
                    }),
                )
                .await
                .unwrap();
        }
        let (control, _) = handle
            .invoke_resolved_published_action(
                903,
                "original-r1",
                "workspace:atlas",
                "open",
                None,
                "{}",
            )
            .await
            .unwrap();
        assert_eq!(
            invocations.read().await.unwrap().id,
            control,
            "later valid control must progress after refused/cancelled resolutions"
        );
        state
            .pending_invocations
            .lock()
            .unwrap()
            .remove(&control)
            .unwrap();
        let (socket2, authorize2) = state
            .connect_local_worker_with_authorizer(route.clone(), &cert)
            .unwrap();
        let pending2 = state.workers.lock().unwrap().pop().unwrap();
        let (rpc2, server2, client2) = rpc_pair(
            state.clone(),
            904,
            pending2.stream,
            socket2,
            Some(pending2.binding),
            directories.clone(),
        );
        let _handle2 = rpc2
            .join_channel(&route.channel_key(), &bytes)
            .await
            .unwrap();
        assert_eq!(
            state.is_current_host_worker(904, &route, &cert).unwrap(),
            cert.capabilities.can_act_as_host,
            "private registration preserves the actual certificate role"
        );
        assert!(
            authorize2(&request).is_err(),
            "same route and cert on a second actual socket is not the original worker"
        );
        // Captured immutable facts survive terminal registry removal; a later
        // unrelated pending invocation cannot overwrite their origin.
        state
            .pending_invocations
            .lock()
            .unwrap()
            .remove(&id)
            .unwrap();
        lease.check().unwrap();
        assert!(
            authorize(&request).is_err(),
            "new captures require retained original pending custody"
        );
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let release = Rc::new(tokio::sync::Notify::new());
        handle
            .publish_action_directory(
                &route.app_id,
                capnp_rpc::new_client(HeldHostDirectory {
                    entered: entered_tx,
                    release: release.clone(),
                    row: directory_state.borrow().row(),
                }),
            )
            .await
            .unwrap();
        let pending_handle = if final_case == "target" {
            _handle2.clone()
        } else {
            handle.clone()
        };
        let resolving = tokio::task::spawn_local(async move {
            pending_handle
                .invoke_resolved_published_action(
                    903,
                    "original-r1",
                    "workspace:atlas",
                    "open",
                    None,
                    "{}",
                )
                .await
        });
        assert_eq!(entered_rx.recv().await, Some("resolve"));
        if final_case == "origin" {
            origin.invalidate();
        } else if final_case == "target" {
            server.abort();
            tokio::task::yield_now().await;
        } else {
            time_control.set_wall_ns(10_000_000_000);
            time_control.advance_monotonic(std::time::Duration::from_secs(
                if final_case == "certificate" { 30 } else { 80 },
            ));
        }
        release.notify_one();
        assert!(
            resolving.await.unwrap().is_err(),
            "original {final_case} invalidated during await must refuse"
        );
        assert!(state.pending_invocations.lock().unwrap().is_empty());
        assert!(
            lease.check().is_err(),
            "original {final_case} cannot be revived by wall rollback"
        );
        origin.invalidate();
        assert!(lease.check().is_err());
        server.abort();
        client.abort();
        server2.abort();
        client2.abort();
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            scenario(tilo_user_identity::DeviceCapabilities::host(), "grant").await;
            scenario(
                tilo_user_identity::DeviceCapabilities::host(),
                "certificate",
            )
            .await;
            scenario(tilo_user_identity::DeviceCapabilities::primary(), "origin").await;
            scenario(tilo_user_identity::DeviceCapabilities::primary(), "target").await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_capacity_is_bounded_and_cancellation_releases_it() {
    struct Held {
        entered: mpsc::UnboundedSender<()>,
        release: Rc<tokio::sync::Notify>,
        row: runtime_collab::ActionDirectoryRow,
    }
    impl action_directory::Server for Held {
        fn resolve(
            self: Rc<Self>,
            _: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            Promise::from_future(async move {
                self.entered.send(()).unwrap();
                self.release.notified().await;
                write_directory_row(results.get().init_resolution().init_found(), &self.row);
                Ok(())
            })
        }
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let (tx, mut entered) = mpsc::unbounded_channel();
            let release = Rc::new(tokio::sync::Notify::new());
            let held = capnp_rpc::new_client(Held {
                entered: tx,
                release,
                row: fixture.directory_state.borrow().row(),
            });
            fixture
                .directories
                .directories
                .borrow_mut()
                .get_mut(&fixture.peer_id)
                .unwrap()
                .directory = held;
            let mut requests = vec![];
            for _ in 0..64 {
                let state = fixture.state.clone();
                let directories = fixture.directories.clone();
                let cluster = fixture.cluster_id.clone();
                let peer = fixture.peer_id;
                requests.push(tokio::task::spawn_local(async move {
                    state
                        .resolve_invocation_offer(
                            &directories,
                            &BrokerActor::person("fixture-owner"),
                            &cluster,
                            peer,
                            "directory-r1",
                            "workspace:atlas",
                            "open",
                        )
                        .await
                }));
            }
            for _ in 0..64 {
                entered.recv().await.unwrap();
            }
            let refused = fixture
                .state
                .resolve_invocation_offer(
                    &fixture.directories,
                    &BrokerActor::person("fixture-owner"),
                    &fixture.cluster_id,
                    fixture.peer_id,
                    "directory-r1",
                    "workspace:atlas",
                    "open",
                )
                .await;
            assert_eq!(
                refused.err().unwrap(),
                "resolved invocation capacity exhausted"
            );
            assert!(fixture.state.pending_invocations.lock().unwrap().is_empty());
            for request in requests {
                request.abort();
                let _ = request.await;
            }
            assert_eq!(fixture.directories.resolved_permits.available_permits(), 64);
            fixture.reconnect_directory();
            let valid = fixture
                .state
                .resolve_invocation_offer(
                    &fixture.directories,
                    &BrokerActor::person("fixture-owner"),
                    &fixture.cluster_id,
                    fixture.peer_id,
                    "directory-r1",
                    "workspace:atlas",
                    "open",
                )
                .await
                .unwrap();
            assert_eq!(fixture.directories.resolved_permits.available_permits(), 63);
            drop(valid);
            assert_eq!(fixture.directories.resolved_permits.available_permits(), 64);
        })
        .await;
}
