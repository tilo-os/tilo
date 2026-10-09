// Genuine signed RPC and original worker custody for revision-bound lazy invocation.
#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_rpc_retains_actual_original_and_exact_worker() {
    use signed_worker::{HeldHostDirectory, OriginalRoom, directory_server, directory_state};
    async fn scenario(capabilities: tilo_user_identity::DeviceCapabilities, final_case: &str) {
        let room = OriginalRoom::new(
            capabilities,
            (final_case == "certificate").then_some(75_000_000_000),
            None,
        );
        let OriginalRoom {
            cert,
            origin,
            time_control,
            route,
            membership,
            state,
            authority,
            ..
        } = &room;
        let (worker, authorize) = room.join_worker(903).await;
        let handle = &worker.handle;
        let directory_state = directory_state("original-r1");
        handle
            .publish_action_directory(&route.app_id, directory_server(&directory_state))
            .await
            .unwrap();
        assert!(
            handle
                .invoke_published_action(
                    903,
                    "workspace:atlas",
                    "open",
                    Some("original argument"),
                    "{}"
                )
                .await
                .is_err(),
            "the genuine lazy-only action must not enter via eager @21"
        );
        let invocations = handle.subscribe_stable_invocations().await.unwrap();
        let (id, _) = handle
            .invoke_resolved_published_action(
                903,
                "original-r1",
                "workspace:atlas",
                "open",
                Some("original argument"),
                "{}",
            )
            .await
            .unwrap();
        let delivered = invocations.read().await.unwrap();
        assert_eq!(delivered.id, id);
        let request = ui::invocation_scope::PublishedInvocationRequest {
            wire_id: id,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                delivered.invocation_uuid,
            )),
            address: tilo_actions::ActionAddress {
                resource: delivered.resource,
                action: delivered.action,
            },
            argument: delivered.argument,
            actor: delivered.actor,
            participant: None,
        };
        let lease = authorize(&request)
            .expect("actual signed worker and original RPC caller must mint the exact lease");
        lease.check().unwrap();
        let mut altered = request.clone();
        altered.argument = Some("substituted".into());
        assert!(authorize(&altered).is_err());
        let retained = state.pending_invocations.lock().unwrap().len();
        for (resource, action, revision) in [
            ("wrong", "open", "original-r1"),
            ("workspace:atlas", "wrong", "original-r1"),
        ] {
            assert!(
                handle
                    .invoke_resolved_published_action(903, revision, resource, action, None, "{}")
                    .await
                    .is_err()
            );
        }
        directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Stale;
        assert!(
            handle
                .invoke_resolved_published_action(
                    903,
                    "stale-r0",
                    "workspace:atlas",
                    "open",
                    None,
                    "{}"
                )
                .await
                .is_err()
        );
        directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Found;
        for case in [
            "directory replacement",
            "revoked certificate",
            "lost membership",
            "oversized reply",
            "timeout",
        ] {
            let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
            let release = Rc::new(tokio::sync::Notify::new());
            let mut row = directory_state.borrow().row();
            if case == "oversized reply" {
                row.descriptor.as_mut().unwrap().action_label = "x".repeat(300 * 1024);
            }
            let held = capnp_rpc::new_client(HeldHostDirectory {
                entered: entered_tx,
                release: release.clone(),
                row,
            });
            handle
                .publish_action_directory(&route.app_id, held)
                .await
                .unwrap();
            let pending_handle = handle.clone();
            let resolving = tokio::task::spawn_local(async move {
                pending_handle
                    .invoke_resolved_published_action(
                        903,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                        None,
                        "{}",
                    )
                    .await
            });
            assert_eq!(entered_rx.recv().await, Some("resolve"));
            let old_caps = if case == "lost membership" {
                Some(std::mem::take(
                    &mut membership.write().workspaces[0].policy.grants,
                ))
            } else {
                None
            };
            if case == "revoked certificate" {
                *authority.certificate.lock().unwrap() = None;
            }
            if case == "directory replacement" {
                handle
                    .publish_action_directory(&route.app_id, directory_server(&directory_state))
                    .await
                    .unwrap();
            }
            if case != "timeout" {
                release.notify_one();
            }
            assert!(
                resolving.await.unwrap().is_err(),
                "{case} accepted after awaited resolution"
            );
            assert_eq!(
                state.pending_invocations.lock().unwrap().len(),
                retained,
                "{case} allocated an invocation"
            );
            if let Some(caps) = old_caps {
                membership.write().workspaces[0].policy.grants = caps;
            }
            if case == "revoked certificate" {
                *authority.certificate.lock().unwrap() = Some(cert.clone());
            }
            handle
                .publish_action_directory(&route.app_id, directory_server(&directory_state))
                .await
                .unwrap();
        }
        let (control, _) = handle
            .invoke_resolved_published_action(
                903,
                "original-r1",
                "workspace:atlas",
                "open",
                None,
                "{}",
            )
            .await
            .unwrap();
        assert_eq!(
            invocations.read().await.unwrap().id,
            control,
            "later valid control must progress after refused/cancelled resolutions"
        );
        state
            .pending_invocations
            .lock()
            .unwrap()
            .remove(&control)
            .unwrap();
        let (second, authorize2) = room.join_worker(904).await;
        assert_eq!(
            state.is_current_host_worker(904, route, cert).unwrap(),
            cert.capabilities.can_act_as_host,
            "private registration preserves the actual certificate role"
        );
        assert!(
            authorize2(&request).is_err(),
            "same route and cert on a second actual socket is not the original worker"
        );
        // The eager path on the second worker binds its lease to the exact
        // original request as well.
        let mut descriptor = directory_state.borrow().row().descriptor.unwrap();
        descriptor.contract.kind = tilo_actions::ActionKind::Undoable;
        second
            .handle
            .publish_actions(&route.app_id, &[descriptor])
            .await
            .unwrap();
        let eager_invocations = second.handle.subscribe_stable_invocations().await.unwrap();
        let (eager_id, _) = second
            .handle
            .invoke_published_action(
                904,
                "workspace:atlas",
                "open",
                Some("original argument"),
                "{}",
            )
            .await
            .unwrap();
        let delivered = eager_invocations.read().await.unwrap();
        assert_eq!(delivered.id, eager_id);
        let eager_request = ui::invocation_scope::PublishedInvocationRequest {
            wire_id: eager_id,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                delivered.invocation_uuid,
            )),
            address: tilo_actions::ActionAddress {
                resource: delivered.resource,
                action: delivered.action,
            },
            argument: delivered.argument,
            actor: delivered.actor,
            participant: None,
        };
        let eager_lease = authorize2(&eager_request)
            .expect("actual signed worker and original eager caller must mint the exact lease");
        eager_lease.check().unwrap();
        let mut altered = eager_request.clone();
        altered.argument = Some("substituted".into());
        assert!(authorize2(&altered).is_err());
        state
            .pending_invocations
            .lock()
            .unwrap()
            .remove(&eager_id)
            .unwrap();
        // Captured immutable facts survive terminal registry removal; a later
        // unrelated pending invocation cannot overwrite their origin.
        state
            .pending_invocations
            .lock()
            .unwrap()
            .remove(&id)
            .unwrap();
        lease.check().unwrap();
        assert!(
            authorize(&request).is_err(),
            "new captures require retained original pending custody"
        );
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let release = Rc::new(tokio::sync::Notify::new());
        handle
            .publish_action_directory(
                &route.app_id,
                capnp_rpc::new_client(HeldHostDirectory {
                    entered: entered_tx,
                    release: release.clone(),
                    row: directory_state.borrow().row(),
                }),
            )
            .await
            .unwrap();
        let pending_handle = if final_case == "target" {
            second.handle.clone()
        } else {
            handle.clone()
        };
        let resolving = tokio::task::spawn_local(async move {
            pending_handle
                .invoke_resolved_published_action(
                    903,
                    "original-r1",
                    "workspace:atlas",
                    "open",
                    None,
                    "{}",
                )
                .await
        });
        assert_eq!(entered_rx.recv().await, Some("resolve"));
        if final_case == "origin" {
            origin.invalidate();
        } else if final_case == "target" {
            worker.server.abort();
            tokio::task::yield_now().await;
        } else {
            time_control.set_wall_ns(10_000_000_000);
            time_control.advance_monotonic(std::time::Duration::from_secs(
                if final_case == "certificate" { 30 } else { 80 },
            ));
        }
        release.notify_one();
        assert!(
            resolving.await.unwrap().is_err(),
            "original {final_case} invalidated during await must refuse"
        );
        assert!(state.pending_invocations.lock().unwrap().is_empty());
        assert!(
            lease.check().is_err(),
            "original {final_case} cannot be revived by wall rollback"
        );
        if matches!(final_case, "grant" | "certificate") {
            assert!(
                eager_lease.check().is_err(),
                "eager original {final_case} expiry cannot be revived by wall rollback"
            );
        }
        origin.invalidate();
        assert!(lease.check().is_err());
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            scenario(tilo_user_identity::DeviceCapabilities::host(), "grant").await;
            scenario(
                tilo_user_identity::DeviceCapabilities::host(),
                "certificate",
            )
            .await;
            scenario(tilo_user_identity::DeviceCapabilities::primary(), "origin").await;
            scenario(tilo_user_identity::DeviceCapabilities::primary(), "target").await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_capacity_is_bounded_and_cancellation_releases_it() {
    struct Held {
        entered: mpsc::UnboundedSender<()>,
        release: Rc<tokio::sync::Notify>,
        row: runtime_collab::ActionDirectoryRow,
    }
    impl action_directory::Server for Held {
        fn resolve(
            self: Rc<Self>,
            _: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            Promise::from_future(async move {
                self.entered.send(()).unwrap();
                self.release.notified().await;
                runtime_collab::write_action_directory_row(
                    results.get().init_resolution().init_found(),
                    &self.row,
                )
            })
        }
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let (tx, mut entered) = mpsc::unbounded_channel();
            let release = Rc::new(tokio::sync::Notify::new());
            let held = capnp_rpc::new_client(Held {
                entered: tx,
                release,
                row: fixture.directory_state.borrow().row(),
            });
            fixture
                .directories
                .directories
                .borrow_mut()
                .get_mut(&fixture.peer_id)
                .unwrap()
                .directory = held;
            let mut requests = vec![];
            for _ in 0..64 {
                let state = fixture.state.clone();
                let directories = fixture.directories.clone();
                let cluster = fixture.cluster_id.clone();
                let peer = fixture.peer_id;
                requests.push(tokio::task::spawn_local(async move {
                    state
                        .resolve_invocation_offer(
                            &directories,
                            &BrokerActor::person("fixture-owner"),
                            &cluster,
                            peer,
                            "directory-r1",
                            "workspace:atlas",
                            "open",
                        )
                        .await
                }));
            }
            for _ in 0..64 {
                entered.recv().await.unwrap();
            }
            let refused = fixture
                .state
                .resolve_invocation_offer(
                    &fixture.directories,
                    &BrokerActor::person("fixture-owner"),
                    &fixture.cluster_id,
                    fixture.peer_id,
                    "directory-r1",
                    "workspace:atlas",
                    "open",
                )
                .await;
            assert_eq!(
                refused.err().unwrap(),
                "resolved invocation capacity exhausted"
            );
            assert!(fixture.state.pending_invocations.lock().unwrap().is_empty());
            for request in requests {
                request.abort();
                let _ = request.await;
            }
            assert_eq!(fixture.directories.resolved_permits.available_permits(), 64);
            fixture.reconnect_directory();
            let valid = fixture
                .state
                .resolve_invocation_offer(
                    &fixture.directories,
                    &BrokerActor::person("fixture-owner"),
                    &fixture.cluster_id,
                    fixture.peer_id,
                    "directory-r1",
                    "workspace:atlas",
                    "open",
                )
                .await
                .unwrap();
            assert_eq!(fixture.directories.resolved_permits.available_permits(), 63);
            drop(valid);
            assert_eq!(fixture.directories.resolved_permits.available_permits(), 64);
        })
        .await;
}

