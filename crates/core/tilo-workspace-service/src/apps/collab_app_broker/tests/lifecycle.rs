#![allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::wildcard_imports,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::large_futures,
    clippy::await_holding_lock,
    clippy::await_holding_refcell_ref,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
)]

use super::*;

tilo_test_env::scenario! {
    fn a_broker_socket_is_bound_only_in_a_private_parent_over_stale_owned_inodes;

    phase socket_parent_must_be_owner_only_before_any_artifact_is_created;
    {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .expect("make parent deliberately non-private");
        let path = directory.path().join("rejected.sock");

        let error = bind_owned_socket(&path)
            .err()
            .expect("non-private parent must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            std::fs::read_dir(directory.path())
                .expect("inspect rejected parent")
                .next()
                .is_none(),
            "parent rejection must precede socket, lease, or staging artifacts"
        );
    }

    phase socket_ownership_reclaims_only_stale_owned_inodes;
    {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let live_path = lifecycle_socket(&directory, "live");
        let live = std::os::unix::net::UnixListener::bind(&live_path).expect("live socket");
        let error = bind_owned_socket(&live_path)
            .err()
            .expect("live broker socket must not be reclaimed");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert!(socket_identity(&live_path).is_ok());
        drop(live);

        let stale_path = lifecycle_socket(&directory, "stale");
        let stale = std::os::unix::net::UnixListener::bind(&stale_path).expect("stale socket");
        drop(stale);
        let (replacement, ownership) =
            bind_owned_socket(&stale_path).expect("proven stale socket is reclaimed");
        assert!(std::os::unix::net::UnixStream::connect(&stale_path).is_ok());
        let contender_error = bind_owned_socket(&stale_path)
            .err()
            .expect("lifetime lease excludes a competing socket owner");
        assert_eq!(contender_error.kind(), std::io::ErrorKind::AddrInUse);
        drop(replacement);
        drop(ownership);
        assert!(
            !stale_path.exists(),
            "owned socket inode is removed on final ownership release"
        );

        let replaced_path = lifecycle_socket(&directory, "replacement");
        let original =
            std::os::unix::net::UnixListener::bind(&replaced_path).expect("original socket");
        let original_identity = socket_identity(&replaced_path).expect("original identity");
        // Retain the old inode while replacing its pathname: unlinking its last
        // name would let the filesystem reuse that inode for the new socket.
        let displaced_path = lifecycle_socket(&directory, "displaced");
        std::fs::rename(&replaced_path, &displaced_path).expect("retain original socket inode");
        assert_eq!(
            socket_identity(&displaced_path).expect("retained original identity"),
            original_identity
        );
        let replacement =
            std::os::unix::net::UnixListener::bind(&replaced_path).expect("replacement socket");
        assert_ne!(
            socket_identity(&replaced_path).expect("replacement identity"),
            original_identity,
            "the replacement fixture must exercise a distinct live inode"
        );
        assert!(
            !remove_socket_if_owned(&replaced_path, original_identity)
                .expect("replacement identity check"),
            "an old owner must not unlink a replacement socket"
        );
        assert!(std::os::unix::net::UnixStream::connect(&replaced_path).is_ok());
        drop(replacement);
        drop(original);
        std::fs::remove_file(&displaced_path).expect("remove retained original pathname");

        let racing_path = lifecycle_socket(&directory, "publish-race");
        let (staged, stage) = bind_socket_stage(&racing_path).expect("private socket staging");
        let racing =
            std::os::unix::net::UnixListener::bind(&racing_path).expect("racing destination");
        let racing_identity = socket_identity(&racing_path).expect("racing socket identity");
        let error = rename_socket_noreplace(&stage.socket, &racing_path)
            .expect_err("no-replace publication must preserve a racing destination");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert_eq!(
            socket_identity(&racing_path).expect("racing socket survives"),
            racing_identity
        );
        assert!(std::os::unix::net::UnixStream::connect(&racing_path).is_ok());
        drop(staged);
        drop(stage);
        assert_eq!(
            socket_identity(&racing_path).expect("stage cleanup preserves destination"),
            racing_identity
        );
        drop(racing);

        let entries_before_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        let identity_failure_path = lifecycle_socket(&directory, "identity-failure");
        FAIL_STAGED_SOCKET_IDENTITY.with(|fail| fail.set(true));
        assert!(bind_socket_stage(&identity_failure_path).is_err());
        let entries_after_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            entries_after_failure, entries_before_failure,
            "staged metadata failure leaves no socket artifact"
        );

        #[cfg(target_os = "macos")]
        {
            let parent_len = directory.path().as_os_str().as_bytes().len();
            let separator = usize::from(!directory.path().as_os_str().is_empty());
            let leaf_len = 103usize
                .checked_sub(parent_len + separator)
                .expect("temporary path leaves room for a valid Unix socket");
            let long_path = directory.path().join("x".repeat(leaf_len));
            assert_eq!(long_path.as_os_str().as_bytes().len(), 103);
            let (listener, ownership) = bind_owned_socket(&long_path)
                .expect("valid near-limit final path must stage without expansion");
            assert!(std::os::unix::net::UnixStream::connect(&long_path).is_ok());
            drop(listener);
            drop(ownership);
        }
    }
}

