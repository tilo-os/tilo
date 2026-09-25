#[tokio::test(flavor = "current_thread")]
async fn durable_local_rpc_exact_uuid_duplicate_cancel_and_retained_observation() {
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
    tokio::task::LocalSet::new().run_until(async {
                let journal_dir = tempfile::tempdir().unwrap();
                let mut owner=qos_session_peer::SessionOwner::initialize(&journal_dir.path().join("owner")).unwrap();
                let cert=owner.peer().authority().clone();
                let key=tilo_user_identity::persist::read_device_key(&tilo_user_identity::persist::IdentityPaths::under(owner.root().canonicalize().unwrap().join("identity")).device_privkey).unwrap();
                let start_ns=cert.issued_at_ns.saturating_add(1_000_000_000);
                let (clock,time_control)=crate::peer_actions::OwnerPolicyClock::for_test(start_ns);
                let origin = clock.new_origin();
                let time = origin.capture().unwrap();
                let mut workspace = crate::test_support::Workspace::test_default(
                    crate::test_support::WorkspaceId(83),
                    "Original room",
                );
                workspace.room=qos_desktop::RoomId::parse(owner.peer().room()).unwrap();
                workspace.set_owner(&principal_id_for_root(cert.user_public_key));
                workspace.owner = None;
                workspace.capabilities = workspace
                    .capabilities
                    .into_iter()
                    .map(|cap| cap.with_expiry(start_ns / 1_000_000_000 + 50))
                    .collect();
                let route = CrdtRoute::record(
                    "so.tilo.word-processor",
                    workspace.room.to_string(),
                    "document-1",
                );
                let mut tree = WorkspaceTree::default();
                tree.workspaces.push(workspace);
                tree.persist_path = Some(journal_dir.path().join("workspace.json"));
                let membership = tree.shared();
                let fanout = Arc::new(FanoutState::new());
                let (owner_handle,mut owner_mailbox)=qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(8);
                let publisher=PeerPublisher::with_owner_commit(Arc::new(LifecycleSink),Arc::new(owner_handle));
                let owner_task=tokio::task::spawn_local(async move {
                    while let Some(pending)=owner_mailbox.recv().await {pending.apply(&mut owner).unwrap();}
                    owner
                });
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
                let directory = capnp_rpc::new_client(DurableDirectory {
                    state: directory_state.clone(),
                });
                handle
                    .publish_action_directory(&route.app_id, directory)
                    .await
                    .unwrap();


                let stream=handle.subscribe_stable_invocations().await.unwrap();
                let (_,binding)=handle.resolve_published_action_for_invocation(903,"original-r1","workspace:atlas","open").await.unwrap();
                let invocation=tilo_actions::InvocationId(uuid::Uuid::from_u128(0xabcdef00000000000000000000000001));
                let (first,duplicate)=tokio::join!(
                    handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&binding,"original-r1","workspace:atlas","open",Some("original"),"{}"),
                    handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&binding,"original-r1","workspace:atlas","open",Some("original"),"{}")
                );
                let first=first.expect("genuine local caller UUID reaches existing durable dispatch");let duplicate=duplicate.unwrap();assert_eq!(first,duplicate);
                assert_eq!(first.invocation,*invocation.0.as_bytes());assert_eq!(first.host,binding.host);
                let accepted=first.result.unwrap();assert!(matches!(serde_json::from_str::<tilo_actions::InvocationUpdate>(&accepted.update_json).unwrap(),tilo_actions::InvocationUpdate::Accepted{invocation:id} if id==invocation));
                let delivered=stream.read().await.unwrap();assert_eq!(delivered.id,accepted.id);assert_eq!(delivered.invocation_uuid,Some(*invocation.0.as_bytes()));
                let request=ui::invocation_scope::PublishedInvocationRequest{wire_id:delivered.id,invocation,address:tilo_actions::ActionAddress{resource:delivered.resource.clone(),action:delivered.action.clone()},argument:delivered.argument.clone(),actor:delivered.actor.clone(),participant:None};
                let lease=authorize(&request).unwrap();lease.check().unwrap();
                let cancelled=handle.cancel_durable_published_invocation(&binding.host,invocation.0.as_bytes()).await.unwrap();assert!(cancelled.result.as_ref().unwrap().cancellation_requested);
                assert_eq!(cancelled,handle.cancel_durable_published_invocation(&binding.host,invocation.0.as_bytes()).await.unwrap());
                assert!(handle.invocation_cancellation(delivered.id).await.unwrap());
                lease.check().unwrap();std::fs::write(journal_dir.path().join("effect-count"),"1").unwrap();
                let completed=tilo_actions::InvocationUpdate::Succeeded{invocation,output:tilo_actions::ActionOutput::default()};
                handle.update_invocation(delivered.id,&serde_json::to_string(&completed).unwrap()).await.unwrap();
                let observed=handle.published_invocation_observation(&binding.host,invocation.0.as_bytes()).await.unwrap();assert_eq!(serde_json::from_str::<tilo_actions::InvocationUpdate>(&observed.result.as_ref().unwrap().update_json).unwrap(),completed);
                assert_eq!(observed,handle.cancel_durable_published_invocation(&binding.host,invocation.0.as_bytes()).await.unwrap());
                directories.remove(903);
                assert_eq!(observed,handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&binding,"original-r1","workspace:atlas","open",Some("original"),"{}").await.unwrap(),"retained UUID lookup precedes missing live target");
                assert!(handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&binding,"original-r1","workspace:atlas","open",Some("changed"),"{}").await.is_err());
                for changed in ["peer", "app", "record", "root", "device", "registration", "host"] {
                    let mut altered=binding.clone();
                    match changed {"peer"=>altered.peer_id+=1,"app"=>altered.app_id.push('x'),"record"=>altered.record_id.push('x'),"root"=>altered.root[0]^=1,"device"=>altered.device[0]^=1,"registration"=>altered.target_binding[0]^=1,"host"=>altered.host[0]^=1,_=>unreachable!()}
                    assert!(handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&altered,"original-r1","workspace:atlas","open",Some("original"),"{}").await.is_err(),"immutable {changed} binding cannot change on retained UUID");
                }
                for (revision,resource,action,argument) in [("changed-r","workspace:atlas","open",Some("original")),("original-r1","changed-resource","open",Some("original")),("original-r1","workspace:atlas","changed-action",Some("original")),("original-r1","workspace:atlas","open",None),("original-r1","workspace:atlas","open",Some(""))] {
                    assert!(handle.invoke_resolved_published_action_durable(invocation.0.as_bytes(),&binding,revision,resource,action,argument,"{}").await.is_err());
                }
                assert_eq!(std::fs::read_to_string(journal_dir.path().join("effect-count")).unwrap(),"1");
                assert_eq!(state.journal.lock().unwrap().as_ref().unwrap().as_ref().unwrap().next_wire_id(),accepted.id+1);
                assert!(tokio::time::timeout(Duration::from_millis(25),stream.read()).await.is_err(),"concurrent duplicate causes one delivery");
                time_control.set_wall_ns(10_000_000_000);time_control.advance_monotonic(Duration::from_secs(80));
                assert!(handle.published_invocation_observation(&binding.host,invocation.0.as_bytes()).await.is_err(),"terminal observation still requires current original caller permission");
                origin.invalidate();server.abort();client.abort();owner_task.abort();
    }).await;
}
