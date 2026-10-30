/// What the app reports, as its SDK sends it on its channel.
async fn admit(
    channel: &runtime_collab::ChannelHandle,
    request: &crate::local_document_action::Request,
) -> Result<runtime_collab::PublishedInvocationObservation, runtime_collab::LocalDocumentActionError>
{
    let report: runtime_collab::LocalDocumentActionReport =
        serde_json::from_slice(&serde_json::to_vec(request).unwrap()).unwrap();
    channel.admit_local_document_action(&report).await
}

fn rejected(error: &runtime_collab::LocalDocumentActionError) -> bool {
    matches!(error, runtime_collab::LocalDocumentActionError::Rejected(_))
}

struct LocalReportGrants(WorkspaceTreeHandle);
impl crate::services::AppActionGrants for LocalReportGrants {
    fn may_grant(&self, _room: &str, _person: &str) -> bool {
        false
    }
    fn record(
        &self,
        _request: &crate::app_actions::AppActionConsentRequest,
        _grant: tilo_action_policy::Grant,
        _answer: crate::app_actions::AppActionConsent,
    ) -> crate::services::GrantRecording {
        Box::pin(std::future::ready(Err("nothing is granted here".into())))
    }
    fn revoke(&self, room: &str, grant: &str) {
        let mut tree = self.0.write();
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room().to_string() == room)
            .unwrap();
        assert!(workspace.revoke_grant(&tilo_action_policy::GrantId::new(grant)));
    }
}