/// An invoke naming no revision asks the directory's owner to resolve the
/// action as the directory is now (an empty revision), in the same turn the
/// broker admits it, so the caller need not read the revision first. A
/// revision named empty is still refused.
#[tokio::test(flavor = "current_thread")]
async fn a_directory_invoke_naming_no_revision_resolves_the_directory_as_it_is_now() {
    use tilo_actions::remote::{
        ActionHostError, Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
    };
    struct Recording {
        revisions: Rc<RefCell<Vec<String>>>,
        row: runtime_collab::ActionDirectoryRow,
    }
    impl action_directory::Server for Recording {
        fn resolve(
            self: Rc<Self>,
            params: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            let revision = params.get().and_then(|request| {
                request
                    .get_revision()?
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            });
            match revision {
                Ok(revision) => self.revisions.borrow_mut().push(revision),
                Err(error) => return Promise::err(error),
            }
            match runtime_collab::write_action_directory_row(
                results.get().init_resolution().init_found(),
                &self.row,
            ) {
                Ok(()) => Promise::ok(()),
                Err(error) => Promise::err(error),
            }
        }
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = local_directory_fixture();
            let revisions = Rc::new(RefCell::new(Vec::new()));
            let recording = capnp_rpc::new_client(Recording {
                revisions: Rc::clone(&revisions),
                row: fixture.directory_state.borrow().row(),
            });
            fixture
                .directories
                .directories
                .borrow_mut()
                .get_mut(&fixture.peer_id)
                .unwrap()
                .directory = recording;
            let caller = Caller::local_person("fixture-owner")
                .in_room(fixture.cluster_id.clone())
                .in_new_execution();
            let mut request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: fixture.peer_id.to_string(),
                    address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: Some(String::new()),
                on_behalf_of: None,
                wait_ms: None,
            };
            assert!(matches!(
                fixture
                    .state
                    .action_invoke(&caller, None, None, &request)
                    .poll_ready(),
                PendingPoll::Ready(Err(ActionHostError::InvalidRequest { .. }))
            ));
            request.revision = None;
            let mut pending = fixture.state.action_invoke(&caller, None, None, &request);
            let admitted = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    drain_directory_bridge(&fixture).await;
                    if let PendingPoll::Ready(result) = pending.poll_ready() {
                        break result;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("bounded directory admission");
            admitted.expect("admitted against the directory as it is now");
            assert_eq!(*revisions.borrow(), [String::new()]);
            let (_, delivered) = fixture
                .take_invocation_identity()
                .expect("the owner receives the invocation");
            assert_eq!(delivered, request.invocation);
        })
        .await;
}
