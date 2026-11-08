async fn canonical_pending<T>(
    workspace: &signed_worker::OriginalWorkspace,
    mut pending: tilo_actions::remote::Pending<T>,
) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        workspace
            .state
            .drain_action_directory_bridge(&workspace.directories);
        if let tilo_actions::remote::PendingPoll::Ready(value) = pending.poll_ready() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "canonical directory request did not finish"
        );
        tokio::task::yield_now().await;
    }
}

fn canonical_query(
    workspace: &signed_worker::OriginalWorkspace,
) -> tilo_actions::remote::CatalogueQuery {
    tilo_actions::remote::CatalogueQuery {
        canonical_route: Some(tilo_actions::remote::CanonicalActionRoute {
            workspace: workspace.route.cluster_id.clone(),
            app_id: workspace.route.app_id.clone(),
            record_id: workspace.route.record_id.clone(),
        }),
        max_results: 0,
        ..Default::default()
    }
}

fn canonical_caller(workspace: &signed_worker::OriginalWorkspace) -> tilo_actions::remote::Caller {
    tilo_actions::remote::Caller::authenticated(
        workspace.cert.user_public_key,
        workspace.cert.device_public_key,
    )
    .in_workspace(workspace.route.cluster_id.clone())
    .in_new_execution()
}

