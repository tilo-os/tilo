mod durable_local_matrix {
    use super::signed_worker::{
        Connection, CurrentHost, DurableDirectory, directory_state, join_worker, presentation,
        rpc_pair,
    };
    use super::*;
    struct Environment {
        root: tempfile::TempDir,
        cert: tilo_user_identity::DeviceCert,
        key: SigningKey,
        person: SigningKey,
        route: CrdtRoute,
        membership: WorkspaceTreeHandle,
        publisher: Arc<PeerPublisher>,
        authority: Arc<CurrentHost>,
        origin: crate::peer_actions::PolicyOrigin,
        control: crate::peer_actions::PolicyClockTestControl,
        owner_task: tokio::task::JoinHandle<crate::test_owner::SessionOwner>,
    }
    struct ProvisionedEnvironment {
        environment: Environment,
        _identity_env: tilo_test_env::ScopedEnv,
    }
    impl std::ops::Deref for ProvisionedEnvironment {
        type Target = Environment;
        fn deref(&self) -> &Environment {
            &self.environment
        }
    }
    impl std::ops::DerefMut for ProvisionedEnvironment {
        fn deref_mut(&mut self) -> &mut Environment {
            &mut self.environment
        }
    }
    impl Environment {
        #[expect(
            clippy::new_ret_no_self,
            reason = "the thread-bound identity guard stays separate from the Send broker fixture"
        )]
        fn new() -> ProvisionedEnvironment {
            Self::with_owner_initializer(crate::test_owner::SessionOwner::initialize)
        }
        fn with_owner_initializer(
            initialize: impl FnOnce(&std::path::Path) -> Result<crate::test_owner::SessionOwner, String>,
        ) -> ProvisionedEnvironment {
            let root = tempfile::tempdir().unwrap();
            let mut identity_env = tilo_test_env::ScopedEnv::new();
            identity_env
                .set("XDG_CONFIG_HOME", root.path().join("config"))
                .set("TILO_IDENTITY_ROOT", root.path().join("identity"))
                .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
            tilo_identity_host::provisioning::ensure_collaboration_identity(
                &root.path().join("config/tilo/identity"),
                &tilo_user_identity::IdentityPaths::under(root.path().join("identity")),
                tilo_identity_host::provisioning::IdentityStorage::Files,
            )
            .expect("provision real host receipt signer");
            let mut owner = initialize(&root.path().join("owner")).unwrap();
            let cert = owner.device();
            let key = owner.signing_key().clone();
            let person = owner.person_key().clone();
            let start = cert.issued_at_ns + 1_000_000_000;
            let (clock, control) = crate::peer_actions::OwnerPolicyClock::for_test(start);
            let origin = clock.new_origin();
            let mut workspace = crate::test_support::Workspace::in_room(
                crate::test_support::WorkspaceId(84),
                tilo_desktop_model::RoomId::parse(owner.peer().room()).unwrap(),
                "Durable actual RPC",
            );
            // Someone the room grants everything until 50 seconds after the
            // start, and nothing after.
            let principal = principal_id_for_root(cert.user_public_key);
            let changes = tilo_action_policy::Grant::new(
                "changes-for-a-while",
                tilo_action_policy::Subject::Person(principal.clone()),
                workspace.node(),
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Subject::Person(principal),
            )
            .with_lifetime(tilo_action_policy::Lifetime::Until(
                (start / 1_000_000_000 + 50) * 1000,
            ));
            workspace.grant(changes);
            let route = CrdtRoute::record(
                "so.tilo.word-processor",
                workspace.room().to_string(),
                "document-1",
            );
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            tree.persist_path = Some(root.path().join("workspace.json"));
            let membership = tree.shared();
            let (handle, mut mailbox) =
                crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(8);
            let publisher =
                PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(handle));
            let owner_task = tokio::task::spawn_local(async move {
                while let Some(pending) = mailbox.recv().await {
                    pending.apply(&mut owner).unwrap();
                }
                owner
            });
            let authority = CurrentHost::new(&route, &cert, Some(origin.capture().unwrap()));
            ProvisionedEnvironment {
                environment: Self {
                    root,
                    cert,
                    key,
                    person,
                    route,
                    membership,
                    publisher,
                    authority,
                    origin,
                    control,
                    owner_task,
                },
                _identity_env: identity_env,
            }
        }
    }
    impl Drop for Environment {
        fn drop(&mut self) {
            self.owner_task.abort();
        }
    }
    struct Rig {
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        worker: Connection,
        stream: Option<runtime_collab::InvocationStream>,
        authorize: ui::invocation_scope::PublishedInvocationAuthorizer,
        bridge_task: tokio::task::JoinHandle<()>,
    }
    impl Rig {
        async fn new(env: &Environment) -> Self {
            Self::with_services(env, env.membership.services()).await
        }
        async fn with_services(env: &Environment, services: HostServices) -> Self {
            let fanout = Arc::new(FanoutState::new());
            let (state, bridge_rx) = CapnpBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.authority()),
                None,
                None,
                services,
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
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
            let (worker, authorize) =
                join_worker(&state, &directories, &env.route, &env.cert, &env.key, 903).await;
            let handle = &worker.handle;
            publish_for(handle, &env.route.app_id).await;
            let stream = handle.subscribe_stable_invocations().await.unwrap();
            // Mirror the production accept loop: later continuations can queue
            // another eager admission after the caller's first explicit drain.
            let bridge_state = Arc::downgrade(&state);
            let bridge_directories = directories.clone();
            let turn = state.action_directory_bridge.turn.clone();
            let bridge_task = tokio::task::spawn_local(async move {
                loop {
                    turn.notified().await;
                    let Some(state) = bridge_state.upgrade() else {
                        break;
                    };
                    state.drain_action_directory_bridge(&bridge_directories);
                }
            });
            Self {
                state,
                directories,
                worker,
                stream: Some(stream),
                authorize,
                bridge_task,
            }
        }
        /// A connection for `app_id`, which publishes the actions it reaches
        /// through its own door: the application a person launched.
        async fn app_caller(
            &self,
            env: &Environment,
            peer: u64,
            app_id: &str,
            agent: Option<&str>,
        ) -> Connection {
            let (server, socket) = std::os::unix::net::UnixStream::pair().unwrap();
            let (rpc, server, client) = rpc_pair(
                self.state.clone(),
                peer,
                server,
                socket,
                None,
                self.directories.clone(),
            );
            let route = CrdtRoute::app(app_id, env.route.cluster_id.clone());
            let handle = rpc
                .join_channel_as_kind(
                    &route.channel_key(),
                    &presentation(&route.cluster_id, &env.cert, &env.key),
                    agent.unwrap_or(""),
                    if agent.is_some() { "agent" } else { "person" },
                    "",
                )
                .await
                .unwrap();
            Connection {
                handle,
                server,
                client,
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
                    "",
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
            self.bridge_task.abort();
            let _ = (&mut self.bridge_task).await;
            self.worker.server.abort();
            self.worker.client.abort();
            let _ = (&mut self.worker.server).await;
            let _ = (&mut self.worker.client).await;
            self.state.stop();
        }
        async fn delivered_effect(&self, env: &Environment, id: tilo_actions::InvocationId) -> u64 {
            let delivered = self.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *id.0.as_bytes());
            let request = ui::invocation_scope::PublishedInvocationRequest {
                wire_id: delivered.id,
                invocation: id,
                address: tilo_actions::ActionAddress {
                    resource: delivered.resource,
                    action: delivered.action,
                },
                argument: delivered.argument,
                actor: delivered.actor,
                participant: None,
            };
            let lease = (self.authorize)(&request).unwrap();
            lease.check().unwrap();
            let path = env.root.path().join("effect-count");
            let prior = std::fs::read_to_string(&path).map_or(0, |n| n.parse::<usize>().unwrap());
            std::fs::write(path, (prior + 1).to_string()).unwrap();
            delivered.id
        }
    }
    async fn publish_for(
        handle: &runtime_collab::ChannelHandle,
        app: &str,
    ) -> Rc<RefCell<TestActionDirectory>> {
        let state = directory_state("original-r1");
        handle
            .publish_action_directory(
                app,
                capnp_rpc::new_client(DurableDirectory {
                    state: state.clone(),
                }),
            )
            .await
            .unwrap();
        state
    }
    /// Ask the rig's application (peer 903) for its action as `handle`'s
    /// caller, the way a person's or agent's press reaches it.
    async fn invoke(
        handle: &runtime_collab::ChannelHandle,
        id: tilo_actions::InvocationId,
    ) -> Result<tilo_actions::remote::InvocationReceipt, String> {
        use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
        let request = tilo_actions::remote::InvokeRequest {
            invocation: id,
            target: tilo_actions::remote::RemoteActionTarget {
                provider: "903".into(),
                address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: Some("original-r1".into()),
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(request))
            .encode()
            .map_err(|error| error.to_string())?;
        let answer = handle
            .remote_action(&frame)
            .await
            .map_err(|error| error.to_string())?;
        match RemoteActionFrame::decode(&answer)
            .map_err(|error| error.to_string())?
            .message
        {
            RemoteActionMessage::Receipt(receipt) => match &receipt.status {
                ReceiptStatus::Rejected { error } => Err(format!("{error:?}")),
                _ => Ok(receipt),
            },
            other => Err(format!("unexpected answer {other:?}")),
        }
    }
    fn id(value: u128) -> tilo_actions::InvocationId {
        tilo_actions::InvocationId(uuid::Uuid::from_u128(value))
    }
    /// Nothing was dispatched to the rig's app (peer 903) that it has not read.
    fn no_delivery(_rig: &Rig) {
        assert_eq!(crate::collab_broker_capnp::unread_invocations(903), 0);
    }

    include!("display_control_tests.rs");
    include!("person_chat_rpc_tests.rs");
    include!("admitted_local_host_tests.rs");
    include!("certified_lifecycle_tests.rs");
    include!("dispatcher_receipt_tests.rs");
    include!("local_document_action_rpc_tests.rs");
    include!("packaged_app_worker_tests.rs");
    include!("certified_launch_tests.rs");
    include!("observed_launch_tests.rs");
    include!("app_open_door_tests.rs");
    include!("app_child_request_tests.rs");
    include!("keyboard_map_read_authority_tests.rs");
    include!("store_directory_publication_tests.rs");
}
