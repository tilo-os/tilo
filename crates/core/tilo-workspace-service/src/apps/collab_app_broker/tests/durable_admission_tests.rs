//! Durable admission: what the invocation journal keeps of an admitted
//! invocation, and the ownership a certified caller is held to.
mod peer_integration {
    include!("../../peer_action_integration_tests.rs");
}
use super::super::invocation_journal::BarrierFailure;
use super::*;

async fn update_app(
    client: &runtime_collab::ChannelHandle,
    id: u64,
    update: &tilo_actions::InvocationUpdate,
) -> Result<(), runtime_portal::NotAvailable> {
    client
        .update_invocation(id, &serde_json::to_string(update).unwrap())
        .await
}

const FULL_ID: u128 = 0xa5a5_a5a5_a5a5_a5a5_0000_0000_0000_002b;

struct DurableFixture {
    directory: LocalDirectoryFixture,
    _identity_env: tilo_test_env::ScopedEnv,
}

impl std::ops::Deref for DurableFixture {
    type Target = LocalDirectoryFixture;

    fn deref(&self) -> &Self::Target {
        &self.directory
    }
}

impl std::ops::DerefMut for DurableFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.directory
    }
}

fn durable_fixture(
    root: &std::path::Path,
    membership: Option<WorkspaceTreeHandle>,
) -> (DurableFixture, VerifiedActionCaller) {
    let mut identity_env = tilo_test_env::ScopedEnv::new();
    identity_env
        .set("XDG_CONFIG_HOME", root.join("config"))
        .set("TILO_IDENTITY_ROOT", root.join("identity"))
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity::provisioning::ensure_collaboration_identity(
        &root.join("config/tilo/identity"),
        &tilo_device_identity::IdentityPaths::under(root.join("identity")),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("provision the host admission and terminal receipt signer");
    let root_key = SigningKey::from_bytes(&[51; 32]);
    let device = SigningKey::from_bytes(&[52; 32]);
    let membership = membership.unwrap_or_else(|| {
        let mut tree = WorkspaceTree::default();
        let mut workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(71),
            "Durable workspace",
        );
        workspace.set_owner(person_id_for_root(root_key.verifying_key().to_bytes()));
        tree.workspaces.push(workspace);
        tree.set_active(crate::apps::test_support::WorkspaceId::from_u128(71));
        tree.persist_path = Some(root.join("state.json"));
        tree.shared()
    });
    let fixture = local_directory_fixture_with_devices(
        membership,
        Some(71),
        Some(Arc::new(vec![
            device.verifying_key().to_bytes(),
            SigningKey::from_bytes(&[53; 32]).verifying_key().to_bytes(),
        ])),
    );
    let mut action = published_directory_action(&fixture.directory_state.borrow().row()).unwrap();
    action.operation_kind = tilo_actions::OperationKind::Effect;
    action.contract = tilo_actions::ActionContract {
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Job,
        ..tilo_actions::ActionContract::default()
    };
    assert!(action.contract.validate_declaration().is_empty());
    fixture.state.publishers.surfaces.lock().unwrap().insert(
        71,
        PublishedSurface {
            peer_id: 71,
            app_id: fixture.route.app_id.clone(),
            route: fixture.route.clone(),
            actions: vec![action],
            live: true,
        },
    );
    fixture
        .state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(71);
    let caller = fixture
        .state
        .verify_action_caller(
            &fixture.cluster_id,
            &certified_join(&fixture.cluster_id, &root_key, &device)
                .encode()
                .unwrap(),
        )
        .unwrap();
    (
        DurableFixture {
            directory: fixture,
            _identity_env: identity_env,
        },
        caller,
    )
}

/// The invocation the fixture's caller asks for.
fn invocation() -> tilo_actions::InvocationId {
    tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID))
}

/// Count one run of the fixture's real effect, durably.
fn effect(root: &std::path::Path) {
    use std::io::Write;
    let path = root.join("effect-counter");
    let before = std::fs::read_to_string(&path).map_or(0, |s| s.parse::<u64>().unwrap());
    let mut file = std::fs::File::create(path).unwrap();
    write!(file, "{}", before + 1).unwrap();
    file.sync_all().unwrap();
    std::fs::File::open(root).unwrap().sync_all().unwrap();
}

/// Admit [`invocation`] as `caller`'s person through the broker's admission, and
/// answer the wire id the app is handed it under.
fn admit(fixture: &mut DurableFixture, caller: &VerifiedActionCaller) -> u64 {
    let surface = fixture.state.publishers.surfaces.lock().unwrap()[&71].clone();
    let action = surface.actions[0].clone();
    let actor = caller.admitted.broker_actor();
    fixture
        .state
        .dispatch_invocation_from_offered_owned(
            71,
            (surface, action),
            InvocationInputs {
                original_request: None,
                requested: Some(invocation()),
                caller: None,
                execution: None,
                argument: None,
                actor: &actor,
                confirmation_token: None,
                context: None,
            },
            AdmissionProvenance {
                revision: None,
                origin: None,
                retained_time: None,
            },
        )
        .unwrap()
        .start_blocking(&fixture.state)
        .unwrap();
    fixture.invocation_rx.try_recv().unwrap().id
}

