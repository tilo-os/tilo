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
