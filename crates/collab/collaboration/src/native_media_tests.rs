#[test]
fn native_media_leave_bypasses_wedged_document_and_retires_unmute_reply() {
    use runtime_collab::native_media::{Command, Request};
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (entered, observed) = mpsc::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    captures
        .lock()
        .unwrap()
        .document_gates
        .push_back(DocumentSendGate {
            entered,
            release: wait,
        });
    let (unmute_reply, unmute_wait) = tokio::sync::oneshot::channel();
    captures.lock().unwrap().media_gates.push_back(unmute_wait);
    let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
        captures,
        ..FakeBroker::default()
    });
    let mut manager =
        CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", Uuid::new_v4());
    assert!(manager.send(CollabRequest::Document {
        mutation: collab_types::DocumentMutation::Snapshot(b"wedged document".to_vec()),
        authorization: None,
    }));
    tilo_test_env::recv("the gated document RPC", &observed);
    assert!(manager.send(CollabRequest::NativeMediaCommand {
        request_id: 1,
        command: Request::new(Command::SetMuted { muted: false })
    }));
    tilo_test_env::wait_until(
        "`captures.lock().unwrap().media_commands.len() == 1`",
        || captures.lock().unwrap().media_commands.len() == 1,
    );
    assert!(manager.send(CollabRequest::NativeMediaCommand {
        request_id: 2,
        command: Request::new(Command::Leave)
    }));
    tilo_test_env::wait_until(
        "Leave to dispatch past the wedged document and unmute RPCs",
        || captures.lock().unwrap().media_commands.len() == 2,
    );
    // Both are still held: Leave went past them, not after they gave up.
    assert!(
        broker_state(&manager).is_healthy(),
        "the wedged document RPC is still in flight"
    );
    let requests = captures.lock().unwrap().media_commands.clone();
    assert!(matches!(requests[1].command, Command::Leave));
    assert!(requests[1].intent > requests[0].intent);
    let _ = unmute_reply.send(());
    let mut retired = false;
    tilo_test_env::wait_until("superseded unmute must retire its pending request", || {
        retired |= manager.poll().into_iter().any(|event| {
            matches!(
                event,
                CollabEvent::NativeMediaCommandResult {
                    request_id: 1,
                    result: Err(_)
                }
            )
        });
        retired
    });
    release.send(Ok(())).unwrap();
    drop(manager);
    worker.join().unwrap();
}

#[test]
fn native_media_manager_drop_tears_down_while_document_rpc_is_wedged() {
    use runtime_collab::native_media::{Command, Request};
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (entered, observed) = mpsc::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    captures
        .lock()
        .unwrap()
        .document_gates
        .push_back(DocumentSendGate {
            entered,
            release: wait,
        });
    let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
        captures,
        ..FakeBroker::default()
    });
    let mut manager =
        CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", Uuid::new_v4());
    assert!(manager.send(CollabRequest::NativeMediaCommand {
        request_id: 1,
        command: Request::new(Command::Join {
            channel_id: "voice".into()
        })
    }));
    tilo_test_env::wait_until(
        "`captures.lock().unwrap().media_commands.len() == 1`",
        || captures.lock().unwrap().media_commands.len() == 1,
    );
    assert!(manager.send(CollabRequest::Document {
        mutation: collab_types::DocumentMutation::Snapshot(b"wedged document".to_vec()),
        authorization: None,
    }));
    tilo_test_env::recv("the gated document RPC", &observed);
    drop(manager);
    tilo_test_env::wait_until(
        "manager drop must use reserved teardown before document completion",
        || {
            captures
                .lock()
                .unwrap()
                .media_commands
                .iter()
                .any(|request| {
                    request.intent == u64::MAX && matches!(request.command, Command::Leave)
                })
        },
    );
    release.send(Ok(())).unwrap();
    worker.join().unwrap();
}

