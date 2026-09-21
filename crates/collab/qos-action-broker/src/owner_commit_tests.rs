// Regressions for the standalone session owner's durability boundary.
mod owner_commit_tests {
    use super::*;

    fn canonical_tempdir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure owner broker fixture root");
        directory
    }

    struct ExplicitAuthority;
    impl crate::peer_actions::CertificateAuthority for ExplicitAuthority {
        fn authorize(
            &self,
            _: uuid::Uuid,
            _: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_owner_commit_void_publisher_is_not_a_durable_ack() {
        tokio::task::LocalSet::new().run_until(async {
            let directory = canonical_tempdir();
            let root = SigningKey::from_bytes(&[24; 32]);
            let device = SigningKey::from_bytes(&[25; 32]);
            let principal = principal_id_for_root(root.verifying_key().to_bytes());
            let mut workspace = crate::test_support::Workspace::test_default(
                crate::test_support::WorkspaceId(5), "Durability boundary");
            workspace.set_owner(&principal);
            let route = CrdtRoute::record("dev.qos.word-processor", workspace.room.to_string(), "tab:0000000000000001");
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let membership = tree.shared();
            let admitted = admit_verified_join(&membership.read(), &route, VerifiedJoinIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
            }).unwrap();
            let fanout = Arc::new(FanoutState::new());
            let sink = Arc::new(RecordingSink::default());
            let publisher = PeerPublisher::new(sink.clone());
            let config = crate::peer_actions::NativePeerBrokerConfig {
                journal_root: directory.path().join("journal"),
                certificate_authority: Arc::new(ExplicitAuthority),
            };
            let (state, bridge_rx) = CapnpBrokerState::new_configured(&fanout,
                Arc::new(Mutex::new(BrokerChannels::default())), publisher,
                Some(membership.authority()), None, Some(&config), HostServices::default());
            fanout.attach_capnp(state.clone());
            state.ensure_channel(&route.channel_key());
            let mut subscriber = state.subscribe_ops(&route.channel_key(), 2);
            let client: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                state, directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
                channel_id: route.channel_key(), peer_id: 1, verified_identity: admitted,
                lease: None,
            });
            let mut document = automerge::AutoCommit::new();
            word_processor::initialize_empty_document(&mut document).unwrap();
            let mut op = SignedOp::new_unsigned(
                DocumentMutation::Snapshot(document.save()).encode().unwrap(),
                document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
                Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
                document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
                collab_types::OpClass::Splice, SystemTime::now(), 1);
            sign_op(&mut op, &device).unwrap();
            let mut request = client.send_request();
            request.get().set_op(&postcard::to_stdvec(&op).unwrap());
            assert!(request.send().promise.await.is_err(),
                "standalone send RPC acknowledged a wired void publisher without an owner commit receipt");
            assert!(subscriber.try_recv().is_err(), "uncommitted state must not fan out");
        }).await;
    }

    /// A transport observer at the existing App boundary. The checked Writer
    /// owns and mutates its actual record; the observer records ordering only.
    struct WorkerTransport {
        route: ui::collaboration::RouteSpec,
        ack: bool,
        sent: Rc<RefCell<Vec<ui::collaboration::CollabRequest>>>,
        events: Rc<RefCell<Vec<ui::collaboration::CollabEvent>>>,
    }
    impl ui::collaboration::PeerTransport for WorkerTransport {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, request: ui::collaboration::CollabRequest) -> Result<(), String> {
            if matches!(request, ui::collaboration::CollabRequest::Hello { .. }) {
                self.ack = true;
            }
            self.sent.borrow_mut().push(request);
            Ok(())
        }
        fn poll(&mut self) -> Vec<ui::collaboration::CollabEvent> {
            std::mem::take(&mut *self.events.borrow_mut())
        }
        fn take_primary_route_result(
            &mut self,
        ) -> Option<Result<ui::collaboration::RouteSpec, String>> {
            std::mem::take(&mut self.ack).then(|| Ok(self.route.clone()))
        }
        fn join_route(&mut self, route: ui::collaboration::RouteSpec) -> Result<(), String> {
            assert_eq!(route, self.route);
            Ok(())
        }
        fn send_to_route(
            &mut self,
            route: ui::collaboration::RouteSpec,
            request: ui::collaboration::CollabRequest,
        ) -> Result<(), String> {
            assert_eq!(route, self.route);
            self.send(request)
        }
        fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn session_owner_commit_checked_writer_publishes_before_terminal() {
        use ui::application::{HostContext, LaunchTarget, RestoreInput};
        use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, RouteSpec};
        let room = uuid::Uuid::new_v4();
        let cache = ui::portable_storage::PortableStorage::new("worker-test", "device").unwrap();
        let mut owners = ui::record::RecordOwners::portable(cache.clone());
        let definition = word_processor::definition();
        let records = owners.open(definition.app_id, &room.to_string()).unwrap();
        let views = Rc::new(
            ui::application_view::InstanceViewStore::portable(cache, &room.to_string()).unwrap(),
        );
        let context = HostContext::prepare(
            definition,
            records.clone(),
            views,
            "device".into(),
            1,
            Rc::new(RefCell::new(
                ui::permissions::PermissionContext::permissive(),
            )),
            ui::permissions::UserId("worker".into()),
            tilo_actions::ActionContext::default(),
            &LaunchTarget::New,
        )
        .unwrap();
        let record = context.record().clone();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let route = RouteSpec::record(definition.app_id, room, record.id.as_str());
        let sent = Rc::new(RefCell::new(vec![]));
        let events = Rc::new(RefCell::new(vec![]));
        let mut manager = CollaborationManager::with_peer_transport(WorkerTransport {
            route: route.clone(),
            ack: false,
            sent: sent.clone(),
            events: events.clone(),
        });
        manager.send(CollabRequest::Hello {
            app_id: definition.app_id.into(),
            cluster_id: room,
            record: Some(record.id.as_str().into()),
            handle: None,
        });
        manager.poll();
        let mut app = ui::App::from_component_with_collaboration(
            component,
            ui::application::ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::new(),
            },
            manager,
        )
        .unwrap();
        events.borrow_mut().push(CollabEvent::Context {
            collaborators: vec![],
            permissions: vec![],
        });
        app.pump_collab_presence();
        let resource = sent
            .borrow()
            .iter()
            .find_map(|request| match request {
                CollabRequest::PublishActions { actions, .. } => actions
                    .iter()
                    .find(|a| a.action == "insert-table")
                    .map(|a| a.resource.clone()),
                _ => None,
            })
            .expect("real checked Writer must publish its operation");
        let before = records
            .read(&record.id, |doc| doc.clone().get_heads())
            .unwrap();
        sent.borrow_mut().clear();
        let invocation = tilo_actions::InvocationId::new();
        events.borrow_mut().push(CollabEvent::Invocation {
            id: 1,
            invocation: Some(invocation),
            resource,
            action: "insert-table".into(),
            argument: None,
            actor: "worker".into(),
            context: None,
        });
        app.pump_collab_presence();
        let after = records
            .read(&record.id, |doc| doc.clone().get_heads())
            .unwrap();
        assert_ne!(
            before, after,
            "the actual Writer operation must mutate its canonical owner"
        );
        let sent = sent.borrow();
        let terminal = sent.iter().position(|request| matches!(request,
            CollabRequest::UpdateInvocation { update: tilo_actions::InvocationUpdate::Succeeded { invocation: id, .. }, .. } if *id == invocation)).expect("Writer terminal result");
        assert!(
            sent[..terminal].iter().any(|request| matches!(
                request,
                CollabRequest::Document {
                    mutation: DocumentMutation::Snapshot(_) | DocumentMutation::Incremental(_),
                }
            )),
            "checked Writer completed before sending its changed owned document to the durable broker"
        );
        app.close_components();
    }

    struct OwnerAuthority(qos_peer_state::StatePeer);
    impl crate::peer_actions::CertificateAuthority for OwnerAuthority {
        fn authorize(
            &self,
            room: uuid::Uuid,
            cert: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            if room.to_string() != self.0.room() || !self.0.certificates().contains(cert) {
                return Err("certificate outside actual owner authority".into());
            }
            self.0
                .permissions(cert.device_public_key, SystemTime::now())
                .map(|_| ())
        }
    }
    fn owner_tree(owner: &qos_session_peer::SessionOwner) -> WorkspaceTreeHandle {
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(72),
            "Actual owner barrier",
        );
        workspace.room = uuid::Uuid::parse_str(owner.peer().room()).unwrap().into();
        workspace.set_owner(&principal_id_for_root(
            owner.peer().authority().user_public_key,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        tree.shared()
    }
    fn owner_config(
        root: &std::path::Path,
        owner: &qos_session_peer::SessionOwner,
    ) -> crate::peer_actions::NativePeerBrokerConfig {
        crate::peer_actions::NativePeerBrokerConfig {
            journal_root: root.join("journal"),
            certificate_authority: Arc::new(OwnerAuthority(owner.peer().clone())),
        }
    }
    struct OwnerFixture {
        membership: WorkspaceTreeHandle,
        owner: qos_session_peer::SessionOwner,
        mailbox: qos_session_peer::owner_commit::OwnerCommitMailbox,
        state: Arc<CapnpBrokerState>,
        client: channel_handle::Client,
        route: CrdtRoute,
        _fanout: Arc<FanoutState>,
    }
    fn owner_fixture(root: &std::path::Path) -> OwnerFixture {
        owner_fixture_with_receipt_fault(root, 0)
    }
    struct AlteredReceipt {
        inner: qos_session_peer::owner_commit::OwnerCommitHandle,
        fault: u8,
    }
    impl qos_peer_state::owner_commit::OwnerCommitService for AlteredReceipt {
        fn health(&self) -> Result<(), String> {
            self.inner.health()
        }
        fn commit(
            &self,
            request: qos_peer_state::owner_commit::OwnerCommitRequest,
        ) -> qos_peer_state::owner_commit::CommitFuture {
            let response = self.inner.commit(request);
            let fault = self.fault;
            Box::pin(async move {
                let mut receipt = response.await?;
                match fault {
                    1 => receipt.binding.room = uuid::Uuid::new_v4().to_string(),
                    2 => {
                        receipt.binding.target =
                            qos_peer_state::owner_commit::CommitTarget::Document {
                                app: "other.app".into(),
                                record: "other".into(),
                            }
                    }
                    3 => receipt.binding.request_digest[0] ^= 1,
                    4 => receipt.binding.basis[0] ^= 1,
                    _ => {}
                }
                Ok(receipt)
            })
        }
    }
    fn owner_fixture_with_receipt_fault(root: &std::path::Path, fault: u8) -> OwnerFixture {
        let owner = qos_session_peer::SessionOwner::initialize(&root.join("owner")).unwrap();
        let (handle, mailbox) = qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(2);
        let route = CrdtRoute::record(
            "dev.qos.word-processor",
            owner.peer().room(),
            "tab:0000000000000001",
        );
        let membership = owner_tree(&owner);
        let admitted = admit_verified_join(
            &membership.read(),
            &route,
            VerifiedJoinIdentity {
                user_public_key: owner.peer().authority().user_public_key,
                device_public_key: owner.peer().authority().device_public_key,
            },
        )
        .unwrap();
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_owner_commit(
                Arc::new(RecordingSink::default()),
                Arc::new(AlteredReceipt {
                    inner: handle,
                    fault,
                }),
            ),
            Some(membership.authority()),
            None,
            Some(&owner_config(root, &owner)),
            HostServices::default(),
        );
        fanout.attach_capnp(state.clone());
        state.ensure_channel(&route.channel_key());
        let client = capnp_rpc::new_client(CapnpChannelHandle {
            state: state.clone(),
            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
            channel_id: route.channel_key(),
            peer_id: 72,
            verified_identity: admitted,
            lease: None,
        });
        OwnerFixture {
            membership,
            owner,
            mailbox,
            state,
            client,
            route,
            _fanout: fanout,
        }
    }
    fn owner_snapshot(f: &OwnerFixture) -> SignedOp {
        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::load(
            &f.owner
                .peer()
                .document(&f.route.app_id, &f.route.record_id)
                .unwrap(),
        )
        .unwrap();
        doc.put(automerge::ROOT, "actual_worker_publication", true)
            .unwrap();
        doc.commit();
        let key = tilo_user_identity::persist::read_device_key(
            &tilo_user_identity::persist::IdentityPaths::under(f.owner.root().join("identity"))
                .device_privkey,
        )
        .unwrap();
        let mut envelope = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(doc.save()).encode().unwrap(),
            document_artifact_id(&f.route.app_id, &f.route.cluster_id, &f.route.record_id),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            document_attestation_id(&f.route.app_id, &f.route.cluster_id, &f.route.record_id),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            1000,
        );
        sign_op(&mut envelope, &key).unwrap();
        envelope
    }
    #[tokio::test(flavor = "current_thread")]
    async fn session_owner_commit_rpc_waits_for_actual_owner_before_fanout() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root = canonical_tempdir();
                let mut f = owner_fixture(root.path());
                let mut subscriber = f.state.subscribe_ops(&f.route.channel_key(), 73);
                let envelope = owner_snapshot(&f);
                let mut request = f.client.send_request();
                request
                    .get()
                    .set_op(&postcard::to_stdvec(&envelope).unwrap());
                let response = tokio::task::spawn_local(request.send().promise);
                let pending = f.mailbox.recv().await.unwrap();
                assert!(!response.is_finished());
                assert!(
                    subscriber.try_recv().is_err(),
                    "fanout cannot precede owner barrier"
                );
                let receipt = pending.apply(&mut f.owner).unwrap();
                response.await.unwrap().unwrap();
                assert!(subscriber.try_recv().is_ok());
                assert!(receipt.generation > 1);
                let after = f.owner.status();
                drop(f);
                let restored =
                    qos_session_peer::SessionOwner::open(&root.path().join("owner")).unwrap();
                assert_eq!(restored.status(), after);
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn owner_wait_rechecks_current_membership_before_document_fanout() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root = canonical_tempdir();
                let mut f = owner_fixture(root.path());
                let before = f.owner.status();
                let mut subscriber = f.state.subscribe_ops(&f.route.channel_key(), 73);
                let mut request = f.client.send_request();
                request
                    .get()
                    .set_op(&postcard::to_stdvec(&owner_snapshot(&f)).unwrap());
                let response = tokio::task::spawn_local(request.send().promise);
                let pending = f.mailbox.recv().await.unwrap();
                f.membership.write().workspaces.clear();
                pending.apply(&mut f.owner).unwrap();
                assert!(
                    response.await.unwrap().is_err(),
                    "revoked caller must not receive a successful acknowledgement after owner await"
                );
                assert!(
                    subscriber.try_recv().is_err(),
                    "current membership must gate fanout after owner await"
                );
                assert!(
                    f.owner.status().generation > before.generation,
                    "admitted commit still happened; revocation does not undo it"
                );
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn session_owner_commit_production_startup_has_real_owner_attachment() {
        let root = canonical_tempdir();
        let mut owner =
            qos_session_peer::SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let membership = owner_tree(&owner);
        let config = owner_config(root.path(), &owner);
        let (handle, mut mailbox) = qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(2);
        let owner_task = tokio::spawn(async move {
            while let Some(pending) = mailbox.recv().await {
                pending.apply(&mut owner).unwrap();
            }
            owner
        });
        let broker = crate::collab_broker::spawn_native_peer(
            root.path().join("app.sock"),
            membership.authority(),
            PeerPublisher::with_owner_commit(Arc::new(RecordingSink::default()), Arc::new(handle)),
            config,
        )
        .unwrap();
        assert!(
            !broker
                .app_action_broker()
                .unwrap()
                .peer_action_attachment()
                .unwrap()
                .1
                .is_nil()
        );
        assert!(root.path().join("journal/records.json").is_file());
        drop(broker);
        owner_task.abort();
        let _ = owner_task.await;
    }
    fn admitted_effect(
        f: &OwnerFixture,
    ) -> (
        InvocationKey,
        std::sync::mpsc::Receiver<InvocationCompletion>,
    ) {
        let invocation = tilo_actions::InvocationId::new();
        let key = InvocationKey {
            room: f.route.cluster_id.clone(),
            host: f.state.invocation_host().unwrap(),
            invocation,
            owner: InvocationOwner::Certified {
                root: f.owner.peer().authority().user_public_key,
                device: f.owner.peer().authority().device_public_key,
            },
        };
        f.state
            .journal
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .admit(key.clone(), serde_json::json!({"effect":"bounded-test"}), 1)
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        f.state.pending_invocations.lock().unwrap().insert(
            1,
            PendingInvocation {
                network_evidence: None,
                journal_key: Some(key.clone()),
                peer_id: 72,
                tx,
                update: tilo_actions::InvocationUpdate::Accepted { invocation },
                last_sequence: 0,
                actor: principal_id_for_root(f.owner.peer().authority().user_public_key),
                cancellable: false,
                retryable_on_disconnect: false,
                execution: tilo_actions::ExecutionMode::Immediate,
                result: tilo_actions::ResultSpec::None,
                operation_kind: tilo_actions::OperationKind::Effect,
                cluster_id: f.route.cluster_id.clone(),
                context: tilo_actions::ActionContext::for_resource(
                    "Publish",
                    "record:bounded",
                    "document",
                    "Receipt test",
                ),
                last_activity: std::time::Instant::now(),
                cancellation_requested_at: None,
            },
        );
        (key, rx)
    }
    #[tokio::test(flavor = "current_thread")]
    async fn session_owner_commit_receipt_precedes_terminal_and_failures_preserve_uncertainty() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for failure in [0, 1, 2, 3] {
                    let root = canonical_tempdir();
                    let mut f = owner_fixture(root.path());
                    let (key, result) = admitted_effect(&f);
                    // A real external side effect has happened before the application reports its result.
                    std::fs::write(root.path().join("effect"), b"once").unwrap();
                    let update = tilo_actions::InvocationUpdate::Succeeded {
                        invocation: key.invocation,
                        output: tilo_actions::ActionOutput::default(),
                    };
                    let mut request = f.client.update_invocation_request();
                    request.get().set_id(1);
                    request
                        .get()
                        .set_update_json(&serde_json::to_string(&update).unwrap());
                    let response = tokio::task::spawn_local(request.send().promise);
                    let pending = f.mailbox.recv().await.unwrap();
                    assert!(!response.is_finished());
                    assert!(result.try_recv().is_err());
                    assert!(matches!(
                        f.state
                            .journal
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .lookup(&key)
                            .unwrap()
                            .unwrap()
                            .update,
                        tilo_actions::InvocationUpdate::Accepted { .. }
                    ));
                    if failure == 1 {
                        let path = f.owner.root().join("session.bin");
                        std::fs::rename(&path, path.with_extension("saved")).unwrap();
                        std::fs::create_dir(path).unwrap();
                    } else if failure == 2 {
                        f.state
                            .journal
                            .lock()
                            .unwrap()
                            .as_mut()
                            .unwrap()
                            .as_mut()
                            .unwrap()
                            .fail_at = Some(invocation_journal::BarrierFailure::File);
                    }
                    if failure == 3 {
                        f.membership.write().workspaces.clear();
                    }
                    let receipt = pending.apply(&mut f.owner);
                    let response = response.await.unwrap();
                    let receipts = qos_peer_state::room::receipt_values(
                        f.owner.peer().admitted_room().unwrap(),
                    );
                    if failure == 0 {
                        assert!(receipt.is_ok());
                        assert!(response.is_ok());
                        assert!(result.try_recv().unwrap().code.is_empty());
                        assert!(receipts.contains_key(&key.invocation.0.to_string()));
                        assert_eq!(
                            f.state
                                .journal
                                .lock()
                                .unwrap()
                                .as_ref()
                                .unwrap()
                                .as_ref()
                                .unwrap()
                                .lookup(&key)
                                .unwrap()
                                .unwrap()
                                .update,
                            update
                        );
                    } else {
                        assert!(response.is_err());
                        assert!(
                            result.try_recv().is_err(),
                            "failure after effect must never report no effect"
                        );
                        assert!(f.state.pending_invocations.lock().unwrap().contains_key(&1));
                        assert_eq!(
                            receipts.contains_key(&key.invocation.0.to_string()),
                            failure == 2 || failure == 3
                        );
                        if failure != 3 {
                            assert!(f.state.invocation_host().is_err());
                        }
                    }
                    assert_eq!(std::fs::read(root.path().join("effect")).unwrap(), b"once");
                    drop(f);
                    let recovered = InvocationJournal::open(&root.path().join("journal"))
                        .unwrap()
                        .lookup(&key)
                        .unwrap()
                        .unwrap();
                    if failure != 0 {
                        assert!(recovered.uncertain);
                    }
                }
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn session_owner_commit_broker_rejects_unbound_receipt_after_real_barrier() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for fault in 1..=4 {
                    let root = canonical_tempdir();
                    let mut f = owner_fixture_with_receipt_fault(root.path(), fault);
                    let mut subscriber = f.state.subscribe_ops(&f.route.channel_key(), 73);
                    let envelope = owner_snapshot(&f);
                    let mut request = f.client.send_request();
                    request
                        .get()
                        .set_op(&postcard::to_stdvec(&envelope).unwrap());
                    let response = tokio::task::spawn_local(request.send().promise);
                    f.mailbox.recv().await.unwrap().apply(&mut f.owner).unwrap();
                    assert!(
                        response.await.unwrap().is_err(),
                        "foreign receipt field {fault} must fail the broker boundary"
                    );
                    assert!(subscriber.try_recv().is_err());
                    assert!(
                        f.state.invocation_host().is_err(),
                        "unproven response withdraws the capability"
                    );
                }
            })
            .await;
    }
    /// Exercises the public portable constructor, real SDK FD, checked Writer and sole actual owner.
    #[tokio::test(flavor = "current_thread")]
    async fn portable_constructor_checked_writer_commits_through_actual_owner_and_retires_authority()
     {
        use std::os::fd::IntoRawFd;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use ui::application::{HostContext, LaunchTarget, RestoreInput};
        use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
        use ui::portable_storage::{PortableStorage, StorageKey};
        use ui::record::{RecordCheckout, RecordId, RecordOwners};
        struct CurrentOwner(Arc<Mutex<Option<qos_peer_state::StatePeer>>>);
        impl crate::peer_actions::CertificateAuthority for CurrentOwner {
            fn authorize(
                &self,
                room: uuid::Uuid,
                cert: &tilo_user_identity::DeviceCert,
            ) -> Result<(), String> {
                let guard = self.0.lock().map_err(|_| "owner authority unavailable")?;
                let peer = guard.as_ref().ok_or("owner authority unavailable")?;
                if peer.room() != room.to_string() || !peer.certificates().contains(cert) {
                    return Err("certificate not admitted by owner".into());
                }
                peer.permissions(cert.device_public_key, SystemTime::now())
                    .map(|_| ())
            }
        }
        impl crate::services::WorkspaceAuthority for CurrentOwner {
            fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
                let guard = self.0.lock().map_err(|_| "owner authority unavailable")?;
                let peer = guard.as_ref().ok_or("owner authority unavailable")?;
                let doc = peer
                    .admitted_room()
                    .ok_or("admitted owner governance unavailable")?;
                let (name, owner, caps) = qos_peer_state::room::extract_governed_fields(doc);
                let mut workspace = qos_desktop::Workspace::new(
                    qos_desktop::WorkspaceId(72),
                    name.unwrap_or_else(|| "Owner room".into()),
                );
                workspace.room = uuid::Uuid::parse_str(peer.room())
                    .map_err(|e| e.to_string())?
                    .into();
                workspace.owner = owner;
                workspace.capabilities = caps;
                let mut tree = qos_desktop::WorkspaceTree::default();
                tree.workspaces.push(workspace);
                Ok(tree)
            }
        }
        struct Published(Arc<AtomicUsize>);
        impl crate::collab_outbound::PeerPublishSink for Published {
            fn app_document(&self, _: String, _: String, _: String, _: SignedOp) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn app_presence(&self, _: String, _: String, _: collab_types::PeerPresence) {}
            fn scrollback_request(&self, _: ArtifactId, _: u64, _: u16) {}
        }
        let root = canonical_tempdir();
        let mut owner =
            qos_session_peer::SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let initial_generation = owner.status().generation;
        let mut remote = owner.peer().clone();
        let cert = owner.peer().authority().clone();
        let room: uuid::Uuid = owner.peer().room().parse().unwrap();
        let key = tilo_user_identity::persist::read_device_key(
            &tilo_user_identity::persist::IdentityPaths::under(owner.root().join("identity"))
                .device_privkey,
        )
        .unwrap();
        let record = RecordId::for_tab(1);
        let definition = word_processor::definition();
        let route = CrdtRoute::record(definition.app_id, room.to_string(), record.as_str());
        let initial = owner
            .peer()
            .document(definition.app_id, record.as_str())
            .unwrap();
        let committed = Arc::new(Mutex::new(Some(owner.peer().clone())));
        let authority = Arc::new(CurrentOwner(committed.clone()));
        let (handle, mut mailbox) = qos_session_peer::owner_commit::OwnerCommitMailbox::bounded(8);
        let published = Arc::new(AtomicUsize::new(0));
        let broker = crate::collab_broker::spawn_native_peer(
            root.path().join("worker.sock"),
            authority.clone(),
            PeerPublisher::with_owner_commit(
                Arc::new(Published(published.clone())),
                Arc::new(handle),
            ),
            crate::peer_actions::NativePeerBrokerConfig {
                journal_root: root.path().join("journal"),
                certificate_authority: authority,
            },
        )
        .unwrap();
        let current = committed.clone();
        let (shutdown, mut stopping) = tokio::sync::oneshot::channel::<()>();
        let owner_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    pending = mailbox.recv() => if let Some(pending) = pending {
                        // Hold the authority read barrier until the committed projection
                        // is refreshed: the RPC receipt may wake the broker immediately.
                        let mut authority = current.lock().unwrap();
                        pending.apply(&mut owner).unwrap();
                        *authority = Some(owner.peer().clone());
                    } else { break; },
                    _ = &mut stopping => break,
                }
            }
            owner
        });
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_string(),
            device_certificate: serde_json::to_vec(&cert).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &room.to_string(),
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        let endpoint = || CertifiedEndpointIdentity::new(presentation.clone(), key.clone());
        // A raw same-user socket cannot choose a trusted application route.
        let raw = std::os::unix::net::UnixStream::connect(broker.capnp_socket()).unwrap();
        assert!(
            CollaborationManager::for_route_fd_with_identity(
                raw.into_raw_fd(),
                definition.app_id,
                room,
                Some(record.as_str().into()),
                None,
                endpoint()
            )
            .is_err()
        );
        // A private worker descriptor also refuses a different record on the same app/room.
        let wrong = broker.connect_local_worker(route.clone(), &cert).unwrap();
        assert!(
            CollaborationManager::for_route_fd_with_identity(
                wrong.into_raw_fd(),
                definition.app_id,
                room,
                Some("other-record".into()),
                None,
                endpoint()
            )
            .is_err()
        );
        let stream = broker.connect_local_worker(route.clone(), &cert).unwrap();
        let manager = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            definition.app_id,
            room,
            Some(record.as_str().into()),
            None,
            endpoint(),
        )
        .unwrap();
        let cache = PortableStorage::new("actual-owner-worker", "device").unwrap();
        cache
            .hydrate(
                StorageKey::Record {
                    package: definition.app_id.into(),
                    workspace: room.to_string(),
                    record: record.as_str().into(),
                },
                initial.clone(),
                Some(cert.device_public_key.to_vec()),
            )
            .unwrap();
        let mut owners = RecordOwners::portable(cache.clone());
        let records = owners.open(definition.app_id, &room.to_string()).unwrap();
        let views = Rc::new(
            ui::application_view::InstanceViewStore::portable(cache, &room.to_string()).unwrap(),
        );
        let checkout = RecordCheckout::new(record.clone(), definition.record_kind.clone());
        let target = LaunchTarget::Record {
            checkout,
            reveal: None,
        };
        let principal = principal_id_for_root(cert.user_public_key);
        let context = HostContext::prepare(
            definition,
            records.clone(),
            views,
            "device".into(),
            1,
            Rc::new(RefCell::new(
                ui::permissions::PermissionContext::permissive(),
            )),
            ui::permissions::UserId(principal.clone()),
            tilo_actions::ActionContext::default(),
            &target,
        )
        .unwrap();
        let component = definition
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        let mut app = ui::App::from_component_with_collaboration(
            component,
            ui::application::ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::new(),
            },
            manager,
        )
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let (surface, action) = loop {
            app.pump_collab_presence();
            if let Some(found) =
                broker
                    .published_action_surfaces()
                    .into_iter()
                    .find_map(|surface| {
                        surface
                            .actions
                            .iter()
                            .find(|action| action.action == "insert-table")
                            .cloned()
                            .map(|action| (surface, action))
                    })
            {
                break found;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "actual checked Writer did not publish actions"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let actions = broker.app_action_broker().unwrap();
        let job = actions
            .start(
                surface.peer_id,
                &action.resource,
                &action.action,
                None,
                &principal,
            )
            .unwrap();
        loop {
            app.pump_collab_presence();
            if job.completion.try_recv().is_ok() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "checked Writer invocation failed to terminate: {:?}",
                actions.status(job.id)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(matches!(
            actions.status(job.id),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert_eq!(
            published.load(Ordering::SeqCst),
            0,
            "durable owner publication must not fall through to the legacy callback"
        );
        let local = records
            .read(&record, |doc| doc.clone().get_heads())
            .unwrap();
        let saved = committed
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .document(definition.app_id, record.as_str())
            .unwrap();
        let mut saved_doc = automerge::AutoCommit::load(&saved).unwrap();
        assert_eq!(
            local,
            saved_doc.get_heads(),
            "terminal must follow actual owner commitment"
        );
        assert_ne!(initial, saved);
        // Loss of the current owner authority withdraws admission and disclosure.
        *committed.lock().unwrap() = None;
        assert!(broker.connect_local_worker(route, &cert).is_err());
        assert!(
            actions
                .start(
                    surface.peer_id,
                    &action.resource,
                    &action.action,
                    None,
                    &principal
                )
                .is_err()
        );
        assert!(actions.peer_action_attachment().is_err());
        app.close_components();
        drop(app);
        drop(broker);
        shutdown.send(()).unwrap();
        let owner = owner_task.await.unwrap();
        assert!(owner.status().generation > initial_generation);
        for bytes in owner.outgoing() {
            remote
                .receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).unwrap(),
                    SystemTime::now(),
                )
                .unwrap();
        }
        let mut remote_doc = automerge::AutoCommit::load(
            &remote.document(definition.app_id, record.as_str()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            remote_doc.get_heads(),
            saved_doc.get_heads(),
            "committed governor-covered publication must reproduce the actual Writer operation"
        );
        let expected = owner.status();
        drop(owner);
        let restored = qos_session_peer::SessionOwner::open(&root.path().join("owner")).unwrap();
        assert_eq!(restored.status(), expected);
    }
}
