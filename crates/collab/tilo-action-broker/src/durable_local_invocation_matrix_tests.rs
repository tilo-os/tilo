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
    impl Environment {
        fn new() -> Self {
            Self::with_owner_initializer(crate::test_owner::SessionOwner::initialize)
        }
        fn with_owner_initializer(
            initialize: impl FnOnce(&std::path::Path) -> Result<crate::test_owner::SessionOwner, String>,
        ) -> Self {
            let root = tempfile::tempdir().unwrap();
            let mut owner = initialize(&root.path().join("owner")).unwrap();
            let cert = owner.peer().authority().clone();
            let key = owner.signing_key().clone();
            let person = owner.person_key().clone();
            let start = cert.issued_at_ns + 1_000_000_000;
            let (clock, control) = crate::peer_actions::OwnerPolicyClock::for_test(start);
            let origin = clock.new_origin();
            let mut workspace = crate::test_support::Workspace::new(
                crate::test_support::WorkspaceId(84),
                "Durable actual RPC",
            );
            workspace.set_room(tilo_desktop_model::RoomId::parse(owner.peer().room()).unwrap());
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
                workspace.room.to_string(),
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
            Self {
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
        binding: runtime_collab::PublishedInvocationBinding,
        authorize: ui::invocation_scope::PublishedInvocationAuthorizer,
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
                worker,
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
            self.worker.server.abort();
            self.worker.client.abort();
            // Abort completion releases the real server's state/journal references.
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
    async fn publish(handle: &runtime_collab::ChannelHandle) -> Rc<RefCell<TestActionDirectory>> {
        publish_for(handle, "so.tilo.word-processor").await
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
    async fn durable_local_rpc_exact_uuid_duplicate_cancel_and_retained_observation() {
        tokio::task::LocalSet::new().run_until(async {
            let env=Environment::new();let rig=Rig::new(&env).await;
            let handle=&rig.worker.handle;let binding=&rig.binding;let stream=rig.stream.as_ref().unwrap();let authorize=&rig.authorize;
            let invocation=id(0xabcd_ef00_0000_0000_0000_0000_0000_0001);
            let (first,duplicate)=tokio::join!(
                handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),binding,"original-r1","workspace:atlas","open",Some("original"),"{}"),
                handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),binding,"original-r1","workspace:atlas","open",Some("original"),"{}")
            );
            let first=first.expect("genuine local caller UUID reaches existing durable dispatch");let duplicate=duplicate.unwrap();assert_eq!(first,duplicate);
            assert_eq!(first.invocation,*invocation.0.as_bytes());assert_eq!(first.host,binding.host);
            let accepted=first.result.unwrap();assert!(matches!(serde_json::from_str::<tilo_actions::InvocationUpdate>(&accepted.update_json).unwrap(),tilo_actions::InvocationUpdate::Accepted{invocation:id} if id==invocation));
            let delivered=stream.read().await.unwrap();assert_eq!(delivered.id,accepted.id);assert_eq!(delivered.invocation_uuid,*invocation.0.as_bytes());
            let request=ui::invocation_scope::PublishedInvocationRequest{wire_id:delivered.id,invocation,address:tilo_actions::ActionAddress{resource:delivered.resource.clone(),action:delivered.action.clone()},argument:delivered.argument.clone(),actor:delivered.actor.clone(),participant:None};
            let lease=authorize(&request).unwrap();lease.check().unwrap();
            let cancelled=handle.cancel_durable_published_invocation(&binding.host,invocation.0.as_bytes()).await.unwrap();assert!(cancelled.result.as_ref().unwrap().cancellation_requested);
            assert_eq!(cancelled,handle.cancel_durable_published_invocation(&binding.host,invocation.0.as_bytes()).await.unwrap());
            assert!(handle.invocation_cancellation(delivered.id).await.unwrap());
            lease.check().unwrap();std::fs::write(env.root.path().join("effect-count"),"1").unwrap();
            let completed=tilo_actions::InvocationUpdate::Succeeded{invocation,output:tilo_actions::ActionOutput::default()};
            handle.update_invocation(delivered.id,&serde_json::to_string(&completed).unwrap()).await.unwrap();
            let observed=handle.published_invocation_observation(&binding.host,invocation.0.as_bytes()).await.unwrap();assert_eq!(serde_json::from_str::<tilo_actions::InvocationUpdate>(&observed.result.as_ref().unwrap().update_json).unwrap(),completed);
            assert_eq!(observed,handle.cancel_durable_published_invocation(&binding.host,invocation.0.as_bytes()).await.unwrap());
            rig.directories.remove(903);
            assert_eq!(observed,handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),binding,"original-r1","workspace:atlas","open",Some("original"),"{}").await.unwrap(),"retained UUID lookup precedes missing live target");
            assert!(handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),binding,"original-r1","workspace:atlas","open",Some("changed"),"{}").await.is_err());
            for changed in ["peer", "app", "record", "root", "device", "registration", "host"] {
                let mut altered=binding.clone();
                match changed {"peer"=>altered.peer_id+=1,"app"=>altered.app_id.push('x'),"record"=>altered.record_id.push('x'),"root"=>altered.root[0]^=1,"device"=>altered.device[0]^=1,"registration"=>altered.target_binding[0]^=1,"host"=>altered.host[0]^=1,_=>unreachable!()}
                assert!(handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&altered,"original-r1","workspace:atlas","open",Some("original"),"{}").await.is_err(),"immutable {changed} binding cannot change on retained UUID");
            }
            for (revision,resource,action,argument) in [("changed-r","workspace:atlas","open",Some("original")),("original-r1","changed-resource","open",Some("original")),("original-r1","workspace:atlas","changed-action",Some("original")),("original-r1","workspace:atlas","open",None),("original-r1","workspace:atlas","open",Some(""))] {
                assert!(handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),binding,revision,resource,action,argument,"{}").await.is_err());
            }
            assert_eq!(std::fs::read_to_string(env.root.path().join("effect-count")).unwrap(),"1");
            assert_eq!(rig.state.journal.lock().unwrap().as_ref().unwrap().as_ref().unwrap().next_wire_id(),accepted.id+1);
            assert!(tokio::time::timeout(Duration::from_millis(25),stream.read()).await.is_err(),"concurrent duplicate causes one delivery");
            env.control.set_wall_ns(10_000_000_000);env.control.advance_monotonic(Duration::from_secs(80));
            assert!(handle.published_invocation_observation(&binding.host,invocation.0.as_bytes()).await.is_err(),"terminal observation still requires current original caller permission");
            env.origin.invalidate();rig.stop().await;
        }).await;
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
                use crate::test_support::{Grants as _, KindSet};
                let agent_grant =
                    env.membership.write().workspaces[0].admit_agent("scribe", KindSet::READ);
                env.membership.write().workspaces[0].allow_app(
                    tilo_action_policy::Subject::agent("scribe"),
                    &env.route.app_id,
                    KindSet::ALL,
                );
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
                    participant: None,
                };
                let lease = (rig.authorize)(&request).unwrap();
                lease.check().unwrap();
                assert!(
                    rig.state.invocation_statuses("agent:scribe").is_empty(),
                    "legacy actor-only status cannot disclose AuthenticatedLocal rows"
                );
                env.membership.write().workspaces[0].revoke_grant(&agent_grant);
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

    /// An edit a durable, certified invocation's effect submits is admitted
    /// as that invocation's caller: under their grants, and refused beyond
    /// them; a certified caller whose certificate no longer verifies
    /// continues nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_certified_invocations_edits_are_admitted_for_its_caller() {
        tokio::task::LocalSet::new()
            .run_until(async {
                use tilo_actions::remote::{
                    InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
                };
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let room = env.route.cluster_id.clone();
                {
                    use crate::test_support::{Grants as _, KindSet, Subject, TreePath};
                    let workspace = &mut env.membership.write().workspaces[0];
                    workspace.admit_agent("scribe", KindSet::READ);
                    workspace.allow_app(Subject::agent("scribe"), &env.route.app_id, KindSet::ALL);
                    workspace.deny(
                        Subject::agent("scribe"),
                        &TreePath::root()
                            .app(env.route.app_id.clone())
                            .any(tilo_action_policy::Level::Kind)
                            .resource("document-1:body")
                            .action("erase"),
                        KindSet::ALL,
                    );
                }
                let agent = rig.caller(&env, 905, Some("scribe")).await;
                let invocation = id(0xc001);
                invoke(&agent.handle, &rig.binding, invocation)
                    .await
                    .unwrap();
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                assert_eq!(delivered.actor, "agent:scribe");

                // The running invocation keeps its certified caller.
                let continued = rig
                    .state
                    .continuing_caller(903, invocation)
                    .expect("the durable invocation's caller");
                assert_eq!(
                    continued,
                    tilo_actions::remote::Caller::local_agent("scribe").in_room(room.clone())
                );
                assert!(rig.state.continuing_caller(904, invocation).is_none());

                // Its edits are admitted as the agent: what the grants allow,
                // and not what one denies.
                let action = |action: &str| PublishedAction {
                    semantic_identity: None,
                    resource: "document-1:body".into(),
                    action: action.into(),
                    resource_kind: "document".into(),
                    resource_label: "Document".into(),
                    action_label: action.into(),
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract::default(),
                    input: PublishedInput::None,
                    view: None,
                };
                rig.state
                    .replace_published_surfaces_for_test(vec![PublishedSurface {
                        peer_id: 903,
                        app_id: env.route.app_id.clone(),
                        route: env.route.clone(),
                        actions: vec![action("edit"), action("erase")],
                        live: true,
                    }]);
                let mut app = rig.state.capture_invocations_for_test(903);
                let edit = |action: &str| InvokeRequest {
                    on_behalf_of: Some(invocation),
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: tilo_actions::remote::OWN_PROVIDER.into(),
                        address: tilo_actions::ActionAddress::new("document-1:body", action),
                    },
                    argument: InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: None,
                };
                assert!(matches!(
                    rig.state
                        .action_invoke(&continued, Some(903), None, &edit("edit"))
                        .poll_ready(),
                    PendingPoll::Ready(Ok(_))
                ));
                assert_eq!(app.try_recv().unwrap().actor, "agent:scribe");
                assert!(!matches!(
                    rig.state
                        .action_invoke(&continued, Some(903), None, &edit("erase"))
                        .poll_ready(),
                    PendingPoll::Ready(Ok(_))
                ));
                assert!(app.try_recv().is_err(), "the denied edit reaches no app");

                // A remote device's certified caller is checked again each
                // time: revoked, it continues nothing.
                let certified = rig
                    .state
                    .verify_action_caller(&room, &presentation(&room, &env.cert, &env.key))
                    .unwrap();
                let certified = crate::collab_broker_capnp::ContinuingCaller::Certified {
                    caller: Box::new(certified),
                    foreign_restriction: false,
                    direct: None,
                };
                assert_eq!(
                    certified.current(&rig.state),
                    Some(
                        tilo_actions::remote::Caller::authenticated(
                            env.cert.user_public_key,
                            env.cert.device_public_key
                        )
                        .in_room(room.clone())
                    )
                );
                *env.authority.certificate.lock().unwrap() = None;
                assert!(certified.current(&rig.state).is_none());
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
                let wire = tilo_test_env::within(
                    "`rig.delivered_effect(&env, invocation)`",
                    rig.delivered_effect(&env, invocation),
                )
                .await;
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
                runtime_collab::write_action_directory_row(
                    results.get().init_resolution().init_found(),
                    &self.row,
                )
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
                    tilo_test_env::within("`rx.recv()`", rx.recv())
                        .await
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

                            rig.worker = join_worker(
                                &rig.state,
                                &rig.directories,
                                &env.route,
                                &env.cert,
                                &env.key,
                                903,
                            )
                            .await
                            .0;
                        }
                        "stream" => {
                            drop(rig.stream.take());
                            tilo_test_env::within(
                                "actual dropped stable capability closes server receiver",
                                async {
                                    loop {
                                        if rig
                                            .state
                                            .invocation_subscribers
                                            .lock()
                                            .unwrap()
                                            .get(&903)
                                            .is_some_and(
                                                tokio::sync::mpsc::UnboundedSender::is_closed,
                                            )
                                        {
                                            break;
                                        }
                                        tokio::task::yield_now().await;
                                    }
                                },
                            )
                            .await;
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
                let root = env.person.clone();
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
                    participant: None,
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
    #[tokio::test(flavor = "current_thread")]
    async fn host_execution_retirement_refuses_actual_retained_worker_custody() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let descriptor = directory_state("original-r1")
                    .borrow()
                    .row()
                    .descriptor
                    .unwrap();
                rig.worker
                    .handle
                    .publish_actions(&env.route.app_id, &[descriptor])
                    .await
                    .unwrap();
                let admitted = rig
                    .state
                    .host_workers
                    .lock()
                    .unwrap()
                    .get(&903)
                    .unwrap()
                    .clone();
                let caller = admitted.caller(&env.route.cluster_id).in_new_execution();
                let origin =
                    crate::collab_broker_capnp::published_invocation::InvocationCaller::Local {
                        route: env.route.clone(),
                        admitted: Box::new(admitted),
                    };
                let invocation = tilo_actions::InvocationId::new();
                let request = tilo_actions::remote::InvokeRequest {
                    invocation,
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: "903".into(),
                        address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                    },
                    argument: tilo_actions::remote::InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: None,
                    on_behalf_of: None,
                    wait_ms: None,
                };
                assert!(matches!(
                    rig.state
                        .action_invoke_with_origin(&caller, None, None, Some(&origin), &request,)
                        .poll_ready(),
                    tilo_actions::remote::PendingPoll::Ready(Ok(_))
                ));
                let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
                let request = ui::invocation_scope::PublishedInvocationRequest {
                    wire_id: delivered.id,
                    invocation,
                    address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
                    argument: delivered.argument,
                    actor: delivered.actor,
                    participant: Some(tilo_actions::ParticipantId::for_execution(
                        caller.execution_id().unwrap(),
                    )),
                };
                let lease = (rig.authorize)(&request).unwrap();
                lease.check().unwrap();
                caller.retire_execution();
                assert!(
                    lease.check().is_err(),
                    "retained checked effects cannot outlive their execution"
                );
                assert!(
                    (rig.authorize)(&request).is_err(),
                    "same delivered request cannot remint custody"
                );
                rig.stop().await;
            })
            .await;
    }

    include!("display_control_tests.rs");
    include!("admitted_local_host_tests.rs");
    include!("certified_launch_tests.rs");
    include!("observed_launch_tests.rs");
    include!("keyboard_map_read_authority_tests.rs");
}
