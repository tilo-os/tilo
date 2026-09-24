mod durable_local_matrix {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    struct CurrentHost {
        route: CrdtRoute,
        certificate: Mutex<Option<tilo_user_identity::DeviceCert>>,
        additional_certificates: Mutex<Vec<tilo_user_identity::DeviceCert>>,
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
                || (self.certificate.lock().unwrap().as_ref() != Some(cert)
                    && !self.additional_certificates.lock().unwrap().contains(cert))
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

    struct DurableDirectory {
        state: Rc<RefCell<TestActionDirectory>>,
    }
    impl action_directory::Server for DurableDirectory {
        fn resolve(
            self: Rc<Self>,
            params: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            let request = match params.get() {
                Ok(request) => request,
                Err(error) => return Promise::err(error),
            };
            let revision = match request
                .get_revision()
                .and_then(|text| text.to_str().map_err(capnp::Error::from))
            {
                Ok(value) => value,
                Err(error) => return Promise::err(error),
            };
            let mut state = self.state.borrow_mut();
            state.resolve_count += 1;
            if revision != state.revision {
                results.get().init_resolution().set_stale_revision(());
                return Promise::ok(());
            }
            let mut row = state.row();
            let contract = tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            };
            assert!(contract.validate_declaration().is_empty());
            let encoded = serde_json::to_string(&contract).unwrap();
            let descriptor = row.descriptor.as_mut().unwrap();
            descriptor.operation_kind = "effect".into();
            descriptor.contract_json = encoded.clone();
            row.visibility_contract_json = encoded;
            write_directory_row(results.get().init_resolution().init_found(), &row);
            Promise::ok(())
        }
    }

    struct Environment {
        root: tempfile::TempDir,
        cert: tilo_user_identity::DeviceCert,
        key: SigningKey,
        route: CrdtRoute,
        membership: WorkspaceTreeHandle,
        publisher: Arc<PeerPublisher>,
        authority: Arc<CurrentHost>,
        origin: crate::peer_actions::PolicyOrigin,
        control: crate::peer_actions::PolicyClockTestControl,
        owner_task: tokio::task::JoinHandle<qos_session_peer::SessionOwner>,
    }
    impl Environment {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let mut owner =
                qos_session_peer::SessionOwner::initialize(&root.path().join("owner")).unwrap();
            let cert = owner.peer().authority().clone();
            let key = tilo_user_identity::persist::read_device_key(
                &tilo_user_identity::persist::IdentityPaths::under(
                    owner.root().canonicalize().unwrap().join("identity"),
                )
                .device_privkey,
            )
            .unwrap();
            let start = cert.issued_at_ns + 1_000_000_000;
            let (clock, control) = crate::peer_actions::OwnerPolicyClock::for_test(start);
            let origin = clock.new_origin();
            let mut workspace = crate::test_support::Workspace::test_default(
                crate::test_support::WorkspaceId(84),
                "Durable actual RPC",
            );
            workspace.room = qos_desktop::RoomId::parse(owner.peer().room()).unwrap();
            workspace.set_owner(&principal_id_for_root(cert.user_public_key));
            workspace.owner = None;
            workspace.capabilities = workspace
                .capabilities
                .into_iter()
                .map(|cap| cap.with_expiry(start / 1_000_000_000 + 50))
                .collect();
            let route = CrdtRoute::record(
                "so.tilo.word-processor",
                workspace.room.to_string(),
                "document-1",
            );
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            tree.persist_path = Some(root.path().join("workspace.json"));
            let membership = tree.shared();
            let (handle, mut mailbox) =
                qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(8);
            let publisher =
                PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(handle));
            let owner_task = tokio::task::spawn_local(async move {
                while let Some(pending) = mailbox.recv().await {
                    pending.apply(&mut owner).unwrap();
                }
                owner
            });
            let authority = Arc::new(CurrentHost {
                route: route.clone(),
                certificate: Mutex::new(Some(cert.clone())),
                additional_certificates: Mutex::new(vec![]),
                checks: AtomicUsize::new(0),
                time: origin.capture().unwrap(),
            });
            Self {
                root,
                cert,
                key,
                route,
                membership,
                publisher,
                authority,
                origin,
                control,
                owner_task,
            }
        }
    }
    impl Drop for Environment {
        fn drop(&mut self) {
            self.owner_task.abort();
        }
    }
    struct Connection {
        handle: runtime_collab::ChannelHandle,
        server: tokio::task::JoinHandle<Result<(), capnp::Error>>,
        client: tokio::task::JoinHandle<()>,
    }
    impl Drop for Connection {
        fn drop(&mut self) {
            self.server.abort();
            self.client.abort();
        }
    }
    struct Rig {
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        worker: Connection,
        stream: Option<runtime_collab::InvocationStream>,
        binding: runtime_collab::PublishedInvocationBinding,
        authorize: ui::invocation_scope::PublishedInvocationAuthorizer,
    }
    impl Rig {
        async fn new(env: &Environment) -> Self {
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.clone()),
                None,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            state
                .invocation_host()
                .expect("actual broker journal available after normal stop/open");
            let (_, rx) = std::sync::mpsc::channel();
            let directories = Rc::new(LocalActionDirectoryRegistry::new(rx));
            let (socket, authorize) = state
                .connect_local_worker_with_authorizer(env.route.clone(), &env.cert)
                .unwrap();
            let pending = state.workers.lock().unwrap().pop().unwrap();
            let (rpc, server, client) = rpc_pair(
                state.clone(),
                903,
                pending.stream,
                socket,
                Some(pending.binding),
                directories.clone(),
            );
            let handle = rpc
                .join_channel(
                    &env.route.channel_key(),
                    &presentation(&env.route.cluster_id, &env.cert, &env.key),
                )
                .await
                .unwrap();
            publish(&handle).await;
            let stream = handle.subscribe_stable_invocations().await.unwrap();
            let (_, binding) = handle
                .resolve_published_action_for_invocation(
                    903,
                    "original-r1",
                    "workspace:atlas",
                    "open",
                )
                .await
                .unwrap();
            Self {
                state,
                directories,
                worker: Connection {
                    handle,
                    server,
                    client,
                },
                stream: Some(stream),
                binding,
                authorize,
            }
        }
        async fn caller(&self, env: &Environment, peer: u64, agent: Option<&str>) -> Connection {
            let (server, socket) = std::os::unix::net::UnixStream::pair().unwrap();
            let (rpc, server, client) = rpc_pair(
                self.state.clone(),
                peer,
                server,
                socket,
                None,
                self.directories.clone(),
            );
            let route = CrdtRoute::cluster(env.route.cluster_id.clone());
            let handle = rpc
                .join_channel_as_kind(
                    &route.channel_key(),
                    &presentation(&route.cluster_id, &env.cert, &env.key),
                    agent.unwrap_or(""),
                    if agent.is_some() { "agent" } else { "person" },
                )
                .await
                .unwrap();
            Connection {
                handle,
                server,
                client,
            }
        }
        async fn stop(mut self) {
            self.worker.server.abort();
            self.worker.client.abort();
            // Abort completion releases the real server's state/journal references.
            let _ = (&mut self.worker.server).await;
            let _ = (&mut self.worker.client).await;
            self.state.stop();
        }
        async fn delivered_effect(&self, env: &Environment, id: tilo_actions::InvocationId) -> u64 {
            let delivered = self.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, Some(*id.0.as_bytes()));
            let request = ui::invocation_scope::PublishedInvocationRequest {
                wire_id: delivered.id,
                invocation: id,
                address: tilo_actions::ActionAddress {
                    resource: delivered.resource,
                    action: delivered.action,
                },
                argument: delivered.argument,
                actor: delivered.actor,
            };
            let lease = (self.authorize)(&request).unwrap();
            lease.check().unwrap();
            let path = env.root.path().join("effect-count");
            let prior = std::fs::read_to_string(&path)
                .ok()
                .map(|n| n.parse::<usize>().unwrap())
                .unwrap_or(0);
            std::fs::write(path, (prior + 1).to_string()).unwrap();
            delivered.id
        }
    }
    async fn publish(handle: &runtime_collab::ChannelHandle) -> Rc<RefCell<TestActionDirectory>> {
        let state = Rc::new(RefCell::new(TestActionDirectory {
            revision: "original-r1".into(),
            resolve_mode: DirectoryResolveMode::Found,
            result: tilo_actions::ResultSpec::None,
            confirmation_required: false,
            query_count: 0,
            resolve_count: 0,
        }));
        handle
            .publish_action_directory(
                "so.tilo.word-processor",
                capnp_rpc::new_client(DurableDirectory {
                    state: state.clone(),
                }),
            )
            .await
            .unwrap();
        state
    }
    async fn invoke(
        handle: &runtime_collab::ChannelHandle,
        binding: &runtime_collab::PublishedInvocationBinding,
        id: tilo_actions::InvocationId,
    ) -> Result<runtime_collab::PublishedInvocationObservation, String> {
        handle
            .invoke_resolved_published_action_durable(
                id.0.as_bytes(),
                binding,
                "original-r1",
                "workspace:atlas",
                "open",
                None,
                "{}",
            )
            .await
            .map_err(|error| error.to_string())
    }
    fn id(value: u128) -> tilo_actions::InvocationId {
        tilo_actions::InvocationId(uuid::Uuid::from_u128(value))
    }
    async fn no_delivery(rig: &Rig) {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                rig.stream.as_ref().unwrap().read()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_local_rpc_restart_and_real_journal_barriers() {
        tokio::task::LocalSet::new().run_until(async {
        use crate::collab_broker_capnp::invocation_journal::BarrierFailure;
        for barrier in [None,Some(BarrierFailure::File),Some(BarrierFailure::Directory),Some(BarrierFailure::AfterAdmission),Some(BarrierFailure::Terminal)] {
            let env=Environment::new();let rig=Rig::new(&env).await;let original=rig.binding.clone();let invocation=id(0xa000+barrier.map_or(0,|b|b as u128+1));
            if let Some(failure)=barrier.filter(|failure|*failure!=BarrierFailure::Terminal){rig.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at=Some(failure);}
            let admission=invoke(&rig.worker.handle,&original,invocation).await;
            if barrier.is_none() || barrier==Some(BarrierFailure::Terminal) {
                admission.unwrap();let wire=rig.delivered_effect(&env,invocation).await;
                if barrier==Some(BarrierFailure::Terminal){rig.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at=Some(BarrierFailure::File);}
                let update=tilo_actions::InvocationUpdate::Succeeded{invocation,output:tilo_actions::ActionOutput::default()};
                let result=rig.worker.handle.update_invocation(wire,&serde_json::to_string(&update).unwrap()).await;
                assert_eq!(result.is_ok(),barrier.is_none());
            }else{assert!(admission.is_err());no_delivery(&rig).await;assert!(!env.root.path().join("effect-count").exists());}
            rig.stop().await;
            let restarted=Rig::new(&env).await;
            let found=restarted.worker.handle.published_invocation_observation(&original.host,invocation.0.as_bytes()).await.unwrap();
            if barrier==Some(BarrierFailure::File){assert!(found.result.is_none());assert!(invoke(&restarted.worker.handle,&original,invocation).await.is_err(),"absent UUID cannot rebind an old target after restart");}
            else {
                let row=found.result.as_ref().unwrap();assert_eq!(row.uncertain,barrier.is_some());
                let update:tilo_actions::InvocationUpdate=serde_json::from_str(&row.update_json).unwrap();
                if barrier.is_some(){assert!(matches!(update,tilo_actions::InvocationUpdate::Failed{error,..} if error.code=="action_outcome_unknown"&&!error.retryable));}else{assert!(matches!(update,tilo_actions::InvocationUpdate::Succeeded{..}));}
                restarted.directories.remove(903);
                assert_eq!(found,invoke(&restarted.worker.handle,&original,invocation).await.unwrap());
                // Confirmation is one-use new-admission evidence, not new intent.
                assert_eq!(found,restarted.worker.handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&original,"original-r1","workspace:atlas","open",None,r#"{"confirm":true,"confirmation_phrase":"changed retry evidence"}"#).await.unwrap());
            }
            no_delivery(&restarted).await;
            assert_eq!(std::fs::read_to_string(env.root.path().join("effect-count")).ok(),if barrier.is_none()||barrier==Some(BarrierFailure::Terminal){Some("1".into())}else{None});
            restarted.stop().await;
        }
    }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_local_rpc_original_caller_agent_isolation_and_current_policy() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let agent_grant = {
                    let mut tree = env.membership.write();
                    tree.workspaces[0].grant_capability(CapOverride::membership(
                        Subject::agent("scribe"),
                        env.route.cluster_id.clone(),
                        Verb::Write,
                    ))
                };
                env.membership.write().workspaces[0].grant_capability(CapOverride::grant(
                    Subject::agent("scribe"),
                    collab_types::Scope::App(env.route.app_id.clone()),
                    Verb::Write,
                ));
                let person = rig.caller(&env, 904, None).await;
                let agent = rig.caller(&env, 905, Some("scribe")).await;
                let invocation = id(0xb001);
                let admitted = invoke(&agent.handle, &rig.binding, invocation)
                    .await
                    .unwrap();
                assert!(
                    person
                        .handle
                        .published_invocation_observation(
                            &rig.binding.host,
                            invocation.0.as_bytes()
                        )
                        .await
                        .unwrap()
                        .result
                        .is_none()
                );
                assert!(
                    person
                        .handle
                        .cancel_durable_published_invocation(
                            &rig.binding.host,
                            invocation.0.as_bytes()
                        )
                        .await
                        .unwrap()
                        .result
                        .is_none()
                );
                let wire = admitted.result.as_ref().unwrap().id;
                assert!(
                    agent
                        .handle
                        .published_invocation_status(wire)
                        .await
                        .unwrap()
                        .is_none(),
                    "legacy wire-id lookup cannot reveal even same actor AuthenticatedLocal row"
                );
                assert!(
                    agent
                        .handle
                        .cancel_published_invocation(wire)
                        .await
                        .is_err()
                );
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(delivered.actor, "agent:scribe");
                let request = ui::invocation_scope::PublishedInvocationRequest {
                    wire_id: delivered.id,
                    invocation,
                    address: tilo_actions::ActionAddress {
                        resource: delivered.resource,
                        action: delivered.action,
                    },
                    argument: delivered.argument,
                    actor: delivered.actor,
                };
                let lease = (rig.authorize)(&request).unwrap();
                lease.check().unwrap();
                assert!(
                    rig.state.invocation_statuses("agent:scribe").is_empty(),
                    "legacy actor-only status cannot disclose AuthenticatedLocal rows"
                );
                env.membership.write().workspaces[0].revoke_capability(&agent_grant);
                assert!(
                    agent
                        .handle
                        .published_invocation_observation(
                            &rig.binding.host,
                            invocation.0.as_bytes()
                        )
                        .await
                        .is_err()
                );
                assert!(lease.check().is_err());
                assert!(
                    invoke(&agent.handle, &rig.binding, invocation)
                        .await
                        .is_err()
                );
                assert!(!admitted.result.unwrap().uncertain);
                no_delivery(&rig).await;
                drop(person);
                drop(agent);
                rig.stop().await;
            })
            .await;
    }

    #[derive(Clone, Default)]
    struct ReadGate(
        Arc<(
            std::sync::atomic::AtomicBool,
            Mutex<Option<std::task::Waker>>,
        )>,
    );
    struct GatedRead {
        inner: tokio::net::unix::OwnedReadHalf,
        gate: ReadGate,
    }
    impl tokio::io::AsyncRead for GatedRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.gate.0.0.load(Ordering::SeqCst) {
                *self.gate.0.1.lock().unwrap() = Some(cx.waker().clone());
                return std::task::Poll::Pending;
            }
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }
    async fn gated_caller(rig: &Rig, env: &Environment, peer: u64) -> (Connection, ReadGate) {
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        client.set_nonblocking(true).unwrap();
        let server = tokio::task::spawn_local(drive_connection(
            tokio::net::UnixStream::from_std(server).unwrap(),
            rig.state.clone(),
            rig.directories.clone(),
            peer,
            None,
        ));
        let (read, write) = tokio::net::UnixStream::from_std(client)
            .unwrap()
            .into_split();
        let gate = ReadGate::default();
        let network = Box::new(twoparty::VatNetwork::new(
            futures::io::BufReader::new(
                GatedRead {
                    inner: read,
                    gate: gate.clone(),
                }
                .compat(),
            ),
            futures::io::BufWriter::new(write.compat_write()),
            rpc_twoparty_capnp::Side::Client,
            capnp::message::ReaderOptions::default(),
        ));
        let mut rpc = RpcSystem::new(network, None);
        let raw = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
        let client = tokio::task::spawn_local(async move {
            let _ = rpc.await;
        });
        let rpc = runtime_collab::CollabClient::from_raw(raw);
        let route = CrdtRoute::cluster(env.route.cluster_id.clone());
        let handle = rpc
            .join_channel(
                &route.channel_key(),
                &presentation(&route.cluster_id, &env.cert, &env.key),
            )
            .await
            .unwrap();
        (
            Connection {
                handle,
                server,
                client,
            },
            gate,
        )
    }
    #[tokio::test(flavor = "current_thread")]
    async fn durable_local_rpc_lost_reply_recovers_original_uuid_on_new_signed_channel() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let (caller, gate) = gated_caller(&rig, &env, 911).await;
                let invocation = id(0xc001);
                // Block actual socket reads before sending: the caller never receives
                // its admission reply, while the target's independent connection runs.
                gate.0.0.store(true, Ordering::SeqCst);
                let handle = caller.handle.clone();
                let binding = rig.binding.clone();
                let pending =
                    tokio::task::spawn_local(
                        async move { invoke(&handle, &binding, invocation).await },
                    );
                let wire = tokio::time::timeout(
                    Duration::from_secs(2),
                    rig.delivered_effect(&env, invocation),
                )
                .await
                .unwrap();
                assert!(!pending.is_finished());
                let terminal = tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                };
                rig.worker
                    .handle
                    .update_invocation(wire, &serde_json::to_string(&terminal).unwrap())
                    .await
                    .unwrap();
                pending.abort();
                drop(caller);
                tokio::task::yield_now().await;
                let recovered = rig.caller(&env, 912, None).await;
                rig.directories.remove(903);
                let found = recovered
                    .handle
                    .published_invocation_observation(&rig.binding.host, invocation.0.as_bytes())
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::from_str::<tilo_actions::InvocationUpdate>(
                        &found.result.as_ref().unwrap().update_json
                    )
                    .unwrap(),
                    terminal
                );
                assert_eq!(
                    found,
                    invoke(&recovered.handle, &rig.binding, invocation)
                        .await
                        .unwrap()
                );
                assert_eq!(
                    std::fs::read_to_string(env.root.path().join("effect-count")).unwrap(),
                    "1"
                );
                no_delivery(&rig).await;
                drop(recovered);
                rig.stop().await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_local_rpc_stale_target_refuses_before_contacting_replacement() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let replacement = publish(&rig.worker.handle).await;
                assert!(
                    invoke(&rig.worker.handle, &rig.binding, id(0xd001))
                        .await
                        .is_err()
                );
                assert_eq!(
                    replacement.borrow().resolve_count,
                    0,
                    "original binding must be checked before contacting a replacement directory"
                );
                assert_eq!(
                    rig.state
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
                no_delivery(&rig).await;
                rig.stop().await;
            })
            .await;
    }

    struct GatedDirectory {
        block: Rc<std::cell::Cell<bool>>,
        entered: mpsc::UnboundedSender<()>,
        release: Rc<tokio::sync::Notify>,
        row: runtime_collab::ActionDirectoryRow,
    }
    impl action_directory::Server for GatedDirectory {
        fn resolve(
            self: Rc<Self>,
            _: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            Promise::from_future(async move {
                if self.block.get() {
                    self.entered.send(()).unwrap();
                    self.release.notified().await;
                }
                write_directory_row(results.get().init_resolution().init_found(), &self.row);
                Ok(())
            })
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn durable_local_rpc_rechecks_original_custody_after_real_resolve_await() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for mode in [
                    "directory",
                    "worker",
                    "stream",
                    "clock",
                    "revocation",
                    "retired",
                ] {
                    let env = Environment::new();
                    let mut rig = Rig::new(&env).await;
                    let row_state = TestActionDirectory {
                        revision: "original-r1".into(),
                        resolve_mode: DirectoryResolveMode::Found,
                        result: tilo_actions::ResultSpec::None,
                        confirmation_required: false,
                        query_count: 0,
                        resolve_count: 0,
                    };
                    let row = row_state.row();
                    let block = Rc::new(std::cell::Cell::new(false));
                    let release = Rc::new(tokio::sync::Notify::new());
                    let (entered, mut rx) = mpsc::unbounded_channel();
                    rig.worker
                        .handle
                        .publish_action_directory(
                            &env.route.app_id,
                            capnp_rpc::new_client(GatedDirectory {
                                block: block.clone(),
                                entered,
                                release: release.clone(),
                                row,
                            }),
                        )
                        .await
                        .unwrap();
                    let (_, binding) = rig
                        .worker
                        .handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open",
                        )
                        .await
                        .unwrap();
                    block.set(true);
                    let caller = rig.caller(&env, 914, None).await;
                    let handle = caller.handle.clone();
                    let pending = tokio::task::spawn_local(async move {
                        invoke(&handle, &binding, id(0xe001)).await
                    });
                    tokio::time::timeout(Duration::from_secs(1), rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    match mode {
                        "directory" => {
                            publish(&rig.worker.handle).await;
                        }
                        "worker" => {
                            rig.worker.server.abort();
                            rig.worker.client.abort();
                            let _ = (&mut rig.worker.server).await;
                            let _ = (&mut rig.worker.client).await;

                            let (socket, _) = rig
                                .state
                                .connect_local_worker_with_authorizer(env.route.clone(), &env.cert)
                                .unwrap();
                            let worker = rig.state.workers.lock().unwrap().pop().unwrap();
                            let (rpc, server, client) = rpc_pair(
                                rig.state.clone(),
                                903,
                                worker.stream,
                                socket,
                                Some(worker.binding),
                                rig.directories.clone(),
                            );
                            let handle = rpc
                                .join_channel(
                                    &env.route.channel_key(),
                                    &presentation(&env.route.cluster_id, &env.cert, &env.key),
                                )
                                .await
                                .unwrap();
                            rig.worker = Connection {
                                handle,
                                server,
                                client,
                            };
                        }
                        "stream" => {
                            drop(rig.stream.take());
                            tokio::time::timeout(Duration::from_secs(1), async {
                                loop {
                                    if rig
                                        .state
                                        .invocation_subscribers
                                        .lock()
                                        .unwrap()
                                        .get(&903)
                                        .is_some_and(|sender| sender.is_closed())
                                    {
                                        break;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("actual dropped stable capability closes server receiver");
                        }
                        "clock" => {
                            env.control.set_wall_ns(1);
                            env.control.advance_monotonic(Duration::from_secs(80));
                        }
                        "revocation" => {
                            *env.authority.certificate.lock().unwrap() = None;
                        }
                        "retired" => {
                            env.origin.invalidate();
                        }
                        _ => unreachable!(),
                    }
                    release.notify_one();
                    assert!(pending.await.unwrap().is_err(), "{mode}");
                    assert_eq!(
                        rig.state
                            .journal
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .next_wire_id(),
                        1,
                        "{mode} must fail before durable admission"
                    );
                    assert!(!env.root.path().join("effect-count").exists());
                    drop(caller);
                    rig.stop().await;
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_local_rpc_signed_device_isolation_and_original_certificate_expiry() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let original = id(0xf001);
                invoke(&rig.worker.handle, &rig.binding, original)
                    .await
                    .unwrap();
                rig.stream.as_ref().unwrap().read().await.unwrap();
                let identity = tilo_user_identity::persist::IdentityPaths::under(
                    env.root
                        .path()
                        .join("owner")
                        .canonicalize()
                        .unwrap()
                        .join("identity"),
                );
                let root =
                    tilo_user_identity::persist::read_signing_key(&identity.user_privkey).unwrap();
                let device = SigningKey::from_bytes(&[83; 32]);
                let mut certificate = env.cert.clone();
                certificate.device_public_key = device.verifying_key().to_bytes();
                certificate.device_label = "Actual signed temporary second device".into();
                certificate.capabilities = tilo_user_identity::DeviceCapabilities::member();
                certificate.expires_at_ns = Some(certificate.issued_at_ns + 6_000_000_000);
                tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
                env.authority
                    .additional_certificates
                    .lock()
                    .unwrap()
                    .push(certificate.clone());
                let (server, socket) = std::os::unix::net::UnixStream::pair().unwrap();
                let (rpc, server, client) = rpc_pair(
                    rig.state.clone(),
                    918,
                    server,
                    socket,
                    None,
                    rig.directories.clone(),
                );
                let route = CrdtRoute::cluster(env.route.cluster_id.clone());
                let handle = rpc
                    .join_channel(
                        &route.channel_key(),
                        &presentation(&route.cluster_id, &certificate, &device),
                    )
                    .await
                    .unwrap();
                let second = Connection {
                    handle,
                    server,
                    client,
                };
                assert!(
                    second
                        .handle
                        .published_invocation_observation(&rig.binding.host, original.0.as_bytes())
                        .await
                        .unwrap()
                        .result
                        .is_none()
                );
                assert!(
                    second
                        .handle
                        .cancel_durable_published_invocation(
                            &rig.binding.host,
                            original.0.as_bytes()
                        )
                        .await
                        .unwrap()
                        .result
                        .is_none()
                );
                let invocation = id(0xf002);
                invoke(&second.handle, &rig.binding, invocation)
                    .await
                    .unwrap();
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                let request = ui::invocation_scope::PublishedInvocationRequest {
                    wire_id: delivered.id,
                    invocation,
                    address: tilo_actions::ActionAddress {
                        resource: delivered.resource,
                        action: delivered.action,
                    },
                    argument: delivered.argument,
                    actor: delivered.actor,
                };
                let lease = (rig.authorize)(&request).unwrap();
                lease.check().unwrap();
                env.control.set_wall_ns(1);
                env.control.advance_monotonic(Duration::from_secs(7));
                assert!(
                    second
                        .handle
                        .published_invocation_observation(
                            &rig.binding.host,
                            invocation.0.as_bytes()
                        )
                        .await
                        .is_err()
                );
                assert!(lease.check().is_err());
                assert!(
                    rig.worker
                        .handle
                        .published_invocation_observation(&rig.binding.host, original.0.as_bytes())
                        .await
                        .unwrap()
                        .result
                        .is_some(),
                    "host and policy grant remain live; only original temporary device expired"
                );
                no_delivery(&rig).await;
                drop(second);
                rig.stop().await;
            })
            .await;
    }
}
