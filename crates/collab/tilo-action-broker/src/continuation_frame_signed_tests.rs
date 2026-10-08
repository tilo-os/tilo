// A coherent test directory using the existing fixture state and protocol
// serializers; query and resolution describe the same declared effect.
struct ContinuationDirectory {
    state: Rc<RefCell<super::TestActionDirectory>>,
}
impl ContinuationDirectory {
    fn row(&self) -> runtime_collab::ActionDirectoryRow {
        let mut row = self.state.borrow().row();
        let descriptor = continuation_descriptor();
        row.visibility_contract_json
            .clone_from(&descriptor.contract_json);
        row.descriptor = Some(descriptor);
        row
    }
}
impl crate::collab_broker_capnp::action_directory::Server for ContinuationDirectory {
    fn query(
        self: Rc<Self>,
        _: crate::collab_broker_capnp::action_directory::QueryParams,
        mut results: crate::collab_broker_capnp::action_directory::QueryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let count = self.state.borrow().query_count.saturating_add(1);
        self.state.borrow_mut().query_count = count;
        let page = runtime_collab::ActionDirectoryPage {
            revision: self.state.borrow().revision.clone(),
            rows: vec![self.row()],
            next_cursor: Vec::new(),
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        };
        super::write_directory_page(results.get().init_page(), &page).unwrap();
        capnp::capability::Promise::ok(())
    }
    fn resolve(
        self: Rc<Self>,
        params: crate::collab_broker_capnp::action_directory::ResolveParams,
        mut results: crate::collab_broker_capnp::action_directory::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let revision = params
            .get()
            .unwrap()
            .get_revision()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let count = self.state.borrow().resolve_count.saturating_add(1);
        self.state.borrow_mut().resolve_count = count;
        if self.state.borrow().revision == revision {
            super::write_directory_row(results.get().init_resolution().init_found(), &self.row());
        } else {
            results.get().init_resolution().set_stale_revision(());
        }
        capnp::capability::Promise::ok(())
    }
}

// Real signed caller, private target RPC, SDK authorizer and owner journal.
fn continuation_descriptor() -> runtime_collab::ExportedAction {
    let mut descriptor = directory_state("original-r1")
        .borrow()
        .row()
        .descriptor
        .unwrap();
    descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
        kind: descriptor.resource_kind.clone(),
        key: "workspace:atlas".into(),
    });
    descriptor.operation_kind = "effect".into();
    descriptor.contract_json = serde_json::to_string(&tilo_actions::ActionContract {
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Job,
        ..tilo_actions::ActionContract::default()
    })
    .unwrap();
    descriptor
}

async fn continuation_parent(
    env: &Environment,
    rig: &Rig,
) -> (
    tilo_actions::remote::Caller,
    Vec<u8>,
    tilo_actions::remote::InvokeRequest,
) {
    rig.worker
        .handle
        .publish_action_directory(
            &env.route.app_id,
            capnp_rpc::new_client(ContinuationDirectory {
                state: directory_state("original-r1"),
            }),
        )
        .await
        .unwrap();
    let descriptor = continuation_descriptor();
    rig.worker
        .handle
        .publish_actions(&env.route.app_id, &[descriptor])
        .await
        .unwrap();
    let (caller, evidence) = local_host_evidence(env);
    let facade = crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
    let host = facade
        .certified_local_action_host(&caller, &evidence)
        .unwrap();
    let request = local_host_request(false);
    local_host_answer(host.invoke(&caller, request.clone()))
        .await
        .unwrap();
    let lease = local_host_lease(rig, &caller, request.invocation).await;
    lease.check().unwrap();
    (caller, evidence, request)
}

fn continuation_query(parent: tilo_actions::InvocationId) -> Vec<u8> {
    tilo_actions::remote::RemoteActionFrame::continuing(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 73,
            query: tilo_actions::remote::CatalogueQuery {
                provider: Some("903".into()),
                ..tilo_actions::remote::CatalogueQuery::default()
            },
        },
        parent,
    )
    .unwrap()
    .encode()
    .unwrap()
}

async fn continuation_round_trip(rig: &Rig, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let request = rig.worker.handle.remote_action(bytes);
    tokio::pin!(request);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            rig.state.drain_action_directory_bridge(&rig.directories);
            tokio::select! {
                result = &mut request => return result.map_err(|error| error.to_string()),
                () = tokio::task::yield_now() => {}
            }
        }
    })
    .await
    .expect("production broker owner turn must produce a bounded response")
}

async fn continuation_query_page(
    rig: &Rig,
    parent: tilo_actions::InvocationId,
) -> Result<tilo_actions::remote::CataloguePage, String> {
    let bytes = continuation_round_trip(rig, &continuation_query(parent)).await?;
    let response = tilo_actions::remote::RemoteActionFrame::decode(&bytes)
        .unwrap()
        .message;
    assert!(
        matches!(
            &response,
            tilo_actions::remote::RemoteActionMessage::CatalogueResponse {
                correlation: 73,
                ..
            }
        ),
        "unexpected catalogue response: {response:?}"
    );
    match response {
        tilo_actions::remote::RemoteActionMessage::CatalogueResponse {
            correlation: 73,
            result,
        } => result.map_err(|error| format!("{error:?}")),
        _ => Err("unmatched catalogue response".into()),
    }
}