tilo_test_env::scenario! {
    fn a_stopped_broker_state_is_inert_and_keeps_what_it_admitted;

    phase stopped_state_releases_journal_and_makes_surviving_facade_inert;
    {
        let directory = tempfile::tempdir().expect("temporary journal directory");
        let fanout = Arc::new(FanoutState::new());
        let config = crate::apps::NativePeerBrokerConfig {
            journal_root: directory.path().join("invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let (state, _bridge_rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_commits(Arc::new(HealthyOwner)),
            Some(WorkspaceTree::default().shared().authority()),
            None,
            Some(&config),
            ProcessServices::default(),
        );
        let facade = crate::apps::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state));
        assert!(
            facade.invocation_host().is_ok(),
            "live facade sees the opened invocation journal"
        );
        let state_for_poison = Arc::clone(&state);
        assert!(
            std::thread::spawn(move || {
                tilo_rt::panic_boundary::isolate(|| {
                    let _journal = state_for_poison.journal.lock().expect("journal lock");
                    panic!("poison journal lock for shutdown recovery");
                })
            })
            .join()
            .unwrap()
            .is_err()
        );

        state.stop();

        assert!(state.is_stopped());
        assert!(facade.invocation_host().is_err());
        assert!(facade.surfaces().is_empty());
        assert!(facade.statuses(&tilo_actions::MemberId::person("owner")).is_empty());
        let reopened = InvocationJournal::open(&config.journal_root)
            .expect("stopped broker releases the invocation-journal writer");
        drop(reopened);
    }

    phase admitted_live_operation_keeps_its_identity_when_shutdown_races_after_effect;
    {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = AppBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let state_for_operation = Arc::clone(&state);
        let (effect_tx, effect_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let operation = std::thread::spawn(move || {
            state_for_operation.while_live(|_| {
                effect_tx.send(()).expect("effect admitted");
                release_rx.recv().expect("release admitted operation");
                Ok(71_u64)
            })
        });
        tilo_test_env::recv("operation reached its admitted effect", &effect_rx);
        state.request_stop();
        release_tx.send(()).expect("release operation");
        assert_eq!(
            operation.join().expect("operation thread"),
            Ok(71),
            "an operation admitted before shutdown keeps its non-retryable identity"
        );
        state.stop();
    }
}