/// An agent, a device admin or another device drives an app through
/// its actions. The host learns of each invocation the app is handed,
/// so a window the app closes because of it is not taken for one it
/// lost.
#[tokio::test(flavor = "current_thread")]
async fn the_host_learns_each_invocation_an_app_is_handed() {
    struct Recording(Mutex<Vec<(String, String)>>);
    impl crate::apps::services::LauncherServices for Recording {
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
            Err("this host launches nothing".into())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
        fn note_acted_on(&self, app: &str, workspace: &str) {
            self.0.lock().unwrap().push((app.into(), workspace.into()));
        }
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let recording = Arc::new(Recording(Mutex::new(Vec::new())));
            let services = ProcessServices {
                launcher: Some(recording.clone()),
                ..Default::default()
            };
            let (tx, rx) = mpsc::unbounded_channel();
            let stream = InvocationStreamServer {
                rx: Rc::new(AsyncMutex::new(rx)),
                delivered: invocation_delivery_notice(
                    &services,
                    "so.tilo.notes".into(),
                    "workspace-7".into(),
                ),
            };
            assert!(recording.0.lock().unwrap().is_empty());
            tx.send(InvocationEnvelope {
                id: 1,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(1)),
                resource: "window".into(),
                action: "close".into(),
                argument: None,
                actor: tilo_actions::Actor::Member(tilo_actions::MemberId::agent("assistant")),
                context: None,
            })
            .unwrap();
            stream.next().await.unwrap();
            assert_eq!(
                *recording.0.lock().unwrap(),
                vec![("so.tilo.notes".to_owned(), "workspace-7".to_owned())]
            );
            assert!(
                invocation_delivery_notice(&ProcessServices::default(), "a".into(), "r".into())
                    .is_none(),
                "a host without a launcher has no one to tell"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_invalid_updates_never_persist() {
    tokio::task::LocalSet::new().run_until(async {
        let root = tempfile::tempdir().unwrap();
        let (mut fixture, caller) = durable_fixture(root.path(), None);
        let channel = fixture.invocation_channel();
        let wire_id = admit(&mut fixture, &caller);
        let foreign = wire_channel(JoinedChannel {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: fixture.state.clone(), directories: fixture.directories.clone(),
            channel_id: fixture.route.channel_key(), peer_id: 999, connection_id: 999,
            verified_identity: caller.admitted.clone(),
            resource_type: None,
            route_access: RouteAccess::Own,
            lease: None, _hold: None,
        });
        assert!(update_app(&foreign, wire_id, &tilo_actions::InvocationUpdate::Succeeded {
            invocation: invocation(), output: tilo_actions::ActionOutput::default(),
        }).await.is_err());
        let invalid = [
            tilo_actions::InvocationUpdate::Accepted { invocation: invocation() },
            tilo_actions::InvocationUpdate::Progress { invocation: invocation(), sequence: 1, phase: String::new(), completed: None, total: None, message: None },
            tilo_actions::InvocationUpdate::Progress { invocation: invocation(), sequence: 1, phase: "working".into(), completed: Some(2), total: Some(1), message: None },
            tilo_actions::InvocationUpdate::Item { invocation: invocation(), sequence: 1, value: serde_json::json!({"private":"value"}) },
            tilo_actions::InvocationUpdate::Failed { invocation: invocation(), error: tilo_actions::ActionFailure { code: "bad".into(), message: "failed".into(), details: None, retryable: true } },
            tilo_actions::InvocationUpdate::Succeeded { invocation: invocation(), output: tilo_actions::ActionOutput { value: Some(serde_json::json!("undeclared")), resources: vec![] } },
        ];
        for update in invalid {
            assert!(update_app(&channel, wire_id, &update).await.is_err());
            assert!(matches!(fixture.state.invocation_status(wire_id).unwrap(), tilo_actions::InvocationUpdate::Accepted { .. }));
        }
        channel.complete_invocation(wire_id, "", "").await.unwrap();
        assert!(matches!(fixture.state.invocation_status(wire_id).unwrap(), tilo_actions::InvocationUpdate::Succeeded { invocation: done, .. } if done == invocation()));
    }).await;
}

/// The broker answers every application and peer on one thread. A
/// completion waiting for its journal write, as on a loaded disk whose
/// syncs take seconds, must leave that thread answering everything
/// else, and nobody may hear of the outcome before it is durable.
#[tokio::test(flavor = "current_thread")]
async fn a_completion_waiting_for_a_slow_disk_leaves_the_broker_loop_answering() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller) = durable_fixture(root.path(), None);
            let channel = fixture.invocation_channel();
            let wire_id = admit(&mut fixture, &caller);
            // The disk: each journal write waits until the test lets it go.
            let (writing, at_write) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let released = Mutex::new(released);
            fixture
                .state
                .journal
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .hold_writes = Some(Arc::new(move || {
                let _ = writing.send(());
                let _ = released
                    .lock()
                    .unwrap()
                    .recv_timeout(tilo_test_env::hang_bound());
            }));
            let completing = tokio::task::spawn_local({
                let channel = channel.clone();
                let id = wire_id;
                async move { channel.complete_invocation(id, "", "").await }
            });
            tilo_test_env::within("the completion to reach its journal write", async {
                while at_write.try_recv().is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await;
            // Another request the loop answers, as it answers an app.
            tilo_test_env::within(
                "the loop to answer while the completion writes",
                channel.invocation_cancellation(wire_id),
            )
            .await
            .unwrap();
            assert!(
                !completing.is_finished(),
                "the answer came only once the completion had written"
            );
            assert!(
                matches!(
                    fixture.state.invocation_status(wire_id).unwrap(),
                    tilo_actions::InvocationUpdate::Accepted { .. }
                ),
                "nobody hears of the outcome before it is durable"
            );
            release.send(()).unwrap();
            tilo_test_env::within("the completion to finish once its write goes", completing)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                fixture.state.invocation_status(wire_id).unwrap(),
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
        })
        .await;
}

