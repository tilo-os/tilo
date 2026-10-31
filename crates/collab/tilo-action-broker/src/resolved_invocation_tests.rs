#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_capacity_is_bounded_and_cancellation_releases_it() {
    struct Held {
        entered: mpsc::UnboundedSender<()>,
        release: Rc<tokio::sync::Notify>,
        row: runtime_collab::ActionDirectoryRow,
    }
    impl wire::DirectorySource for Held {
        fn query(
            &self,
            _query: runtime_collab::ActionDirectoryQuery,
            _max_bytes: u32,
        ) -> wire::Asked<runtime_collab::ActionDirectoryPage> {
            Box::pin(std::future::ready(Err("not listed here".into())))
        }

        fn resolve(
            &self,
            _target: (&str, &str, &str),
            _local_document: bool,
            _max_bytes: u32,
        ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
            let entered = self.entered.clone();
            let release = Rc::clone(&self.release);
            let row = self.row.clone();
            Box::pin(async move {
                entered.send(()).unwrap();
                release.notified().await;
                Ok(runtime_collab::ActionDirectoryResolution::Found {
                    row: Box::new(row),
                    revision: None,
                })
            })
        }
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let (tx, mut entered) = mpsc::unbounded_channel();
            let release = Rc::new(tokio::sync::Notify::new());
            let held = Rc::new(Held {
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

impl wire::DirectorySource for RecordingDirectory {
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
        _local_document: bool,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
        self.revisions.borrow_mut().push(revision.to_owned());
        Box::pin(std::future::ready(Ok(
            runtime_collab::ActionDirectoryResolution::Found {
                row: Box::new(self.row.clone()),
                revision: self.resolved.map(str::to_owned),
            },
        )))
    }
}

/// Serve the fixture's directory through a [`RecordingDirectory`] naming
/// `resolved`, returning the revisions it was asked to resolve.
fn record_resolutions(
    fixture: &LocalDirectoryFixture,
    resolved: Option<&'static str>,
) -> Rc<RefCell<Vec<String>>> {
    let revisions = Rc::new(RefCell::new(Vec::new()));
    let recording = Rc::new(RecordingDirectory {
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
        appearance: None,
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
    tilo_test_env::within("bounded directory admission", async {
        loop {
            drain_directory_bridge(fixture).await;
            if let PendingPoll::Ready(result) = pending.poll_ready() {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
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

/// #3928: an admission that named no revision is bound to the one its
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
                matches!(&refused, Err(ActionHostError::StaleRevision)),
                "{refused:?}"
            );
            assert!(
                fixture.take_invocation_identity().is_none(),
                "an unbound admission never reaches the owner"
            );
            let named = directory_request(&fixture, Some("directory-r1"), None);
            admit_directory_invoke(&fixture, &named)
                .await
                .expect("a named revision binds the admission");
            assert!(fixture.take_invocation_identity().is_some());
        })
        .await;
}

/// #3928: a person's confirmation covers the action as they were shown
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

// A surface sent as deltas of shared catalogue entries installs exactly the
// surface a full publication would, and a delta that does not apply changes
// nothing, so every reader of the surface sees no difference.
#[tokio::test(flavor = "current_thread")]
async fn a_surface_delta_installs_the_same_surface_as_a_full_publication() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use runtime_collab::{SHAPE_ID_BYTES, SurfaceDelta, SurfaceResource};
            use signed_worker::{OriginalRoom, directory_state};
            let room =
                OriginalRoom::new(tilo_user_identity::DeviceCapabilities::host(), None, None);
            let (worker, _authorize) = room.join_worker(905).await;
            let handle = &worker.handle;
            let mut base = directory_state("original-r1")
                .borrow()
                .row()
                .descriptor
                .unwrap();
            base.view = None;
            let (shape_id, template) = ([5; SHAPE_ID_BYTES], [6; SHAPE_ID_BYTES]);
            let resources = ["workspace:a", "workspace:b", "workspace:c"];
            let expanded = |resource: &str, label: &str| runtime_collab::ExportedAction {
                state: tilo_actions::ActionState::default(),
                resource: resource.into(),
                resource_label: label.into(),
                semantic_identity: None,
                ..base.clone()
            };
            let resource =
                |name: &str, label: &str, template: [u8; SHAPE_ID_BYTES]| SurfaceResource {
                    resource: name.into(),
                    fields: runtime_collab::ResourceFields {
                        resource_label: label.into(),
                        semantic_identity: None,
                        ..base.resource_fields()
                    },
                    template,
                };
            let surface = || {
                room.state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get(&905)
                    .map(|surface| surface.actions.clone())
            };

            let first = SurfaceDelta {
                reset: true,
                shapes: vec![(shape_id, base.shape())],
                templates: vec![(template, vec![shape_id])],
                resources: resources
                    .iter()
                    .map(|r| resource(r, "Page", template))
                    .collect(),
                removed: Vec::new(),
            };
            handle.publish_action_surface(&first).await.unwrap();
            assert_eq!(
                surface().unwrap(),
                resources
                    .iter()
                    .map(|r| expanded(r, "Page"))
                    .collect::<Vec<_>>()
            );

            // A later delta names only what changed; the catalogue is not
            // sent again.
            let edit = SurfaceDelta {
                resources: vec![resource("workspace:b", "Renamed", template)],
                removed: vec!["workspace:c".into()],
                ..SurfaceDelta::default()
            };
            handle.publish_action_surface(&edit).await.unwrap();
            assert_eq!(
                surface().unwrap(),
                vec![
                    expanded("workspace:a", "Page"),
                    expanded("workspace:b", "Renamed")
                ]
            );

            // A delta that does not apply is refused and changes nothing.
            let unknown = SurfaceDelta {
                resources: vec![resource("workspace:z", "Z", [9; SHAPE_ID_BYTES])],
                ..SurfaceDelta::default()
            };
            assert!(handle.publish_action_surface(&unknown).await.is_err());
            let invalid = SurfaceDelta {
                reset: true,
                shapes: vec![(shape_id, base.shape())],
                templates: vec![(template, vec![shape_id])],
                resources: vec![resource("workspace:a", " ", template)],
                removed: Vec::new(),
            };
            assert!(handle.publish_action_surface(&invalid).await.is_err());
            assert_eq!(surface().unwrap().len(), 2);

            // A whole-surface publication supersedes the delta state: a delta
            // that leans on an earlier template is refused, a reset is
            // accepted.
            handle
                .publish_actions(&[expanded("workspace:only", "Only")])
                .await
                .unwrap();
            assert!(handle.publish_action_surface(&edit).await.is_err());
            handle.publish_action_surface(&first).await.unwrap();
            assert_eq!(surface().unwrap().len(), 3);
        })
        .await;
}
