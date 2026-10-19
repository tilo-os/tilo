// Included in external_moderation_rpc_tests so the same admitted RPC fixture is used.
// The host supplies committed Store bytes and answers the exact disclosure witness;
// enrollment, purpose-key signing, HTTPS, registry verification and persistence are real.
mod https_completion {
    use super::*;
    use crate::app_actions::{
        AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
        ModerationConsentPreview, ModerationConsentPurpose,
    };

    #[allow(
        clippy::panic,
        reason = "test code included into the broker module: clippy does not see the cfg(test) on its include!, so it reads the test's checks as production code"
    )]
    async fn bounded<T>(stage: &str, future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(15), future)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {stage}"))
    }

    #[allow(
        clippy::panic,
        reason = "test code included into the broker module: clippy does not see the cfg(test) on its include!, so it reads the test's checks as production code"
    )]
    async fn scenario(stage: &str, future: impl std::future::Future<Output = ()>) {
        tokio::time::timeout(std::time::Duration::from_secs(60), future)
            .await
            .unwrap_or_else(|_| panic!("timed out during {stage}"));
    }

    async fn native_app(room: &Room, peer: u64) -> signed_worker::Connection {
        use std::io::Write as _;
        // A native app needs the launcher's kernel-pinned process custody,
        // not a private worker socket (which intentionally skips the handoff).
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        let (marker, handoff) = launcher_handoff_bytes(&room.route.app_id, &room.route.cluster_id);
        client.write_all(&marker).unwrap();
        client.write_all(&handoff).unwrap();
        let (rpc, server, client) = signed_worker::rpc_pair(
            room.state.clone(),
            peer,
            server,
            client,
            None,
            room.directories.clone(),
        );
        let handle = rpc
            .join_channel(
                &room.route.channel_key(),
                &signed_worker::presentation(
                    &room.route.cluster_id,
                    &room.original.cert,
                    &room.original.key,
                ),
            )
            .await
            .unwrap();
        signed_worker::Connection {
            handle,
            server,
            client,
        }
    }

    struct Sheet(
        tokio::sync::mpsc::UnboundedSender<(
            AppActionConsentRequest,
            tokio::sync::oneshot::Sender<AppActionConsent>,
        )>,
    );

    impl AppActionConsentPrompter for Sheet {
        fn ask(
            &self,
            request: AppActionConsentRequest,
        ) -> Option<tokio::sync::oneshot::Receiver<AppActionConsent>> {
            let (answer, receive) = tokio::sync::oneshot::channel();
            self.0.send((request, answer)).ok()?;
            Some(receive)
        }
    }

    fn assert_preview(
        question: &AppActionConsentRequest,
        operation: &ExternalModerationOperation,
        root: &[u8; 32],
    ) {
        assert!(question.confirming);
        assert_eq!(question.target_app, "so.tilo.store");
        let (purpose, target, category, content, evidence_digest) = match &operation.mutation {
            ModerationMutation::FileReport {
                package_id,
                category,
                summary,
                evidence_digest_hex,
            } => (
                ModerationConsentPurpose::Report,
                package_id.clone(),
                Some(category.clone()),
                summary.clone(),
                Some(evidence_digest_hex.clone()),
            ),
            ModerationMutation::PublisherAppeal {
                case_id,
                reason,
                evidence_digest_hex,
            } => (
                ModerationConsentPurpose::Appeal,
                case_id.clone(),
                None,
                reason.clone(),
                evidence_digest_hex.clone(),
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            question.moderation,
            Some(ModerationConsentPreview {
                purpose,
                destination: operation.destination.base_url.clone(),
                owner_root: operation.destination.owner_pubkey_hex.clone(),
                signing_root: hex::encode(root),
                target,
                category,
                content,
                evidence_digest,
            })
        );
    }
    use registry_server::{
        AppState, ModerationApiService, PublisherAllowlist, ServerIdentity, Storage,
    };
    use registry_types::{ModerationCaseStatus, RegistryEntry, RegistryTarget, RegistryVersion};

    struct DirectoryPump(tokio::task::JoinHandle<()>);

    impl Drop for DirectoryPump {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    struct StoreDirectory;

    impl runtime_collab::raw::action_directory::Server for StoreDirectory {
        fn resolve(
            self: Rc<Self>,
            _: runtime_collab::raw::action_directory::ResolveParams,
            mut results: runtime_collab::raw::action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            let mut row = signed_worker::directory_state("moderation-r1")
                .borrow()
                .row();
            let descriptor = descriptor("submit-report");
            row.action = Some(descriptor.action.clone());
            row.visibility
                .as_mut()
                .unwrap()
                .action
                .clone_from(&descriptor.action);
            row.visibility.as_mut().unwrap().contract = descriptor.contract.clone();
            row.visibility.as_mut().unwrap().governing_kind = Some(descriptor.operation_kind);
            row.descriptor = Some(descriptor);
            runtime_collab::write_action_directory_row(
                results.get().init_resolution().init_found(),
                &row,
            )
            .unwrap();
            Promise::ok(())
        }
    }

    struct HttpsRegistry {
        base: String,
        identity: ServerIdentity,
        task: tokio::task::JoinHandle<()>,
        directory: tempfile::TempDir,
    }

    impl Drop for HttpsRegistry {
        fn drop(&mut self) {
            self.task.abort();
            external_moderation::TEST_ROOTS
                .lock()
                .unwrap()
                .retain(|(base, _)| base != &self.base);
        }
    }

    impl HttpsRegistry {
        async fn new(publisher: &ed25519_dalek::SigningKey) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let storage = Storage::open(directory.path()).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!(
                "https://localhost:{}",
                listener.local_addr().unwrap().port()
            );
            let owner = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
            let identity = ServerIdentity {
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                name: "Local registry".into(),
                description: String::new(),
                base_url: base.clone(),
            };
            storage.save_identity(&identity).unwrap();
            let mut allowlist = PublisherAllowlist::empty();
            allowlist.upsert(registry_server::PublisherRow {
                pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
                can_add_new: true,
                note: String::new(),
                namespaces: vec!["org.example".into()],
            });
            storage.save_allowlist(&allowlist).unwrap();
            storage.save_entry(&entry(publisher)).unwrap();
            let state = AppState::new(storage.clone(), identity.clone());
            let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let der = certificate.cert.der().clone();
            let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![der.clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(
                    certificate.signing_key.serialize_der(),
                )
                .into(),
            )
            .unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
            external_moderation::TEST_ROOTS
                .lock()
                .unwrap()
                .push((base.clone(), der.to_vec()));
            // One request per connection keeps every accepted connection owned by
            // this task: aborting the guard cannot leak a detached test server.
            let task = tokio::spawn(async move {
                loop {
                    let (socket, _) = listener.accept().await.unwrap();
                    let socket = acceptor.accept(socket).await.unwrap();
                    let state = state.clone();
                    hyper::server::conn::http1::Builder::new()
                        .keep_alive(false)
                        .serve_connection(
                            hyper_util::rt::TokioIo::new(socket),
                            hyper::service::service_fn(move |request| {
                                registry_server::handle(state.clone(), request)
                            }),
                        )
                        .await
                        .unwrap();
                }
            });
            Self {
                base,
                identity,
                task,
                directory,
            }
        }

        fn service(&self) -> ModerationApiService {
            // Reopen from disk rather than asserting solely on the live handler's state.
            ModerationApiService::new(
                Storage::open(self.directory.path()).unwrap(),
                self.identity.clone(),
            )
        }
    }

    fn entry(publisher: &ed25519_dalek::SigningKey) -> RegistryEntry {
        let mut entry = RegistryEntry {
            package_id: "org.example.app".into(),
            kind: registry_types::EntryKind::App,
            name: "Example".into(),
            description: "An example package".into(),
            tagline: "Example".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://example.org/icon.svg",
                "Example",
            ),
            supported_languages: vec!["en".into()],
            categories: Vec::new(),
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                released_at_unix: tilo_clock::unix_now(),
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: "https://example.org/app.tapp".into(),
                    tapp_root_hash_hex: "ab".repeat(32),
                    tapp_size_bytes: 1,
                    tapp_blake3_hex: "cd".repeat(32),
                    target: RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: tilo_clock::unix_now(),
            signature_hex: String::new(),
        };
        registry_types::sign_entry(&mut entry, publisher);
        entry
    }

    // Minimal typed owner: only a saved, committed Automerge snapshot is readable.
    // It cannot substitute caller-supplied bytes during the disclosure continuation.
    struct StoreOwner(std::path::PathBuf);
    impl StoreOwner {
        fn load(&self) -> tilo_resource::Replica<store::Reports> {
            if self.0.exists() {
                tilo_resource::Replica::load(&std::fs::read(&self.0).unwrap()).unwrap()
            } else {
                tilo_resource::Replica::new()
            }
        }

        fn commit(&self, note: &str) {
            let mut reports = self.load();
            if reports.state().unwrap().report("org.example.app").is_none() {
                reports
                    .apply(store::Reports::DRAFT_REPORT.with("org.example.app"))
                    .unwrap();
            }
            // Use the Store's real resource action and typed collaborative text edit.
            reports
                .apply(
                    tilo_resource::Invoke::edit(
                        &store::Reports::note("org.example.app").replace(note),
                    )
                    .unwrap(),
                )
                .unwrap();
            std::fs::write(&self.0, reports.save()).unwrap();
        }

        fn record_case(&self, receipt: &registry_types::ModerationCase) {
            let mut reports = self.load();
            reports
                .apply(
                    store::Reports::RECORD_MODERATION_CASE.with(
                        store::reports::reports::RecordModerationCase {
                            app_id: "org.example.app".into(),
                            case_id: receipt.case_id.clone(),
                            status: if receipt.status == ModerationCaseStatus::Open {
                                "Open"
                            } else {
                                "Appealed"
                            }
                            .into(),
                        },
                    ),
                )
                .unwrap();
            std::fs::write(&self.0, reports.save()).unwrap();
            assert_eq!(
                reports
                    .state()
                    .unwrap()
                    .report("org.example.app")
                    .unwrap()
                    .case_id(),
                Some(receipt.case_id.as_str())
            );
        }
    }
    impl NativeDocumentReads for StoreOwner {
        fn requires_room(&self, _: &str) -> bool {
            true
        }
        fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
            context.revalidate()
        }
        fn capture(
            &self,
            context: &NativeDocumentReadContext,
            _: Option<[u8; 32]>,
            maximum: usize,
        ) -> Result<NativeDocumentRead, String> {
            self.authorize(context)?;
            let bytes = std::fs::read(&self.0).map_err(|error| error.to_string())?;
            if bytes.len() > maximum {
                return Err("Store snapshot exceeds limit".into());
            }
            Ok(NativeDocumentRead::Captured {
                revision: *blake3::hash(&bytes).as_bytes(),
                heads: Vec::new(),
                body: Box::new(ReportDocument(bytes)),
            })
        }
    }

    fn approved_operation(
        server: &HttpsRegistry,
        note: &str,
        case: Option<&str>,
    ) -> ExternalModerationOperation {
        let digest = registry_types::report_evidence_digest(
            "org.example.app",
            if case.is_some() {
                "Open"
            } else {
                store::reports::DRAFT_STATUS
            },
            note,
            case,
        )
        .unwrap();
        let now = tilo_clock::unix_now();
        ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: server.base.clone(),
                owner_pubkey_hex: server.identity.owner_pubkey_hex.clone(),
            },
            request_id: uuid::Uuid::new_v4().to_string(),
            issued_at_unix: now,
            expires_at_unix: now + 300,
            mutation: match case {
                None => ModerationMutation::FileReport {
                    package_id: "org.example.app".into(),
                    category: "policy".into(),
                    summary: note.into(),
                    evidence_digest_hex: digest,
                },
                Some(id) => ModerationMutation::PublisherAppeal {
                    case_id: id.into(),
                    reason: note.into(),
                    evidence_digest_hex: Some(digest),
                },
            },
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_store_host_sheet_allow_deny_absence_and_retry() {
        tokio::task::LocalSet::new().run_until(scenario("durable Store", async {
            let files = tempfile::tempdir().unwrap();
            let mut config = tilo_test_env::ScopedEnv::new();
            config.set("TILO_STORE_CONFIG_DIR", files.path());
            let original = signed_worker::OriginalRoom::current_native();
            let (device, _) = tilo_identity_host::broker::trusted_current_device(&original.route.cluster_id).unwrap();
            let server = HttpsRegistry::new(&device).await;
            let owner = Arc::new(StoreOwner(files.path().join("store.automerge")));
            owner.commit("The description is misleading");
            let room = Room::with_original(true, Some(owner), original);
            let target = bounded("durable Store worker", room.worker(988)).await;
            let state = Arc::downgrade(&room.state);
            let directories = room.directories.clone();
            let turn = room.state.action_directory_bridge.turn.clone();
            let _pump = DirectoryPump(tokio::task::spawn_local(async move {
                loop {
                    turn.notified().await;
                    let Some(state) = state.upgrade() else { break };
                    state.drain_action_directory_bridge(&directories);
                }
            }));
            target.handle.publish_action_directory(&room.route.app_id, capnp_rpc::new_client(StoreDirectory)).await.unwrap();
            let descriptor = descriptor("submit-report");
            // A reliable target binding captures an already-live UUID stream.
            let stream = target.handle.subscribe_stable_invocations().await.unwrap();
            let (_, binding) = target.handle.resolve_published_action_for_invocation(988, "moderation-r1", &descriptor.resource, &descriptor.action).await.unwrap();
            let operation = approved_operation(&server, "The description is misleading", None);
            let exact = argument(&operation);
            let evidence = r#"{"confirm":true,"confirmation_token":"caller-supplied"}"#;
            let absent = uuid::Uuid::new_v4();
            let unavailable = target.handle.invoke_resolved_published_action_durable(
                absent.as_bytes(), &binding, "moderation-r1", &descriptor.resource, &descriptor.action, Some(&exact), evidence,
            ).await.expect_err("caller evidence cannot replace an absent host");
            assert!(unavailable.to_string().contains("host confirmation unavailable"), "{unavailable}");
            let (send, mut sheets) = tokio::sync::mpsc::unbounded_channel();
            room.state.set_app_action_consent(Arc::new(Sheet(send)));
            // Keep the same RPC read across absence checks: dropping its caller
            // does not cancel the server's receive and can discard a delivery.
            let mut delivery = Box::pin(stream.read());
            for allow in [false, true] {
                let invocation = uuid::Uuid::new_v4();
                let call = target.handle.invoke_resolved_published_action_durable(
                    invocation.as_bytes(), &binding, "moderation-r1", &descriptor.resource, &descriptor.action, Some(&exact), evidence,
                );
                tokio::pin!(call);
                let (question, answer) = tokio::select! {
                    result = &mut call => panic!("durable invocation bypassed host sheet: {result:?}"),
                    sheet = bounded("durable host sheet", sheets.recv()) => sheet.unwrap(),
                };
                assert_preview(&question, &operation, &room.original.cert.user_public_key);
                assert!(server.service().transparency(1, 10).unwrap().events.is_empty());
                assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut delivery).await.is_err());
                answer.send(if allow { AppActionConsent::Once } else { AppActionConsent::Deny }).unwrap();
                let result = bounded("durable admission after consent", call).await;
                if allow {
                    let admitted = result.unwrap();
                    let delivered = bounded("durable invocation delivery", &mut delivery).await.unwrap();
                    delivery = Box::pin(stream.read());
                    assert_eq!(delivered.invocation_uuid, *invocation.as_bytes());
                    let retry = target.handle.invoke_resolved_published_action_durable(
                        invocation.as_bytes(), &binding, "moderation-r1", &descriptor.resource, &descriptor.action, Some(&exact), "{}",
                    ).await.unwrap();
                    assert_eq!(retry, admitted);
                    assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut delivery).await.is_err());
                } else {
                    let denied = result.expect_err("the host declined disclosure");
                    assert!(denied.to_string().contains("Store disclosure declined"), "{denied}");
                    assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut delivery).await.is_err());
                }
                assert!(matches!(sheets.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
                assert!(server.service().transparency(1, 10).unwrap().events.is_empty());
            }
        })).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_store_report_and_appeal_reach_v2_verifier_over_https_and_persist() {
        tokio::task::LocalSet::new()
            .run_until(scenario("native Store", async {
                let files = tempfile::tempdir().unwrap();
                let mut config = tilo_test_env::ScopedEnv::new();
                config.set("TILO_STORE_CONFIG_DIR", files.path());
                let original = signed_worker::OriginalRoom::current_native();
                let (device, cert) =
                    tilo_identity_host::broker::trusted_current_device(&original.route.cluster_id)
                        .unwrap();
                assert_eq!(cert, original.cert);
                let server = HttpsRegistry::new(&device).await;
                std::fs::write(
                    files.path().join("registries.toml"),
                    format!(
                        "[[registry]]\nname = 'Local'\nbase_url = '{}'\nowner_pubkey_hex = '{}'\n",
                        server.base, server.identity.owner_pubkey_hex,
                    ),
                )
                .unwrap();
                let owner = Arc::new(StoreOwner(files.path().join("store.automerge")));
                let room = Room::with_original(true, Some(owner.clone()), original);
                let target = bounded("native Store launch", native_app(&room, 987)).await;
                let (send, mut sheets) = tokio::sync::mpsc::unbounded_channel();
                room.state.set_app_action_consent(Arc::new(Sheet(send)));
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let mut case_id = None;
                for action in ["submit-report", "appeal"] {
                    let note = if case_id.is_none() {
                        "The description is misleading"
                    } else {
                        "The description is now corrected"
                    };
                    owner.commit(note);
                    let operation = approved_operation(&server, note, case_id.as_deref());
                    let descriptor = descriptor(action);
                    target
                        .handle
                        .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    let exact = argument(&operation);
                    let call = target.handle.invoke_published_action(
                        987, &descriptor.resource, action, Some(&exact),
                        r#"{"confirm":true,"confirmation_token":"caller-supplied"}"#,
                    );
                    tokio::pin!(call);
                    let (question, answer) = tokio::select! {
                        result = &mut call => panic!("invocation bypassed the host sheet: {result:?}"),
                        sheet = bounded("native host sheet", sheets.recv()) => sheet.unwrap(),
                    };
                    assert_preview(&question, &operation, &room.original.cert.user_public_key);
                    assert_eq!(server.service().transparency(1, 10).unwrap().events.len(), usize::from(case_id.is_some()));
                    let delivery = stream.read();
                    tokio::pin!(delivery);
                    assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut delivery).await.is_err());
                    answer.send(AppActionConsent::Once).unwrap();
                    bounded("native admission after consent", call).await.unwrap();
                    let delivered = bounded("native invocation delivery", &mut delivery).await.unwrap();
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(delivered.invocation_uuid).to_string(),
                        operation: operation.clone(),
                    };
                    let receipt = tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        target
                            .handle
                            .external_moderation(&request, delivered.invocation_uuid),
                    )
                    .await
                    .expect("HTTPS disclosure completed")
                    .expect("registry accepted v2 disclosure");
                    assert_eq!(
                        server.service().case(&receipt.case_id).unwrap(),
                        Some(receipt.clone())
                    );
                    if let Some(id) = &case_id {
                        assert_eq!(&receipt.case_id, id);
                        assert_eq!(receipt.status, ModerationCaseStatus::Appealed);
                    } else {
                        assert_eq!(receipt.status, ModerationCaseStatus::Open);
                    }
                    case_id = Some(receipt.case_id.clone());
                    let events = server.service().transparency(1, 10).unwrap().events;
                    let signed = &events.last().unwrap().signed_mutation;
                    assert_eq!(signed.protocol, registry_types::MODERATION_PROTOCOL);
                    assert_eq!(signed.external_operation().unwrap(), operation);
                    assert_eq!(
                        signed.actor_pubkey_hex,
                        hex::encode(device.verifying_key().to_bytes())
                    );
                    assert_ne!(
                        signed.credential.as_ref().unwrap().public_key_hex,
                        signed.actor_pubkey_hex
                    );
                    registry_types::verify_moderation_mutation(signed).unwrap();
                    // The exact retained continuation returns its receipt without duplicating the write.
                    let retry = target
                        .handle
                        .external_moderation(&request, delivered.invocation_uuid)
                        .await
                        .unwrap();
                    assert_eq!(retry, receipt);
                    assert!(matches!(sheets.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
                    assert_eq!(
                        server.service().transparency(1, 10).unwrap().events.len(),
                        events.len()
                    );
                    owner.record_case(&receipt);
                }
                assert_eq!(
                    server.service().transparency(1, 10).unwrap().events.len(),
                    2
                );
            }))
            .await;
    }
}