/// A record another workspace holds (a person's profile record, shown in a
/// window of a workspace they only read) is admitted by the workspace holding
/// it when it runs: allowed there while that workspace's grants allow it, and
/// refused once they no longer do, even while the window still holds the
/// verdict that allowed it.
#[tokio::test(flavor = "current_thread")]
async fn a_held_record_is_admitted_by_its_workspace_whatever_its_verdict_said() {
    use crate::apps::test_support::{Role, Workspace, WorkspaceId};
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let person =
                person_id_for_root(SigningKey::from_bytes(&[51; 32]).verifying_key().to_bytes());
            let mut board = Workspace::new(WorkspaceId::from_u128(71), "Board");
            board.set_owner(actor_root(60));
            board.admit(person.clone(), Role::Viewer);
            let holding = WorkspaceId::from_u128(72);
            let mut own = Workspace::new(holding, "Their own");
            own.set_owner(actor_root(60));
            own.admit(person.clone(), Role::Editor);
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(board);
            tree.workspaces.push(own);
            tree.set_active(WorkspaceId::from_u128(71));
            tree.persist_path = Some(root.path().join("state.json"));
            let membership = tree.shared();
            let (mut fixture, caller) = durable_fixture(root.path(), Some(membership.clone()));
            let mut surface = fixture.state.publishers.surfaces.lock().unwrap()[&71].clone();
            surface.actions[0].contract.kind = tilo_actions::ActionKind::Undoable;
            let action = surface.actions[0].clone();
            fixture
                .state
                .publishers
                .surfaces
                .lock()
                .unwrap()
                .insert(71, surface);
            // The window publishes the record as held in the person's own workspace.
            let (delta, _) = runtime_collab::SurfacePublication::next_held(
                None,
                std::slice::from_ref(&action),
                &|_| Some(holding.to_string()),
            );
            let mut table = runtime_collab::SurfaceTable::default();
            table.apply(&delta).unwrap();
            fixture
                .state
                .surface_tables
                .lock()
                .unwrap()
                .insert(71, table);

            let actor = caller.admitted.broker_actor();
            let record = runtime_collab::shown_actions::ShownAction::new(
                Some(&holding.to_string()),
                &fixture.route.app_id,
                (&action.resource_kind, &action.resource, &action.action),
                &action.contract.need(),
            );
            let mut verdicts = fixture.state.subscribe_shown_actions(
                actor.clone(),
                fixture.route.cluster_id.clone(),
                vec![record],
            );
            assert_eq!(
                verdicts.next().await.unwrap().unwrap()[0].verdict,
                runtime_collab::shown_actions::ActionVerdict::Allowed,
                "an editor of the holding workspace may run it, though they only read the board"
            );
            admit(&mut fixture, &caller);

            // The holding workspace takes the grant back; the window has not heard yet.
            membership
                .write()
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id() == holding)
                .unwrap()
                .admit(person, Role::Viewer);
            let surface = fixture.state.publishers.surfaces.lock().unwrap()[&71].clone();
            let refused = fixture.state.dispatch_invocation_from_offered_owned(
                71,
                (surface, action),
                InvocationInputs {
                    original_request: None,
                    requested: Some(tilo_actions::InvocationId::new()),
                    caller: None,
                    execution: None,
                    argument: None,
                    actor: &actor,
                    confirmation_token: None,
                    context: None,
                },
                AdmissionProvenance {
                    revision: None,
                    origin: None,
                    retained_time: None,
                },
            );
            assert_eq!(
                refused.err().map(|refused| refused.to_string()).as_deref(),
                Some("caller no longer authorized for invocation"),
                "the stale verdict lets nothing through"
            );
        })
        .await;
}

/// The person id of the root seeded with `seed`.
fn actor_root(seed: u8) -> String {
    person_id_for_root(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}
