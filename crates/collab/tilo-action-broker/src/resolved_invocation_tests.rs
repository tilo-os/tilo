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
                    &mut membership.write().workspaces[0].capabilities,
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
                membership.write().workspaces[0].capabilities = caps;
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
        descriptor.contract.permission = tilo_actions::PermissionRequirement::Write;
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

/// A directory owner that records the revision each resolution asked for
/// and answers the fixture row, naming `resolved` as the revision it
/// resolved at (`None`: an owner built before it named one).
struct RecordingDirectory {
    revisions: Rc<RefCell<Vec<String>>>,
    row: runtime_collab::ActionDirectoryRow,
    resolved: Option<&'static str>,
}

impl action_directory::Server for RecordingDirectory {
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
        let mut out = results.get().init_resolution();
        if let Err(error) =
            runtime_collab::write_action_directory_row(out.reborrow().init_found(), &self.row)
        {
            return Promise::err(error);
        }
        if let Some(resolved) = self.resolved {
            out.set_resolved_revision(resolved);
        }
        Promise::ok(())
    }
}

/// Serve the fixture's directory through a [`RecordingDirectory`] naming
/// `resolved`, returning the revisions it was asked to resolve.
fn record_resolutions(
    fixture: &LocalDirectoryFixture,
    resolved: Option<&'static str>,
) -> Rc<RefCell<Vec<String>>> {
    let revisions = Rc::new(RefCell::new(Vec::new()));
    let recording = capnp_rpc::new_client(RecordingDirectory {
        revisions: Rc::clone(&revisions),
        row: fixture.directory_state.borrow().row(),
        resolved,
    });
    fixture
        .directories
        .directories
        .borrow_mut()
        .get_mut(&fixture.peer_id)
        .unwrap()
        .directory = recording;
    revisions
}

/// A fixture whose broker journals admissions under `root`.
fn journaled_directory_fixture(root: &std::path::Path) -> LocalDirectoryFixture {
    let base = local_directory_fixture();
    let membership = base.membership.as_ref().unwrap().clone();
    membership.write().persist_path = Some(root.join("state.json"));
    drop(base);
    local_directory_fixture_with_membership(membership)
}

/// The fixture's directory action, named at `revision`.
fn directory_request(
    fixture: &LocalDirectoryFixture,
    revision: Option<&str>,
    confirmation: Option<tilo_actions::remote::Confirmation>,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: fixture.peer_id.to_string(),
            address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
        },
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation,
        revision: revision.map(str::to_owned),
        on_behalf_of: None,
        wait_ms: None,
    }
}

/// Start `request` as the fixture's owner and drive the directory bridge
/// until the broker answers.
async fn admit_directory_invoke(
    fixture: &LocalDirectoryFixture,
    request: &tilo_actions::remote::InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError> {
    use tilo_actions::remote::{Caller, PendingPoll};
    let caller = Caller::local_person("fixture-owner")
        .in_room(fixture.cluster_id.clone())
        .in_new_execution();
    let mut pending = fixture.state.action_invoke(&caller, None, None, request);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            drain_directory_bridge(fixture).await;
            if let PendingPoll::Ready(result) = pending.poll_ready() {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded directory admission")
}

/// An invoke naming no revision asks the directory's owner to resolve the
/// action as the directory is now (an empty revision), in the same turn the
/// broker admits it, so the caller need not read the revision first. A
/// revision named empty is still refused.
#[tokio::test(flavor = "current_thread")]
async fn a_directory_invoke_naming_no_revision_resolves_the_directory_as_it_is_now() {
    use tilo_actions::remote::{ActionHostError, Caller, PendingPoll};
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = local_directory_fixture();
            let revisions = record_resolutions(&fixture, Some("r9"));
            let caller = Caller::local_person("fixture-owner")
                .in_room(fixture.cluster_id.clone())
                .in_new_execution();
            let mut request = directory_request(&fixture, Some(""), None);
            assert!(matches!(
                fixture
                    .state
                    .action_invoke(&caller, None, None, &request)
                    .poll_ready(),
                PendingPoll::Ready(Err(ActionHostError::InvalidRequest { .. }))
            ));
            request.revision = None;
            admit_directory_invoke(&fixture, &request)
                .await
                .expect("admitted against the directory as it is now");
            assert_eq!(*revisions.borrow(), [String::new()]);
            let (_, delivered) = fixture
                .take_invocation_identity()
                .expect("the owner receives the invocation");
            assert_eq!(delivered, request.invocation);
        })
        .await;
}

