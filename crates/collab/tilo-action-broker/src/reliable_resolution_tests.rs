#[tokio::test(flavor = "current_thread")]
async fn reliable_resolution_binding_uses_exact_captured_signed_worker() {
    use signed_worker::{HeldHostDirectory, OriginalRoom, directory_server, directory_state};
    tokio::task::LocalSet::new()
        .run_until(async {
            for scenario in ["no-journal", "clock", "origin"] {
                let journal_dir = tempfile::tempdir().unwrap();
                let room = OriginalRoom::new(
                    tilo_user_identity::DeviceCapabilities::host(),
                    None,
                    (scenario != "no-journal").then(|| journal_dir.path().join("workspace.json")),
                );
                let OriginalRoom {
                    cert,
                    origin,
                    time_control,
                    route,
                    state,
                    authority,
                    ..
                } = &room;
                if scenario == "no-journal" {
                    // OriginalRoom provisions a journal for signed effects by
                    // default; explicitly remove it to test a browsing-only host.
                    *state.journal.lock().unwrap() = Ok(None);
                }
                let (worker, _authorize) = room.join_worker(903).await;
                let handle = &worker.handle;
                let directory_state = directory_state("original-r1");
                handle
                    .publish_action_directory(&route.app_id, directory_server(&directory_state))
                    .await
                    .unwrap();
                assert!(matches!(
                    handle
                        .resolve_published_action(903, "original-r1", "workspace:atlas", "open")
                        .await
                        .unwrap(),
                    runtime_collab::ActionDirectoryResolution::Found { .. }
                ));
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_err(),
                    "no stable UUID stream means no reliable binding"
                );
                let _invocations = handle.subscribe_stable_invocations().await.unwrap();
                if scenario == "no-journal" {
                    assert!(
                        handle
                            .resolve_published_action_for_invocation(
                                903,
                                "original-r1",
                                "workspace:atlas",
                                "open"
                            )
                            .await
                            .is_err(),
                        "read-only browsing cannot manufacture a durable journal binding"
                    );
                    assert!(matches!(
                        handle
                            .resolve_published_action(903, "original-r1", "workspace:atlas", "open")
                            .await
                            .unwrap(),
                        runtime_collab::ActionDirectoryResolution::Found { .. }
                    ));
                    continue;
                }
                let (row, binding) = handle
                    .resolve_published_action_for_invocation(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                    )
                    .await
                    .expect("same captured real directory and worker provide reliable binding");
                assert_eq!(row.resource, "workspace:atlas");
                assert_eq!(binding.peer_id, 903);
                assert_eq!(binding.host, *state.invocation_host().unwrap().as_bytes());
                assert_eq!(
                    (&binding.app_id, &binding.record_id),
                    (&route.app_id, &route.record_id)
                );
                assert_eq!(binding.root, cert.user_public_key);
                assert_eq!(binding.device, cert.device_public_key);
                assert_ne!(binding.target_binding, [0; 32]);
                // Drop only the returned remote stream capability. The signed
                // channel and actual private worker remain live.
                drop(_invocations);
                let deadline = tilo_test_env::hang_deadline();
                loop {
                    if state
                        .invocation_subscribers
                        .lock()
                        .unwrap()
                        .get(&903)
                        .is_some_and(tokio::sync::mpsc::UnboundedSender::is_closed)
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "remote stream capability release must close its receiver"
                    );
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                assert!(state.host_workers.lock().unwrap().holds(903));
                assert!(matches!(
                    handle
                        .resolve_published_action(903, "original-r1", "workspace:atlas", "open")
                        .await
                        .unwrap(),
                    runtime_collab::ActionDirectoryResolution::Found { .. }
                ));
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_err(),
                    "closed actual stream cannot qualify via a stale stable flag"
                );
                let legacy_stream = handle.subscribe_invocations().await.unwrap();
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_err(),
                    "live legacy replacement cannot qualify as a stable stream"
                );
                let _current_stream = handle.subscribe_stable_invocations().await.unwrap();
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_ok()
                );
                drop(legacy_stream);
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_ok(),
                    "dropping an older stream must not invalidate the current stable stream"
                );

                handle
                    .publish_action_directory(&route.app_id, directory_server(&directory_state))
                    .await
                    .unwrap();
                let (_, replacement) = handle
                    .resolve_published_action_for_invocation(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                    )
                    .await
                    .unwrap();
                assert_ne!(
                    binding.target_binding, replacement.target_binding,
                    "same route/certificate directory replacement changes exact target binding"
                );
                for mode in ["replace", "revoke"] {
                    let (entered, mut entered_rx) = mpsc::unbounded_channel();
                    let release = Rc::new(tokio::sync::Notify::new());
                    let directory = capnp_rpc::new_client(HeldHostDirectory {
                        entered,
                        release: release.clone(),
                        row: directory_state.borrow().row(),
                    });
                    handle
                        .publish_action_directory(&route.app_id, directory)
                        .await
                        .unwrap();
                    let c = handle.clone();
                    let request = tokio::task::spawn_local(async move {
                        c.resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open",
                        )
                        .await
                    });
                    assert_eq!(entered_rx.recv().await, Some("resolve"));
                    if mode == "replace" {
                        handle
                            .publish_action_directory(
                                &route.app_id,
                                directory_server(&directory_state),
                            )
                            .await
                            .unwrap();
                    } else {
                        *authority.certificate.lock().unwrap() = None;
                    }
                    release.notify_one();
                    assert!(
                        request.await.unwrap().is_err(),
                        "old captured resolution refused after {mode}"
                    );
                    *authority.certificate.lock().unwrap() = Some(cert.clone());
                }
                handle
                    .publish_action_directory(&route.app_id, directory_server(&directory_state))
                    .await
                    .unwrap();
                assert!(
                    handle
                        .resolve_published_action_for_invocation(
                            903,
                            "original-r1",
                            "workspace:atlas",
                            "open"
                        )
                        .await
                        .is_ok()
                );
                let (entered, mut entered_rx) = mpsc::unbounded_channel();
                let release = Rc::new(tokio::sync::Notify::new());
                handle
                    .publish_action_directory(
                        &route.app_id,
                        capnp_rpc::new_client(HeldHostDirectory {
                            entered,
                            release: release.clone(),
                            row: directory_state.borrow().row(),
                        }),
                    )
                    .await
                    .unwrap();
                let c = handle.clone();
                let pending = tokio::task::spawn_local(async move {
                    c.resolve_published_action_for_invocation(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                    )
                    .await
                });
                assert_eq!(entered_rx.recv().await, Some("resolve"));
                if scenario == "origin" {
                    origin.invalidate();
                } else {
                    time_control.set_wall_ns(10_000_000_000);
                    time_control.advance_monotonic(Duration::from_secs(80));
                }
                release.notify_one();
                assert!(
                    pending.await.unwrap().is_err(),
                    "the original retained clock/authority must survive resolution await"
                );
                assert!(
                    state.pending_invocations.lock().unwrap().is_empty(),
                    "read-only bindings admit no invocation"
                );
                assert_eq!(
                    state
                        .journal
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .next_wire_id(),
                    1
                );
            }
        })
        .await;
}
