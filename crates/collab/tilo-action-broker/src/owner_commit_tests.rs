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
            let mut workspace = crate::test_support::Workspace::new(
                crate::test_support::WorkspaceId(5), "Durability boundary");
            workspace.set_owner(&principal);
            let route = CrdtRoute::record("so.tilo.word-processor", workspace.room.to_string(), "tab:0000000000000001");
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let membership = tree.shared();
            let admitted = admit_verified_join(&membership.read(), &route, VerifiedJoinIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                capped: false,
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
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state, directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
                channel_id: route.channel_key(), peer_id: 1, verified_identity: admitted,
                record_kind: None,
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

    struct RequiredNativeReadUnavailable;
    impl NativeDocumentReads for RequiredNativeReadUnavailable {
        fn requires_room(&self, _: &str) -> bool {
            true
        }
        fn authorize(&self, _: &NativeDocumentReadContext) -> Result<(), String> {
            Err("actual native owner unavailable".into())
        }
        fn capture(
            &self,
            _: &NativeDocumentReadContext,
            _: Option<[u8; 32]>,
            _: usize,
        ) -> Result<NativeDocumentRead, String> {
            Err("actual native capture unavailable".into())
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn committed_read_mode_distinguishes_portable_durability_from_native_requirement() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let directory = canonical_tempdir();
                let root = SigningKey::from_bytes(&[24; 32]);
                let device = SigningKey::from_bytes(&[25; 32]);
                let principal = principal_id_for_root(root.verifying_key().to_bytes());
                let mut workspace = crate::test_support::Workspace::new(
                    crate::test_support::WorkspaceId(5),
                    "read mode",
                );
                workspace.set_owner(&principal);
                let route = CrdtRoute::record(
                    "so.tilo.word-processor",
                    workspace.room.to_string(),
                    "tab:0000000000000001",
                );
                let mut tree = WorkspaceTree::default();
                tree.workspaces.push(workspace);
                let membership = tree.shared();
                for native in [false, true] {
                    let admitted = admit_verified_join(
                        &membership.read(),
                        &route,
                        VerifiedJoinIdentity {
                            user_public_key: root.verifying_key().to_bytes(),
                            device_public_key: device.verifying_key().to_bytes(),
                            capped: false,
                        },
                    )
                    .unwrap();
                    let fanout = Arc::new(FanoutState::new());
                    let config = crate::peer_actions::NativePeerBrokerConfig {
                        journal_root: directory.path().join(if native {
                            "native"
                        } else {
                            "portable"
                        }),
                        certificate_authority: Arc::new(ExplicitAuthority),
                    };
                    let services = HostServices {
                        committed_documents: native.then(|| {
                            Arc::new(RequiredNativeReadUnavailable) as Arc<dyn NativeDocumentReads>
                        }),
                        ..Default::default()
                    };
                    let (state, bridge_rx) = CapnpBrokerState::new_configured(
                        &fanout,
                        Arc::new(Mutex::new(BrokerChannels::default())),
                        PeerPublisher::disabled(),
                        Some(membership.authority()),
                        None,
                        Some(&config),
                        services,
                    );
                    assert!(
                        state.durable_owner_required,
                        "exercise actual configured durable worker mode"
                    );
                    let client: channel_handle::Client =
                        capnp_rpc::new_client(CapnpChannelHandle {
                            committed_read: Rc::new(RefCell::new(
                                committed_document::Slot::default(),
                            )),
                            state,
                            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
                            channel_id: route.channel_key(),
                            peer_id: 1,
                            verified_identity: admitted,
                            record_kind: None,
                            lease: None,
                        });
                    let mut request = client.committed_document_request();
                    request.get().set_request(
                        &runtime_collab::committed_document::Request::Open {
                            known_revision: None,
                        }
                        .encode()
                        .unwrap(),
                    );
                    let response = request.send().promise.await;
                    if native {
                        assert!(
                            response.is_err()
                                || response.as_ref().is_ok_and(|response| {
                                    runtime_collab::committed_document::Reply::decode(
                                        response.get().unwrap().get_reply().unwrap(),
                                    )
                                    .unwrap()
                                        == runtime_collab::committed_document::Reply::Unavailable
                                }),
                            "required native owner cannot become Legacy"
                        );
                    } else {
                        let response = response.unwrap();
                        assert_eq!(
                            runtime_collab::committed_document::Reply::decode(
                                response.get().unwrap().get_reply().unwrap()
                            )
                            .unwrap(),
                            runtime_collab::committed_document::Reply::Legacy,
                            "portable worker is already hydrated and must preserve storage behavior"
                        );
                    }
                }
            })
            .await;
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
            if let ui::collaboration::CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } = &request
            {
                self.route = ui::collaboration::RouteSpec {
                    app_id: app_id.clone(),
                    cluster_id: *cluster_id,
                    record: record.clone(),
                };
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
        use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
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
            HostContextInputs {
                records: records.clone(),
                views,
                device_id: "device".into(),
                instance_id: 1,
                permissions: Rc::new(RefCell::new(
                    ui::permissions::PermissionContext::permissive(),
                )),
                author: ui::permissions::UserId("worker".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
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
            route,
            ack: false,
            sent: sent.clone(),
            events: events.clone(),
        });
        manager.send(CollabRequest::Hello {
            app_id: definition.app_id.into(),
            cluster_id: room,
            record: Some(record.id.as_str().into()),
            record_kind: None,
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
                    .find(|a| a.action == "append-paragraph")
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
            invocation,
            resource,
            action: "append-paragraph".into(),
            argument: Some("Added through the broker".into()),
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
                    ..
                }
            )),
            "checked Writer completed before sending its changed owned document to the durable broker"
        );
        app.close_components();
    }

    struct OwnerAuthority(tilo_peer_state::StatePeer);
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
    fn owner_tree(owner: &crate::test_owner::SessionOwner) -> WorkspaceTreeHandle {
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(72),
            "Actual owner barrier",
        );
        workspace.room = uuid::Uuid::parse_str(owner.peer().room()).unwrap().into();
        workspace.set_owner(principal_id_for_root(
            owner.peer().authority().user_public_key,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        tree.shared()
    }
    fn owner_config(
        root: &std::path::Path,
        owner: &crate::test_owner::SessionOwner,
    ) -> crate::peer_actions::NativePeerBrokerConfig {
        crate::peer_actions::NativePeerBrokerConfig {
            journal_root: root.join("journal"),
            certificate_authority: Arc::new(OwnerAuthority(owner.peer().clone())),
        }
    }
    struct OwnerFixture {
        membership: WorkspaceTreeHandle,
        owner: crate::test_owner::SessionOwner,
        mailbox: crate::test_owner::owner_commit::OwnerCommitMailbox,
        state: Arc<CapnpBrokerState>,
        client: channel_handle::Client,
        route: CrdtRoute,
        _fanout: Arc<FanoutState>,
    }
    #[tokio::test(flavor = "current_thread")]
    async fn owner_commit_failure_retains_answered_reason_and_cancelled_uncertainty() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for cancelled in [false, true] {
                    let root = canonical_tempdir();
                    let mut fixture = owner_fixture_with_receipt_fault(root.path(), 1);
                    let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
                        room: fixture.route.cluster_id.clone(),
                        operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                            app: fixture.route.app_id.clone(),
                            record: fixture.route.record_id.clone(),
                            envelope: Box::new(owner_snapshot(&fixture)),
                        },
                    };
                    let state = fixture.state.clone();
                    let response = tokio::task::spawn_local(async move {
                        state.commit_owner_request(request, None).await
                    });
                    let pending = tilo_test_env::within(
                        "owner did not receive the actual commit",
                        fixture.mailbox.recv(),
                    )
                    .await
                    .unwrap();
                    if cancelled {
                        response.abort();
                        assert!(response.await.unwrap_err().is_cancelled());
                        assert_eq!(
                            fixture.state.owner_health().unwrap_err(),
                            "owner commit interrupted; outcome unknown"
                        );
                        drop(pending);
                    } else {
                        pending.apply(&mut fixture.owner).unwrap();
                        let error = response.await.unwrap().unwrap_err();
                        assert_eq!(
                            fixture.state.owner_health().unwrap_err(),
                            format!("owner commit failed: {}", error.extra)
                        );
                        assert!(!error.extra.contains("interrupted"));
                    }
                }
            })
            .await;
    }

    fn owner_fixture(root: &std::path::Path) -> OwnerFixture {
        owner_fixture_with_receipt_fault(root, 0)
    }
    struct AlteredReceipt {
        inner: crate::test_owner::owner_commit::OwnerCommitHandle,
        fault: u8,
        required_room: Option<String>,
    }
    impl tilo_peer_state::owner_commit::OwnerCommitService for AlteredReceipt {
        fn requires_room(&self, room: &str) -> bool {
            self.required_room
                .as_deref()
                .is_none_or(|required| required == room)
        }
        fn health(&self) -> Result<(), String> {
            self.inner.health()
        }
        fn commit(
            &self,
            request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        ) -> tilo_peer_state::owner_commit::CommitFuture {
            let response = self.inner.commit(request);
            let fault = self.fault;
            Box::pin(async move {
                let mut receipt = response.await?;
                match fault {
                    1 => receipt.binding.room = uuid::Uuid::new_v4().to_string(),
                    2 => {
                        receipt.binding.target =
                            tilo_peer_state::owner_commit::CommitTarget::Document {
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
        owner_fixture_with_native_config(root, fault, false, true)
    }
    fn owner_fixture_with_native_config(
        root: &std::path::Path,
        fault: u8,
        native: bool,
        enrolled: bool,
    ) -> OwnerFixture {
        let owner = crate::test_owner::SessionOwner::initialize(&root.join("owner")).unwrap();
        let (handle, mailbox) = crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(2);
        let route = CrdtRoute::record(
            "so.tilo.word-processor",
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
                capped: false,
            },
        )
        .unwrap();
        let fanout = Arc::new(FanoutState::new());
        let config = (!native).then(|| owner_config(root, &owner));
        let (state, bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_owner_commit(
                Arc::new(RecordingSink::default()),
                Arc::new(AlteredReceipt {
                    inner: handle,
                    fault,
                    required_room: native.then(|| {
                        if enrolled {
                            route.cluster_id.clone()
                        } else {
                            uuid::Uuid::new_v4().to_string()
                        }
                    }),
                }),
            ),
            Some(membership.authority()),
            None,
            config.as_ref(),
            HostServices::default(),
        );
        fanout.attach_capnp(state.clone());
        state.ensure_channel(&route.channel_key());
        let client = capnp_rpc::new_client(CapnpChannelHandle {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: state.clone(),
            directories: Rc::new(LocalActionDirectoryRegistry::new(bridge_rx)),
            channel_id: route.channel_key(),
            peer_id: 72,
            verified_identity: admitted,
            record_kind: None,
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
        let key = f.owner.signing_key().clone();
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
                    crate::test_owner::SessionOwner::open(&root.path().join("owner")).unwrap();
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
            crate::test_owner::SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let membership = owner_tree(&owner);
        let config = owner_config(root.path(), &owner);
        let (handle, mut mailbox) = crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(2);
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
        let actor = principal_id_for_root(f.owner.peer().authority().user_public_key);
        let context =
            tilo_actions::ActionContext::for_resource("record:bounded", "document", "Receipt test");
        f.state
            .journal
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .admit(
                key.clone(),
                serde_json::json!({
                    "operation_kind": tilo_actions::OperationKind::Effect,
                    "actor": actor,
                    "context": context,
                }),
                1,
            )
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        f.state.pending_invocations.lock().unwrap().insert(
            1,
            PendingInvocation {
                caller: None,
                activity: None,
                owner: key.owner.clone(),
                network_evidence: None,
                original: None,
                policy_time: None,
                journal_key: Some(key.clone()),
                peer_id: 72,
                tx,
                update: tilo_actions::InvocationUpdate::Accepted { invocation },
                prepared_effect: Arc::default(),
                last_sequence: 0,
                actor,
                cancellable: false,
                retryable_on_disconnect: false,
                execution: tilo_actions::ExecutionMode::Immediate,
                result: tilo_actions::ResultSpec::None,
                operation_kind: tilo_actions::OperationKind::Effect,
                cluster_id: f.route.cluster_id.clone(),
                context,
                last_activity: std::time::Instant::now(),
                cancellation_requested_at: None,
                cancellation_fence: Arc::new(AtomicBool::new(false)),
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
                        .set_update_json(serde_json::to_string(&update).unwrap());
                    let mut response = tokio::task::spawn_local(request.send().promise);
                    let pending = tokio::select! {
                        pending = f.mailbox.recv() => pending.unwrap(),
                        rejected = &mut response => {
                            let rejected = rejected.map(|reply| reply.map(|_| ()));
                            panic!("terminal update ended before owner receipt admission: {rejected:?}");
                        }
                    };
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
                    let receipts = tilo_peer_state::room::receipt_values(
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
        use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
        use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
        use ui::portable_storage::{PortableStorage, StorageKey};
        use ui::record::{RecordCheckout, RecordId, RecordOwners};
        struct CurrentOwner(Arc<Mutex<Option<tilo_peer_state::StatePeer>>>);
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
            fn with_tree(
                &self,
                read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
            ) -> Result<(), String> {
                let guard = self.0.lock().map_err(|_| "owner authority unavailable")?;
                let peer = guard.as_ref().ok_or("owner authority unavailable")?;
                let doc = peer
                    .admitted_room()
                    .ok_or("admitted owner governance unavailable")?;
                let (name, owner, caps) = tilo_peer_state::room::extract_governed_fields(doc);
                let mut workspace = tilo_desktop_model::Workspace::new(
                    tilo_desktop_model::WorkspaceId(72),
                    name.unwrap_or_else(|| "Owner room".into()),
                );
                workspace.room = uuid::Uuid::parse_str(peer.room())
                    .map_err(|e| e.to_string())?
                    .into();
                workspace.owner = owner;
                workspace.capabilities = caps;
                let mut tree = tilo_desktop_model::DesktopTree::default();
                tree.workspaces.push(workspace);
                read(&tree);
                Ok(())
            }
        }
        struct Published(Arc<AtomicUsize>);
        impl crate::collab_outbound::PeerPublishSink for Published {
            fn app_document(&self, _: String, _: String, _: String, _: SignedOp) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn app_presence(&self, _: String, _: String, _: collab_types::PeerPresence) {}
            fn scrollback_request(&self, _: &str, _: ArtifactId, _: u64, _: u16) {}
        }
        let root = canonical_tempdir();
        let mut owner =
            crate::test_owner::SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let initial_generation = owner.status().generation;
        let mut remote = owner.peer().clone();
        let cert = owner.peer().authority().clone();
        let room: uuid::Uuid = owner.peer().room().parse().unwrap();
        let key = owner.signing_key().clone();
        let record = RecordId::for_tab(1);
        let definition = word_processor::definition();
        let route = CrdtRoute::record(definition.app_id, room.to_string(), record.as_str());
        let initial = owner
            .peer()
            .document(definition.app_id, record.as_str())
            .unwrap();
        let committed = Arc::new(Mutex::new(Some(owner.peer().clone())));
        let authority = Arc::new(CurrentOwner(committed.clone()));
        let (handle, mut mailbox) = crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(8);
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
            HostContextInputs {
                records: records.clone(),
                views,
                device_id: "device".into(),
                instance_id: 1,
                permissions: Rc::new(RefCell::new(
                    ui::permissions::PermissionContext::permissive(),
                )),
                author: ui::permissions::UserId(principal.clone()),
                action_context: tilo_actions::ActionContext::default(),
            },
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
        // The checked app needs its ordinary model and retained surface phases;
        // presence-only pumping cannot adopt the model or service an invocation.
        struct HeadlessSurface;
        impl ui::app::AppSurface for HeadlessSurface {
            fn scale_factor(&self) -> f64 {
                1.0
            }
            fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
                ui::winit::dpi::PhysicalSize::new(1024, 768)
            }
            fn request_redraw(&self) {}
            fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
        }
        let mut scene = ui::vello::Scene::new();
        let deadline = tilo_test_env::hang_deadline();
        let (surface, action) = loop {
            scene.reset();
            app.surface_frame(&HeadlessSurface, &mut scene);
            if let Some(found) =
                broker
                    .published_action_surfaces()
                    .into_iter()
                    .find_map(|surface| {
                        surface
                            .actions
                            .iter()
                            .find(|action| action.action == "append-paragraph")
                            .cloned()
                            .map(|action| (surface, action))
                    })
            {
                break found;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "actual checked Writer did not publish actions: connection={:?}, coverage={:?}, resources={:?}, surfaces={:?}",
                app.collaboration_state(),
                app.control_action_coverage(),
                {
                    fn addresses(
                        resources: &[tilo_actions::Resource],
                    ) -> Vec<(String, Vec<String>)> {
                        let mut rows = Vec::new();
                        for resource in resources {
                            rows.push((
                                resource.id.as_str().to_owned(),
                                resource
                                    .actions
                                    .iter()
                                    .map(|action| action.id.clone())
                                    .collect(),
                            ));
                            rows.extend(addresses(&resource.children));
                        }
                        rows
                    }
                    addresses(&app.collect_resources())
                },
                broker
                    .published_action_surfaces()
                    .iter()
                    .map(|surface| (
                        surface.app_id.clone(),
                        surface.route.clone(),
                        surface
                            .actions
                            .iter()
                            .map(|action| (action.resource.clone(), action.action.clone()))
                            .collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let actions = broker.app_action_broker().unwrap();
        let job = actions
            .start(
                surface.peer_id,
                &action.resource,
                &action.action,
                Some("Added through the broker"),
                &principal,
            )
            .unwrap();
        loop {
            scene.reset();
            app.surface_frame(&HeadlessSurface, &mut scene);
            if job.completion.try_recv().is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "checked Writer invocation failed to terminate: {:?}",
                actions.status(job.id)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            matches!(
                actions.status(job.id),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ),
            "{:?}",
            actions.status(job.id)
        );
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
                    tilo_peer_state::TransportOrigin::RelayDelta(cert.device_public_key),
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
        let restored = crate::test_owner::SessionOwner::open(&root.path().join("owner")).unwrap();
        assert_eq!(restored.status(), expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_host_worker_private_rpc_requires_descriptor_and_current_authority() {
        use super::signed_worker::{
            CurrentHost, HeldHostDirectory, empty_directories, presentation, rpc_pair,
            signed_device,
        };
        async fn scenario() {
            let root = SigningKey::from_bytes(&[81; 32]);
            let key = SigningKey::from_bytes(&[82; 32]);
            let cert = signed_device(
                &root,
                &key,
                "local host",
                tilo_user_identity::DeviceCapabilities::host(),
                None,
            );
            let mut workspace = crate::test_support::Workspace::new(
                crate::test_support::WorkspaceId(81),
                "Host worker room",
            );
            workspace.set_owner(principal_id_for_root(cert.user_public_key));
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
                crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(8);
            let publisher =
                PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(owner_handle));
            let (state, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                Some(membership.clone()),
                None,
            );
            assert!(
                crate::peer_actions::CertificateAuthority::authorize_host_worker(
                    &ExplicitAuthority,
                    &route,
                    &cert
                )
                .is_err(),
                "host worker authority defaults to deny"
            );
            let authority = CurrentHost::new(&route, &cert, None);
            assert!(state.certificate_authority.set(authority.clone()).is_ok());
            let bytes = presentation(&route.cluster_id, &cert, &key);
            let client = state
                .connect_local_worker(route.clone(), &cert)
                .expect("genuine host gets only a private worker descriptor");
            let pending = state.workers.lock().unwrap().pop().unwrap();
            let (rpc, server_task, client_task) = rpc_pair(
                state.clone(),
                901,
                pending.stream,
                client,
                Some(pending.binding),
                empty_directories(),
            );
            let handle = rpc
                .join_channel(&route.channel_key(), &bytes)
                .await
                .expect("actual descriptor signed host join");
            assert!(handle.published_invocation_status(999).await.is_ok());
            assert!(
                state.is_current_host_worker(901, &route, &cert).unwrap(),
                "signed actual host worker establishes live provenance"
            );
            let (entered, mut callbacks) = mpsc::unbounded_channel();
            let release = Rc::new(tokio::sync::Notify::new());
            let row = TestActionDirectory {
                revision: "directory-r1".into(),
                resolve_mode: DirectoryResolveMode::Found,
                result: tilo_actions::ResultSpec::None,
                confirmation_required: false,
                query_count: 0,
                resolve_count: 0,
            }
            .row();
            handle
                .publish_action_directory(
                    &route.app_id,
                    capnp_rpc::new_client(HeldHostDirectory {
                        entered,
                        release: release.clone(),
                        row,
                    }),
                )
                .await
                .unwrap();
            for revoke in [false, true] {
                let query_handle = handle.clone();
                let query = tokio::task::spawn_local(async move {
                    query_handle
                        .query_action_directory(&runtime_collab::ActionDirectoryQuery {
                            peer_id: 901,
                            revision: "directory-r1".into(),
                            cursor: vec![],
                            needle: String::new(),
                            parent_resource: String::new(),
                            max_results: 8,
                            max_bytes: 65536,
                            max_work: 64,
                            max_depth: 4,
                        })
                        .await
                });
                let resolve_handle = handle.clone();
                let resolve = tokio::task::spawn_local(async move {
                    resolve_handle
                        .resolve_published_action(901, "directory-r1", "workspace:atlas", "open")
                        .await
                });
                let mut called = vec![
                    callbacks.recv().await.unwrap(),
                    callbacks.recv().await.unwrap(),
                ];
                called.sort_unstable();
                assert_eq!(called, ["query", "resolve"]);
                if revoke {
                    *authority.certificate.lock().unwrap() = None;
                }
                release.notify_waiters();
                let query = query.await.unwrap();
                let resolve = resolve.await.unwrap();
                if revoke {
                    assert!(
                        query.is_err() && resolve.is_err(),
                        "held directory query/resolve must reject after host revocation: query={query:?}, resolve={resolve:?}"
                    );
                } else {
                    assert_eq!(query.unwrap().rows.len(), 1);
                    assert!(matches!(
                        resolve.unwrap(),
                        runtime_collab::ActionDirectoryResolution::Found(_)
                    ));
                }
                *authority.certificate.lock().unwrap() = Some(cert.clone());
            }
            assert!(
                !state.durable_owner_required,
                "host checks must not depend on durable mode"
            );
            let descriptor = TestActionDirectory {
                revision: "directory-r1".into(),
                resolve_mode: DirectoryResolveMode::Found,
                result: tilo_actions::ResultSpec::None,
                confirmation_required: false,
                query_count: 0,
                resolve_count: 0,
            }
            .row()
            .descriptor
            .unwrap();
            handle
                .publish_actions(&route.app_id, &[descriptor])
                .await
                .unwrap();
            let invocations = handle.subscribe_stable_invocations().await.unwrap();
            for revoked in [false, true] {
                let mut rejected = vec![];
                for method in ["complete", "update", "cancel"] {
                    let job = state
                        .start_invocation(
                            901,
                            "workspace:atlas",
                            "open",
                            None,
                            &principal_id_for_root(cert.user_public_key),
                        )
                        .unwrap();
                    let delivered = invocations.read().await.unwrap();
                    assert_eq!(delivered.id, job.id);
                    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                        delivered.invocation_uuid,
                    ));
                    if revoked {
                        *authority.certificate.lock().unwrap() = None;
                    }
                    let result = match method {
                        "complete" => handle.complete_invocation(job.id, "", "").await,
                        "update" => {
                            handle
                                .update_invocation(
                                    job.id,
                                    &serde_json::to_string(
                                        &tilo_actions::InvocationUpdate::Succeeded {
                                            invocation,
                                            output: tilo_actions::ActionOutput::default(),
                                        },
                                    )
                                    .unwrap(),
                                )
                                .await
                        }
                        _ => handle
                            .invocation_cancellation(job.id)
                            .await
                            .map(|cancelled| assert!(!cancelled)),
                    };
                    rejected.push((method, result.is_err()));
                    *authority.certificate.lock().unwrap() = Some(cert.clone());
                    if result.is_err() || method == "cancel" {
                        handle.complete_invocation(job.id, "", "").await.unwrap();
                    }
                }
                assert!(
                    rejected.iter().all(|(_, denied)| *denied == revoked),
                    "actual host completion/update/cancellation current checks: revoked={revoked}, rejected={rejected:?}"
                );
            }
            let retained_catalogue = handle.subscribe_action_catalogue().await.unwrap();
            let waiting_catalogue = handle.subscribe_action_catalogue().await.unwrap();
            assert!(waiting_catalogue.read().await.is_ok());
            let catalogue_sender = state
                .action_catalogue_subscribers
                .lock()
                .unwrap()
                .get(&901)
                .unwrap()
                .2
                .clone();
            let before_read = authority.checks.load(Ordering::SeqCst);
            let pending_read =
                tokio::task::spawn_local(async move { waiting_catalogue.read().await });
            while authority.checks.load(Ordering::SeqCst) == before_read {
                tokio::task::yield_now().await;
            }
            assert!(
                !pending_read.is_finished(),
                "read waits for the next actual catalogue publication"
            );
            for wrong in [
                CrdtRoute::record("so.tilo.chat", &route.cluster_id, &route.record_id),
                route.clone().on_record("other-record"),
                CrdtRoute::record(
                    &route.app_id,
                    uuid::Uuid::new_v4().to_string(),
                    &route.record_id,
                ),
            ] {
                assert!(
                    rpc.join_channel(&wrong.channel_key(), &bytes)
                        .await
                        .is_err()
                );
                assert!(!state.is_current_host_worker(901, &wrong, &cert).unwrap());
                assert!(state.connect_local_worker(wrong, &cert).is_err());
            }
            assert!(
                rpc.join_channel_as_kind(&route.channel_key(), &bytes, "agent-a", "agent", "")
                    .await
                    .is_err()
            );
            let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
            let (ordinary, ordinary_server, ordinary_client) = rpc_pair(
                state.clone(),
                902,
                server,
                client,
                None,
                empty_directories(),
            );
            assert!(
                ordinary
                    .join_channel(&route.channel_key(), &bytes)
                    .await
                    .is_err(),
                "the exact same host certificate is not ordinary join authority"
            );
            assert!(
                verify_join_identity(
                    &route,
                    &bytes,
                    state.membership.as_ref(),
                    Some(&[cert.device_public_key]),
                    None
                )
                .is_err(),
                "trusted-device fallback cannot admit a host certificate"
            );
            state.surfaces.lock().unwrap().insert(
                902,
                PublishedSurface {
                    peer_id: 902,
                    app_id: route.app_id.clone(),
                    route: route.clone(),
                    actions: vec![],
                    live: true,
                },
            );
            assert!(
                !state.is_current_host_worker(902, &route, &cert).unwrap(),
                "same-route ordinary publication is not host-worker provenance"
            );
            let mut changed = cert.clone();
            changed.device_label = "changed pinned cert".into();
            tilo_user_identity::sign_device_cert(&mut changed, &root).unwrap();
            *authority.certificate.lock().unwrap() = Some(changed.clone());
            assert!(!state.is_current_host_worker(901, &route, &cert).unwrap());
            assert!(handle.published_invocation_status(999).await.is_err());
            assert!(
                rpc.join_channel(
                    &route.channel_key(),
                    &presentation(&route.cluster_id, &changed, &key)
                )
                .await
                .is_err(),
                "full certificate must match descriptor despite same keys"
            );
            *authority.certificate.lock().unwrap() = None;
            catalogue_sender
                .send(Some(Arc::from("revoked private snapshot")))
                .unwrap();
            assert!(
                pending_read.await.unwrap().is_err(),
                "a stream must recheck authority after its read wakes"
            );
            assert!(handle.published_invocation_status(999).await.is_err());
            assert!(
                retained_catalogue.read().await.is_err(),
                "retained stream must recheck host authority before disclosing a buffered snapshot"
            );
            assert!(state.connect_local_worker(route.clone(), &cert).is_err());
            let mut expired = cert.clone();
            expired.expires_at_ns = Some(2);
            tilo_user_identity::sign_device_cert(&mut expired, &root).unwrap();
            *authority.certificate.lock().unwrap() = Some(expired.clone());
            assert!(state.connect_local_worker(route.clone(), &expired).is_err());
            *authority.certificate.lock().unwrap() = Some(cert.clone());
            let current_workspaces = membership.read().workspaces.clone();
            membership.write().workspaces.clear();
            assert!(
                handle.published_invocation_status(999).await.is_err(),
                "live membership remains required"
            );
            membership.write().workspaces = current_workspaces;
            assert!(state.is_current_host_worker(901, &route, &cert).unwrap());
            server_task.abort();
            let _ = server_task.await;
            client_task.abort();
            assert!(
                !state.is_current_host_worker(901, &route, &cert).unwrap(),
                "disconnect removes host provenance"
            );
            ordinary_server.abort();
            ordinary_client.abort();
        }
        tokio::task::LocalSet::new()
            .run_until(async {
                tilo_test_env::within("bounded actual host worker RPC regression", scenario())
                    .await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_enrolled_document_config_none_crosses_actual_owner_barrier() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for fail in [false, true] {
                    let root = canonical_tempdir();
                    let mut f = owner_fixture_with_native_config(root.path(), 0, true, true);
                    assert!(
                        !f.state.durable_owner_required,
                        "exercise actual native config=None"
                    );
                    let mut subscriber = f.state.subscribe_ops(&f.route.channel_key(), 73);
                    let mut request = f.client.send_request();
                    request
                        .get()
                        .set_op(&postcard::to_stdvec(&owner_snapshot(&f)).unwrap());
                    let response = tokio::task::spawn_local(request.send().promise);
                    let pending = tilo_test_env::within(
                        "native enrolled document bypassed the actual owner service",
                        f.mailbox.recv(),
                    )
                    .await
                    .unwrap();
                    assert!(
                        !response.is_finished(),
                        "RPC acknowledged before durable owner"
                    );
                    assert!(
                        subscriber.try_recv().is_err(),
                        "fanout preceded durable owner"
                    );
                    if fail {
                        drop(pending);
                        assert!(response.await.unwrap().is_err());
                        assert!(
                            subscriber.try_recv().is_err(),
                            "failed barrier must never fan out"
                        );
                    } else {
                        let receipt = pending.apply(&mut f.owner).unwrap();
                        assert!(receipt.generation > 1);
                        response.await.unwrap().unwrap();
                        assert!(subscriber.try_recv().is_ok());
                        let status = f.owner.status();
                        drop(f);
                        assert_eq!(
                            crate::test_owner::SessionOwner::open(&root.path().join("owner"))
                                .unwrap()
                                .status(),
                            status
                        );
                    }
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_offline_document_keeps_actual_owner_barrier() {
        for fail in [false, true] {
            let root = canonical_tempdir();
            let mut f = owner_fixture_with_native_config(root.path(), 0, true, true);
            let (handle, mut mailbox) =
                crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(1);
            let publisher = PeerPublisher::with_optional_owner_commit(None, Arc::new(handle));
            assert!(publisher.requires_owner_for(&f.route.cluster_id));
            let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
                room: f.route.cluster_id.clone(),
                operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                    app: f.route.app_id.clone(),
                    record: f.route.record_id.clone(),
                    envelope: Box::new(owner_snapshot(&f)),
                },
            };
            let expected = request.clone();
            let result = tokio::spawn(async move { publisher.commit_owner(request, None).await });
            let pending = tilo_test_env::within("offline commit bypassed owner", mailbox.recv())
                .await
                .unwrap();
            assert!(!result.is_finished());
            if fail {
                drop(pending);
                assert!(result.await.unwrap().is_err());
            } else {
                pending.apply(&mut f.owner).unwrap();
                result
                    .await
                    .unwrap()
                    .unwrap()
                    .validate_for(&expected)
                    .unwrap();
                let status = f.owner.status();
                drop(f);
                assert_eq!(
                    crate::test_owner::SessionOwner::open(&root.path().join("owner"))
                        .unwrap()
                        .status(),
                    status
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_unenrolled_document_preserves_legacy_routing() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root = canonical_tempdir();
                let mut f = owner_fixture_with_native_config(root.path(), 0, true, false);
                assert!(!f.state.publisher.requires_owner_for(&f.route.cluster_id));
                let mut subscriber = f.state.subscribe_ops(&f.route.channel_key(), 73);
                let mut request = f.client.send_request();
                request
                    .get()
                    .set_op(&postcard::to_stdvec(&owner_snapshot(&f)).unwrap());
                request.send().promise.await.unwrap();
                assert!(subscriber.try_recv().is_ok());
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), f.mailbox.recv())
                        .await
                        .is_err(),
                    "unenrolled legacy route must not acquire an unrelated native owner"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_enrolled_effect_config_none_requires_actual_owner_receipt() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root = canonical_tempdir();
                let mut f = owner_fixture_with_native_config(root.path(), 0, true, true);
                let invocation = tilo_actions::InvocationId::new();
                let (tx, _rx) = std::sync::mpsc::channel();
                let pending = PendingInvocation {
                    caller: None,
                    activity: None,
                    owner: InvocationOwner::Certified {
                        root: f.owner.peer().authority().user_public_key,
                        device: f.owner.peer().authority().device_public_key,
                    },
                    network_evidence: None,
                    original: None,
                    policy_time: None,
                    journal_key: None,
                    peer_id: 72,
                    tx,
                    update: tilo_actions::InvocationUpdate::Accepted { invocation },
                    prepared_effect: Arc::default(),
                    last_sequence: 0,
                    actor: principal_id_for_root(f.owner.peer().authority().user_public_key),
                    cancellable: false,
                    retryable_on_disconnect: false,
                    execution: tilo_actions::ExecutionMode::Immediate,
                    result: tilo_actions::ResultSpec::None,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    cluster_id: f.route.cluster_id.clone(),
                    context: tilo_actions::ActionContext::for_resource(
                        "record:bounded",
                        "document",
                        "Native receipt test",
                    ),
                    last_activity: std::time::Instant::now(),
                    cancellation_requested_at: None,
                    cancellation_fence: Arc::new(AtomicBool::new(false)),
                };
                let update = tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                };
                let retry_pending = pending.clone();
                let retry_update = update.clone();
                let state = f.state.clone();
                let completion = tokio::task::spawn_local(async move {
                    state.commit_effect_before_terminal(&pending, &update).await
                });
                let commit = tilo_test_env::within(
                    "enrolled native effect bypassed owner receipt",
                    f.mailbox.recv(),
                )
                .await
                .unwrap();
                assert!(!completion.is_finished());
                // An unavailable native operation must remain failure, never a legacy receipt.
                drop(commit);
                assert!(completion.await.unwrap().is_err());
                assert_exact_effect_retry(retry_pending, retry_update).await;
            })
            .await;
    }
    async fn assert_exact_effect_retry(
        mut pending: PendingInvocation,
        update: tilo_actions::InvocationUpdate,
    ) {
        // The refusal above intentionally poisons that owner. A separate live
        // owner checks identical prepared bytes and successful durable retries.
        let root = canonical_tempdir();
        let mut f = owner_fixture_with_native_config(root.path(), 0, true, true);
        pending.cluster_id = f.route.cluster_id.clone();
        pending.owner = InvocationOwner::Certified {
            root: f.owner.peer().authority().user_public_key,
            device: f.owner.peer().authority().device_public_key,
        };
        pending.actor = principal_id_for_root(f.owner.peer().authority().user_public_key);
        pending.prepared_effect = Arc::default();
        let state = f.state.clone();
        let retry_pending = pending.clone();
        let retry_update = update.clone();
        let retry = tokio::task::spawn_local(async move {
            state
                .commit_effect_before_terminal(&retry_pending, &retry_update)
                .await
        });
        let commit = tilo_test_env::within(
            "exact effect retry lost its owner barrier",
            f.mailbox.recv(),
        )
        .await
        .unwrap();
        let first_request = prepared_effect_bytes(&pending);
        commit.apply(&mut f.owner).unwrap();
        assert!(retry.await.unwrap().is_ok());
        let state = f.state.clone();
        let replay_pending = pending.clone();
        let replay_update = update.clone();
        let replay = tokio::task::spawn_local(async move {
            state
                .commit_effect_before_terminal(&replay_pending, &replay_update)
                .await
        });
        let commit = tilo_test_env::within(
            "accepted effect retry must use the same owner",
            f.mailbox.recv(),
        )
        .await
        .unwrap();
        assert_eq!(prepared_effect_bytes(&pending), first_request);
        commit.apply(&mut f.owner).unwrap();
        assert!(replay.await.unwrap().is_ok());
        let changed = tilo_actions::InvocationUpdate::Cancelled {
            invocation: update.invocation(),
            message: Some("different terminal outcome".into()),
        };
        assert!(
            f.state
                .commit_effect_before_terminal(&pending, &changed)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.mailbox.recv())
                .await
                .is_err()
        );
    }

    fn prepared_effect_bytes(pending: &PendingInvocation) -> Vec<u8> {
        let prepared = pending.prepared_effect.lock().unwrap();
        serde_json::to_vec(&prepared.as_ref().unwrap().request).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepared_effect_owner_accept_then_restart_reconciles_without_action_delivery() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let root = canonical_tempdir();
                let mut f = owner_fixture(root.path());
                let caller = effect_owner_caller(&f);
                let (key, request, prepared) = prepare_owner_effect(&f, &caller);
                let state = f.state.clone();
                let original = request.clone();
                let submit = tokio::task::spawn_local(async move {
                    state.publisher.commit_owner(original, None).await
                });
                let commit = tilo_test_env::within("effect owner submission", f.mailbox.recv())
                    .await
                    .unwrap();
                commit.apply(&mut f.owner).unwrap();
                submit.await.unwrap().unwrap();
                let before = f.owner.status();
                // Simulate the exact crash gap: owner durable, broker terminal not yet durable.
                let journal = f
                    .state
                    .journal
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .take()
                    .unwrap();
                drop(journal);
                *f.state.journal.lock().unwrap() = Ok(Some(
                    InvocationJournal::open(&root.path().join("journal")).unwrap(),
                ));
                let record = f
                    .state
                    .verified_invocation_status(&caller, key.invocation)
                    .unwrap()
                    .unwrap();
                assert!(record.uncertain);
                assert_eq!(record.prepared_effect.as_ref(), Some(&prepared));
                let state = f.state.clone();
                let retained_caller = caller.clone();
                let reconcile = tokio::task::spawn_local(async move {
                    state
                        .reconcile_prepared_effect(record, || {
                            state.verified_invocation_status(&retained_caller, key.invocation)?;
                            Ok(())
                        })
                        .await
                });
                let commit =
                    tilo_test_env::within("exact prepared reconciliation", f.mailbox.recv())
                        .await
                        .unwrap();
                commit.apply(&mut f.owner).unwrap();
                let terminal = reconcile.await.unwrap().unwrap();
                assert!(!terminal.uncertain);
                assert_eq!(terminal.update, prepared.update);
                assert_eq!(terminal.prepared_effect, Some(prepared));
                assert_eq!(
                    f.owner.status(),
                    before,
                    "exact retry must not create new owner history"
                );
                assert!(
                    f.state.pending_invocations.lock().unwrap().is_empty(),
                    "no action redispatch"
                );
                let journal = f
                    .state
                    .journal
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .take()
                    .unwrap();
                drop(journal);
                let journal = InvocationJournal::open(&root.path().join("journal")).unwrap();
                assert_eq!(journal.lookup(&terminal.key).unwrap().unwrap(), terminal);
            })
            .await;
    }

    fn effect_owner_caller(f: &OwnerFixture) -> VerifiedActionCaller {
        let certificate = f.owner.peer().authority();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: f.route.cluster_id.clone(),
            device_certificate: serde_json::to_vec(certificate).unwrap(),
            proof: f
                .owner
                .signing_key()
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &f.route.cluster_id,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        f.state
            .verify_action_caller(&f.route.cluster_id, &presentation)
            .unwrap()
    }

    fn prepare_owner_effect(
        f: &OwnerFixture,
        caller: &VerifiedActionCaller,
    ) -> (
        InvocationKey,
        tilo_peer_state::owner_commit::OwnerCommitRequest,
        invocation_journal::PreparedEffect,
    ) {
        let key = f
            .state
            .caller_key(caller, tilo_actions::InvocationId::new())
            .unwrap();
        let actor = caller.admitted.broker_actor().id;
        let context = tilo_actions::ActionContext::default();
        let contract = tilo_actions::ActionContract::default();
        let intent = serde_json::json!({"operation_kind":tilo_actions::OperationKind::Effect,"context":context,
            "actor":actor,"contract":contract,"app_id":f.route.app_id,
            "resource":format!("app.{}", f.route.app_id),"action":"open"});
        let update = tilo_actions::InvocationUpdate::Succeeded {
            invocation: key.invocation,
            output: tilo_actions::ActionOutput::default(),
        };
        let mut effect = crate::policy::redacted_effect_receipt(
            key.invocation,
            &actor,
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        effect.completed_at = 100;
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: f.route.cluster_id.clone(),
            operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
                invocation: key.invocation.0,
                redacted: serde_json::to_vec(&effect).unwrap(),
            },
        };
        let mut guard = f.state.journal.lock().unwrap();
        let journal = guard.as_mut().unwrap().as_mut().unwrap();
        journal.admit(key.clone(), intent, 1).unwrap();
        let prepared = journal.prepare_effect(&key, update, &request).unwrap();
        (key, request, prepared)
    }
}