/// TIL-652: an admission that named no revision is bound to the one its
/// owner resolved it at, so the journal records which directory state the
/// action was admitted against and a host worker can refuse it once that
/// state moved on.
#[tokio::test(flavor = "current_thread")]
async fn revisionless_admission_records_the_resolved_revision() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let mut fixture = journaled_directory_fixture(root.path());
            let revisions = record_resolutions(&fixture, Some("r9"));
            let request = directory_request(&fixture, None, None);
            admit_directory_invoke(&fixture, &request)
                .await
                .expect("admitted against the directory as it is now");
            assert_eq!(*revisions.borrow(), [String::new()]);
            let (wire_id, _) = fixture
                .take_invocation_identity()
                .expect("the owner receives the invocation");
            let snapshot =
                invocation_journal::durable_snapshot(&root.path().join("state.json.invocations"));
            let records = snapshot["records"].as_array().unwrap();
            assert_eq!(records.len(), 1, "{snapshot}");
            assert_eq!(records[0]["wire_id"], wire_id);
            assert_eq!(
                records[0]["intent"]["revision"], "r9",
                "the journal names the revision the owner resolved at: {snapshot}"
            );
        })
        .await;
}

/// An owner built before resolutions named their revision resolves an empty
/// one, but leaves nothing to bind the admission to: the broker refuses it
/// as stale rather than admitting it unbound.
#[tokio::test(flavor = "current_thread")]
async fn a_revisionless_invoke_an_owner_cannot_bind_is_refused() {
    use tilo_actions::remote::ActionHostError;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = local_directory_fixture();
            record_resolutions(&fixture, None);
            let request = directory_request(&fixture, None, None);
            let refused = admit_directory_invoke(&fixture, &request).await;
            assert!(
                matches!(
                    &refused,
                    Err(ActionHostError::InvalidRequest { reason })
                        if reason.contains("changed since that revision")
                ),
                "{refused:?}"
            );
            assert!(
                fixture.take_invocation_identity().is_none(),
                "an unbound admission never reaches the owner"
            );
            // The same owner admits the action at a revision the caller names.
            let named = directory_request(&fixture, Some("directory-r1"), None);
            admit_directory_invoke(&fixture, &named)
                .await
                .expect("a named revision binds the admission");
            assert!(fixture.take_invocation_identity().is_some());
        })
        .await;
}

/// TIL-652: a person's confirmation covers the action as they were shown
/// it, which only the revision it was shown at identifies. A directory
/// action that asks for confirmation is refused without one; with one it is
/// admitted, and so is an action asking no confirmation without one.
#[tokio::test(flavor = "current_thread")]
async fn confirmed_directory_action_needs_a_revision() {
    use tilo_actions::remote::{ActionHostError, CONFIRMED_ACTION_NEEDS_REVISION, Confirmation};
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let mut fixture = journaled_directory_fixture(root.path());
            fixture.set_confirmation_required(true);
            let unbound = directory_request(&fixture, None, Some(Confirmation::Confirmed));
            let refused = admit_directory_invoke(&fixture, &unbound).await;
            assert!(
                matches!(
                    &refused,
                    Err(ActionHostError::InvalidRequest { reason })
                        if reason == CONFIRMED_ACTION_NEEDS_REVISION
                ),
                "{refused:?}"
            );
            assert!(fixture.take_invocation_identity().is_none());

            let bound = directory_request(
                &fixture,
                Some("directory-r1"),
                Some(Confirmation::Confirmed),
            );
            admit_directory_invoke(&fixture, &bound)
                .await
                .expect("a confirmed action at the revision it was shown at");
            assert!(fixture.take_invocation_identity().is_some());

            fixture.set_confirmation_required(false);
            let plain = directory_request(&fixture, None, None);
            admit_directory_invoke(&fixture, &plain)
                .await
                .expect("an action asking no confirmation acts on the directory as it is now");
            assert!(fixture.take_invocation_identity().is_some());
            let snapshot =
                invocation_journal::durable_snapshot(&root.path().join("state.json.invocations"));
            let records = snapshot["records"].as_array().unwrap();
            assert_eq!(records.len(), 2, "{snapshot}");
            assert!(
                records
                    .iter()
                    .all(|record| record["intent"]["revision"] == "directory-r1"),
                "every directory admission is bound to a revision: {snapshot}"
            );
        })
        .await;
}