async fn continuation_answer(
    rig: &Rig,
    message: tilo_actions::remote::RemoteActionMessage,
    parent: tilo_actions::InvocationId,
) -> Result<tilo_actions::remote::RemoteActionMessage, String> {
    let frame = tilo_actions::remote::RemoteActionFrame::continuing(message, parent).unwrap();
    let bytes = continuation_round_trip(rig, &frame.encode().unwrap()).await?;
    Ok(tilo_actions::remote::RemoteActionFrame::decode(&bytes)
        .unwrap()
        .message)
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_parent_cancellation_requests_its_actual_owned_child_to_stop() {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
            let mut child = local_host_request(false);
            child.on_behalf_of = Some(parent.invocation);
            let bytes = continuation_round_trip(
                &rig,
                &RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone()))
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap();
            assert!(matches!(RemoteActionFrame::decode(&bytes).unwrap().message,
                RemoteActionMessage::Receipt(receipt) if receipt.invocation == child.invocation
                    && matches!(receipt.status, ReceiptStatus::Accepted)));
            let child_wire = rig.state.remote_job(&caller, &child.invocation).unwrap();
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            host.cancel(&caller, &parent.invocation).unwrap();
            // This asserts the existing broker's cooperative request, not guest
            // termination, an acknowledgement, or successful cleanup.
            let requested = rig.state.cancellation_requests.lock().unwrap().contains(&child_wire);
            assert!(rig.worker.handle.invocation_cancellation(child_wire).await.unwrap(),
                "the admitted target RPC delivers the child cancellation request");
            rig.stop().await;
            assert!(requested, "cancelling the original parent must request its admitted child to stop before original custody retires");
        })
        .await;
}

struct HeldContinuationChildDirectory {
    started: Rc<std::cell::Cell<bool>>,
    release: Rc<tokio::sync::Notify>,
}

impl crate::collab_broker_capnp::action_directory::Server for HeldContinuationChildDirectory {
    fn resolve(
        self: Rc<Self>,
        params: crate::collab_broker_capnp::action_directory::ResolveParams,
        mut results: crate::collab_broker_capnp::action_directory::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let action = params
            .get()
            .unwrap()
            .get_action()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        capnp::capability::Promise::from_future(async move {
            if action == "child" {
                self.started.set(true);
                self.release.notified().await;
            }
            let mut row = directory_state("original-r1").borrow().row();
            let mut descriptor = continuation_descriptor();
            descriptor.action = action;
            row.action = Some(descriptor.action.clone());
            row.visibility_action = Some(descriptor.action.clone());
            row.visibility_contract_json
                .clone_from(&descriptor.contract_json);
            row.descriptor = Some(descriptor);
            super::write_directory_row(results.get().init_resolution().init_found(), &row);
            Ok(())
        })
    }
}