/// An app whose launch the shell holds no record of (one started by hand
/// through tilo-launcher, or one whose record is gone) is refused. It used
/// to be refused by dropping its socket with its first call unread, so the
/// app saw only "Connection reset by peer" and the shell logged nothing it
/// keeps. The app must hear why, as an ordinary failed call.
#[test]
fn an_unattested_launch_is_told_why_it_was_refused() {
    struct NoLaunchDocuments;
    impl crate::apps::services::LauncherServices for NoLaunchDocuments {
        fn applications(&self) -> Vec<crate::apps::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::apps::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("this broker launches nothing".into())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
    }
    let directory = tempfile::tempdir().expect("temporary socket directory");
    let socket = lifecycle_socket(&directory, "unattested");
    let config = crate::apps::NativePeerBrokerConfig {
        journal_root: directory.path().join("unattested-invocations"),
        certificate_authority: Arc::new(LifecycleAuthority),
    };
    let _broker = crate::apps::collab_broker::spawn_with_test_services(
        socket.clone(),
        None,
        PeerPublisher::disabled(),
        Some(&config),
        ProcessServices {
            launcher: Some(Arc::new(NoLaunchDocuments)),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(BrokerChannels::default())),
        Arc::new(FanoutState::new()),
    )
    .expect("test broker");

    // What tilo-launcher writes before handing the descriptor to the app.
    let mut stream = std::os::unix::net::UnixStream::connect(&socket).expect("connect");
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": "so.tilo.calculator",
        "workspace_id": "5ba25f73-87c5-447f-a5a6-880dc4249a81",
    }))
    .expect("handoff json");
    std::io::Write::write_all(&mut stream, &launcher::services::COLLAB_LAUNCH_MAGIC)
        .expect("marker");
    std::io::Write::write_all(
        &mut stream,
        &launcher::services::COLLAB_LAUNCH_FRAMING
            .encode(&handoff)
            .expect("framed handoff"),
    )
    .expect("handoff");
    stream.set_nonblocking(true).expect("nonblocking");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let error = tokio::task::LocalSet::new().block_on(&runtime, async move {
        let stream = tokio::net::UnixStream::from_std(stream).expect("tokio stream");
        let (client, _end) = runtime_collab::CollabClient::connected_local(stream);
        match client
            .workspace_status("5ba25f73-87c5-447f-a5a6-880dc4249a81")
            .await
        {
            Ok(_) => panic!("an unattested launch was answered"),
            Err(error) => error.to_string(),
        }
    });
    assert!(
        error.contains("is not one this computer made"),
        "the refused app must learn why, not see its connection reset: {error}",
    );
    assert!(
        !error.contains("reset by peer"),
        "the refusal arrived as a reset: {error}",
    );
}

/// Attestation is one look at the launch record: a launcher whose host
/// recorded it (before releasing its launch hold) is attested at
/// once, and a process with the marker but no record — someone running
/// tilo-launcher by hand — is refused at once, without a retry window.
/// The clock is paused, so any wait would show as elapsed time.
#[tokio::test(start_paused = true)]
async fn launcher_attestation_decides_on_one_look() {
    use tokio::io::AsyncWriteExt as _;
    const APP: &str = tilo_app_id::apps::CALCULATOR;
    const WORKSPACE: &str = "5ba25f73-87c5-447f-a5a6-880dc4249a81";
    let launches = Arc::new(RecordedLaunches(Mutex::new(Vec::new())));
    let services = ProcessServices {
        launcher: Some(Arc::clone(&launches) as Arc<dyn crate::apps::services::LauncherServices>),
        ..Default::default()
    };
    let (marker, handoff) = launcher_handoff_bytes(APP, WORKSPACE);
    for recorded in [false, true] {
        let (mut broker_end, mut launcher_end) = tokio::net::UnixStream::pair().unwrap();
        if recorded {
            launches.0.lock().expect("records").push((
                std::process::id(),
                APP.to_owned(),
                WORKSPACE.to_owned(),
            ));
        }
        launcher_end.write_all(&marker).await.expect("marker");
        launcher_end.write_all(&handoff).await.expect("handoff");
        let started = tokio::time::Instant::now();
        let attested = authenticate_launcher_handoff(&mut broker_end, &services).await;
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "attestation waited (recorded: {recorded})"
        );
        match attested {
            Ok(handoff) => {
                assert!(recorded, "an unrecorded launcher was attested");
                // The connection names the launch that started it, from
                // the host's record, for the app and workspace it was
                // admitted for.
                let launch = handoff.launch.expect("the recorded launch's ticket");
                assert_eq!(
                    (
                        launch.ticket,
                        launch.app_id.as_str(),
                        launch.workspace.as_str()
                    ),
                    (31, APP, WORKSPACE)
                );
            }
            Err(refusal) => {
                assert!(
                    !recorded,
                    "a recorded launcher was refused: {}",
                    refusal.error
                );
                assert!(
                    refusal
                        .error
                        .to_string()
                        .contains("is not one this computer made"),
                    "{}",
                    refusal.error
                );
            }
        }
    }
}