struct LocalDocumentDirectory {
    kind: tilo_actions::OperationKind,
    read: bool,
    pin_dropped: Rc<std::cell::Cell<bool>>,
    resolutions: Rc<std::cell::Cell<usize>>,
}
impl wire::DirectorySource for LocalDocumentDirectory {
    fn query(
        &self,
        _query: runtime_collab::ActionDirectoryQuery,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryPage> {
        Box::pin(std::future::ready(Err("not listed here".into())))
    }

    fn resolve(
        &self,
        (revision, _, _): (&str, &str, &str),
        local_document: bool,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
        // The mutation has already advanced the live directory. Ordinary
        // invocation must not gain access to the retained pre-event revision.
        if !local_document {
            return Box::pin(std::future::ready(Ok(
                runtime_collab::ActionDirectoryResolution::StaleRevision,
            )));
        }
        self.resolutions.set(self.resolutions.get() + 1);
        if self.pin_dropped.get() || revision != "original-r1" {
            return Box::pin(std::future::ready(Ok(
                runtime_collab::ActionDirectoryResolution::StaleRevision,
            )));
        }
        let mut row = directory_state("original-r1").borrow().row();
        let descriptor = row.descriptor.as_mut().unwrap();
        descriptor.operation_kind = self.kind;
        descriptor.contract = if self.read {
            tilo_actions::ActionContract::view()
        } else {
            tilo_actions::ActionContract::default()
        };
        let visibility = row.visibility.as_mut().unwrap();
        visibility.governing_kind = Some(self.kind);
        visibility.contract = descriptor.contract.clone();
        Box::pin(std::future::ready(Ok(
            runtime_collab::ActionDirectoryResolution::Found {
                row: Box::new(row),
                revision: Some("original-r1".into()),
            },
        )))
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "test code included into the broker module: clippy does not see the cfg(test) on its include!, so it reads the test's checks as production code"
)]
async fn local_report_admission(
    kind: tilo_actions::OperationKind,
    mutation_hashes: Vec<[u8; 32]>,
    read: bool,
) {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let mut services = env.membership.services();
            services.app_action_grants = Some(Arc::new(LocalReportGrants(env.membership.clone())));
            let rig = Rig::with_services(&env, services).await;
            let pin_dropped = Rc::new(std::cell::Cell::new(false));
            let resolutions = Rc::new(std::cell::Cell::new(0));
            rig.worker
                .handle
                .publish_action_directory(wire_answerer(Rc::new(LocalDocumentDirectory {
                    kind,
                    read,
                    pin_dropped: pin_dropped.clone(),
                    resolutions: resolutions.clone(),
                })))
                .await
                .unwrap();
            let identity = rig
                .state
                .host_workers
                .lock()
                .unwrap()
                .only(903)
                .unwrap()
                .clone();
            let raw = wire_channel(CapnpChannelHandle {
                as_app: None,
                person_chat_views: std::cell::RefCell::default(),
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state: rig.state.clone(),
                directories: rig.directories.clone(),
                channel_id: env.route.channel_key(),
                peer_id: 903,
                connection_id: 903,
                verified_identity: identity.clone(),
                record_kind: None,
                route_access: RouteAccess::Own,
                lease: None,
                _hold: None,
            });
            let before_stale = receipt_barriers(&rig);
            no_delivery(&rig).await;
            let mut request = crate::local_document_action::Request {
                custody: Some(runtime_collab::committed_document::ConnectionSnapshot {
                    root: env.cert.user_public_key,
                    device: env.cert.device_public_key,
                    agent: identity.agent_id.clone(),
                    app_id: env.route.app_id.clone(),
                    room: env.route.cluster_id.clone(),
                    document: env.route.record_id.clone(),
                }),
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
            // Even a correctly rebound report cannot move historical work to
            // the current caller or another document. No policy grant is spent.
            for field in [
                "missing", "root", "device", "agent", "app", "room", "document",
            ] {
                let mut wrong = request.clone();
                if field == "missing" {
                    wrong.custody = None;
                } else {
                    let custody = wrong.custody.as_mut().unwrap();
                    match field {
                        "root" => custody.root = [91; 32],
                        "device" => custody.device = [92; 32],
                        "agent" => custody.agent = Some("other-agent".into()),
                        "app" => custody.app_id.push_str("-other"),
                        "room" => custody.room.push_str("-other"),
                        "document" => custody.document.push_str("-other"),
                        _ => unreachable!(),
                    }
                }
                wrong.invocation = wrong.bound_invocation().unwrap();
                assert_ne!(wrong.invocation, invocation, "{field}");
                let error = admit(&raw, &wrong)
                    .await
                    .expect_err("custody must be refused");
                assert!(rejected(&error), "{field}: {error}");
                assert_eq!(receipt_barriers(&rig).writes, before_stale.writes);
                no_delivery(&rig).await;
            }
            // A connection no launcher vouched for is no worker. Nothing
            // reported on it can be admitted, so the broker refuses
            // definitively: the app stops holding the report (and the
            // surface it was made under) instead of waiting forever.
            let app_connection = wire_channel(CapnpChannelHandle {
                as_app: None,
                person_chat_views: std::cell::RefCell::default(),
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state: rig.state.clone(),
                directories: rig.directories.clone(),
                channel_id: env.route.channel_key(),
                peer_id: 904,
                connection_id: 904,
                verified_identity: identity.clone(),
                record_kind: None,
                route_access: RouteAccess::Own,
                lease: None,
                _hold: None,
            });
            let error = admit(&app_connection, &request)
                .await
                .expect_err("a connection that is no host worker admits no report");
            assert!(
                rejected(&error)
                    && error
                        .to_string()
                        .contains(runtime_collab::LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING),
                "{error}"
            );
            assert_eq!(receipt_barriers(&rig).writes, before_stale.writes);
            no_delivery(&rig).await;
            drop(app_connection);
            let mut wrong_revision = request.clone();
            wrong_revision.revision = "not-retained".into();
            wrong_revision.invocation = wrong_revision.bound_invocation().unwrap();
            let error = admit(&raw, &wrong_revision)
                .await
                .expect_err("local admission also requires the exact retained revision");
            assert!(
                !rejected(&error),
                "unknown requests must retain reconciliation custody: {error}"
            );
            assert_eq!(receipt_barriers(&rig).writes, before_stale.writes);
            let once_grant = |id: &str| {
                let principal = tilo_action_policy::Subject::Person(principal_id_for_root(
                    env.cert.user_public_key,
                ));
                tilo_action_policy::Grant::new(
                    id,
                    principal.clone(),
                    env.membership.read().workspaces[0]
                        .node()
                        .app(env.route.app_id.clone()),
                    tilo_action_policy::KindSet::ALL,
                    tilo_action_policy::Answer::Allow,
                    principal,
                )
                .with_lifetime(tilo_action_policy::Lifetime::Once)
            };
            let first_grant = once_grant("first-report");
            let first_grant = env.membership.write().workspaces[0].grant(first_grant);
            let before = receipt_barriers(&rig);
            let reply = admit(&raw, &request).await;
            if (kind == tilo_actions::OperationKind::Effect && !read)
                || ((kind == tilo_actions::OperationKind::View || read)
                    && !request.mutation_hashes.is_empty())
            {
                let error = reply.expect_err("malformed non-document report must fail");
                assert!(
                    rejected(&error),
                    "definitive structural refusal must release SDK custody: {error}"
                );
                assert_eq!(receipt_barriers(&rig).writes, before.writes);
                assert!(rig.state.pending_invocations.lock().unwrap().is_empty());
                no_delivery(&rig).await;
                drop(raw);
                rig.stop().await;
                return;
            }
            let observation = reply.unwrap().result.expect("the report was found");
            assert!(
                observation
                    .terminal_json
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
            );
            let evidence: crate::action_evidence::AdmissionEvidence =
                serde_json::from_str(observation.admission_json.as_deref().unwrap()).unwrap();
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
                exact_intent["request"] = private_request;
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
            assert!(
                !env.membership.write().workspaces[0].revoke_grant(&first_grant),
                "first admission spends its one-use grant"
            );
            let next_grant = once_grant("next-report");
            let next_grant = env.membership.write().workspaces[0].grant(next_grant);
            // The SDK drops its pre-event pin after storing the first receipt.
            // Transport retries must observe the cache before consulting it.
            pin_dropped.set(true);
            let resolved_before_retry = resolutions.get();
            let replay = admit(&raw, &request).await.unwrap().result.unwrap();
            assert_eq!(resolutions.get(), resolved_before_retry);
            assert!(
                env.membership.write().workspaces[0].revoke_grant(&next_grant),
                "exact retry must not spend another one-use grant"
            );
            assert_eq!(replay.id, observation.id);
            assert_eq!(replay.admission_json, observation.admission_json);
            no_delivery(&rig).await;
            let mut substituted = request.clone();
            substituted.encoded.payload = b"null".to_vec();
            assert!(admit(&raw, &substituted).await.is_err());
            assert_eq!(receipt_barriers(&rig).writes, before.writes);
            // New content-bound captures still need a retained directory pin.
            substituted.invocation = substituted.bound_invocation().unwrap();
            assert!(admit(&raw, &substituted).await.is_err());
            substituted.invocation = request.invocation;
            pin_dropped.set(false);
            if kind == tilo_actions::OperationKind::Document && !request.mutation_hashes.is_empty()
            {
                // Sustained typing crosses the bounded host-cache horizon.
                // No report appends to the journal or performs a data barrier.
                let before_burst = receipt_barriers(&rig);
                for _ in 0..144 {
                    let mut next = request.clone();
                    next.capture_id = uuid::Uuid::new_v4();
                    next.invocation = next.bound_invocation().unwrap();
                    assert!(admit(&raw, &next).await.unwrap().result.is_some());
                }
                {
                    let receipts = rig.state.local_report_receipts.lock().unwrap();
                    assert_eq!(receipts.records.len(), 128);
                    assert!(receipts.bytes <= 4 * 1024 * 1024);
                }
                assert_eq!(receipt_barriers(&rig), before_burst);
                // A failed fresh lookup cannot prove that this evicted
                // report was never admitted; SDK custody must remain pending.
                pin_dropped.set(true);
                let error = admit(&raw, &request).await.err().unwrap();
                assert!(
                    !rejected(&error),
                    "replay lookup failure must not release an admitted capture"
                );
                pin_dropped.set(false);
                // An evicted exact retry reconstructs the same content-bound
                // evidence; it still cannot redeliver or produce a terminal.
                let replay = admit(&raw, &request).await.unwrap().result.unwrap();
                assert_eq!(replay.admission_json, observation.admission_json);
                assert!(
                    admit(&raw, &substituted).await.is_err(),
                    "eviction cannot permit content substitution"
                );
                assert_eq!(receipt_barriers(&rig), before_burst);
                no_delivery(&rig).await;
            }
            // Cached observations still consult current host custody, even if
            // the original connection and receipt are both retained.
            let original_host = env.authority.certificate.lock().unwrap().take();
            assert!(admit(&raw, &request).await.is_err());
            *env.authority.certificate.lock().unwrap() = original_host;
            // An old endpoint must not silently inherit a missing/replaced host.
            rig.state.host_workers.lock().unwrap().remove(903);
            pin_dropped.set(true);
            assert!(
                admit(&raw, &request).await.is_err(),
                "cached receipt requires live host custody"
            );
            substituted.capture_id = uuid::Uuid::new_v4();
            substituted.invocation = substituted.bound_invocation().unwrap();
            assert!(admit(&raw, &substituted).await.is_err());
            assert_eq!(receipt_barriers(&rig).writes, before.writes);
            no_delivery(&rig).await;
            drop(raw);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_report_admits_original_owner_without_delivery_or_execution() {
    local_report_admission(tilo_actions::OperationKind::Document, vec![[1; 32]], false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_noop_has_admission_but_never_execution_evidence() {
    local_report_admission(tilo_actions::OperationKind::Document, vec![], false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_read_only_view_has_admission_but_never_execution_evidence() {
    local_report_admission(tilo_actions::OperationKind::View, vec![], false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_view_cannot_report_document_mutations() {
    local_report_admission(tilo_actions::OperationKind::View, vec![[1; 32]], false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_effect_cannot_use_read_only_report_admission() {
    local_report_admission(tilo_actions::OperationKind::Effect, vec![], false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_read_has_signed_admission_but_never_execution_evidence() {
    local_report_admission(tilo_actions::OperationKind::Effect, vec![], true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_document_read_cannot_report_document_mutations() {
    local_report_admission(tilo_actions::OperationKind::Effect, vec![[1; 32]], true).await;
}