async fn continuation_retire_parent(
    rig: &Rig,
    caller: &tilo_actions::remote::Caller,
    evidence: &[u8],
    invocation: tilo_actions::InvocationId,
    retirement: &str,
) {
    let facade = crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
    let host = facade
        .certified_local_action_host(caller, evidence)
        .unwrap();
    let parent_wire = rig.state.remote_job(caller, &invocation).unwrap();
    match retirement {
        "cancel" => host.cancel(caller, &invocation).unwrap(),
        "terminal" => {
            let terminal = tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            rig.worker
                .handle
                .update_invocation(parent_wire, &serde_json::to_string(&terminal).unwrap())
                .await
                .unwrap();
        }
        "replace" => {
            let mut registry = rig.state.pending_invocations.lock().unwrap();
            let retained = registry.get_mut(&parent_wire).unwrap();
            // Fault injection preserves every wire/payload identity while
            // replacing the actual broker-held original data allocation.
            retained.original = retained
                .original
                .as_ref()
                .map(|data| Arc::new((**data).clone()));
        }
        "disconnect" => rig.state.remove_connection(903),
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_cancelled_parent_cannot_admit_child_after_directory_answer() {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    for retirement in ["cancel", "terminal", "replace", "disconnect"] {
        tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
            let started = Rc::new(std::cell::Cell::new(false));
            let release = Rc::new(tokio::sync::Notify::new());
            rig.worker.handle.publish_action_directory(
                &env.route.app_id,
                capnp_rpc::new_client(HeldContinuationChildDirectory {
                    started: started.clone(), release: release.clone(),
                }),
            ).await.unwrap();
            let mut child = local_host_request(true);
            child.target.address.action = "child".into();
            child.on_behalf_of = Some(parent.invocation);
            let bytes = RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone())).encode().unwrap();
            let handle = rig.worker.handle.clone();
            let request = handle.remote_action(&bytes);
            tokio::pin!(request);
            tokio::time::timeout(Duration::from_secs(2), async {
                while !started.get() {
                    rig.state.drain_action_directory_bridge(&rig.directories);
                    tokio::select! {
                        answer = &mut request => panic!("child answered before held directory: {answer:?}"),
                        () = tokio::task::yield_now() => {},
                    }
                }
            }).await.unwrap();
            continuation_retire_parent(&rig, &caller, &evidence, parent.invocation, retirement).await;
            let journal_frontier = rig.state.journal.lock().unwrap().as_ref().unwrap()
                .as_ref().unwrap().next_wire_id();
            release.notify_one();
            let answer = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    rig.state.drain_action_directory_bridge(&rig.directories);
                    tokio::select! {
                        answer = &mut request => break answer,
                        () = tokio::task::yield_now() => {},
                    }
                }
            }).await.unwrap();
            let child_admitted = rig.state.pending_invocations.lock().unwrap().values()
                .any(|entry| entry.update.invocation() == child.invocation);
            assert_eq!(rig.state.journal.lock().unwrap().as_ref().unwrap()
                .as_ref().unwrap().next_wire_id(), journal_frontier,
                "refused late child must not leave a durable ghost admission");
            rig.stop().await;
            assert!(!child_admitted, "response refusal alone must not conceal an admitted child after original cancellation: {answer:?}");
            let refused = match answer {
                Err(_) => true,
                Ok(bytes) => matches!(RemoteActionFrame::decode(&bytes).unwrap().message,
                    RemoteActionMessage::Receipt(receipt)
                        if receipt.invocation == child.invocation
                            && matches!(receipt.status, tilo_actions::remote::ReceiptStatus::Rejected { .. })),
            };
            assert!(refused, "cancelled continuation must refuse admission through the original protocol");
        }).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_frame_actual_child_lifecycle_never_borrows_another_valid_parent() {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
            let bytes = continuation_round_trip(&rig, &continuation_query(parent.invocation))
                .await
                .unwrap();
            let response = RemoteActionFrame::decode(&bytes).unwrap().message;
            assert!(
                matches!(
                    &response,
                    RemoteActionMessage::CatalogueResponse {
                        correlation: 73,
                        result: Ok(_)
                    }
                ),
                "catalogue response: {response:?}"
            );
            let mut child = local_host_request(false);
            child.on_behalf_of = Some(parent.invocation);
            let bytes = continuation_round_trip(
                &rig,
                &RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone()))
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap();
            assert!(matches!(RemoteActionFrame::decode(&bytes).unwrap().message,
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == child.invocation
                && matches!(receipt.status, ReceiptStatus::Accepted)));
            assert!(matches!(
                continuation_answer(
                    &rig,
                    RemoteActionMessage::Observe {
                        invocation: child.invocation
                    },
                    parent.invocation
                )
                .await
                .unwrap(),
                RemoteActionMessage::Update { state: Some(_), .. }
            ));
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let unrelated = local_host_request(false);
            local_host_answer(host.invoke(&caller, unrelated.clone()))
                .await
                .unwrap();
            assert!(
                continuation_answer(
                    &rig,
                    RemoteActionMessage::Observe {
                        invocation: child.invocation
                    },
                    unrelated.invocation
                )
                .await
                .is_err(),
                "same caller and worker with another genuine parent cannot observe this child"
            );
            assert!(
                continuation_answer(
                    &rig,
                    RemoteActionMessage::Cancel {
                        invocation: child.invocation
                    },
                    unrelated.invocation
                )
                .await
                .is_err()
            );
            assert!(
                continuation_answer(
                    &rig,
                    RemoteActionMessage::Observe {
                        invocation: tilo_actions::InvocationId::new()
                    },
                    parent.invocation
                )
                .await
                .is_err()
            );
            assert!(matches!(
                continuation_answer(
                    &rig,
                    RemoteActionMessage::Cancel {
                        invocation: child.invocation
                    },
                    parent.invocation
                )
                .await
                .unwrap(),
                RemoteActionMessage::CancelResponse { result: Ok(()), .. }
            ));
            host.cancel(&caller, &parent.invocation).unwrap();
            assert!(
                continuation_round_trip(&rig, &continuation_query(parent.invocation))
                    .await
                    .is_err(),
                "cancellation retires original permission even while the pending entry is retained"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_frame_revoked_caller_and_replaced_publication_refuse_before_query() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (_caller, _evidence, parent) = continuation_parent(&env, &rig).await;
            assert!(
                continuation_query_page(&rig, parent.invocation)
                    .await
                    .is_ok()
            );
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[])
                .await
                .unwrap();
            assert!(
                continuation_round_trip(&rig, &continuation_query(parent.invocation))
                    .await
                    .is_err(),
                "retained original UUID cannot revive an unpublished resource"
            );
            let (_, _, replaced) = continuation_parent(&env, &rig).await;
            continuation_query_page(&rig, replaced.invocation)
                .await
                .unwrap();
            let mut changed = continuation_descriptor();
            changed.semantic_identity.as_mut().unwrap().key = "workspace:another".into();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[changed])
                .await
                .unwrap();
            assert!(
                continuation_query_page(&rig, replaced.invocation)
                    .await
                    .is_err(),
                "matching action UUID with a different current resource identity must refuse"
            );
            let (_, _, fresh) = continuation_parent(&env, &rig).await;
            assert!(
                continuation_query_page(&rig, fresh.invocation)
                    .await
                    .is_ok()
            );
            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .clear();
            assert!(
                continuation_round_trip(&rig, &continuation_query(fresh.invocation))
                    .await
                    .is_err(),
                "a live target connection cannot substitute for revoked original caller custody"
            );
            assert!(
                continuation_round_trip(
                    &rig,
                    &continuation_query(tilo_actions::InvocationId::new())
                )
                .await
                .is_err()
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_frame_lazy_revision_and_terminal_original_refuse_before_child_discovery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (_caller, _evidence, eager) = continuation_parent(&env, &rig).await;
            let state = directory_state("original-r1");
            rig.worker
                .handle
                .publish_action_directory(
                    &env.route.app_id,
                    capnp_rpc::new_client(ContinuationDirectory {
                        state: state.clone(),
                    }),
                )
                .await
                .unwrap();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[])
                .await
                .unwrap();
            let (caller, evidence) = local_host_evidence(&env);
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let parent = local_host_request(true);
            let pending = host.invoke(&caller, parent.clone());
            rig.state.drain_action_directory_bridge(&rig.directories);
            local_host_answer(pending).await.unwrap();
            local_host_lease(&rig, &caller, parent.invocation)
                .await
                .check()
                .unwrap();
            assert_eq!(
                state.borrow().resolve_count,
                1,
                "original admission actually selected directory resolution"
            );
            let page = continuation_query_page(&rig, parent.invocation)
                .await
                .unwrap();
            assert_eq!(page.rows.len(), 1);
            let queries = state.borrow().query_count;
            let resolutions = state.borrow().resolve_count;
            state.borrow_mut().revision = "changed-r2".into();
            assert!(
                continuation_query_page(&rig, parent.invocation)
                    .await
                    .is_err(),
                "an old accepted parent cannot resolve against a changed directory revision"
            );
            assert!(state.borrow().resolve_count > resolutions);
            assert_eq!(
                state.borrow().query_count,
                queries,
                "refused before querying child discovery"
            );
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[continuation_descriptor()])
                .await
                .unwrap();
            continuation_query_page(&rig, eager.invocation)
                .await
                .unwrap();
            let wire = rig
                .state
                .pending_invocations
                .lock()
                .unwrap()
                .iter()
                .find_map(|(wire, entry)| {
                    (entry.update.invocation() == eager.invocation).then_some(*wire)
                })
                .unwrap();
            let terminal = tilo_actions::InvocationUpdate::Succeeded {
                invocation: eager.invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            rig.worker
                .handle
                .update_invocation(wire, &serde_json::to_string(&terminal).unwrap())
                .await
                .unwrap();
            assert!(
                continuation_query_page(&rig, eager.invocation)
                    .await
                    .is_err(),
                "a genuine terminal original invocation cannot regain continuation authority"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_frame_legacy_invoke_rechecks_cancelled_original_before_dispatch() {
    use tilo_actions::remote::{
        ActionHostError, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            host.cancel(&caller, &parent.invocation).unwrap();
            let count = rig.state.pending_invocations.lock().unwrap().len();
            let mut child = local_host_request(false);
            child.on_behalf_of = Some(parent.invocation);
            let bytes = RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone()))
                .encode()
                .unwrap();
            let answer = continuation_round_trip(&rig, &bytes).await.unwrap();
            assert!(
                matches!(RemoteActionFrame::decode(&answer).unwrap().message,
            RemoteActionMessage::Receipt(receipt)
            if receipt.invocation == child.invocation && matches!(receipt.status,
                ReceiptStatus::Rejected { error: ActionHostError::NotPermitted }))
            );
            assert_eq!(
                rig.state.pending_invocations.lock().unwrap().len(),
                count,
                "refused original must not dispatch another child"
            );
            rig.stop().await;
        })
        .await;
}