/// The workspace's own certified door, and the caller `workspace`'s device
/// asks through it.
fn canonical_host(
    workspace: &signed_worker::OriginalWorkspace,
) -> (
    tilo_actions::remote::Caller,
    Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
) {
    let caller = canonical_caller(workspace);
    let host = workspace
        .state
        .certified_local_action_host(
            &caller,
            &signed_worker::presentation(
                &workspace.route.cluster_id,
                &workspace.cert,
                &workspace.key,
            ),
        )
        .unwrap();
    (caller, host)
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_route_rechecks_registration_certificate_and_stream_after_resolve_await() {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    tokio::task::LocalSet::new()
        .run_until(async {
            for change in ["registration", "certificate", "stream"] {
                let workspace = signed_worker::OriginalWorkspace::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let (worker, _) = workspace.join_worker(32).await;
                let old_stream = worker.handle.subscribe_stable_invocations().await.unwrap();
                let directory = signed_worker::directory_state("directory-r1");
                let (entered, mut entered_rx) = mpsc::unbounded_channel();
                let release = Rc::new(tokio::sync::Notify::new());
                worker
                    .handle
                    .publish_action_directory(crate::collab_app_broker::wire_answerer(Rc::new(
                        signed_worker::HeldHostDirectory {
                            entered,
                            release: release.clone(),
                            row: directory.borrow().row(),
                        },
                    )))
                    .await
                    .unwrap();
                let (caller, host) = canonical_host(&workspace);
                let page =
                    canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                        .await
                        .unwrap();
                let request = InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: page.providers[0].provider.clone(),
                        address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                    },
                    argument: InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: Some("directory-r1".into()),
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                };
                let pending = host.invoke(&caller, request);
                workspace
                    .state
                    .drain_action_directory_bridge(&workspace.directories);
                assert_eq!(entered_rx.recv().await, Some("resolve"));
                let replacement_stream = if change == "stream" {
                    Some(worker.handle.subscribe_stable_invocations().await.unwrap())
                } else {
                    None
                };
                if change == "registration" {
                    worker
                        .handle
                        .publish_action_directory(signed_worker::directory_server(&directory))
                        .await
                        .unwrap();
                } else if change == "certificate" {
                    *workspace.authority.certificate.lock().unwrap() = None;
                }
                release.notify_one();
                assert!(
                    canonical_pending(&workspace, pending).await.is_err(),
                    "{change} during real directory RPC must prevent dispatch"
                );
                assert!(
                    workspace
                        .state
                        .pending_invocations
                        .lock()
                        .unwrap()
                        .is_empty(),
                    "no invocation was admitted before the changed custody check"
                );
                drop(replacement_stream);
                drop(old_stream);
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_route_promotes_retired_publisher_without_retargeting_admitted_uuid() {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    tokio::task::LocalSet::new()
        .run_until(async {
            let storage = tempfile::tempdir().unwrap();
            let workspace = signed_worker::OriginalWorkspace::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                Some(storage.path().join("workspace.json")),
            );
            let (first, _) = workspace.join_worker(91).await;
            let (second, _) = workspace.join_worker(12).await;
            let first_stream = first.handle.subscribe_stable_invocations().await.unwrap();
            let second_stream = second.handle.subscribe_stable_invocations().await.unwrap();
            for worker in [&first, &second] {
                worker
                    .handle
                    .publish_action_directory(signed_worker::directory_server(
                        &signed_worker::directory_state("r1"),
                    ))
                    .await
                    .unwrap();
            }
            let (caller, host) = canonical_host(&workspace);
            let initial =
                canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                    .await
                    .unwrap();
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: initial.providers[0].provider.clone(),
                    address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: Some("r1".into()),
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            };
            let receipt = canonical_pending(&workspace, host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(receipt.invocation, request.invocation);
            let delivered = first_stream.read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *request.invocation.0.as_bytes());
            drop(first);
            let deadline = tilo_test_env::hang_deadline();
            while workspace.state.host_workers.lock().unwrap().holds(91) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "actual retired socket retained worker custody"
                );
                tokio::task::yield_now().await;
            }
            let promoted =
                canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                    .await
                    .unwrap();
            assert_eq!(
                workspace
                    .state
                    .canonical_token_directory(&promoted.providers[0].provider)
                    .unwrap()
                    .peer_id,
                12
            );
            let retry = canonical_pending(&workspace, host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(retry.invocation, receipt.invocation);
            let mut changed = request.clone();
            changed.argument = InvokeArgument::Text("different intent".into());
            assert!(
                canonical_pending(&workspace, host.invoke(&caller, changed))
                    .await
                    .is_err()
            );
            let mut stale = request.clone();
            stale.invocation = tilo_actions::InvocationId::new();
            assert!(
                canonical_pending(&workspace, host.invoke(&caller, stale))
                    .await
                    .is_err(),
                "a fresh UUID cannot use retired publisher custody"
            );
            let mut fresh = request;
            fresh.invocation = tilo_actions::InvocationId::new();
            fresh.target.provider = promoted.providers[0].provider.clone();
            canonical_pending(&workspace, host.invoke(&caller, fresh.clone()))
                .await
                .unwrap();
            assert_eq!(
                second_stream.read().await.unwrap().invocation_uuid,
                *fresh.invocation.0.as_bytes(),
                "the only promoted dispatch is the explicitly submitted fresh request"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_route_uses_signed_directory_admission_order_and_fences_replacement() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = signed_worker::OriginalWorkspace::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            // Lower connection id arrives first, but only the later connected
            // application successfully publishes its directory first.
            let (early, _) = workspace.join_worker(11).await;
            let (first, _) = workspace.join_worker(99).await;
            let _early_stream = early.handle.subscribe_stable_invocations().await.unwrap();
            let _first_stream = first.handle.subscribe_stable_invocations().await.unwrap();
            let first_directory = signed_worker::directory_state("first-r1");
            first
                .handle
                .publish_action_directory(signed_worker::directory_server(&first_directory))
                .await
                .unwrap();
            let other_directory = signed_worker::directory_state("other-r1");
            early
                .handle
                .publish_action_directory(signed_worker::directory_server(&other_directory))
                .await
                .unwrap();
            let (caller, host) = canonical_host(&workspace);
            let page =
                canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                    .await
                    .unwrap();
            assert_eq!(page.providers.len(), 1);
            let token = &page.providers[0].provider;
            assert_eq!(
                workspace
                    .state
                    .canonical_token_directory(token)
                    .unwrap()
                    .peer_id,
                99
            );
            let query = tilo_actions::remote::CatalogueQuery {
                provider: Some(token.clone()),
                ..Default::default()
            };
            let rows = canonical_pending(&workspace, host.query(&caller, query.clone()))
                .await
                .unwrap();
            assert_eq!(rows.revision, "first-r1");
            assert_eq!(rows.rows.len(), 1);
            assert_eq!(rows.rows[0].provider, *token);
            assert_eq!(first_directory.borrow().query_count, 1);
            assert_eq!(other_directory.borrow().query_count, 0);
            first
                .handle
                .publish_action_directory(signed_worker::directory_server(&first_directory))
                .await
                .unwrap();
            assert!(
                canonical_pending(&workspace, host.query(&caller, query))
                    .await
                    .is_err(),
                "same revision does not rescue an old registration token"
            );
            let replacement =
                canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                    .await
                    .unwrap();
            assert_ne!(replacement.providers[0].provider, *token);
            assert_eq!(
                workspace
                    .state
                    .canonical_token_directory(&replacement.providers[0].provider)
                    .unwrap()
                    .peer_id,
                99
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_route_refuses_cross_workspace_forged_tokens_and_filtered_lookup() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = signed_worker::OriginalWorkspace::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (worker, _) = workspace.join_worker(21).await;
            let _stream = worker.handle.subscribe_stable_invocations().await.unwrap();
            worker
                .handle
                .publish_action_directory(signed_worker::directory_server(
                    &signed_worker::directory_state("r1"),
                ))
                .await
                .unwrap();
            let (caller, host) = canonical_host(&workspace);
            let mut wrong_workspace = canonical_query(&workspace);
            wrong_workspace.canonical_route.as_mut().unwrap().workspace =
                uuid::Uuid::new_v4().to_string();
            assert!(
                canonical_pending(&workspace, host.query(&caller, wrong_workspace))
                    .await
                    .is_err()
            );
            let mut filtered = canonical_query(&workspace);
            filtered.provider = Some("21".into());
            assert!(
                canonical_pending(&workspace, host.query(&caller, filtered))
                    .await
                    .is_err()
            );
            let forged = tilo_actions::remote::CatalogueQuery {
                provider: Some(format!("canonical:{}", uuid::Uuid::new_v4())),
                ..Default::default()
            };
            assert!(
                canonical_pending(&workspace, host.query(&caller, forged))
                    .await
                    .is_err()
            );
            let mut other_record = canonical_query(&workspace);
            other_record.canonical_route.as_mut().unwrap().record_id = "other-record".into();
            assert!(
                canonical_pending(&workspace, host.query(&caller, other_record))
                    .await
                    .unwrap()
                    .providers
                    .is_empty()
            );
            *workspace.authority.certificate.lock().unwrap() = None;
            assert!(
                canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                    .await
                    .is_err()
            );
        })
        .await;
}

/// A live directory's provider token answers an application filter: the rows
/// of another application are answered empty without asking the directory,
/// and the exact target it holds is re-resolved only for its own app.
#[tokio::test(flavor = "current_thread")]
async fn a_canonical_provider_answers_another_apps_query_without_asking_its_directory() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = signed_worker::OriginalWorkspace::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (worker, _) = workspace.join_worker(32).await;
            // Canonical custody requires the worker's stable invocation stream
            // to stay subscribed, as the other canonical route fixtures keep it.
            let _stream = worker.handle.subscribe_stable_invocations().await.unwrap();
            let directory = signed_worker::directory_state("directory-r1");
            worker
                .handle
                .publish_action_directory(signed_worker::directory_server(&directory))
                .await
                .unwrap();
            let (caller, host) = canonical_host(&workspace);
            let announced =
                canonical_pending(&workspace, host.query(&caller, canonical_query(&workspace)))
                    .await
                    .unwrap();
            let token = announced.providers[0].provider.clone();
            let query = |app_id: Option<&str>| tilo_actions::remote::CatalogueQuery {
                provider: Some(token.clone()),
                app_id: app_id.map(str::to_owned),
                max_results: 8,
                ..Default::default()
            };
            let ask =
                |query: &tilo_actions::remote::CatalogueQuery| host.query(&caller, query.clone());

            let page = canonical_pending(&workspace, ask(&query(Some("so.tilo.notes"))))
                .await
                .unwrap();
            assert!(
                page.rows.is_empty() && page.exhausted,
                "another application's rows cannot be this directory's"
            );
            assert_eq!(
                directory.borrow().query_count,
                0,
                "a query another app may not satisfy never reaches the directory"
            );

            let own = canonical_pending(
                &workspace,
                ask(&query(Some(workspace.route.app_id.as_str()))),
            )
            .await
            .unwrap();
            assert_eq!(directory.borrow().query_count, 1);
            assert_eq!(own.rows.len(), 1);
            assert_eq!(
                own.rows[0].provider, token,
                "the rows keep the provider token they were asked by"
            );
            assert_eq!(own.rows[0].app_id, workspace.route.app_id);

            let unscoped = canonical_pending(&workspace, ask(&query(None)))
                .await
                .unwrap();
            assert_eq!(directory.borrow().query_count, 2);
            assert_eq!(
                unscoped.rows, own.rows,
                "a query naming no app is unchanged"
            );

            let address = unscoped.rows[0]
                .action
                .as_ref()
                .expect("a directory row offering the action")
                .address
                .clone();
            let target = |app_id: Option<&str>, revision: Option<&str>| {
                tilo_actions::remote::CatalogueQuery {
                    target: Some(address.clone()),
                    revision: revision.map(str::to_owned),
                    ..query(app_id)
                }
            };
            let page = canonical_pending(
                &workspace,
                ask(&target(Some("so.tilo.notes"), Some("directory-r1"))),
            )
            .await
            .unwrap();
            assert!(
                page.rows.is_empty() && page.exhausted,
                "an exact target under another app is answered empty"
            );
            assert_eq!(
                directory.borrow().resolve_count,
                0,
                "an exact target another app cannot own is not resolved"
            );
            let page = canonical_pending(
                &workspace,
                ask(&target(
                    Some(workspace.route.app_id.as_str()),
                    Some("directory-r1"),
                )),
            )
            .await
            .unwrap();
            assert_eq!(directory.borrow().resolve_count, 1);
            assert_eq!(page.rows.len(), 1);
        })
        .await;
}