#[test]
fn native_media_full_normal_lane_still_dispatches_reserved_leave() {
    use runtime_collab::native_media::{Command, Request};
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let mut releases = Vec::new();
    for _ in 0..8 {
        let (release, wait) = tokio::sync::oneshot::channel();
        releases.push(release);
        captures.lock().unwrap().media_gates.push_back(wait);
    }
    let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
        captures,
        ..FakeBroker::default()
    });
    let cluster_id = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", cluster_id);
    let routes = (0..8)
        .map(|index| RouteSpec {
            app_id: "so.tilo.chat".into(),
            cluster_id,
            record: (index != 0).then(|| format!("record-{index}")),
        })
        .collect::<Vec<_>>();
    for route in &routes {
        manager.join_route(
            route.clone(),
            crate::record::RecordKind::new("tilo.rich-text"),
        );
    }
    manager
        .begin_document_publication_barrier()
        .unwrap()
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap();
    for (index, route) in routes.into_iter().enumerate() {
        assert!(
            manager
                .try_send_to_route(
                    route,
                    CollabRequest::NativeMediaCommand {
                        request_id: index as u64 + 1,
                        command: Request::new(Command::SetMuted { muted: false })
                    }
                )
                .is_ok()
        );
    }
    tilo_test_env::wait_until(
        "`captures.lock().unwrap().media_commands.len() == 8`",
        || captures.lock().unwrap().media_commands.len() == 8,
    );
    assert!(
        !manager.send(CollabRequest::NativeMediaCommand {
            request_id: 9,
            command: Request::new(Command::Status)
        }),
        "normal in-flight budget must stay bounded"
    );
    assert!(
        manager.send(CollabRequest::NativeMediaCommand {
            request_id: 10,
            command: Request::new(Command::Leave)
        }),
        "full normal lane cannot block teardown"
    );
    tilo_test_env::wait_until("the reserved Leave to reach the broker", || {
        captures
            .lock()
            .unwrap()
            .media_commands
            .iter()
            .any(|request| matches!(request.command, Command::Leave))
    });
    for release in releases {
        let _ = release.send(());
    }
    drop(manager);
    worker.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_media_newer_effect_cannot_skip_a_queued_stop_obligation() {
    use runtime_collab::native_media::{Command, Request};
    tokio::task::LocalSet::new()
        .run_until(async {
            for leave in [false, true] {
                let captures = Arc::new(std::sync::Mutex::new(Captures {
                    media_active_fixture: true,
                    media_capture_active: true,
                    media_pending_consent: true,
                    media_joined: true,
                    ..Default::default()
                }));
                let (client, _) = fake_client(FakeBroker {
                    captures: captures.clone(),
                    ..FakeBroker::default()
                });
                let channel = client
                    .join_channel("fixture-admitted-route", &harness_presentation())
                    .await
                    .unwrap();
                let (tx, _events) = tokio_mpsc::channel(32);
                let (sender, receiver) =
                    native_media::channel(InboundBridge::new(tx, BridgeState::new()));
                let routes = native_media::Routes::default();
                routes.insert("fixture-admitted-route".into(), channel);
                let worker = native_media::start(receiver, routes);
                // No yield: both real enqueues occur before the lane can run.
                assert!(sender.send(
                    "fixture-admitted-route".into(),
                    1,
                    Request::new(if leave {
                        Command::Leave
                    } else {
                        Command::SetMuted { muted: true }
                    })
                ));
                assert!(sender.send(
                    "fixture-admitted-route".into(),
                    2,
                    Request::new(if leave {
                        Command::SelectInput {
                            input_id: "unavailable".into(),
                        }
                    } else {
                        Command::SetMuted { muted: false }
                    })
                ));
                tilo_test_env::within("the stop command to reach the broker", async {
                    while captures.lock().unwrap().media_commands.is_empty() {
                        tokio::task::yield_now().await;
                    }
                    for _ in 0..10 {
                        tokio::task::yield_now().await;
                    }
                })
                .await;
                let captured = captures.lock().unwrap();
                assert_eq!(
                    captured.media_stops, 1,
                    "latest completion must not erase the earlier stop effect"
                );
                assert!(!captured.media_capture_active);
                if leave {
                    assert!(!captured.media_joined);
                    assert!(!captured.media_pending_consent);
                } else {
                    assert!(
                        captured.media_pending_consent,
                        "later unmute requires fresh consent after actual stop"
                    );
                }
                drop(captured);
                drop(sender);
                drop(worker);
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_media_pending_or_uncertain_stop_blocks_later_effects() {
    use runtime_collab::native_media::{Command, Request};
    tokio::task::LocalSet::new()
        .run_until(async {
            for succeed in [false, true] {
                let captures = Arc::new(std::sync::Mutex::new(Captures {
                    media_active_fixture: true,
                    media_capture_active: true,
                    media_pending_consent: true,
                    media_joined: true,
                    ..Default::default()
                }));
                let (release, gate) = tokio::sync::oneshot::channel();
                captures.lock().unwrap().media_gates.push_back(gate);
                let (client, _) = fake_client(FakeBroker {
                    captures: captures.clone(),
                    ..FakeBroker::default()
                });
                let channel = client
                    .join_channel("fixture-admitted-route", &harness_presentation())
                    .await
                    .unwrap();
                let (tx, mut events) = tokio_mpsc::channel(32);
                let (sender, receiver) =
                    native_media::channel(InboundBridge::new(tx, BridgeState::new()));
                let routes = native_media::Routes::default();
                routes.insert("fixture-admitted-route".into(), channel);
                let worker = native_media::start(receiver, routes);
                assert!(sender.send(
                    "fixture-admitted-route".into(),
                    1,
                    Request::new(Command::SetMuted { muted: true })
                ));
                tilo_test_env::within("the mute command to reach the broker", async {
                    while captures.lock().unwrap().media_commands.is_empty() {
                        tokio::task::yield_now().await;
                    }
                })
                .await;
                assert!(sender.send(
                    "fixture-admitted-route".into(),
                    2,
                    Request::new(Command::SetMuted { muted: false })
                ));
                for _ in 0..20 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    captures.lock().unwrap().media_commands.len(),
                    1,
                    "unacknowledged stop cannot be aborted or bypassed"
                );
                assert!(!captures.lock().unwrap().media_pending_consent);
                if succeed {
                    release.send(()).unwrap();
                } else {
                    drop(release);
                }
                let later = tilo_test_env::within("the later unmute result", async {
                    loop {
                        if let CollabEvent::NativeMediaCommandResult {
                            request_id: 2,
                            result,
                        } = events.recv().await.unwrap().event
                        {
                            break result;
                        }
                    }
                })
                .await;
                if succeed {
                    assert!(later.is_ok());
                    assert!(captures.lock().unwrap().media_pending_consent);
                } else {
                    assert!(later.is_err());
                    assert_eq!(captures.lock().unwrap().media_commands.len(), 1);
                    assert!(
                        !sender.send(
                            "fixture-admitted-route".into(),
                            3,
                            Request::new(Command::SetMuted { muted: false })
                        ),
                        "uncertain stop remains a route barrier"
                    );
                    assert!(
                        sender.send(
                            "fixture-admitted-route".into(),
                            4,
                            Request::new(Command::Leave)
                        ),
                        "reserved teardown remains available"
                    );
                }
                drop(sender);
                drop(worker);
            }
        })
        .await;
}