async fn continuation_admit_child(
    rig: &Rig,
    parent: tilo_actions::InvocationId,
    action: &str,
) -> tilo_actions::remote::InvokeRequest {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
    let mut child = local_host_request(false);
    child.target.address.action = action.into();
    child.on_behalf_of = Some(parent);
    let bytes = continuation_round_trip(
        rig,
        &RemoteActionFrame::new(RemoteActionMessage::Invoke(child.clone()))
            .encode()
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(RemoteActionFrame::decode(&bytes).unwrap().message,
        RemoteActionMessage::Receipt(receipt) if receipt.invocation == child.invocation
            && matches!(receipt.status, ReceiptStatus::Accepted)));
    child
}

async fn continuation_other_device_job(
    env: &Environment,
    rig: &Rig,
    caller: &tilo_actions::remote::Caller,
) -> u64 {
    let facade = crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
    let (other_device, other_evidence) = member_device_evidence(env, 94, false);
    assert_eq!(other_device.user_public_key(), caller.user_public_key());
    assert_ne!(other_device.device_public_key(), caller.device_public_key());
    let other_host = facade
        .certified_local_action_host(&other_device, &other_evidence)
        .unwrap();
    let other = local_host_request(false);
    local_host_answer(other_host.invoke(&other_device, other.clone()))
        .await
        .unwrap();
    rig.state
        .remote_job(&other_device, &other.invocation)
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_cancel_traverses_non_cancellable_child_and_fences_its_future_children() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
            let mut noncancellable = continuation_descriptor();
            noncancellable.action = "noncancellable".into();
            noncancellable.contract_json = serde_json::to_string(&tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: false },
                result: tilo_actions::ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            })
            .unwrap();
            rig.worker
                .handle
                .publish_actions(
                    &env.route.app_id,
                    &[continuation_descriptor(), noncancellable],
                )
                .await
                .unwrap();
            let child = continuation_admit_child(&rig, parent.invocation, "noncancellable").await;
            let grandchild = continuation_admit_child(&rig, child.invocation, "open").await;
            let child_wire = rig.state.remote_job(&caller, &child.invocation).unwrap();
            let grandchild_wire = rig
                .state
                .remote_job(&caller, &grandchild.invocation)
                .unwrap();
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let unrelated = local_host_request(false);
            local_host_answer(host.invoke(&caller, unrelated.clone()))
                .await
                .unwrap();
            let unrelated_wire = rig
                .state
                .remote_job(&caller, &unrelated.invocation)
                .unwrap();
            let other_wire = continuation_other_device_job(&env, &rig, &caller).await;
            host.cancel(&caller, &parent.invocation).unwrap();
            {
                let registry = rig.state.pending_invocations.lock().unwrap();
                assert!(
                    registry
                        .get(&other_wire)
                        .unwrap()
                        .cancellation_requested_at
                        .is_none()
                );
                assert!(
                    registry
                        .get(&child_wire)
                        .unwrap()
                        .cancellation_requested_at
                        .is_none()
                );
                assert!(
                    registry
                        .get(&grandchild_wire)
                        .unwrap()
                        .cancellation_requested_at
                        .is_some()
                );
                assert!(
                    registry
                        .get(&unrelated_wire)
                        .unwrap()
                        .cancellation_requested_at
                        .is_none()
                );
            }
            let count = rig.state.pending_invocations.lock().unwrap().len();
            let mut refused = local_host_request(false);
            refused.on_behalf_of = Some(child.invocation);
            use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
            let bytes = continuation_round_trip(
                &rig,
                &RemoteActionFrame::new(RemoteActionMessage::Invoke(refused.clone()))
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap();
            assert!(matches!(RemoteActionFrame::decode(&bytes).unwrap().message,
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == refused.invocation
                && matches!(receipt.status, ReceiptStatus::Rejected { .. })));
            assert_eq!(rig.state.pending_invocations.lock().unwrap().len(), count);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_parent_terminal_requests_only_active_owned_children() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, _, parent) = continuation_parent(&env, &rig).await;
            let active = continuation_admit_child(&rig, parent.invocation, "open").await;
            let completed = continuation_admit_child(&rig, parent.invocation, "open").await;
            let active_wire = rig.state.remote_job(&caller, &active.invocation).unwrap();
            let completed_wire = rig
                .state
                .remote_job(&caller, &completed.invocation)
                .unwrap();
            let parent_wire = rig.state.remote_job(&caller, &parent.invocation).unwrap();
            let terminal = |invocation| tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            rig.worker
                .handle
                .update_invocation(
                    completed_wire,
                    &serde_json::to_string(&terminal(completed.invocation)).unwrap(),
                )
                .await
                .unwrap();
            rig.worker
                .handle
                .update_invocation(
                    parent_wire,
                    &serde_json::to_string(&terminal(parent.invocation)).unwrap(),
                )
                .await
                .unwrap();
            {
                let requests = rig.state.cancellation_requests.lock().unwrap();
                assert!(requests.contains(&active_wire));
                assert!(!requests.contains(&completed_wire));
            }
            assert!(
                rig.state
                    .pending_invocations
                    .lock()
                    .unwrap()
                    .get(&parent_wire)
                    .is_none()
            );
            assert!(matches!(
                rig.state
                    .action_observe(&caller, &completed.invocation)
                    .unwrap()
                    .update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            rig.stop().await;
        })
        .await;
}