/// A launch marker that arrives in pieces is read once it is whole —
/// the later bytes wake the read — and the stream is left untouched; one
/// left unfinished is refused after its completion window, and a peer
/// that hangs up mid-marker is refused for hanging up, rather than
/// either holding the connection.
#[tokio::test]
async fn a_launch_marker_in_pieces_wakes_on_its_bytes() {
    use tokio::io::AsyncWriteExt as _;
    let marker = launcher::services::COLLAB_LAUNCH_MAGIC;
    let (head, tail) = marker.split_at(marker.len() / 2);

    let (mut broker_end, mut launcher_end) = tokio::net::UnixStream::pair().unwrap();
    launcher_end.write_all(head).await.expect("first half");
    {
        let reading = has_collab_launch_marker(&mut broker_end);
        tokio::pin!(reading);
        // Let the read see the first half and wait for the rest.
        for _ in 0..4 {
            assert!(futures::poll!(reading.as_mut()).is_pending());
            tokio::task::yield_now().await;
        }
        launcher_end.write_all(tail).await.expect("second half");
        assert!(reading.await.expect("a whole marker"));
    }
    let mut kept = vec![0_u8; marker.len()];
    broker_end.readable().await.expect("readable");
    broker_end
        .try_read(&mut kept)
        .expect("the marker is still unread");
    assert_eq!(kept, marker, "peeking leaves the stream untouched");

    let (mut broker_end, mut launcher_end) = tokio::net::UnixStream::pair().unwrap();
    launcher_end.write_all(head).await.expect("first half");
    let unfinished = has_collab_launch_marker(&mut broker_end).await;
    assert!(
        unfinished
            .expect_err("an unfinished marker is refused")
            .to_string()
            .contains("incomplete protocol header"),
        "an unfinished marker is refused at the end of its completion window"
    );
    drop(launcher_end);

    let (mut broker_end, mut launcher_end) = tokio::net::UnixStream::pair().unwrap();
    launcher_end.write_all(head).await.expect("first half");
    drop(launcher_end);
    assert!(
        has_collab_launch_marker(&mut broker_end)
            .await
            .expect_err("a peer that hung up mid-marker is refused")
            .to_string()
            .contains("closed mid protocol header"),
        "the hang-up is an event, not the end of the completion window"
    );
}

