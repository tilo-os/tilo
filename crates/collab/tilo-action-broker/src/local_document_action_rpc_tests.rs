struct LocalDocumentDirectory(tilo_actions::OperationKind);
impl action_directory::Server for LocalDocumentDirectory {
    fn resolve(
        self: Rc<Self>,
        params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        // The mutation has already advanced the live directory. Ordinary
        // invocation must not gain access to the retained pre-event revision.
        let _ = params;
        results.get().init_resolution().set_stale_revision(());
        Promise::ok(())
    }

    fn resolve_local_document_action(
        self: Rc<Self>,
        params: action_directory::ResolveLocalDocumentActionParams,
        mut results: action_directory::ResolveLocalDocumentActionResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        if request.get_revision().unwrap().to_str().unwrap() != "original-r1" {
            results.get().init_resolution().set_stale_revision(());
            return Promise::ok(());
        }
        let mut row = directory_state("original-r1").borrow().row();
        let descriptor = row.descriptor.as_mut().unwrap();
        descriptor.operation_kind = self.0;
        descriptor.contract = tilo_actions::ActionContract::default();
        let visibility = row.visibility.as_mut().unwrap();
        visibility.governing_kind = Some(self.0);
        visibility.contract = descriptor.contract.clone();
        let mut resolution = results.get().init_resolution();
        resolution.set_resolved_revision("original-r1");
        runtime_collab::write_action_directory_row(resolution.init_found(), &row).unwrap();
        Promise::ok(())
    }
}

