// Genuine native delivery, isolated range parsing and the file-backed owner.
mod native_document_rpc_tests {
    use super::*;
    use automerge::{ReadDoc as _, transaction::Transactable as _};
    use tilo_peer_state::owner_commit::{
        CommitFuture, DocumentCommitCustody, OwnerCommitRequest, OwnerCommitService,
    };
    use ui::invocation_scope::{NativeContributionClaim, NativePublicationClaims};

    struct FileOwner {
        owner: tilo_joined_room::NativeWorkspaceOwner,
        keys: channel_crypto::ChannelContentKeys,
    }
    impl OwnerCommitService for FileOwner {
        fn health(&self) -> Result<(), String> {
            self.owner.health()
        }
        fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
            let owner = self.owner.clone();
            Box::pin(async move { owner.commit_document(request, SystemTime::now()) })
        }
        fn commit_with_document_custody(
            &self,
            request: OwnerCommitRequest,
            custody: Arc<dyn DocumentCommitCustody>,
            _: Option<String>,
        ) -> CommitFuture {
            let owner = self.owner.clone();
            let keys = self.keys.clone();
            Box::pin(async move {
                owner.commit_document_with_custody(
                    request,
                    &mut |envelope| {
                        keys.open_current_content(
                            envelope.artifact,
                            envelope
                                .encryption
                                .as_ref()
                                .ok_or("missing sealed metadata")?,
                            &envelope.payload,
                        )
                        .map(|bytes| bytes.to_vec())
                        .map_err(|error| error.to_string())
                    },
                    custody.as_ref(),
                    SystemTime::now(),
                )
            })
        }
    }
    struct ExactLaunch {
        route: CrdtRoute,
    }
    impl crate::services::LauncherServices for ExactLaunch {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
            Err("test never launches an app".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            pid == std::process::id() && app == self.route.app_id && room == self.route.cluster_id
        }
    }
    struct Fixture {
        _root: tempfile::TempDir,
        _fanout: Arc<FanoutState>,
        path: std::path::PathBuf,
        owner: tilo_joined_room::NativeWorkspaceOwner,
        original: signed_worker::OriginalRoom,
        state: Arc<CapnpBrokerState>,
        connection: signed_worker::Connection,
        claims: NativePublicationClaims,
        encoded: Vec<u8>,
        wire: u64,
    }
    fn enrolled_owner() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        tilo_joined_room::NativeWorkspaceOwner,
    ) {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let path = root.path().join("joined.json");
        let owner = tilo_joined_room::open_file(&path).unwrap();
        let (intent, bootstrap, messages, governor, now) = tilo_joined_room::testing::raw_fixture();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        owner
            .install(
                &tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap(),
                now,
            )
            .unwrap();
        let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        for message in messages.into_iter().skip(1) {
            owner
                .receive(
                    room,
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(governor),
                    message,
                    now,
                )
                .unwrap();
        }
        (root, path, owner)
    }
    fn keys(route: &CrdtRoute) -> channel_crypto::ChannelContentKeys {
        channel_crypto::ChannelGroup::create(
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id),
            channel_crypto::MemberKey::generate("actual test steward").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap()
    }
    async fn native_connection(
        original: &signed_worker::OriginalRoom,
        owner: Arc<FileOwner>,
    ) -> (
        Arc<FanoutState>,
        Arc<CapnpBrokerState>,
        signed_worker::Connection,
    ) {
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_optional_owner_commit(None, owner),
            Some(original.membership.authority()),
            None,
            None,
            HostServices {
                launcher: Some(Arc::new(ExactLaunch {
                    route: original.route.clone(),
                })),
                ..Default::default()
            },
        );
        fanout.attach_capnp(state.clone());
        assert!(
            state
                .certificate_authority
                .set(original.authority.clone())
                .is_ok()
        );
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC).unwrap();
        let handoff = serde_json::to_vec(&serde_json::json!({"protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION, "app_id":original.route.app_id, "workspace_id": original.route.cluster_id})).unwrap();
        std::io::Write::write_all(
            &mut client,
            &launcher::services::COLLAB_LAUNCH_FRAMING
                .encode(&handoff)
                .unwrap(),
        )
        .unwrap();
        let (rpc, server, client) =
            signed_worker::rpc_pair(state.clone(), 993, server, client, None, directories);
        let handle = rpc
            .join_channel(
                &original.route.channel_key(),
                &signed_worker::presentation(
                    &original.route.cluster_id,
                    &original.cert,
                    &original.key,
                ),
            )
            .await
            .unwrap();
        (
            fanout,
            state,
            signed_worker::Connection {
                handle,
                server,
                client,
            },
        )
    }
    async fn fixture(action: &str) -> Fixture {
        let (root, path, owner) = enrolled_owner();
        let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        let cert = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap();
        let mut original = signed_worker::OriginalRoom::in_room_as(
            SigningKey::from_bytes(&[63; 32]),
            cert,
            uuid::Uuid::parse_str(tilo_joined_room::testing::ROOM)
                .unwrap()
                .into(),
        );
        original.route = CrdtRoute::record(
            tilo_joined_room::testing::APP,
            tilo_joined_room::testing::ROOM,
            "",
        );
        original.authority = signed_worker::CurrentHost::new(
            &original.route,
            &original.cert,
            Some(original.origin.capture().unwrap()),
        );
        let keys = keys(&original.route);
        let service = Arc::new(FileOwner {
            owner: owner.clone(),
            keys: keys.clone(),
        });
        let (fanout, state, connection) = native_connection(&original, service).await;
        let (wire, invocation) = begin_document_action(&connection, &original, action).await;
        let (claims, encoded) = sealed_snapshot(&original, &keys, wire, invocation);
        Fixture {
            _root: root,
            _fanout: fanout,
            path,
            owner,
            original,
            state,
            connection,
            claims,
            encoded,
            wire,
        }
    }
    async fn begin_document_action(
        connection: &signed_worker::Connection,
        original: &signed_worker::OriginalRoom,
        action: &str,
    ) -> (u64, [u8; 16]) {
        begin_document_action_from(connection, original, action, &connection.handle).await
    }
    async fn begin_document_action_from(
        connection: &signed_worker::Connection,
        original: &signed_worker::OriginalRoom,
        action: &str,
        caller: &runtime_collab::ChannelHandle,
    ) -> (u64, [u8; 16]) {
        let mut descriptor = signed_worker::directory_state("native-document-r1")
            .borrow()
            .row()
            .descriptor
            .unwrap();
        descriptor.action = action.into();
        descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
            kind: descriptor.resource_kind.clone(),
            key: descriptor.resource.clone(),
        });
        descriptor.operation_kind = tilo_actions::OperationKind::Document;
        descriptor.contract = tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Write,
            ..Default::default()
        };
        connection
            .handle
            .publish_actions(&original.route.app_id, &[descriptor])
            .await
            .unwrap();
        let stream = connection
            .handle
            .subscribe_stable_invocations()
            .await
            .unwrap();
        let (wire, _) = caller
            .invoke_published_action(
                993,
                "workspace:atlas",
                action,
                Some(r#"{"text":"native durable write"}"#),
                "{}",
            )
            .await
            .unwrap();
        let delivered = stream.read().await.unwrap();
        connection
            .handle
            .retained_invocation(wire, &delivered.invocation_uuid)
            .await
            .unwrap();
        (wire, delivered.invocation_uuid)
    }
    fn sealed_snapshot(
        original: &signed_worker::OriginalRoom,
        keys: &channel_crypto::ChannelContentKeys,
        wire: u64,
        invocation: [u8; 16],
    ) -> (NativePublicationClaims, Vec<u8>) {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", "native durable write")
            .unwrap();
        doc.commit();
        let hashes = doc
            .get_changes(&[])
            .iter()
            .map(|change| change.hash().0)
            .collect::<Vec<_>>();
        let mutation = DocumentMutation::Snapshot(doc.save());
        let plaintext = mutation.encode().unwrap();
        assert_eq!(
            tilo_automerge_isolate::Isolate::shared()
                .change_hashes(mutation.data())
                .unwrap()
                .iter()
                .map(|hash| hash.0)
                .collect::<Vec<_>>(),
            hashes,
            "actual isolated snapshot range must match the writer history"
        );
        let artifact = document_artifact_id(
            &original.route.app_id,
            &original.route.cluster_id,
            &original.route.record_id,
        );
        let (metadata, ciphertext) = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Checkpoint,
                &plaintext,
            )
            .unwrap();
        let mut envelope = SignedOp::new_unsigned(
            ciphertext,
            artifact,
            Ed25519PublicKey::from_bytes(original.cert.device_public_key),
            document_attestation_id(
                &original.route.app_id,
                &original.route.cluster_id,
                &original.route.record_id,
            ),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            1,
        );
        envelope.encryption = Some(metadata);
        sign_op(&mut envelope, &original.key).unwrap();
        let claims = NativePublicationClaims {
            body: *blake3::hash(&plaintext).as_bytes(),
            from: vec![],
            to: doc.get_heads().iter().map(|hash| hash.0).collect(),
            exported: hashes.clone(),
            contributors: vec![NativeContributionClaim {
                wire_id: wire,
                invocation,
                changes: hashes,
            }],
        };
        (claims, postcard::to_stdvec(&envelope).unwrap())
    }
    async fn publish(f: &Fixture, claims: &NativePublicationClaims) -> Result<Vec<u8>, String> {
        f.connection
            .handle
            .send_scoped_document(&f.encoded, &claims.encode().unwrap())
            .await
            .map_err(|error| error.to_string())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn native_document_exact_range_refuses_forgery_then_receipts_reopened_history() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for forgery in 0..3 {
                    let f = fixture("edit").await;
                    let before = std::fs::read(&f.path).unwrap();
                    let mut subscriber =
                        f.state.subscribe_ops(&f.original.route.channel_key(), 995);
                    let mut forged = f.claims.clone();
                    match forgery {
                        0 => forged.exported.clear(),
                        1 => forged.exported.push([0x5a; 32]),
                        _ => forged.contributors[0].changes.push([0x5a; 32]),
                    }
                    assert!(
                        publish(&f, &forged).await.is_err(),
                        "forged actual range {forgery} must refuse at the actual owner"
                    );
                    assert_eq!(std::fs::read(&f.path).unwrap(), before);
                    assert!(
                        subscriber.try_recv().is_err(),
                        "failed owner evaluation cannot fan out or acknowledge"
                    );
                }
                let f = fixture("edit").await;
                let mut subscriber = f.state.subscribe_ops(&f.original.route.channel_key(), 995);
                let receipt: ui::invocation_scope::NativeDocumentReceipt =
                    postcard::from_bytes(&publish(&f, &f.claims).await.unwrap()).unwrap();
                assert!(receipt.generation > 1);
                let request = OwnerCommitRequest {
                    room: f.original.route.cluster_id.clone(),
                    operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                        app: f.original.route.app_id.clone(),
                        record: f.original.route.record_id.clone(),
                        envelope: Box::new(postcard::from_bytes(&f.encoded).unwrap()),
                    },
                };
                assert_eq!(receipt.basis, request.binding().unwrap().basis);
                assert!(
                    subscriber.try_recv().is_ok(),
                    "actual durable acknowledgement precedes fanout"
                );
                let reopened = tilo_joined_room::open_file(&f.path).unwrap();
                reopened
                    .with_current(
                        tilo_desktop_model::RoomId::parse(&receipt.room).unwrap(),
                        |view| {
                            assert_eq!(view.generation(), receipt.generation);
                            let doc = automerge::AutoCommit::load(
                                &view.peer().document(&receipt.app, &receipt.record).unwrap(),
                            )
                            .unwrap();
                            assert_eq!(
                                doc.get(automerge::ROOT, "text")
                                    .unwrap()
                                    .unwrap()
                                    .0
                                    .to_string(),
                                "\"native durable write\""
                            );
                            Ok(())
                        },
                    )
                    .unwrap();
                let committed = std::fs::read(&f.path).unwrap();
                f.connection
                    .handle
                    .complete_invocation(f.wire, "", "")
                    .await
                    .unwrap();
                assert!(
                    publish(&f, &f.claims).await.is_err(),
                    "terminal original custody cannot resurrect an exact durable write"
                );
                assert_eq!(std::fs::read(&f.path).unwrap(), committed);
            })
            .await;
    }
    fn set_current_role(f: &Fixture, role: tilo_peer_state::join::MemberRole) {
        govern(f, |peer, key, now| {
            peer.set_member_role(f.original.cert.user_public_key, role, key, now)?;
            Ok(())
        });
    }
    fn govern(
        f: &Fixture,
        mutation: impl FnOnce(
            &mut tilo_peer_state::StatePeer,
            &SigningKey,
            SystemTime,
        ) -> Result<(), String>,
    ) {
        let room = tilo_desktop_model::RoomId::parse(&f.original.route.cluster_id).unwrap();
        let now = SystemTime::now();
        let key = SigningKey::from_bytes(&[61; 32]);
        let messages = f
            .owner
            .with_current(room, |view| {
                let before = view.peer().clone();
                let mut candidate = before.clone();
                mutation(&mut candidate, &key, now)?;
                candidate.durable_mutation_messages(&before, &key, now, view.generation() + 1)
            })
            .unwrap();
        let governor = key.verifying_key().to_bytes();
        for message in messages {
            f.owner
                .receive(
                    room,
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(governor),
                    message,
                    now,
                )
                .unwrap();
        }
    }
    async fn admitted_agent(f: &Fixture) -> (signed_worker::Connection, String) {
        use collab_types::caps::{CapOverride, Scope, Subject, Verb};
        use tilo_relay_protocol::agents::{AgentDelegation, AgentScope};
        let person = SigningKey::from_bytes(&[62; 32]);
        let agent_key = SigningKey::from_bytes(&[65; 32]);
        let cert = signed_worker::signed_device(
            &person,
            &agent_key,
            "Agent: Scribe",
            tilo_user_identity::DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
            None,
        );
        let action = CapOverride::grant(
            Subject::agent("scribe"),
            Scope::Action {
                app: f.original.route.app_id.clone(),
                resource: "workspace:atlas".into(),
                action: "edit".into(),
            },
            Verb::Write,
        )
        .with_id("native-scribe-edit");
        let app = CapOverride::grant(
            Subject::app(&f.original.route.app_id),
            Scope::App(f.original.route.app_id.clone()),
            Verb::Write,
        )
        .with_id("native-chat-write");
        govern(f, |peer, key, now| {
            let issued = u64::try_from(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            )
            .unwrap();
            let delegation = AgentDelegation::sign(
                &person,
                cert.device_public_key,
                "scribe",
                "Scribe",
                [10, 20, 30],
                AgentScope {
                    rooms: vec![f.original.route.cluster_id.clone()],
                    apps: vec![],
                    may_confirm: false,
                },
                issued - 1,
                issued + 60_000_000_000,
            )
            .map_err(|error| error.to_string())?;
            peer.admit_agent(cert.clone(), delegation, key, now)?;
            peer.put_governed_capabilities(&[action.clone(), app], key, now)?;
            Ok(())
        });
        f.original
            .authority
            .additional_certificates
            .lock()
            .unwrap()
            .push(cert.clone());
        {
            let mut tree = f.original.membership.write();
            let workspace = tree.workspaces.first_mut().unwrap();
            workspace.grant_capability(CapOverride::membership(
                Subject::person(principal_id_for_root(cert.user_public_key)),
                f.original.route.cluster_id.clone(),
                Verb::Admin,
            ));
            workspace.grant_capability(CapOverride::membership(
                Subject::agent("scribe"),
                f.original.route.cluster_id.clone(),
                Verb::Write,
            ));
            workspace.grant_capability(action.clone());
        }
        let directories = Rc::new(LocalActionDirectoryRegistry::new(
            std::sync::mpsc::channel().1,
        ));
        let (server, socket) = std::os::unix::net::UnixStream::pair().unwrap();
        let (rpc, server, client) =
            signed_worker::rpc_pair(f.state.clone(), 996, server, socket, None, directories);
        let route = CrdtRoute::cluster(f.original.route.cluster_id.clone());
        let handle = rpc
            .join_channel_as_kind(
                &route.channel_key(),
                &signed_worker::presentation(&route.cluster_id, &cert, &agent_key),
                "scribe",
                "agent",
                "",
            )
            .await
            .unwrap();
        (
            signed_worker::Connection {
                handle,
                server,
                client,
            },
            action.id,
        )
    }
    #[tokio::test(flavor = "current_thread")]
    async fn native_document_agent_withdrawn_action_cannot_inherit_app_grant() {
        tokio::task::LocalSet::new().run_until(async {
            for withdraw in [false, true] {
                let mut f = fixture("edit").await;
                let (agent, action) = admitted_agent(&f).await;
                let (wire, invocation) = begin_document_action_from(&f.connection, &f.original, "edit", &agent.handle).await;
                f.claims.contributors[0].wire_id = wire;
                f.claims.contributors[0].invocation = invocation;
                if withdraw {
                    govern(&f, |peer, key, now| {
                        peer.remove_governed_capabilities(&[action], key, now)?;
                        Ok(())
                    });
                }
                let before = std::fs::read(&f.path).unwrap();
                assert_eq!(publish(&f, &f.claims).await.is_ok(), !withdraw,
                    "an acting agent needs its own current original action grant even when the app can write");
                if withdraw { assert_eq!(std::fs::read(&f.path).unwrap(), before); }
            }
        }).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn native_document_current_viewer_refuses_original() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let f = fixture("edit").await;
                set_current_role(&f, tilo_peer_state::join::MemberRole::Viewer);
                let before = std::fs::read(&f.path).unwrap();
                assert!(
                    publish(&f, &f.claims).await.is_err(),
                    "retained editor delivery cannot override the actual current viewer role"
                );
                assert_eq!(std::fs::read(&f.path).unwrap(), before);
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn native_document_approved_check_and_revoked_original_never_receipt() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let f = fixture(runtime_collab::approved_checks::ACTION_ID).await;
                let before = std::fs::read(&f.path).unwrap();
                assert!(
                    publish(&f, &f.claims).await.is_err(),
                    "approved task check is never a document permission"
                );
                assert_eq!(std::fs::read(&f.path).unwrap(), before);
                let f = fixture("edit").await;
                let before = std::fs::read(&f.path).unwrap();
                *f.original.authority.certificate.lock().unwrap() = None;
                assert!(
                    publish(&f, &f.claims).await.is_err(),
                    "actual current host revocation refuses retained native authority"
                );
                assert_eq!(std::fs::read(&f.path).unwrap(), before);
            })
            .await;
    }
}