fn continuation_inject_child_cancellation_failure(rig: &Rig, child: u64, failure: &str) {
    match failure {
        "journal" => {
            let mut registry = rig.state.pending_invocations.lock().unwrap();
            // Break only the child's journal identity. The production journal's
            // exact-key cancellation lookup must refuse while parent persistence
            // remains independently available.
            registry
                .get_mut(&child)
                .unwrap()
                .journal_key
                .as_mut()
                .unwrap()
                .invocation = tilo_actions::InvocationId::new();
        }
        "queue" => {
            let state = rig.state.clone();
            assert!(
                std::thread::spawn(move || {
                    let _held = state.cancellation_requests.lock().unwrap();
                    std::panic::resume_unwind(Box::new(
                        "inject cancellation request queue failure",
                    ));
                })
                .join()
                .is_err()
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_child_delivery_failure_preserves_parent_terminal() {
    for failure in ["journal", "queue"] {
        for terminal in ["typed", "legacy"] {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let env = Environment::new();
                    let rig = Rig::new(&env).await;
                    let (caller, _, parent) = continuation_parent(&env, &rig).await;
                    let child = continuation_admit_child(&rig, parent.invocation, "open").await;
                    let parent_wire = rig.state.remote_job(&caller, &parent.invocation).unwrap();
                    let child_wire = rig.state.remote_job(&caller, &child.invocation).unwrap();
                    continuation_inject_child_cancellation_failure(&rig, child_wire, failure);
                    let observed = tilo_actions::InvocationUpdate::Succeeded {
                        invocation: parent.invocation,
                        output: tilo_actions::ActionOutput::default(),
                    };
                    if terminal == "typed" {
                        rig.worker
                            .handle
                            .update_invocation(
                                parent_wire,
                                &serde_json::to_string(&observed).unwrap(),
                            )
                            .await
                            .unwrap();
                    } else {
                        rig.worker
                            .handle
                            .complete_invocation(parent_wire, "", "")
                            .await
                            .unwrap();
                    }
                    assert_eq!(
                        rig.state
                            .action_observe(&caller, &parent.invocation)
                            .unwrap()
                            .update,
                        observed,
                        "child request failure cannot replace an observed parent outcome"
                    );
                    assert!(
                        rig.state
                            .pending_invocations
                            .lock()
                            .unwrap()
                            .get(&parent_wire)
                            .is_none()
                    );
                    assert!(
                        rig.state
                            .owner_commit_failure
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .contains("continuation cancellation failed")
                    );
                    assert!(
                        continuation_query_page(&rig, parent.invocation)
                            .await
                            .is_err()
                    );
                    // The child remains independently nonterminal; no stop/cleanup
                    // is inferred from the parent's successful retirement.
                    assert!(
                        !rig.state
                            .action_observe(&caller, &child.invocation)
                            .unwrap()
                            .is_terminal()
                    );
                    rig.stop().await;
                })
                .await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_partial_cancel_failure_fences_original_parent_before_later_admission() {
    for failure in ["journal", "queue"] {
        for entry in ["host", "journal-record"] {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let env = Environment::new();
                    let rig = Rig::new(&env).await;
                    let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
                    let child = continuation_admit_child(&rig, parent.invocation, "open").await;
                    let parent_wire = rig.state.remote_job(&caller, &parent.invocation).unwrap();
                    let child_wire = rig.state.remote_job(&caller, &child.invocation).unwrap();
                    continuation_inject_child_cancellation_failure(&rig, child_wire, failure);
                    let facade = crate::collab_broker::AppActionBroker::from_state_for_test(
                        rig.state.clone(),
                    );
                    let host = facade
                        .certified_local_action_host(&caller, &evidence)
                        .unwrap();
                    let cancellation = if entry == "host" {
                        host.cancel(&caller, &parent.invocation)
                            .map_err(|error| error.to_string())
                    } else {
                        let key = rig
                            .state
                            .pending_invocations
                            .lock()
                            .unwrap()
                            .get(&parent_wire)
                            .unwrap()
                            .journal_key
                            .clone()
                            .unwrap();
                        let record = rig
                            .state
                            .journal
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .lookup(&key)
                            .unwrap()
                            .unwrap();
                        rig.state.cancel_durable_record(&record)
                    };
                    assert!(
                        cancellation.is_err(),
                        "partial request failure remains explicit"
                    );
                    {
                        let registry = rig.state.pending_invocations.lock().unwrap();
                        let retained = registry.get(&parent_wire).unwrap();
                        assert!(retained.cancellation_fence.load(Ordering::Acquire));
                        assert!(retained.cancellation_requested_at.is_some());
                    }
                    let before = rig.state.pending_invocations.lock().unwrap().len();
                    let mut later = local_host_request(false);
                    later.on_behalf_of = Some(parent.invocation);
                    use tilo_actions::remote::{
                        ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
                    };
                    let answer = continuation_round_trip(
                        &rig,
                        &RemoteActionFrame::new(RemoteActionMessage::Invoke(later.clone()))
                            .encode()
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                    assert!(
                        matches!(RemoteActionFrame::decode(&answer).unwrap().message,
                RemoteActionMessage::Receipt(receipt) if receipt.invocation == later.invocation
                    && matches!(receipt.status, ReceiptStatus::Rejected { .. }))
                    );
                    assert_eq!(rig.state.pending_invocations.lock().unwrap().len(), before);
                    assert!(
                        !rig.state
                            .action_observe(&caller, &child.invocation)
                            .unwrap()
                            .is_terminal()
                    );
                    rig.stop().await;
                })
                .await;
        }
    }
}

fn selection_host_environment() -> Environment {
    let mut env = Environment::new();
    env.cert.capabilities = tilo_user_identity::DeviceCapabilities::host();
    tilo_user_identity::sign_device_cert(&mut env.cert, &env.person).unwrap();
    *env.authority.certificate.lock().unwrap() = Some(env.cert.clone());
    env
}

struct SelectionService {
    seen: Arc<std::sync::Mutex<Vec<tilo_actions::remote::ResourceSelectionPromptRequest>>>,
    foreign: bool,
}
impl crate::services::ResourceSelectionServices for SelectionService {
    fn select(
        &self,
        context: crate::services::NativeDocumentReadContext,
        request: tilo_actions::remote::ResourceSelectionPromptRequest,
    ) -> crate::services::ResourceSelectionFuture {
        let foreign = self.foreign;
        let seen = self.seen.clone();
        Box::pin(async move {
            context.revalidate()?;
            if context.route().cluster_id != request.room
                || context.route().app_id != request.source_app_id
                || context.route().record_id != request.source_record_id
            {
                return Err("selection lost admitted source route".into());
            }
            let token = if foreign {
                Some(uuid::Uuid::new_v4())
            } else {
                request.candidates.first().map(|candidate| candidate.token)
            };
            seen.lock().unwrap().push(request);
            Ok(token)
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_resource_selection_returns_original_authorized_row_and_refuses_foreign_token()
{
    tokio::task::LocalSet::new()
        .run_until(async {
            for foreign in [false, true] {
                let env = selection_host_environment();
                let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
                let rig = Rig::with_services(
                    &env,
                    HostServices {
                        resource_selections: Some(Arc::new(SelectionService {
                            seen: seen.clone(),
                            foreign,
                        })),
                        ..env.membership.services()
                    },
                )
                .await;
                let (_, _, parent) = continuation_parent(&env, &rig).await;
                let descriptor = continuation_descriptor();
                let filter = tilo_actions::remote::ResourceSelectionFilter {
                    app_id: env.route.app_id.clone(),
                    resource_kind: descriptor.resource_kind,
                    action: descriptor.action,
                };
                let unscoped = tilo_actions::remote::RemoteActionFrame::new(tilo_actions::remote::RemoteActionMessage::SelectResource { correlation: 70, filter: filter.clone() }).encode().unwrap();
                let unscoped = continuation_round_trip(&rig, &unscoped).await.unwrap();
                assert!(matches!(tilo_actions::remote::RemoteActionFrame::decode(&unscoped).unwrap().message, tilo_actions::remote::RemoteActionMessage::ResourceSelectionResponse { result: Err(tilo_actions::remote::ActionHostError::NotPermitted), .. }));
                assert!(seen.lock().unwrap().is_empty(), "plain application request cannot mint an attended prompt without original custody");
                let absent = continuation_answer(&rig, tilo_actions::remote::RemoteActionMessage::SelectResource { correlation: 69, filter: tilo_actions::remote::ResourceSelectionFilter { action: "does-not-exist".into(), ..filter.clone() } }, parent.invocation).await.unwrap();
                assert!(matches!(absent, tilo_actions::remote::RemoteActionMessage::ResourceSelectionResponse { result: Err(tilo_actions::remote::ActionHostError::Unavailable { .. }), .. }));
                assert!(seen.lock().unwrap().is_empty(), "absent semantic resource must not become a fabricated default selection");
                let result = continuation_answer(
                    &rig,
                    tilo_actions::remote::RemoteActionMessage::SelectResource {
                        correlation: 71,
                        filter,
                    },
                    parent.invocation,
                )
                .await
                .unwrap();
                let prompts = seen.lock().unwrap();
                assert_eq!(prompts.len(), 1);
                assert!(
                    !prompts[0].candidates.is_empty(),
                    "actual signed directory must expose an authorized semantic row"
                );
                let tilo_actions::remote::RemoteActionMessage::ResourceSelectionResponse {
                    correlation,
                    result,
                } = result
                else {
                    panic!("selection response expected")
                };
                assert_eq!(correlation, 71);
                if foreign {
                    assert_eq!(
                        result,
                        Err(tilo_actions::remote::ActionHostError::NotPermitted)
                    );
                } else {
                    let selected = result.unwrap().unwrap();
                    assert_eq!(selected, prompts[0].candidates[0].selected);
                    assert_eq!(selected.provider.record_id, env.route.record_id);
                    assert!(selected.row.action.unwrap().semantic_identity.is_some());
                }
                drop(prompts);
                rig.stop().await;
            }
        })
        .await;
}

struct HeldSelectionService {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<std::sync::atomic::AtomicBool>,
}
struct SelectionWaitGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for SelectionWaitGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
impl crate::services::ResourceSelectionServices for HeldSelectionService {
    fn select(
        &self,
        context: crate::services::NativeDocumentReadContext,
        _: tilo_actions::remote::ResourceSelectionPromptRequest,
    ) -> crate::services::ResourceSelectionFuture {
        let entered = self.entered.clone();
        let guard = SelectionWaitGuard(self.dropped.clone());
        Box::pin(async move {
            let _guard = guard;
            context.revalidate()?;
            entered.notify_one();
            std::future::pending().await
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_resource_selection_parent_cancel_drops_actual_host_prompt_future() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = selection_host_environment();
            let entered = Arc::new(tokio::sync::Notify::new());
            let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let rig = Rig::with_services(
                &env,
                HostServices {
                    resource_selections: Some(Arc::new(HeldSelectionService {
                        entered: entered.clone(),
                        dropped: dropped.clone(),
                    })),
                    ..env.membership.services()
                },
            )
            .await;
            let (caller, evidence, parent) = continuation_parent(&env, &rig).await;
            let descriptor = continuation_descriptor();
            let selection = continuation_answer(
                &rig,
                tilo_actions::remote::RemoteActionMessage::SelectResource {
                    correlation: 72,
                    filter: tilo_actions::remote::ResourceSelectionFilter {
                        app_id: env.route.app_id.clone(),
                        resource_kind: descriptor.resource_kind,
                        action: descriptor.action,
                    },
                },
                parent.invocation,
            );
            let cancel = async {
                entered.notified().await;
                let facade =
                    crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
                let host = facade
                    .certified_local_action_host(&caller, &evidence)
                    .unwrap();
                host.cancel(&caller, &parent.invocation).unwrap();
            };
            let (answer, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(selection, cancel)
            })
            .await
            .expect(
                "actual host prompt must be observable before bounded original cancellation proof",
            );
            assert!(matches!(
                answer.unwrap(),
                tilo_actions::remote::RemoteActionMessage::ResourceSelectionResponse {
                    result: Err(tilo_actions::remote::ActionHostError::NotPermitted),
                    ..
                }
            ));
            assert!(
                dropped.load(std::sync::atomic::Ordering::SeqCst),
                "actual pending host choice must be dropped when original parent retires"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_resource_selection_preserves_actual_signed_agent_original_authority() {
    tokio::task::LocalSet::new().run_until(async {
        let env = selection_host_environment();
        let primary = crate::test_owner::SessionOwner::open(&env.root.path().join("owner")).unwrap().peer().authority().clone();
        env.authority.additional_certificates.lock().unwrap().push(primary.clone());
        let grants: Vec<_> = [
            CapOverride::membership(Subject::agent("scribe"), env.route.cluster_id.clone(), Verb::Write),
            CapOverride::grant(Subject::agent("scribe"), collab_types::Scope::App(env.route.app_id.clone()), Verb::Write),
        ].into_iter().map(|capability| env.membership.write().workspaces[0].grant_capability(capability)).collect();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rig = Rig::with_services(&env, HostServices { resource_selections: Some(Arc::new(SelectionService { seen: seen.clone(), foreign: false })), ..env.membership.services() }).await;
        rig.worker.handle.publish_action_directory(&env.route.app_id, capnp_rpc::new_client(ContinuationDirectory { state: directory_state("original-r1") })).await.unwrap();
        rig.worker.handle.publish_actions(&env.route.app_id, &[continuation_descriptor()]).await.unwrap();
        let (_, binding) = rig.worker.handle.resolve_published_action_for_invocation(903, "original-r1", "workspace:atlas", "open").await.unwrap();
        let (server_socket, client_socket) = std::os::unix::net::UnixStream::pair().unwrap();
        let (rpc, server, client) = rpc_pair(rig.state.clone(), 906, server_socket, client_socket, None, rig.directories.clone());
        let agent_route = CrdtRoute::cluster(env.route.cluster_id.clone());
        let agent = rpc.join_channel_as_kind(&agent_route.channel_key(), &presentation(&agent_route.cluster_id, &primary, &env.key), "scribe", "agent", "").await.unwrap();
        let original = tilo_actions::InvocationId::new();
        invoke(&agent, &binding, original).await.unwrap();
        let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
        assert_eq!(delivered.actor, "agent:scribe");
        let lease = (rig.authorize)(&ui::invocation_scope::PublishedInvocationRequest { wire_id: delivered.id, invocation: original, address: tilo_actions::ActionAddress { resource: delivered.resource, action: delivered.action }, argument: delivered.argument, actor: delivered.actor, participant: None }).unwrap();
        lease.check().unwrap();
        let descriptor = continuation_descriptor();
        let selection = tilo_actions::remote::RemoteActionMessage::SelectResource { correlation: 75, filter: tilo_actions::remote::ResourceSelectionFilter { app_id: env.route.app_id.clone(), resource_kind: descriptor.resource_kind, action: descriptor.action } };
        assert!(matches!(continuation_answer(&rig, selection.clone(), original).await.unwrap(), tilo_actions::remote::RemoteActionMessage::ResourceSelectionResponse { result: Ok(Some(_)), .. }));
        assert_eq!(seen.lock().unwrap().len(), 1);
        for grant in grants { env.membership.write().workspaces[0].revoke_capability(&grant); }
        assert!(lease.check().is_err());
        assert!(continuation_answer(&rig, selection, original).await.is_err());
        assert_eq!(seen.lock().unwrap().len(), 1, "native Host producer cannot elevate a revoked signed Agent original to its own authority");
        server.abort(); client.abort();
        rig.stop().await;
    }).await;
}
