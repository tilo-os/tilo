// Exercise production RPC and retained target ownership. Refusal cases stop before
// disclosure; the included HTTPS regression enrolls an identity and reaches the
// real registry verifier and durable case store.
mod external_moderation_rpc_tests {
    use super::*;
    use registry_types::{ExternalModerationOperation, ModerationDestination, ModerationMutation};
    use runtime_collab::external_moderation::{PROTOCOL, Request};
    use tilo_actions::remote::asked::{
        AskedDispatch, AskedFacts, AskedStore, Offer, RecordRef, Resolve,
    };
    use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};

    struct ExactLaunch {
        app: String,
        workspace: String,
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
        fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
            pid == std::process::id() && app == self.app && workspace == self.workspace
        }
    }

    struct Workspace {
        original: signed_worker::OriginalWorkspace,
        route: CrdtRoute,
        state: Arc<AppBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        /// The store the calls this app makes that wait for its person are kept in, as the
        /// device that carries the app's calls keeps them, and what admits one again.
        asked: Arc<AskedStore>,
        answering: Arc<Answering>,
        _owner_mailbox: crate::test_owner::writes::OwnerCommitMailbox,
    }

    /// What admits a call this app made again once its person answers the record the fixture
    /// keeps for them: this process's dispatch, as the device that carried the app's call.
    struct Answering {
        state: std::sync::Weak<AppBrokerState>,
        /// The connection's own admission, captured once the fixture exists: what admitting a
        /// call again carries, and what its effect goes on to do stays within.
        origin: std::sync::OnceLock<InvocationCaller>,
        outcome: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    }

    impl Answering {
        /// What admitting the answered call again came to, once it did.
        fn outcome(&self) -> Option<Result<(), String>> {
            self.outcome.lock().unwrap().clone()
        }
    }

    impl tilo_actions::remote::asked::AskedDispatch for Answering {
        fn context(&self, _: tilo_actions::InvocationId) -> serde_json::Value {
            serde_json::Value::Null
        }

        fn readmit(
            &self,
            caller: &Caller,
            request: InvokeRequest,
            _: &serde_json::Value,
        ) -> Result<(), ActionCallError> {
            let Some(state) = self.state.upgrade() else {
                return Err(ActionCallError::Unavailable {
                    reason: "the fixture's broker stopped".to_owned(),
                });
            };
            let written = Arc::clone(&self.outcome);
            let (caller, asking) = (caller.clone(), request);
            let origin = self.origin.get().cloned();
            tokio::task::spawn_local(async move {
                let Some(origin) = origin else {
                    *written.lock().unwrap() =
                        Some(Err("the fixture's origin is not named".to_owned()));
                    return;
                };
                // The same admission the connection's own provider makes of an app's call.
                let came = match state
                    .action_invoke_with_origin(&caller, None, None, Some(&origin), &asking)
                    .await
                {
                    Some(Ok(_)) => Ok(()),
                    Some(Err(error)) => Err(error.to_string()),
                    None => Err("the fixture's broker abandoned the call".to_owned()),
                };
                *written.lock().unwrap() = Some(came);
            });
            Ok(())
        }

        fn declined(
            &self,
            _: &Caller,
            _: tilo_actions::InvocationId,
        ) -> Result<(), ActionCallError> {
            Ok(())
        }
    }

    impl Workspace {
        fn new() -> Self {
            Self::with_document(None)
        }

        fn with_document(document: Option<Arc<dyn NativeDocumentReads>>) -> Self {
            let original = signed_worker::OriginalWorkspace::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            Self::with_original(document, original)
        }

        fn with_original(
            document: Option<Arc<dyn NativeDocumentReads>>,
            original: signed_worker::OriginalWorkspace,
        ) -> Self {
            let route = CrdtRoute::record("so.tilo.store", &original.route.cluster_id, "store");
            // This fixture replaces OriginalWorkspace's broker, not its journal.
            // Release that unused writer before opening the Store broker.
            drop(
                original
                    .state
                    .journal
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .take(),
            );
            let fanout = Arc::new(FanoutState::new());
            let mut services = original.membership.services();
            services.committed_documents = document;
            services.launcher = Some(Arc::new(ExactLaunch {
                app: route.app_id.clone(),
                workspace: route.cluster_id.clone(),
            }));
            let (owner_handle, owner_mailbox) =
                crate::test_owner::writes::OwnerCommitMailbox::bounded(8);
            let publisher = PeerPublisher::with_commits(Arc::new(owner_handle));
            let (state, bridge) = AppBrokerState::new_configured(
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
            let answering = Arc::new(Answering {
                state: Arc::downgrade(&state),
                origin: std::sync::OnceLock::new(),
                outcome: Arc::new(std::sync::Mutex::new(None)),
            });
            let asked = AskedStore::opened(None, None, Vec::new())
                .expect("the fixture keeps the calls that wait");
            let me = Self {
                original,
                route,
                state,
                directories: Rc::new(LocalActionDirectoryRegistry::new(bridge)),
                asked,
                answering,
                _owner_mailbox: owner_mailbox,
            };
            let (_, origin) = me.caller();
            let _ = me.answering.origin.set(origin);
            me
        }

        /// The person the app's calls ask.
        fn person(&self) -> Caller {
            Caller::local_person(hex::encode(self.original.cert.user_public_key))
                .in_workspace(&self.route.cluster_id)
        }

        /// The caller the app's own calls are made as: the person, through the app.
        fn app_caller(&self) -> Caller {
            Caller::local_app(self.route.app_id.clone())
                .acting_for(hex::encode(self.original.cert.user_public_key))
                .in_workspace(&self.route.cluster_id)
        }

        /// `request`, which the app made and which waits showing `facts`, kept as its asked
        /// record for the person to answer.
        fn waiting(&self, request: &InvokeRequest, facts: &AskedFacts) -> RecordRef {
            let answering: Arc<Answering> = Arc::clone(&self.answering);
            let dispatch: Arc<dyn AskedDispatch> = answering;
            let dispatch = Arc::downgrade(&dispatch);
            self.asked
                .for_dispatch("device", dispatch)
                .write_asked(&self.route.cluster_id, &self.app_caller(), request, facts)
                .expect("the fixture keeps the call for its person")
        }

        /// The person's `answer` to `record`: what admitting the call again came to.
        async fn answered(&self, record: &RecordRef, answer: Offer) -> Result<(), String> {
            self.asked
                .resolve(
                    record,
                    &self.person(),
                    &Resolve {
                        resolution: answer,
                        ..Resolve::default()
                    },
                )
                .await
                .ok_or_else(|| "the fixture's store stopped".to_owned())?
                .map_err(|error| error.to_string())?;
            // Admitting it again runs on the fixture's own task: read how it went once it did.
            tilo_test_env::within("the answered call is admitted again", async {
                loop {
                    if let Some(came) = self.answering.outcome() {
                        return came;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
        }

        fn caller(&self) -> (BrokerActor, InvocationCaller) {
            let admitted = verify_join_identity(
                &self.route,
                &signed_worker::presentation(
                    &self.route.cluster_id,
                    &self.original.cert,
                    &self.original.key,
                ),
                self.state.membership.as_ref(),
                None,
                self.state.certificate_authority.get().map(AsRef::as_ref),
            )
            .unwrap();
            (
                admitted.broker_actor(),
                InvocationCaller::Local {
                    route: self.route.clone(),
                    admitted: Box::new(admitted),
                    time: self.state.registered_policy_time().unwrap(),
                    pressed: false,
                },
            )
        }

        #[allow(
            clippy::indexing_slicing,
            reason = "test code included into the broker module: clippy does not see the cfg(test) on its include!, so it reads the test's checks as production code"
        )]
        fn invoke(&self, peer: u64, resource: &str, action: &str, argument: &str) -> u64 {
            let offered = self.state.publishers.surfaces.lock().unwrap()[&peer]
                .actions
                .iter()
                .find(|offered| offered.resource == resource && offered.action == action)
                .unwrap()
                .clone();
            let (actor, origin) = self.caller();
            let invocation = tilo_actions::InvocationId::new();
            let approval =
                crate::collab_app_broker::person_approved(&self.route.cluster_id, invocation);
            let token = self
                .state
                .confirmation_token(
                    peer,
                    &self.route.app_id,
                    &offered,
                    Some(argument),
                    &actor,
                    (Some(&approval), true),
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
                        requested: Some(invocation),
                        caller: None,
                        execution: None,
                        argument: Some(argument),
                        actor: &actor,
                        confirmation_token: token.as_deref(),
                        context: None,
                    },
                    Some(origin),
                )
                .unwrap()
                .start_blocking(&self.state)
                .unwrap();
            job.id
        }

        /// A connection the shell launched for the Store app, joined to the
        /// Store's record the way a packaged app joins its own route.
        async fn worker(&self, peer: u64) -> signed_worker::Connection {
            signed_worker::join_worker(
                &self.state,
                &self.directories,
                &self.route,
                &self.original.cert,
                peer,
            )
            .await
            .0
        }

        async fn launched(
            &self,
            peer: u64,
        ) -> (runtime_collab::CollabClient, signed_worker::Connection) {
            let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
            std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC)
                .unwrap();
            let handoff = serde_json::to_vec(&serde_json::json!({
                "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
                "app_id": self.route.app_id,
                "workspace_id": self.route.cluster_id,
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
                self.state.clone(),
                peer,
                server,
                client,
                None,
                self.directories.clone(),
            );
            let handle = opened(&rpc, &self.route.channel_key()).await.unwrap();
            (
                rpc,
                signed_worker::Connection {
                    handle,
                    server,
                    client,
                },
            )
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
            request_id: "ownership-report".into(),
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
        handle: &runtime_collab::RecordHandle,
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
    async fn external_moderation_rpc_confirmation_token_binds_payload_and_destination() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let workspace = Workspace::new();
                    let (_target_rpc, target) = workspace.launched(980).await;
                    let descriptor = descriptor(action);
                    target.handle.publish_actions(&[descriptor]).await.unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let offered = workspace.state.publishers.surfaces.lock().unwrap()[&980].actions
                        [0]
                    .clone();
                    let (actor, origin) = workspace.caller();
                    let operation = operation(action == "appeal");
                    let exact = argument(&operation);
                    let invocation = tilo_actions::InvocationId::new();
                    assert!(matches!(
                        workspace.state.confirmation_token(
                            980,
                            &workspace.route.app_id,
                            &offered,
                            Some(&exact),
                            &actor,
                            (None, false),
                        ),
                        // No token: its admission asks the person.
                        Ok(None)
                    ));
                    assert!(actor.direct(), "the app's own identity is a direct person");
                    assert!(matches!(
                        workspace.state.confirmation_token(
                            980,
                            &workspace.route.app_id,
                            &offered,
                            Some(&exact),
                            &actor,
                            (Some(&tilo_actions::remote::Confirmation::Operated), false),
                        ),
                        // No token: its admission asks the person.
                        Ok(None)
                    ));
                    for altered in ["missing", "payload", "destination"] {
                        let approval = crate::collab_app_broker::person_approved(
                            &workspace.route.cluster_id,
                            invocation,
                        );
                        let token = workspace
                            .state
                            .confirmation_token(
                                980,
                                &workspace.route.app_id,
                                &offered,
                                Some(&exact),
                                &actor,
                                (Some(&approval), true),
                            )
                            .unwrap()
                            .unwrap();
                        let mut changed = operation.clone();
                        match altered {
                            "destination" => {
                                changed.destination.base_url = "http://other.invalid".into();
                            }
                            "payload" => match &mut changed.mutation {
                                ModerationMutation::FileReport { summary, .. } => summary.push('!'),
                                ModerationMutation::PublisherAppeal { reason, .. } => {
                                    reason.push('!');
                                }
                                _ => unreachable!(),
                            },
                            "missing" => {}
                            _ => unreachable!(),
                        }
                        let changed = argument(&changed);
                        let result = workspace.state.start_invocation_confirmed_for_origin(
                            980,
                            &offered.resource,
                            action,
                            InvocationInputs {
                                original_request: None,
                                requested: Some(invocation),
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
                            .expect("unconfirmed or changed operation must be refused")
                            .to_string();
                        assert!(error.contains("confirmation"), "{altered}: {error}");
                    }
                    workspace.invoke(980, &offered.resource, action, &exact);
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
    async fn external_moderation_rpc_requires_approval_without_publisher_confirmation_policy() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let workspace = Workspace::new();
                    let (_target_rpc, target) = workspace.launched(980).await;
                    let mut descriptor = descriptor(action);
                    descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::None;
                    target.handle.publish_actions(&[descriptor]).await.unwrap();
                    let _stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let offered = workspace.state.publishers.surfaces.lock().unwrap()[&980].actions
                        [0]
                    .clone();
                    let (actor, _) = workspace.caller();
                    assert!(actor.direct());
                    let exact = argument(&operation(action == "appeal"));
                    let _invocation = tilo_actions::InvocationId::new();
                    assert!(matches!(
                        workspace.state.confirmation_token(
                            980,
                            &workspace.route.app_id,
                            &offered,
                            Some(&exact),
                            &actor,
                            (Some(&tilo_actions::remote::Confirmation::Operated), false),
                        ),
                        // No token: its admission asks the person.
                        Ok(None)
                    ));
                    // A generic internal/native-task approval is not evidence
                    // that the host showed this external disclosure.
                    let generic = workspace
                        .state
                        .mint_confirmation_grant(980, &offered, Some(&exact), &actor)
                        .unwrap();
                    for token in [None, Some(generic.as_str())] {
                        let result = workspace.state.start_invocation_confirmed_for_origin(
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
                                confirmation_token: token,
                                context: None,
                            },
                            Some(workspace.caller().1),
                        );
                        let error = result
                            .err()
                            .expect("Store disclosure always requires a host grant")
                            .to_string();
                        assert!(error.contains("confirmation"), "{error}");
                    }
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
        fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
            context.revalidate()
        }
        fn capture(
            &self,
            context: &NativeDocumentReadContext,
            _: Option<[u8; 32]>,
            maximum: usize,
        ) -> crate::collab_app_broker::ReadAnswering<'_, NativeDocumentRead> {
            Box::pin(std::future::ready(
                (|| -> Result<NativeDocumentRead, String> {
                    self.authorize(context)?;
                    if self.0.len() > maximum {
                        return Err("report exceeds bound".into());
                    }
                    Ok(NativeDocumentRead::Captured {
                        revision: *blake3::hash(&self.0).as_bytes(),
                        heads: Vec::new(),
                        body: Box::new(ReportDocument(self.0.clone())),
                    })
                })(),
            ))
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
                    let workspace = Workspace::with_document(Some(committed_report(note, None)));
                    let (_target_rpc, target) = workspace.launched(977).await;
                    let descriptor = descriptor("submit-report");
                    target.handle.publish_actions(&[descriptor]).await.unwrap();
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
                    workspace.invoke(
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
                    let workspace =
                        Workspace::with_document(Some(committed_report(note, Some("case-one"))));
                    let (_target_rpc, target) = workspace.launched(978).await;
                    let descriptor = descriptor("appeal");
                    target.handle.publish_actions(&[descriptor]).await.unwrap();
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
                    workspace.invoke(978, "workspace:atlas", "appeal", &argument(&operation));
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
                    let workspace = Workspace::new();
                    let (_target_rpc, target) = workspace.launched(976).await;
                    let descriptor = descriptor("submit-report");
                    target.handle.publish_actions(&[descriptor]).await.unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let mut operation = operation(false);
                    operation.destination.base_url = "https://custom.example".into();
                    workspace.invoke(976, "workspace:atlas", "submit-report", &argument(&operation));
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
                let workspace = Workspace::new();
                let (_target_rpc, target) = workspace.launched(975).await;
                let descriptor = descriptor("submit-report");
                target.handle.publish_actions(&[descriptor]).await.unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let mut operation = operation(false);
                operation.issued_at_unix = 1;
                operation.expires_at_unix = 100;
                workspace.invoke(
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

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_native_target_requires_original_channel_and_publication() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let workspace = Workspace::new();
                let (rpc, target) = workspace.launched(973).await;
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let wire = workspace.invoke(
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
                let foreign_route = CrdtRoute::record(
                    &workspace.route.app_id,
                    &workspace.route.cluster_id,
                    "foreign-store",
                );
                let foreign = opened(&rpc, &foreign_route.channel_key()).await.unwrap();
                // The foreign record is its own route hold, never handed this invocation.
                refused(
                    &foreign,
                    &request,
                    id,
                    "the connection was delivered no such invocation",
                )
                .await;
                let mut replacement = descriptor;
                replacement.resource = "replacement-store-resource".into();
                target.handle.publish_actions(&[replacement]).await.unwrap();
                refused(&target.handle, &request, id, "publication retired").await;
                workspace.state.remove_connection(973);
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
                let workspace = Workspace::new();
                let (_target_rpc, target) = workspace.launched(974).await;
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let wire = workspace.invoke(
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
                workspace
                    .original
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

    fn operation_for_other_action(action: &str) -> ModerationMutation {
        operation(action == "submit-report").mutation
    }

    /// The original caller cancels the invocation `wire` they made.
    fn cancel_as_original(workspace: &Workspace, wire: u64) {
        let (_, InvocationCaller::Local { admitted, .. }) = workspace.caller() else {
            unreachable!("the workspace's original caller is local")
        };
        let owner = InvocationOwner::AuthenticatedLocal {
            root: workspace.original.cert.user_public_key,
            device: workspace.original.cert.device_public_key,
            agent: admitted.agent_id.clone(),
        };
        assert!(workspace.state.can_cancel_for_owner(wire, &owner));
        let _ = workspace
            .state
            .request_cancellation_for_owner(wire, &owner)
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_binds_exact_operation_and_live_target_ownership() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let workspace = Workspace::new();
                    let (_target_rpc, target) = workspace.launched(971).await;
                    let (_other_rpc, other) = workspace.launched(972).await;
                    let descriptor = descriptor(action);
                    target
                        .handle
                        .publish_actions(std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let operation = operation(action == "appeal");
                    let wire =
                        workspace.invoke(971, &descriptor.resource, action, &argument(&operation));
                    let delivered = stream.read().await.unwrap();
                    assert_eq!(delivered.id, wire);
                    let id = delivered.invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    request.validate().unwrap();
                    // Admission reached the host worker's real effect, not a
                    // ownership check that refuses every request.
                    refused(&target.handle, &request, id, "HTTPS").await;
                    for field in ["destination", "owner", "mutation"] {
                        let mut changed = request.clone();
                        match field {
                            "destination" => changed.operation.destination.base_url.push('/'),
                            "owner" => {
                                changed.operation.destination.owner_pubkey_hex = "ef".repeat(32);
                            }
                            "mutation" => match &mut changed.operation.mutation {
                                ModerationMutation::FileReport { summary, .. } => summary.push('!'),
                                ModerationMutation::PublisherAppeal { reason, .. } => {
                                    reason.push('!');
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
                        "the connection was delivered no such invocation",
                    )
                    .await;
                    refused(
                        &other.handle,
                        &request,
                        id,
                        "the connection was delivered no such invocation",
                    )
                    .await;

                    let mut changed = descriptor.clone();
                    changed.contract.kind = tilo_actions::ActionKind::Read;
                    target.handle.publish_actions(&[changed]).await.unwrap();
                    refused(&target.handle, &request, id, "mismatch").await;
                    target
                        .handle
                        .publish_actions(std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "HTTPS").await;
                    cancel_as_original(&workspace, wire);
                    refused(&target.handle, &request, id, "retired or cancelled").await;
                    workspace.state.remove_connection(971);
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
                    let workspace = Workspace::new();
                    let (_target_rpc, target) = workspace.launched(979).await;
                    let mut descriptor = descriptor(action);
                    descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::None;
                    let resource = descriptor.resource.clone();
                    target.handle.publish_actions(&[descriptor]).await.unwrap();
                    let _stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let operation = operation(action == "appeal");
                    let serde_json::Value::Object(argument) =
                        serde_json::from_str(&argument(&operation)).unwrap()
                    else {
                        unreachable!("the argument is an object")
                    };
                    // Even with a broad Outside grant, the unconfirmed call
                    // fails at admission, before it can reach the host worker.
                    let handle = target.handle.clone();
                    let request = signed_worker::invoke_request(
                        "979",
                        &resource,
                        action,
                        Some(serde_json::Value::Object(argument)),
                    );
                    let mut asked = tokio::task::spawn_local(async move {
                        signed_worker::ask(
                            &handle,
                            tilo_actions::remote::RemoteActionMessage::Invoke(request),
                        )
                        .await
                    });
                    // The app's own directory answers on the broker's turn.
                    let answer = loop {
                        workspace.state.take_turn(&workspace.directories);
                        if asked.is_finished() {
                            break (&mut asked).await.unwrap().unwrap();
                        }
                        tokio::task::yield_now().await;
                    };
                    let receipt = signed_worker::receipt(answer);
                    assert!(
                        format!("{:?}", receipt.status).contains("Confirmation"),
                        "a broad grant cannot replace the trusted host answer: {receipt:?}"
                    );
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_holds_a_launched_app_to_the_same_execution_frontier() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let workspace = Workspace::new();
                let (_rpc, target) = workspace.launched(974).await;
                assert!(
                    workspace
                        .state
                        .host_workers
                        .lock()
                        .unwrap()
                        .get(974, &workspace.route)
                        .is_some_and(|admitted| admitted.worker_registration.is_some()),
                    "the launched Store is the registered worker of its own route"
                );
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let wire = workspace.invoke(
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
                cancel_as_original(&workspace, wire);
                refused(&target.handle, &request, id, "retired or cancelled").await;
                workspace.state.remove_connection(974);
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

    include!("external_moderation_https_tests.rs");
}