async fn local_report_admission(kind: tilo_actions::OperationKind, mutation_hashes: Vec<[u8; 32]>) {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            rig.worker
                .handle
                .publish_action_directory(
                    &env.route.app_id,
                    capnp_rpc::new_client(LocalDocumentDirectory(kind)),
                )
                .await
                .unwrap();
            let identity = rig
                .state
                .host_workers
                .lock()
                .unwrap()
                .get(&903)
                .unwrap()
                .clone();
            let raw: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                person_chat_views: RefCell::default(),
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state: rig.state.clone(),
                directories: rig.directories.clone(),
                channel_id: env.route.channel_key(),
                peer_id: 903,
                verified_identity: identity,
                record_kind: None,
                lease: None,
            });
            let before_stale = receipt_barriers(&rig);
            assert!(matches!(
                rig.worker
                    .handle
                    .resolve_published_action(903, "original-r1", "workspace:atlas", "open",)
                    .await
                    .unwrap(),
                runtime_collab::ActionDirectoryResolution::StaleRevision
            ));
            let mut ordinary = raw.invoke_resolved_published_action_request();
            {
                let mut wire = ordinary.get();
                wire.set_peer_id(903);
                wire.set_revision("original-r1");
                wire.set_resource("workspace:atlas");
                wire.set_action("open");
                wire.set_confirmation_json("{}");
            }
            assert!(
                ordinary.send().promise.await.is_err(),
                "a retained pre-event revision must never authorize ordinary invocation"
            );
            assert_eq!(receipt_barriers(&rig).writes, before_stale.writes);
            no_delivery(&rig).await;
            let mut request = crate::local_document_action::Request {
                version: 1,
                invocation: uuid::Uuid::nil(),
                capture_id: uuid::Uuid::new_v4(),
                revision: "original-r1".into(),
                resource: "workspace:atlas".into(),
                action: "open".into(),
                argument: None,
                encoded: tilo_actions::EncodedAction {
                    type_id: "example.open".into(),
                    payload: br#"{"private":"executor-input"}"#.to_vec(),
                },
                context: Some(tilo_actions::ActionContext {
                    resource: Some(tilo_actions::ActionResource {
                        id: env.route.record_id.clone(),
                        kind: "parent-document".into(),
                        label: "Private parent".into(),
                    }),
                    source_invocation: Some(tilo_actions::InvocationId(uuid::Uuid::new_v4())),
                    purpose: Some("private original purpose".into()),
                    invoker: Some(tilo_actions::ActionInvoker::actor(
                        "app-claimed-other-person",
                    )),
                    ..tilo_actions::ActionContext::default()
                }),
                original_heads: vec![[0; 32]],
                mutation_hashes,
                group: None,
            };
            request.invocation = request.bound_invocation().unwrap();
            let invocation = request.invocation;
            let mut wrong_revision = request.clone();
            wrong_revision.revision = "not-retained".into();
            wrong_revision.invocation = wrong_revision.bound_invocation().unwrap();
            let mut stale = raw.admit_local_document_action_request();
            stale
                .get()
                .set_request(&serde_json::to_vec(&wrong_revision).unwrap());
            assert!(
                stale.send().promise.await.is_err(),
                "local admission also requires the exact retained revision"
            );
            assert_eq!(receipt_barriers(&rig).writes, before_stale.writes);
            let bytes = serde_json::to_vec(&request).unwrap();
            let before = receipt_barriers(&rig);
            let mut rpc = raw.admit_local_document_action_request();
            rpc.get().set_request(&bytes);
            let reply = rpc.send().promise.await;
            if kind == tilo_actions::OperationKind::Effect
                || (kind == tilo_actions::OperationKind::View
                    && !request.mutation_hashes.is_empty())
            {
                assert!(reply.is_err());
                assert_eq!(receipt_barriers(&rig).writes, before.writes);
                assert!(rig.state.pending_invocations.lock().unwrap().is_empty());
                no_delivery(&rig).await;
                drop(raw);
                rig.stop().await;
                return;
            }
            let reply = reply.unwrap();
            let observation = reply.get().unwrap().get_observation().unwrap();
            assert!(observation.get_found());
            assert!(
                observation
                    .get_terminal_json()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .is_empty()
            );
            let evidence: crate::action_evidence::AdmissionEvidence =
                serde_json::from_str(observation.get_admission_json().unwrap().to_str().unwrap())
                    .unwrap();
            let facts = &evidence.public_receipt().facts;
            assert_eq!(facts.invocation.0, invocation);
            assert_eq!(facts.app_id, env.route.app_id);
            assert_eq!(facts.record_id, env.route.record_id);
            assert_eq!(
                facts.actor,
                crate::action_evidence::VerifiedActor::AuthenticatedLocal {
                    root: env.cert.user_public_key,
                    device: env.cert.device_public_key,
                    agent: None,
                }
            );
            evidence.validate(facts).unwrap();
            let mut substituted_host = facts.clone();
            substituted_host.host = uuid::Uuid::new_v4();
            assert!(evidence.validate(&substituted_host).is_err());
            let public = serde_json::to_string(evidence.public_receipt()).unwrap();
            for private in [
                "executor-input",
                "private original purpose",
                "Private parent",
            ] {
                assert!(!public.contains(private));
            }
            {
                let journal = rig.state.journal.lock().unwrap();
                let journal = journal.as_ref().unwrap().as_ref().unwrap();
                let key = InvocationKey {
                    room: env.route.cluster_id.clone(),
                    host: journal.host(),
                    invocation: tilo_actions::InvocationId(invocation),
                    owner: InvocationOwner::AuthenticatedLocal {
                        root: env.cert.user_public_key,
                        device: env.cert.device_public_key,
                        agent: None,
                    },
                };
                assert!(
                    journal.lookup(&key).unwrap().is_none(),
                    "reports never enter the execution journal"
                );
                let receipts = rig.state.local_report_receipts.lock().unwrap();
                let record = receipts.lookup(&key).unwrap();
                let retained = &record.intent;
                assert_eq!(retained["record_id"], env.route.record_id);
                assert_eq!(retained["resource"], request.resource);
                assert!(
                    retained.get("request").is_none(),
                    "no plaintext request at rest"
                );
                let private_request = serde_json::json!({
                    "kind": "local-document-action-report-v1",
                    "claim": "semantic-admission-and-app-reported-input-and-hashes-not-execution",
                    "resolved_request": {
                        "app_id": env.route.app_id,
                        "record_id": env.route.record_id,
                        "resource": request.resource,
                        "action": request.action,
                        "argument": request.argument,
                        "original_request": null,
                    },
                    "report": request,
                    "typed_arguments": {},
                });
                assert!(record.admitted_request_is(&private_request));
                evidence.validate_retained(retained).unwrap();
                let mut exact_intent = retained.clone();
                exact_intent["request"] = private_request.clone();
                let opening = evidence.opening_for_owner(&facts.actor).unwrap();
                opening
                    .verify(evidence.public_receipt(), &exact_intent)
                    .unwrap();
                assert_ne!(facts.actor_id, "app-claimed-other-person");
                assert!(retained["context"].get("invoker").is_none());
                // Every reported byte and the original parent/child mapping is
                // private, committed intent, not replaceable receipt decoration.
                for field in ["encoded", "context", "original_heads", "mutation_hashes"] {
                    let mut changed = exact_intent.clone();
                    changed["request"]["report"][field] = serde_json::Value::Null;
                    assert!(!record.admitted_request_is(&changed["request"]));
                    assert!(opening.verify(evidence.public_receipt(), &changed).is_err());
                }
                for field in ["record_id", "resource", "actor"] {
                    let mut changed = retained.clone();
                    changed[field] = serde_json::json!("substitution");
                    assert!(evidence.validate_retained(&changed).is_err());
                }
                assert_eq!(record.admission, evidence);
            }
            assert!(rig.state.pending_invocations.lock().unwrap().is_empty());
            assert_eq!(receipt_barriers(&rig).writes, before.writes);
            no_delivery(&rig).await;
            let mut retry = raw.admit_local_document_action_request();
            retry.get().set_request(&bytes);
            let replay = retry.send().promise.await.unwrap();
            assert_eq!(
                replay.get().unwrap().get_observation().unwrap().get_id(),
                observation.get_id()
            );
            assert_eq!(
                replay
                    .get()
                    .unwrap()
                    .get_observation()
                    .unwrap()
                    .get_admission_json()
                    .unwrap(),
                observation.get_admission_json().unwrap()
            );
            no_delivery(&rig).await;
            let mut substituted = request.clone();
            substituted.encoded.payload = b"null".to_vec();
            let mut retry = raw.admit_local_document_action_request();
            retry
                .get()
                .set_request(&serde_json::to_vec(&substituted).unwrap());
            assert!(retry.send().promise.await.is_err());
            assert_eq!(receipt_barriers(&rig).writes, before.writes);
            if kind == tilo_actions::OperationKind::Document && !request.mutation_hashes.is_empty()
            {
                // Sustained typing crosses the bounded host-cache horizon.
                // No report appends to the journal or performs a data barrier.
                let before_burst = receipt_barriers(&rig);
                for _ in 0..144 {
                    let mut next = request.clone();
                    next.capture_id = uuid::Uuid::new_v4();
                    next.invocation = next.bound_invocation().unwrap();
                    let mut rpc = raw.admit_local_document_action_request();
                    rpc.get().set_request(&serde_json::to_vec(&next).unwrap());
                    assert!(
                        rpc.send()
                            .promise
                            .await
                            .unwrap()
                            .get()
                            .unwrap()
                            .get_observation()
                            .unwrap()
                            .get_found()
                    );
                }
                {
                    let receipts = rig.state.local_report_receipts.lock().unwrap();
                    assert_eq!(receipts.records.len(), 128);
                    assert!(receipts.bytes <= 4 * 1024 * 1024);
                }
                assert_eq!(receipt_barriers(&rig), before_burst);
                // An evicted exact retry reconstructs the same content-bound
                // evidence; it still cannot redeliver or produce a terminal.
                let mut retry = raw.admit_local_document_action_request();
                retry.get().set_request(&bytes);
                let replay = retry.send().promise.await.unwrap();
                assert_eq!(
                    replay
                        .get()
                        .unwrap()
                        .get_observation()
                        .unwrap()
                        .get_admission_json()
                        .unwrap(),
                    observation.get_admission_json().unwrap()
                );
                let mut retry = raw.admit_local_document_action_request();
                retry
                    .get()
                    .set_request(&serde_json::to_vec(&substituted).unwrap());
                assert!(
                    retry.send().promise.await.is_err(),
                    "eviction cannot permit content substitution"
                );
                assert_eq!(receipt_barriers(&rig), before_burst);
                no_delivery(&rig).await;
            }
            // An old endpoint must not silently inherit a missing/replaced host.
            rig.state.host_workers.lock().unwrap().remove(&903);
            substituted.capture_id = uuid::Uuid::new_v4();
            substituted.invocation = substituted.bound_invocation().unwrap();
            let mut stale = raw.admit_local_document_action_request();
            stale
                .get()
                .set_request(&serde_json::to_vec(&substituted).unwrap());
            assert!(stale.send().promise.await.is_err());
            assert_eq!(receipt_barriers(&rig).writes, before.writes);
            no_delivery(&rig).await;
            drop(raw);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_report_admits_original_owner_without_delivery_or_execution() {
    local_report_admission(tilo_actions::OperationKind::Document, vec![[1; 32]]).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_noop_has_admission_but_never_execution_evidence() {
    local_report_admission(tilo_actions::OperationKind::Document, vec![]).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_read_only_view_has_admission_but_never_execution_evidence() {
    local_report_admission(tilo_actions::OperationKind::View, vec![]).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_view_cannot_report_document_mutations() {
    local_report_admission(tilo_actions::OperationKind::View, vec![[1; 32]]).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_effect_cannot_use_read_only_report_admission() {
    local_report_admission(tilo_actions::OperationKind::Effect, vec![]).await;
}