tilo_test_env::scenario! {
    fn a_broker_dropped_from_its_own_callbacks_or_unwound_releases_what_it_held;

    phase public_sink_can_drop_its_broker_without_self_join_or_early_unlink;
    {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "self-drop");
        let fanout = Arc::new(FanoutState::new());
        let config = crate::apps::NativePeerBrokerConfig {
            journal_root: directory.path().join("self-drop-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::apps::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let surviving_facade = broker.app_action_broker().expect("live broker facade");
        let state = fanout.app_broker().expect("attached broker state");
        let holder = Arc::new(Mutex::new(Some(broker)));
        let holder_for_sink = Arc::clone(&holder);
        let socket_for_sink = socket.clone();
        let (sink_done, sink_finished) = std::sync::mpsc::sync_channel(1);
        let subscription = holder
            .lock()
            .expect("broker holder")
            .as_ref()
            .expect("broker")
            .subscribe_workspaces(
                vec!["self-drop-workspace".into()],
                Box::new(move |_, _| {
                    let broker = holder_for_sink
                        .lock()
                        .expect("broker holder in sink")
                        .take()
                        .expect("broker dropped once");
                    drop(broker);
                    assert!(
                        std::os::unix::net::UnixListener::bind(&socket_for_sink).is_err(),
                        "self-drop keeps the owned pathname until this callback returns"
                    );
                    sink_done.send(()).expect("signal sink completion");
                }),
            );
        let fanout_for_callback = Arc::clone(&fanout);
        state.turn_work.hand(Box::new(move |_| {
            fanout_for_callback.fanout_to_route(
                &CollabEvent::FollowChanged { peer_id: None },
                u64::MAX,
                &CrdtRoute::cluster("self-drop-workspace"),
            );
        }));
        tilo_test_env::recv(
            "broker-thread public sink completed without self-join",
            &sink_finished,
        );
        drop(subscription);

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(
            surviving_facade.invocation_host().is_err(),
            "facade retained across thread exit is inert"
        );
        let reopened_journal = InvocationJournal::open(&config.journal_root)
            .expect("thread exit releases the journal before unlink");
        drop(reopened_journal);
        fanout.fanout_to_route(
            &CollabEvent::FollowChanged {
                peer_id: Some(tilo_actions::MemberId::person("stale")),
            },
            u64::MAX,
            &CrdtRoute::cluster("self-drop-workspace"),
        );
        let replacement = crate::apps::collab_broker::spawn_with_test_services(
            socket,
            None,
            PeerPublisher::disabled(),
            Some(&config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("broker restarts after self-drop thread exits");
        drop(replacement);
    }

    phase off_thread_public_sink_can_drop_broker_without_join_deadlock;
    {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "off-thread-drop");
        let fanout = Arc::new(FanoutState::new());
        let broker = crate::apps::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            None,
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let holder = Arc::new(Mutex::new(Some(broker)));
        let holder_for_sink = Arc::clone(&holder);
        let (sink_done, sink_finished) = std::sync::mpsc::sync_channel(1);
        let subscription = holder
            .lock()
            .expect("broker holder")
            .as_ref()
            .expect("broker")
            .subscribe_workspaces(
                vec!["off-thread-workspace".into()],
                Box::new(move |_, _| {
                    let broker = holder_for_sink
                        .lock()
                        .expect("broker holder in sink")
                        .take()
                        .expect("broker dropped once");
                    drop(broker);
                    sink_done.send(()).expect("signal sink completion");
                }),
            );
        let fanout_for_thread = Arc::clone(&fanout);
        let caller = std::thread::spawn(move || {
            fanout_for_thread.fanout_to_route(
                &CollabEvent::FollowChanged { peer_id: None },
                u64::MAX,
                &CrdtRoute::cluster("off-thread-workspace"),
            );
        });
        tilo_test_env::recv(
            "off-thread sink drop joined broker without deadlock",
            &sink_finished,
        );
        caller.join().expect("off-thread fanout caller");
        drop(subscription);

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(!socket.exists());
    }

    phase owner_health_callback_can_drop_broker_without_facade_join_deadlock;
    {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let socket = lifecycle_socket(&directory, "owner-health-drop");
        let journal_root = directory.path().join("owner-health-invocations");
        let holder = Arc::new(Mutex::new(None));
        let publisher = PeerPublisher::with_commits(
            Arc::new(DropBrokerOnHealth {
                broker: Arc::clone(&holder),
            }),
        );
        let membership = WorkspaceTree::default().shared();
        let config = crate::apps::NativePeerBrokerConfig {
            journal_root: journal_root.clone(),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::apps::collab_broker::spawn_with_test_services(
            socket.clone(),
            Some(membership.authority()),
            publisher,
            Some(&config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("test broker");
        let facade = broker.app_action_broker().expect("live broker facade");
        *holder.lock().expect("broker holder") = Some(broker);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            result_tx
                .send(facade.invocation_host())
                .expect("send facade result");
        });
        let result = tilo_test_env::recv(
            "owner health callback returned without join deadlock",
            &result_rx,
        );
        assert!(
            result.is_err(),
            "shutdown requested by the health callback fences its facade result"
        );

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(!socket.exists());
        let reopened = InvocationJournal::open(&journal_root)
            .expect("health callback shutdown releases journal");
        drop(reopened);
    }

    phase facade_guard_does_not_detach_an_unrelated_broker_drop;
    {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let other_socket = lifecycle_socket(&directory, "other");
        let other_journal = directory.path().join("other-invocations");
        let other_config = crate::apps::NativePeerBrokerConfig {
            journal_root: other_journal.clone(),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let other = crate::apps::collab_broker::spawn_with_test_services(
            other_socket.clone(),
            Some(WorkspaceTree::default().shared().authority()),
            PeerPublisher::with_commits(Arc::new(HealthyOwner)),
            Some(&other_config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("other broker");
        let other_holder = Arc::new(Mutex::new(Some(other)));
        let proof = Arc::new(Mutex::new(None));

        let broker_socket = lifecycle_socket(&directory, "guarded");
        let broker_config = crate::apps::NativePeerBrokerConfig {
            journal_root: directory.path().join("guarded-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::apps::collab_broker::spawn_with_test_services(
            broker_socket,
            Some(WorkspaceTree::default().shared().authority()),
            PeerPublisher::with_commits(
                Arc::new(DropOtherBrokerOnHealth {
                    broker: Arc::clone(&other_holder),
                    socket: other_socket,
                    journal_root: other_journal,
                    proof: Arc::clone(&proof),
                }),
            ),
            Some(&broker_config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("guarded broker");
        let facade = broker.app_action_broker().expect("guarded broker facade");

        facade
            .invocation_host()
            .expect("unrelated broker drop does not fence this broker");
        proof
            .lock()
            .expect("other broker proof")
            .take()
            .expect("health callback recorded cleanup")
            .expect("unrelated broker drop synchronously released resources");
        drop(broker);
    }

    phase broker_thread_unwind_releases_journal_and_owned_socket;
    {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let socket = lifecycle_socket(&directory, "unwind");
        let config = crate::apps::NativePeerBrokerConfig {
            journal_root: directory.path().join("unwind-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let fanout = Arc::new(FanoutState::new());
        let broker = crate::apps::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let facade = broker.app_action_broker().expect("live broker facade");
        let state = fanout.app_broker().expect("attached broker state");

        // A failing broker thread is started again on the same socket, and
        // the session's handles to it stay live.
        state
            .turn_work
            .hand(Box::new(|_| panic!("broker-thread unwind fixture")));
        tilo_test_env::wait_until("the broker thread to start again", || {
            state.thread_restarts() == 1
        });
        let (reached, answered) = std::sync::mpsc::channel();
        state.turn_work.hand(Box::new(move |_| {
            let _ = reached.send(tilo_rt::panic_boundary::is_isolated());
        }));
        assert!(
            tilo_test_env::recv("the restarted thread's turn", &answered),
            "the restarted thread runs inside its panic boundary"
        );
        assert!(socket.exists(), "a restart keeps the socket");
        assert!(
            !state.stopped.load(Ordering::Acquire),
            "a restart keeps the broker's state live"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("client runtime");
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::net::UnixStream::connect(&socket)
                .await
                .expect("a client connects after the restart");
        });

        // One that keeps failing stays down and releases what it held.
        for _ in 0..BROKER_THREAD_RESTARTS {
            state
                .turn_work
                .hand(Box::new(|_| panic!("broker-thread unwind fixture")));
            tilo_test_env::wait_until("the broker thread to fail", || {
                !socket.exists() || state.turn_work.handed.lock_recover().is_empty()
            });
        }
        drop(state);

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(!socket.exists(), "unwind releases owned socket ownership");
        assert!(
            facade.invocation_host().is_err(),
            "unwind makes surviving broker facades inert"
        );
        let reopened = InvocationJournal::open(&config.journal_root)
            .expect("unwind releases the invocation journal");
        drop(reopened);
        drop(broker);

        let replacement = crate::apps::collab_broker::spawn_with_test_services(
            socket,
            None,
            PeerPublisher::disabled(),
            Some(&config),
            ProcessServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("broker restarts after broker-thread unwind");
        drop(replacement);
    }
}
