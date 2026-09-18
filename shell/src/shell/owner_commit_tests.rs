// Regressions for the standalone session owner's durability boundary.
mod owner_commit_tests {
    use super::*;

    struct ExplicitAuthority;
    impl crate::shell::peer_actions::CertificateAuthority for ExplicitAuthority {
        fn authorize(
            &self,
            _: uuid::Uuid,
            _: &qos_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_owner_commit_void_publisher_is_not_a_durable_ack() {
        tokio::task::LocalSet::new().run_until(async {
            let directory = tempfile::tempdir().unwrap();
            let root = SigningKey::from_bytes(&[24; 32]);
            let device = SigningKey::from_bytes(&[25; 32]);
            let principal = principal_id_for_root(root.verifying_key().to_bytes());
            let mut workspace = crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(5), "Durability boundary");
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
            let config = crate::shell::peer_actions::NativePeerBrokerConfig {
                journal_root: directory.path().join("journal"),
                certificate_authority: Arc::new(ExplicitAuthority),
            };
            let (state, bridge_rx) = CapnpBrokerState::new_configured(&fanout,
                Arc::new(Mutex::new(BrokerChannels::default())), publisher,
                Some(membership), None, Some(&config));
            fanout.attach_capnp(state.clone());
            state.ensure_channel(&route.channel_key());
            let mut subscriber = state.subscribe_ops(&route.channel_key(), 2);
            let client: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                state, directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
                channel_id: route.channel_key(), peer_id: 1, verified_identity: admitted,
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
            qos_actions::ActionContext::default(),
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
        let invocation = qos_actions::InvocationId::new();
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
            CollabRequest::UpdateInvocation { update: qos_actions::InvocationUpdate::Succeeded { invocation: id, .. }, .. } if *id == invocation)).expect("Writer terminal result");
        assert!(
            sent[..terminal].iter().any(|request| matches!(
                request,
                CollabRequest::DocumentSnapshot { .. } | CollabRequest::DocumentIncremental { .. }
            )),
            "checked Writer completed before sending its changed owned document to the durable broker"
        );
        app.close_components();
    }

    struct OwnerAuthority(qos_peer_state::StatePeer);
    impl crate::shell::peer_actions::CertificateAuthority for OwnerAuthority {
        fn authorize(
            &self,
            room: uuid::Uuid,
            cert: &qos_user_identity::DeviceCert,
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
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(72),
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
    ) -> crate::shell::peer_actions::NativePeerBrokerConfig {
        crate::shell::peer_actions::NativePeerBrokerConfig {
            journal_root: root.join("journal"),
            certificate_authority: Arc::new(OwnerAuthority(owner.peer().clone())),
        }
    }
    struct OwnerFixture {
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
            Some(membership),
            None,
            Some(&owner_config(root, &owner)),
        );
        fanout.attach_capnp(state.clone());
        state.ensure_channel(&route.channel_key());
        let client = capnp_rpc::new_client(CapnpChannelHandle {
            state: state.clone(),
            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
            channel_id: route.channel_key(),
            peer_id: 72,
            verified_identity: admitted,
        });
        OwnerFixture {
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
        let key = qos_user_identity::persist::read_device_key(
            &qos_user_identity::persist::IdentityPaths::under(f.owner.root().join("identity"))
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
                let root = tempfile::tempdir().unwrap();
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
    async fn session_owner_commit_production_startup_has_real_owner_attachment() {
        let root = tempfile::tempdir().unwrap();
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
        let broker = crate::shell::collab_broker::spawn_native_peer(
            root.path().join("app.sock"),
            membership,
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
        let invocation = qos_actions::InvocationId::new();
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
                update: qos_actions::InvocationUpdate::Accepted { invocation },
                last_sequence: 0,
                actor: principal_id_for_root(f.owner.peer().authority().user_public_key),
                cancellable: false,
                retryable_on_disconnect: false,
                execution: qos_actions::ExecutionMode::Immediate,
                result: qos_actions::ResultSpec::None,
                operation_kind: qos_actions::OperationKind::Effect,
                cluster_id: f.route.cluster_id.clone(),
                context: qos_actions::ActionContext::for_resource(
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
                for failure in [0, 1, 2] {
                    let root = tempfile::tempdir().unwrap();
                    let mut f = owner_fixture(root.path());
                    let (key, result) = admitted_effect(&f);
                    // A real external side effect has happened before the application reports its result.
                    std::fs::write(root.path().join("effect"), b"once").unwrap();
                    let update = qos_actions::InvocationUpdate::Succeeded {
                        invocation: key.invocation,
                        output: qos_actions::ActionOutput::default(),
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
                        qos_actions::InvocationUpdate::Accepted { .. }
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
                            failure == 2
                        );
                        assert!(f.state.invocation_host().is_err());
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
                    let root = tempfile::tempdir().unwrap();
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
}
