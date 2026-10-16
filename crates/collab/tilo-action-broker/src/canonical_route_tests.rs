async fn canonical_pending<T>(
    room: &signed_worker::OriginalRoom,
    mut pending: tilo_actions::remote::Pending<T>,
) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        room.state.drain_action_directory_bridge(&room.directories);
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

fn canonical_query(room: &signed_worker::OriginalRoom) -> tilo_actions::remote::CatalogueQuery {
    tilo_actions::remote::CatalogueQuery {
        canonical_route: Some(tilo_actions::remote::CanonicalActionRoute {
            room: room.route.cluster_id.clone(),
            app_id: room.route.app_id.clone(),
            record_id: room.route.record_id.clone(),
        }),
        max_results: 0,
        ..Default::default()
    }
}

fn canonical_caller(room: &signed_worker::OriginalRoom) -> tilo_actions::remote::Caller {
    tilo_actions::remote::Caller::authenticated(
        room.cert.user_public_key,
        room.cert.device_public_key,
    )
    .in_room(room.route.cluster_id.clone())
    .in_new_execution()
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_route_rechecks_registration_certificate_and_stream_after_resolve_await() {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    tokio::task::LocalSet::new()
        .run_until(async {
            for change in ["registration", "certificate", "stream"] {
                let room = signed_worker::OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let (worker, _) = room.join_worker(32).await;
                let old_stream = worker.handle.subscribe_stable_invocations().await.unwrap();
                let directory = signed_worker::directory_state("directory-r1");
                let (entered, mut entered_rx) = mpsc::unbounded_channel();
                let release = Rc::new(tokio::sync::Notify::new());
                worker
                    .handle
                    .publish_action_directory(
                        &room.route.app_id,
                        capnp_rpc::new_client(signed_worker::HeldHostDirectory {
                            entered,
                            release: release.clone(),
                            row: directory.borrow().row(),
                        }),
                    )
                    .await
                    .unwrap();
                let caller = canonical_caller(&room);
                let host = room
                    .state
                    .certified_local_action_host(
                        &caller,
                        &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
                    )
                    .unwrap();
                let page = canonical_pending(&room, host.query(&caller, canonical_query(&room)))
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
                room.state.drain_action_directory_bridge(&room.directories);
                assert_eq!(entered_rx.recv().await, Some("resolve"));
                let replacement_stream = if change == "stream" {
                    Some(worker.handle.subscribe_stable_invocations().await.unwrap())
                } else {
                    None
                };
                if change == "registration" {
                    worker
                        .handle
                        .publish_action_directory(
                            &room.route.app_id,
                            signed_worker::directory_server(&directory),
                        )
                        .await
                        .unwrap();
                } else if change == "certificate" {
                    *room.authority.certificate.lock().unwrap() = None;
                }
                release.notify_one();
                assert!(
                    canonical_pending(&room, pending).await.is_err(),
                    "{change} during real directory RPC must prevent dispatch"
                );
                assert!(
                    room.state.pending_invocations.lock().unwrap().is_empty(),
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
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                Some(storage.path().join("workspace.json")),
            );
            let (first, _) = room.join_worker(91).await;
            let (second, _) = room.join_worker(12).await;
            let first_stream = first.handle.subscribe_stable_invocations().await.unwrap();
            let second_stream = second.handle.subscribe_stable_invocations().await.unwrap();
            for worker in [&first, &second] {
                worker
                    .handle
                    .publish_action_directory(
                        &room.route.app_id,
                        signed_worker::directory_server(&signed_worker::directory_state("r1")),
                    )
                    .await
                    .unwrap();
            }
            let caller = canonical_caller(&room);
            let host = room
                .state
                .certified_local_action_host(
                    &caller,
                    &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
                )
                .unwrap();
            let initial = canonical_pending(&room, host.query(&caller, canonical_query(&room)))
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
            let receipt = canonical_pending(&room, host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(receipt.invocation, request.invocation);
            let delivered = first_stream.read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *request.invocation.0.as_bytes());
            drop(first);
            let deadline = tilo_test_env::hang_deadline();
            while room.state.host_workers.lock().unwrap().contains_key(&91) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "actual retired socket retained worker custody"
                );
                tokio::task::yield_now().await;
            }
            let promoted = canonical_pending(&room, host.query(&caller, canonical_query(&room)))
                .await
                .unwrap();
            assert_eq!(
                room.state
                    .canonical_token_directory(&promoted.providers[0].provider)
                    .unwrap()
                    .peer_id,
                12
            );
            let retry = canonical_pending(&room, host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(retry.invocation, receipt.invocation);
            let mut changed = request.clone();
            changed.argument = InvokeArgument::Text("different intent".into());
            assert!(
                canonical_pending(&room, host.invoke(&caller, changed))
                    .await
                    .is_err()
            );
            let mut stale = request.clone();
            stale.invocation = tilo_actions::InvocationId::new();
            assert!(
                canonical_pending(&room, host.invoke(&caller, stale))
                    .await
                    .is_err(),
                "a fresh UUID cannot use retired publisher custody"
            );
            let mut fresh = request;
            fresh.invocation = tilo_actions::InvocationId::new();
            fresh.target.provider = promoted.providers[0].provider.clone();
            canonical_pending(&room, host.invoke(&caller, fresh.clone()))
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
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            // Lower connection id arrives first, but only the later connected
            // application successfully publishes its directory first.
            let (early, _) = room.join_worker(11).await;
            let (first, _) = room.join_worker(99).await;
            let _early_stream = early.handle.subscribe_stable_invocations().await.unwrap();
            let _first_stream = first.handle.subscribe_stable_invocations().await.unwrap();
            let first_directory = signed_worker::directory_state("first-r1");
            first
                .handle
                .publish_action_directory(
                    &room.route.app_id,
                    signed_worker::directory_server(&first_directory),
                )
                .await
                .unwrap();
            let other_directory = signed_worker::directory_state("other-r1");
            early
                .handle
                .publish_action_directory(
                    &room.route.app_id,
                    signed_worker::directory_server(&other_directory),
                )
                .await
                .unwrap();
            let caller = canonical_caller(&room);
            let host = room
                .state
                .certified_local_action_host(
                    &caller,
                    &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
                )
                .unwrap();
            let page = canonical_pending(&room, host.query(&caller, canonical_query(&room)))
                .await
                .unwrap();
            assert_eq!(page.providers.len(), 1);
            let token = &page.providers[0].provider;
            assert_eq!(
                room.state.canonical_token_directory(token).unwrap().peer_id,
                99
            );
            let query = tilo_actions::remote::CatalogueQuery {
                provider: Some(token.clone()),
                ..Default::default()
            };
            let rows = canonical_pending(&room, host.query(&caller, query.clone()))
                .await
                .unwrap();
            assert_eq!(rows.revision, "first-r1");
            assert_eq!(rows.rows.len(), 1);
            assert_eq!(rows.rows[0].provider, *token);
            assert_eq!(first_directory.borrow().query_count, 1);
            assert_eq!(other_directory.borrow().query_count, 0);
            first
                .handle
                .publish_action_directory(
                    &room.route.app_id,
                    signed_worker::directory_server(&first_directory),
                )
                .await
                .unwrap();
            assert!(
                canonical_pending(&room, host.query(&caller, query))
                    .await
                    .is_err(),
                "same revision does not rescue an old registration token"
            );
            let replacement = canonical_pending(&room, host.query(&caller, canonical_query(&room)))
                .await
                .unwrap();
            assert_ne!(replacement.providers[0].provider, *token);
            assert_eq!(
                room.state
                    .canonical_token_directory(&replacement.providers[0].provider)
                    .unwrap()
                    .peer_id,
                99
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_route_refuses_cross_room_forged_tokens_and_filtered_lookup() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let (worker, _) = room.join_worker(21).await;
            let _stream = worker.handle.subscribe_stable_invocations().await.unwrap();
            worker
                .handle
                .publish_action_directory(
                    &room.route.app_id,
                    signed_worker::directory_server(&signed_worker::directory_state("r1")),
                )
                .await
                .unwrap();
            let caller = canonical_caller(&room);
            let host = room
                .state
                .certified_local_action_host(
                    &caller,
                    &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
                )
                .unwrap();
            let mut wrong_room = canonical_query(&room);
            wrong_room.canonical_route.as_mut().unwrap().room = uuid::Uuid::new_v4().to_string();
            assert!(
                canonical_pending(&room, host.query(&caller, wrong_room))
                    .await
                    .is_err()
            );
            let mut filtered = canonical_query(&room);
            filtered.provider = Some("21".into());
            assert!(
                canonical_pending(&room, host.query(&caller, filtered))
                    .await
                    .is_err()
            );
            let forged = tilo_actions::remote::CatalogueQuery {
                provider: Some(format!("canonical:{}", uuid::Uuid::new_v4())),
                ..Default::default()
            };
            assert!(
                canonical_pending(&room, host.query(&caller, forged))
                    .await
                    .is_err()
            );
            let mut other_record = canonical_query(&room);
            other_record.canonical_route.as_mut().unwrap().record_id = "other-record".into();
            assert!(
                canonical_pending(&room, host.query(&caller, other_record))
                    .await
                    .unwrap()
                    .providers
                    .is_empty()
            );
            *room.authority.certificate.lock().unwrap() = None;
            assert!(
                canonical_pending(&room, host.query(&caller, canonical_query(&room)))
                    .await
                    .is_err()
            );
        })
        .await;
}
