// Exercise the production RPC and retained target custody without opening an
// identity key or making a network request. HTTP passes structural validation,
// but the host worker must reject it at its HTTPS gate before signing.
mod external_moderation_rpc_tests {
    use super::*;
    use registry_types::{ExternalModerationOperation, ModerationDestination, ModerationMutation};
    use runtime_collab::external_moderation::{PROTOCOL, Request};

    struct ExactLaunch {
        app: String,
        room: String,
    }

    impl crate::services::LauncherServices for ExactLaunch {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("fixture never launches applications".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            pid == std::process::id() && app == self.app && room == self.room
        }
    }

    struct Room {
        original: signed_worker::OriginalRoom,
        route: CrdtRoute,
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        _owner_mailbox: crate::test_owner::owner_commit::OwnerCommitMailbox,
    }

    impl Room {
        fn new(native: bool) -> Self {
            Self::with_document(native, None)
        }

        fn with_document(native: bool, document: Option<Arc<dyn NativeDocumentReads>>) -> Self {
            let original = signed_worker::OriginalRoom::new(
                if native {
                    tilo_user_identity::DeviceCapabilities::primary()
                } else {
                    tilo_user_identity::DeviceCapabilities::host()
                },
                None,
                None,
            );
            let route = CrdtRoute::record("so.tilo.store", &original.route.cluster_id, "store");
            let fanout = Arc::new(FanoutState::new());
            let mut services = original.membership.services();
            services.committed_documents = document;
            if native {
                services.launcher = Some(Arc::new(ExactLaunch {
                    app: route.app_id.clone(),
                    room: route.cluster_id.clone(),
                }));
            }
            let (owner_handle, owner_mailbox) =
                crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(8);
            let publisher =
                PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(owner_handle));
            let (state, bridge) = CapnpBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                Some(original.membership.authority()),
                None,
                None,
                services,
            );
            let authority = signed_worker::CurrentHost::new(
                &route,
                &original.cert,
                Some(original.origin.capture().unwrap()),
            );
            assert!(state.certificate_authority.set(authority).is_ok());
            Self {
                original,
                route,
                state,
                directories: Rc::new(LocalActionDirectoryRegistry::new(bridge)),
                _owner_mailbox: owner_mailbox,
            }
        }

        fn caller(&self) -> (BrokerActor, InvocationCaller) {
            let scope =
                self.original
                    .cert
                    .capabilities
                    .can_act_as_host
                    .then(|| LocalHostWorkerScope {
                        route: self.route.clone(),
                        certificate: self.original.cert.clone(),
                    });
            let admitted = verify_join_identity_with_host_scope(
                &self.route,
                &signed_worker::presentation(
                    &self.route.cluster_id,
                    &self.original.cert,
                    &self.original.key,
                ),
                self.state.membership.as_ref(),
                None,
                self.state
                    .certificate_authority
                    .get()
                    .map(|authority| authority.as_ref()),
                scope.as_ref(),
            )
            .unwrap();
            (
                admitted.broker_actor(),
                InvocationCaller::Local {
                    route: self.route.clone(),
                    admitted: Box::new(admitted),
                },
            )
        }

        fn invoke(&self, peer: u64, resource: &str, action: &str, argument: &str) -> u64 {
            let offered = self.state.surfaces.lock().unwrap()[&peer]
                .actions
                .iter()
                .find(|offered| offered.resource == resource && offered.action == action)
                .unwrap()
                .clone();
            let (actor, origin) = self.caller();
            // Simulate the trusted host sheet answering these exact bytes.
            let witness = self
                .state
                .witness_confirmation(actor.id(), (resource, action), Some(argument))
                .unwrap();
            let token = self
                .state
                .confirmation_token(
                    peer,
                    &self.route.app_id,
                    &offered,
                    Some(argument),
                    &actor,
                    Some(&tilo_actions::remote::Confirmation::Witnessed(witness)),
                )
                .unwrap();
            let job = self
                .state
                .start_invocation_confirmed_for_origin(
                    peer,
                    resource,
                    action,
                    InvocationInputs {
                        original_request: None,
                        requested: None,
                        caller: None,
                        execution: None,
                        argument: Some(argument),
                        actor: &actor,
                        confirmation_token: token.as_deref(),
                        context: None,
                    },
                    Some(origin),
                )
                .unwrap();
            job.id
        }

        async fn worker(&self, peer: u64) -> signed_worker::Connection {
            signed_worker::join_worker(
                &self.state,
                &self.directories,
                &self.route,
                &self.original.cert,
                &self.original.key,
                peer,
            )
            .await
            .0
        }
    }

    fn operation(appeal: bool) -> ExternalModerationOperation {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: "http://registry.invalid".into(),
                owner_pubkey_hex: "ab".repeat(32),
            },
            request_id: "custody-report".into(),
            issued_at_unix: now,
            expires_at_unix: now + 3600,
            mutation: if appeal {
                ModerationMutation::PublisherAppeal {
                    case_id: "case-one".into(),
                    reason: "The reported problem has been addressed".into(),
                    evidence_digest_hex: None,
                }
            } else {
                ModerationMutation::FileReport {
                    package_id: "org.example.app".into(),
                    category: "misleading".into(),
                    summary: "The package description is misleading".into(),
                    evidence_digest_hex: "cd".repeat(32),
                }
            },
        }
    }

    fn descriptor(action: &str) -> runtime_collab::ExportedAction {
        let directory = signed_worker::directory_state("moderation-r1");
        let mut descriptor = directory.borrow().row().descriptor.unwrap();
        descriptor.action = action.into();
        descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
            kind: descriptor.resource_kind.clone(),
            key: descriptor.resource.clone(),
        });
        descriptor.operation_kind = tilo_actions::OperationKind::Effect;
        descriptor.contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            result: tilo_actions::ResultSpec::Job,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
            ..Default::default()
        };
        descriptor
    }

    fn argument(operation: &ExternalModerationOperation) -> String {
        serde_json::json!({
            "app": "org.example.app",
            "operation": serde_json::to_string(operation).unwrap(),
        })
        .to_string()
    }

    async fn refused(
        handle: &runtime_collab::ChannelHandle,
        request: &Request,
        on_behalf_of: [u8; 16],
        expected: &str,
    ) {
        let error = handle
            .external_moderation(request, on_behalf_of)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_binds_exact_operation_and_live_target_custody() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let room = Room::new(false);
                    let target = room.worker(971).await;
                    let other = room.worker(972).await;
                    let descriptor = descriptor(action);
                    target
                        .handle
                        .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let operation = operation(action == "appeal");
                    let wire =
                        room.invoke(971, &descriptor.resource, action, &argument(&operation));
                    let delivered = stream.read().await.unwrap();
                    assert_eq!(delivered.id, wire);
                    let id = delivered.invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    request.validate().unwrap();
                    // This proves admission reached the production host worker,
                    // not just a custody helper that always refuses requests.
                    refused(&target.handle, &request, id, "HTTPS").await;
                    for field in ["destination", "owner", "mutation"] {
                        let mut changed = request.clone();
                        match field {
                            "destination" => changed.operation.destination.base_url.push('/'),
                            "owner" => {
                                changed.operation.destination.owner_pubkey_hex = "ef".repeat(32)
                            }
                            "mutation" => match &mut changed.operation.mutation {
                                ModerationMutation::FileReport { summary, .. } => summary.push('!'),
                                ModerationMutation::PublisherAppeal { reason, .. } => {
                                    reason.push('!')
                                }
                                _ => unreachable!(),
                            },
                            _ => unreachable!(),
                        }
                        refused(&target.handle, &changed, id, "differs").await;
                    }
                    let mut wrong_mutation = request.clone();
                    wrong_mutation.operation.mutation = operation_for_other_action(action);
                    refused(&target.handle, &wrong_mutation, id, "differs").await;
                    let unknown = uuid::Uuid::new_v4().into_bytes();
                    refused(&target.handle, &request, unknown, "invocation").await;
                    let unknown_request = Request {
                        invocation: uuid::Uuid::from_bytes(unknown).to_string(),
                        ..request.clone()
                    };
                    refused(
                        &target.handle,
                        &unknown_request,
                        unknown,
                        "no retained target invocation",
                    )
                    .await;
                    refused(&other.handle, &request, id, "no retained target invocation").await;

                    let mut changed = descriptor.clone();
                    changed.contract.kind = tilo_actions::ActionKind::Read;
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[changed])
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "mismatch").await;
                    target
                        .handle
                        .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "HTTPS").await;
                    target
                        .handle
                        .cancel_published_invocation(wire)
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "retired or cancelled").await;
                    room.state.remove_connection(971);
                    assert!(
                        target
                            .handle
                            .external_moderation(&request, id)
                            .await
                            .is_err()
                    );
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_outside_grant_does_not_replace_exact_confirmation() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let room = Room::new(false);
                    let target = room.worker(979).await;
                    let mut descriptor = descriptor(action);
                    descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::None;
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[descriptor])
                        .await
                        .unwrap();
                    let _stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let operation = operation(action == "appeal");
                    // Even with a broad Outside grant, the unconfirmed call must
                    // now fail at admission, before it can reach the host worker.
                    let error = target
                        .handle
                        .invoke_published_action(
                            979,
                            "workspace:atlas",
                            action,
                            Some(&argument(&operation)),
                            "{}",
                        )
                        .await
                        .expect_err("a broad grant cannot replace the trusted host answer");
                    assert!(error.to_string().contains("confirmation"), "{error}");
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_confirmation_token_binds_payload_and_destination() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let room = Room::new(false);
                    let target = room.worker(980).await;
                    let descriptor = descriptor(action);
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[descriptor])
                        .await
                        .unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let offered = room.state.surfaces.lock().unwrap()[&980].actions[0].clone();
                    let (actor, origin) = room.caller();
                    let operation = operation(action == "appeal");
                    let exact = argument(&operation);
                    assert!(matches!(
                        room.state.confirmation_token(
                            980,
                            &room.route.app_id,
                            &offered,
                            Some(&exact),
                            &actor,
                            None
                        ),
                        Err(tilo_actions::remote::ActionHostError::ConfirmationRequired)
                    ));
                    assert!(actor.direct(), "the app's own identity is a direct person");
                    assert!(matches!(
                        room.state.confirmation_token(
                            980,
                            &room.route.app_id,
                            &offered,
                            Some(&exact),
                            &actor,
                            Some(&tilo_actions::remote::Confirmation::Confirmed),
                        ),
                        Err(tilo_actions::remote::ActionHostError::ConfirmationRequired)
                    ));
                    for altered in ["missing", "payload", "destination"] {
                        let witness = room
                            .state
                            .witness_confirmation(
                                actor.id(),
                                (&offered.resource, action),
                                Some(&exact),
                            )
                            .unwrap();
                        let token = room
                            .state
                            .confirmation_token(
                                980,
                                &room.route.app_id,
                                &offered,
                                Some(&exact),
                                &actor,
                                Some(&tilo_actions::remote::Confirmation::Witnessed(witness)),
                            )
                            .unwrap()
                            .unwrap();
                        let mut changed = operation.clone();
                        match altered {
                            "destination" => {
                                changed.destination.base_url = "http://other.invalid".into()
                            }
                            "payload" => match &mut changed.mutation {
                                ModerationMutation::FileReport { summary, .. } => summary.push('!'),
                                ModerationMutation::PublisherAppeal { reason, .. } => {
                                    reason.push('!')
                                }
                                _ => unreachable!(),
                            },
                            "missing" => {}
                            _ => unreachable!(),
                        }
                        let changed = argument(&changed);
                        let result = room.state.start_invocation_confirmed_for_origin(
                            980,
                            &offered.resource,
                            action,
                            InvocationInputs {
                                original_request: None,
                                requested: None,
                                caller: None,
                                execution: None,
                                argument: Some(&changed),
                                actor: &actor,
                                confirmation_token: (altered != "missing")
                                    .then_some(token.as_str()),
                                context: None,
                            },
                            Some(origin.clone()),
                        );
                        let error = result
                            .err()
                            .expect("unconfirmed or changed operation must be refused");
                        assert!(error.contains("confirmation"), "{altered}: {error}");
                    }
                    // A fresh answer for exactly these bytes reaches the host gate.
                    room.invoke(980, &offered.resource, action, &exact);
                    let id = stream.read().await.unwrap().invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    refused(&target.handle, &request, id, "HTTPS").await;
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_requires_witness_without_publisher_confirmation_policy() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let room = Room::new(false);
                    let target = room.worker(980).await;
                    let mut descriptor = descriptor(action);
                    descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::None;
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[descriptor])
                        .await
                        .unwrap();
                    let _stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let offered = room.state.surfaces.lock().unwrap()[&980].actions[0].clone();
                    let (actor, origin) = room.caller();
                    assert!(actor.direct());
                    let exact = argument(&operation(action == "appeal"));
                    assert!(matches!(
                        room.state.confirmation_token(
                            980,
                            &room.route.app_id,
                            &offered,
                            Some(&exact),
                            &actor,
                            Some(&tilo_actions::remote::Confirmation::Confirmed),
                        ),
                        Err(tilo_actions::remote::ActionHostError::ConfirmationRequired)
                    ));
                    let result = room.state.start_invocation_confirmed_for_origin(
                        980,
                        &offered.resource,
                        action,
                        InvocationInputs {
                            original_request: None,
                            requested: None,
                            caller: None,
                            execution: None,
                            argument: Some(&exact),
                            actor: &actor,
                            confirmation_token: None,
                            context: None,
                        },
                        Some(origin),
                    );
                    let error = result
                        .err()
                        .expect("Store disclosure always requires a host grant");
                    assert!(error.contains("confirmation"), "{error}");
                }
            })
            .await;
    }

    struct ReportDocument(Vec<u8>);

    impl DocumentBytes for ReportDocument {
        fn bytes(&self) -> &[u8] {
            &self.0
        }
    }

    impl NativeDocumentReads for ReportDocument {
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
            if self.0.len() > maximum {
                return Err("report exceeds bound".into());
            }
            Ok(NativeDocumentRead::Captured {
                revision: *blake3::hash(&self.0).as_bytes(),
                heads: Vec::new(),
                body: Box::new(ReportDocument(self.0.clone())),
            })
        }
    }

    fn committed_report(note: &str, case: Option<&str>) -> Arc<dyn NativeDocumentReads> {
        use automerge::transaction::Transactable as _;
        let mut doc = automerge::AutoCommit::new();
        let reports = doc
            .put_object(automerge::ROOT, "reports", automerge::ObjType::Map)
            .unwrap();
        let report = doc
            .put_object(reports, "org.example.app", automerge::ObjType::Map)
            .unwrap();
        doc.put(&report, "status", "Not submitted").unwrap();
        if let Some(case) = case {
            doc.put(&report, "case", case).unwrap();
        }
        let text = doc
            .put_object(&report, "note", automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(text, 0, 0, note).unwrap();
        Arc::new(ReportDocument(doc.save()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_checks_committed_original_report_before_identity() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", temp.path())
            .set("XDG_CONFIG_HOME", temp.path().join("config"))
            .set("TILO_IDENTITY_ROOT", temp.path().join("identity"));
        std::fs::write(temp.path().join("registries.toml"), format!(
            "[[registry]]\nname = 'Custom'\nbase_url = 'https://custom.example'\nowner_pubkey_hex = '{}'\n", "ab".repeat(32),
        )).unwrap();
        tokio::task::LocalSet::new()
            .run_until(async {
                for altered in ["digest", "summary", "package", "category", "none"] {
                    let note = "The package description is misleading";
                    let room = Room::with_document(false, Some(committed_report(note, None)));
                    let target = room.worker(977).await;
                    let descriptor = descriptor("submit-report");
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[descriptor])
                        .await
                        .unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let mut operation = operation(false);
                    operation.destination.base_url = "https://custom.example".into();
                    operation.mutation = ModerationMutation::FileReport {
                        package_id: if altered == "package" {
                            "other.app"
                        } else {
                            "org.example.app"
                        }
                        .into(),
                        category: if altered == "category" {
                            "security"
                        } else {
                            "policy"
                        }
                        .into(),
                        summary: if altered == "summary" {
                            "A caller-invented report"
                        } else {
                            note
                        }
                        .into(),
                        evidence_digest_hex: if altered == "digest" {
                            "cd".repeat(32)
                        } else {
                            registry_types::report_evidence_digest(
                                "org.example.app",
                                "Not submitted",
                                note,
                                None,
                            )
                            .unwrap()
                        },
                    };
                    room.invoke(
                        977,
                        "workspace:atlas",
                        "submit-report",
                        &argument(&operation),
                    );
                    let id = stream.read().await.unwrap().invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    refused(
                        &target.handle,
                        &request,
                        id,
                        if altered == "none" {
                            "identity"
                        } else {
                            "committed report evidence"
                        },
                    )
                    .await;
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_appeal_requires_committed_case_and_reason() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", temp.path())
            .set("XDG_CONFIG_HOME", temp.path().join("config"))
            .set("TILO_IDENTITY_ROOT", temp.path().join("identity"));
        std::fs::write(temp.path().join("registries.toml"), format!(
            "[[registry]]\nname = 'Custom'\nbase_url = 'https://custom.example'\nowner_pubkey_hex = '{}'\n", "ab".repeat(32),
        )).unwrap();
        tokio::task::LocalSet::new()
            .run_until(async {
                for altered in ["case", "reason", "digest", "none"] {
                    let note = "The reported problem has been addressed";
                    let room =
                        Room::with_document(false, Some(committed_report(note, Some("case-one"))));
                    let target = room.worker(978).await;
                    let descriptor = descriptor("appeal");
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[descriptor])
                        .await
                        .unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let mut operation = operation(true);
                    operation.destination.base_url = "https://custom.example".into();
                    operation.mutation = ModerationMutation::PublisherAppeal {
                        case_id: if altered == "case" {
                            "case-other"
                        } else {
                            "case-one"
                        }
                        .into(),
                        reason: if altered == "reason" {
                            "An invented appeal reason"
                        } else {
                            note
                        }
                        .into(),
                        evidence_digest_hex: (altered == "digest").then(|| "cd".repeat(32)),
                    };
                    room.invoke(978, "workspace:atlas", "appeal", &argument(&operation));
                    let id = stream.read().await.unwrap().invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    refused(
                        &target.handle,
                        &request,
                        id,
                        if altered == "none" {
                            "identity"
                        } else {
                            "committed report evidence"
                        },
                    )
                    .await;
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_uses_host_registry_pins_not_caller_trust() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", temp.path());
        tokio::task::LocalSet::new()
            .run_until(async {
                for (enabled, base, owner, expected) in [
                    (true, "https://other.example", "ab".repeat(32), "trusted configured registry"),
                    (true, "https://custom.example", "ef".repeat(32), "trusted configured registry"),
                    (false, "https://custom.example", "ab".repeat(32), "trusted configured registry"),
                    (true, "https://custom.example", "ab".repeat(32), "committed report owner unavailable"),
                ] {
                    std::fs::write(
                        temp.path().join("registries.toml"),
                        format!("[[registry]]\nname = 'Custom'\nbase_url = '{base}'\nowner_pubkey_hex = '{owner}'\nenabled = {enabled}\n"),
                    ).unwrap();
                    let room = Room::new(false);
                    let target = room.worker(976).await;
                    let descriptor = descriptor("submit-report");
                    target.handle.publish_actions(&room.route.app_id, &[descriptor]).await.unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let mut operation = operation(false);
                    operation.destination.base_url = "https://custom.example".into();
                    room.invoke(976, "workspace:atlas", "submit-report", &argument(&operation));
                    let delivered = stream.read().await.unwrap();
                    let id = delivered.invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    refused(&target.handle, &request, id, expected).await;
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_refuses_expired_exact_operation_before_signing() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = Room::new(false);
                let target = room.worker(975).await;
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(&room.route.app_id, &[descriptor])
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let mut operation = operation(false);
                operation.issued_at_unix = 1;
                operation.expires_at_unix = 100;
                room.invoke(
                    975,
                    "workspace:atlas",
                    "submit-report",
                    &argument(&operation),
                );
                let delivered = stream.read().await.unwrap();
                let id = delivered.invocation_uuid;
                let request = Request {
                    protocol: PROTOCOL.into(),
                    invocation: uuid::Uuid::from_bytes(id).to_string(),
                    operation,
                };
                refused(&target.handle, &request, id, "operation expired").await;
            })
            .await;
    }

    fn operation_for_other_action(action: &str) -> ModerationMutation {
        operation(action == "submit-report").mutation
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_native_target_requires_original_channel_and_publication() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = Room::new(true);
                let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
                std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC)
                    .unwrap();
                let handoff = serde_json::to_vec(&serde_json::json!({
                    "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
                    "app_id": room.route.app_id,
                    "workspace_id": room.route.cluster_id,
                }))
                .unwrap();
                std::io::Write::write_all(
                    &mut client,
                    &launcher::services::COLLAB_LAUNCH_FRAMING
                        .encode(&handoff)
                        .unwrap(),
                )
                .unwrap();
                let (rpc, server, client) = signed_worker::rpc_pair(
                    room.state.clone(),
                    973,
                    server,
                    client,
                    None,
                    room.directories.clone(),
                );
                let presentation = signed_worker::presentation(
                    &room.route.cluster_id,
                    &room.original.cert,
                    &room.original.key,
                );
                let handle = rpc
                    .join_channel(&room.route.channel_key(), &presentation)
                    .await
                    .unwrap();
                let target = signed_worker::Connection {
                    handle,
                    server,
                    client,
                };
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let wire = room.invoke(
                    973,
                    &descriptor.resource,
                    "submit-report",
                    &argument(&operation),
                );
                let delivered = stream.read().await.unwrap();
                assert_eq!(delivered.id, wire);
                let id = delivered.invocation_uuid;
                let request = Request {
                    protocol: PROTOCOL.into(),
                    invocation: uuid::Uuid::from_bytes(id).to_string(),
                    operation,
                };
                refused(&target.handle, &request, id, "HTTPS").await;
                let foreign_route =
                    CrdtRoute::record(&room.route.app_id, &room.route.cluster_id, "foreign-store");
                let foreign = rpc
                    .join_channel(&foreign_route.channel_key(), &presentation)
                    .await
                    .unwrap();
                refused(&foreign, &request, id, "mismatch").await;
                let mut replacement = descriptor;
                replacement.resource = "replacement-store-resource".into();
                target
                    .handle
                    .publish_actions(&room.route.app_id, &[replacement])
                    .await
                    .unwrap();
                refused(&target.handle, &request, id, "publication retired").await;
                room.state.remove_connection(973);
                assert!(
                    target
                        .handle
                        .external_moderation(&request, id)
                        .await
                        .is_err()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_rechecks_expired_original_policy() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = Room::new(false);
                let target = room.worker(974).await;
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let wire = room.invoke(
                    974,
                    &descriptor.resource,
                    "submit-report",
                    &argument(&operation),
                );
                let delivered = stream.read().await.unwrap();
                assert_eq!(delivered.id, wire);
                let id = delivered.invocation_uuid;
                let request = Request {
                    protocol: PROTOCOL.into(),
                    invocation: uuid::Uuid::from_bytes(id).to_string(),
                    operation,
                };
                refused(&target.handle, &request, id, "HTTPS").await;
                room.original
                    .time_control
                    .advance_monotonic(std::time::Duration::from_secs(51));
                let error = target
                    .handle
                    .external_moderation(&request, id)
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(
                    !error.contains("HTTPS"),
                    "expired authority reached the effect worker: {error}"
                );
                assert!(
                    error.contains("certified collaboration session is no longer admitted")
                        && error.contains("not a workspace member"),
                    "expected original authority expiry refusal: {error}"
                );
            })
            .await;
    }
}
