//! Durable admission: the invocation journal that lets an admitted effect run
//! once across duplicates, lost responses, crashes and restarts, and the
//! caller reverification a continued invocation passes.
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
) -> (DurableFixture, VerifiedActionCaller, CallerInvocation) {
    let mut identity_env = tilo_test_env::ScopedEnv::new();
    identity_env
        .set("XDG_CONFIG_HOME", root.join("config"))
        .set("TILO_IDENTITY_ROOT", root.join("identity"))
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &root.join("config/tilo/identity"),
        &tilo_user_identity::IdentityPaths::under(root.join("identity")),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("provision the host admission and terminal receipt signer");
    let root_key = SigningKey::from_bytes(&[51; 32]);
    let device = SigningKey::from_bytes(&[52; 32]);
    let membership = membership.unwrap_or_else(|| {
        let mut tree = WorkspaceTree::default();
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(71),
            "Durable room",
        );
        workspace.set_owner(principal_id_for_root(root_key.verifying_key().to_bytes()));
        tree.workspaces.push(workspace);
        tree.set_active(crate::test_support::WorkspaceId(71));
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
    let mut action =
        published_directory_action(&fixture.directory_state.borrow().row()).unwrap();
    action.operation_kind = tilo_actions::OperationKind::Effect;
    action.contract = tilo_actions::ActionContract {
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Job,
        ..tilo_actions::ActionContract::default()
    };
    assert!(action.contract.validate_declaration().is_empty());
    fixture.state.surfaces.lock().unwrap().insert(
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
    let request = CallerInvocation {
        recipient: None,
        revision: None,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID)),
        host: fixture.state.invocation_host().unwrap(),
        peer_id: 71,
        app_id: fixture.route.app_id.clone(),
        record_id: fixture.route.record_id.clone(),
        resource: "workspace:atlas".into(),
        action: "open".into(),
        argument: None,
    };
    (
        DurableFixture {
            directory: fixture,
            _identity_env: identity_env,
        },
        caller,
        request,
    )
}

fn effect(root: &std::path::Path) {
    use std::io::Write;
    let path = root.join("effect-counter");
    let before = std::fs::read_to_string(&path).map_or(0, |s| s.parse::<u64>().unwrap());
    let mut file = std::fs::File::create(path).unwrap();
    write!(file, "{}", before + 1).unwrap();
    file.sync_all().unwrap();
    std::fs::File::open(root).unwrap().sync_all().unwrap();
}

/// Status, retry and the effect barrier re-verify the caller's own
/// custody. A delegated caller's recheck must pass while it stays the
/// same person on the same device, and every other continuation — the
/// person without that delegation, or another agent — must be refused
/// by the same sites.
#[tokio::test(flavor = "current_thread")]
async fn a_delegated_callers_continuation_passes_reverification_and_others_do_not() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            let caller = caller.delegated_to("mcp-7");
            let person = fixture
                .state
                .verify_action_caller(
                    &fixture.cluster_id,
                    &certified_join(
                        &fixture.cluster_id,
                        &SigningKey::from_bytes(&[51; 32]),
                        &SigningKey::from_bytes(&[52; 32]),
                    )
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            let other_agent = person.clone().delegated_to("mcp-8");
            let started = fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            assert_eq!(
                started.key.owner,
                InvocationOwner::AuthenticatedLocal {
                    root: caller.admitted.verified.user_public_key,
                    device: caller.admitted.verified.device_public_key,
                    agent: Some("mcp-7".into()),
                },
                "the durable owner names the delegated agent"
            );
            let delivered = fixture.take_invocation_identity().unwrap();
            // Status: the agent's own recheck passes; the person's
            // continuation and another agent's are refused.
            assert_eq!(
                fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .map(|record| record.key.owner),
                Some(started.key.owner.clone())
            );
            // The person themselves, without this delegation, is
            // refused: the admission no longer carries the delegation
            // its invocation is owned by, never downgraded to them.
            let person_key = fixture
                .state
                .caller_key(&person, request.invocation)
                .unwrap();
            assert_eq!(
                person_key.owner,
                InvocationOwner::Certified {
                    root: person.admitted.verified.user_public_key,
                    device: person.admitted.verified.device_public_key,
                }
            );
            assert_ne!(person_key, started.key);
            // Another agent reaches its own key, and that key is not
            // this invocation's: it never observes or retries it.
            let other_key = fixture
                .state
                .caller_key(&other_agent, request.invocation)
                .unwrap();
            assert_ne!(other_key, started.key);
            assert!(
                fixture
                    .state
                    .verified_invocation_status(&other_agent, request.invocation)
                    .unwrap()
                    .is_none(),
                "another agent never observes this agent's invocation"
            );
            // A retry under another agent's admission is a different
            // invocation: it never joins this one's record or delivery.
            let retried =
                fixture
                    .state
                    .start_verified_invocation(&other_agent, &request, None);
            if let Ok(retried) = &retried {
                assert_ne!(retried.key, started.key);
            }
            assert!(fixture.invocation_rx.try_recv().is_err());
            // Retry: the same request under the agent's custody is the
            // same admission, never a second delivery.
            let retry = fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            assert_eq!(retry.key.owner, started.key.owner);
            assert!(fixture.invocation_rx.try_recv().is_err());
            // The effect barrier: the agent completes its own invocation.
            effect(root.path());
            fixture
                .complete_invocation(
                    delivered.0,
                    &tilo_actions::InvocationUpdate::Succeeded {
                        invocation: request.invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .expect("the agent observes its own terminal")
                    .update,
                tilo_actions::InvocationUpdate::Succeeded { invocation, .. }
                    if invocation == request.invocation
            ));
            assert_eq!(
                std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                "1"
            );
        })
        .await;
}

/// A caller the door admits without a retained certificate (a network
/// door's admission can be) cannot name the agent it was delegated
/// to. Status, retry and the effect barrier must refuse it rather than
/// silently re-own its invocation as the person's.
#[tokio::test(flavor = "current_thread")]
async fn a_delegated_caller_whose_admission_cannot_name_its_agent_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            let mut keyless = caller.clone().delegated_to("mcp-7");
            assert_eq!(
                caller.owner().unwrap(),
                InvocationOwner::Certified {
                    root: caller.admitted.verified.user_public_key,
                    device: caller.admitted.verified.device_public_key,
                }
            );
            assert!(matches!(
                keyless.owner().unwrap(),
                InvocationOwner::AuthenticatedLocal { agent: Some(ref agent), .. }
                    if agent == "mcp-7"
            ));
            keyless.admitted.certificate = None;
            assert_eq!(
                keyless.owner().unwrap_err(),
                "admitted caller certificate missing"
            );
            assert_eq!(
                fixture
                    .state
                    .start_verified_invocation(&keyless, &request, None)
                    .unwrap_err(),
                "admitted caller certificate missing"
            );
            assert!(fixture.invocation_rx.try_recv().is_err());
            // The effect barrier: it re-verifies the original caller,
            // and that re-verification checks the same owner. A
            // delegation the admission cannot back is refused, never
            // downgraded to the person.
            let person = caller.admitted.clone();
            assert!(matches!(
                fixture
                    .state
                    .caller_key(&keyless, request.invocation),
                Err(ref reason) if reason == "admitted caller certificate missing"
            ));
            for admission in [
                person
                    .clone()
                    .delegated_to(Some("mcp-7"))
                    .delegated_to(Some("mcp-8")),
                person.clone().delegated_to(None),
            ] {
                let mut mismatched = keyless.clone();
                mismatched.admitted = admission;
                assert_eq!(
                    mismatched.owner().unwrap_err(),
                    "admitted caller does not carry its delegation"
                );
            }
            // The person themselves never inherits a delegation.
            assert_eq!(
                caller.owner().unwrap(),
                InvocationOwner::Certified {
                    root: person.verified.user_public_key,
                    device: person.verified.device_public_key,
                }
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_uuid_collisions_never_reach_application_stream() {
    tokio::task::LocalSet::new()
        .run_until(async {
            // Both legacy directions matter: a caller can reserve a future
            // numeric UUID, or attempt to reuse a previously delivered one.
            for scenario in [
                "cross-device",
                "caller-before-legacy",
                "legacy-before-caller",
            ] {
                let root = tempfile::tempdir().unwrap();
                let (mut fixture, caller, mut request) = durable_fixture(root.path(), None);
                let other = fixture
                    .state
                    .verify_action_caller(
                        &fixture.cluster_id,
                        &certified_join(
                            &fixture.cluster_id,
                            &SigningKey::from_bytes(&[51; 32]),
                            &SigningKey::from_bytes(&[53; 32]),
                        )
                        .encode()
                        .unwrap(),
                    )
                    .unwrap();
                if scenario == "caller-before-legacy" {
                    request.invocation =
                        tilo_actions::InvocationId(uuid::Uuid::from_u128(2));
                }
                let wire_id = if scenario == "legacy-before-caller" {
                    let job = fixture
                        .state
                        .start_invocation(
                            71,
                            &request.resource,
                            &request.action,
                            None,
                            &caller.admitted.principal_id,
                        )
                        .unwrap();
                    request.invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(
                        u128::from(job.id),
                    ));
                    job.id
                } else {
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap()
                        .wire_id
                };
                let (_, empty_rx) = mpsc::unbounded_channel();
                let rx = Rc::new(AsyncMutex::new(std::mem::replace(
                    &mut fixture.invocation_rx,
                    empty_rx,
                )));
                let stream = InvocationStreamServer {
                    host_guard: None,
                    rx: rx.clone(),
                    delivered: None,
                };
                let response = stream.next().await.unwrap();
                assert_eq!(response.id, wire_id);
                assert_eq!(response.invocation, request.invocation);
                effect(root.path());
                let conflict = if scenario == "caller-before-legacy" {
                    fixture
                        .state
                        .start_invocation(
                            71,
                            &request.resource,
                            &request.action,
                            None,
                            &caller.admitted.principal_id,
                        )
                        .map(|_| ())
                } else {
                    fixture
                        .state
                        .start_verified_invocation(&other, &request, None)
                        .map(|_| ())
                };
                assert!(
                    conflict
                        .err()
                        .unwrap()
                        .contains("action_invocation_identity_conflict"),
                    "{scenario}: conflicting UUID must fail before app dispatch"
                );
                assert!(
                    rx.lock().await.try_recv().is_err(),
                    "only one application delivery"
                );
                assert!(
                    fixture
                        .state
                        .verified_invocation_status(&other, request.invocation)
                        .unwrap()
                        .is_none()
                );
                assert!(
                    fixture
                        .state
                        .cancel_verified_invocation(&other, request.invocation)
                        .await
                        .is_err()
                );
                let channel = fixture.invocation_channel();
                let terminal = tilo_actions::InvocationUpdate::Succeeded {
                    invocation: request.invocation,
                    output: tilo_actions::ActionOutput::default(),
                };
                update_app(&channel, wire_id, &terminal).await.unwrap();
                if scenario != "legacy-before-caller" {
                    let duplicate = fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    assert_eq!(duplicate.wire_id, wire_id);
                    assert_eq!(duplicate.update, terminal);
                }
                let membership = fixture.membership.as_ref().unwrap().clone();
                drop(channel);
                let gone = Arc::downgrade(&fixture.state);
                drop(fixture);
                broker_gone(gone).await;
                let (mut reopened, _, mut again) =
                    durable_fixture(root.path(), Some(membership));
                again.invocation = request.invocation;
                assert!(
                    reopened
                        .state
                        .start_verified_invocation(&other, &again, None)
                        .is_err(),
                    "{scenario}: UUID reservation survives terminal completion and restart"
                );
                assert!(reopened.invocation_rx.try_recv().is_err());
                assert_eq!(
                    std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                    "1"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_real_effect_once_across_concurrent_duplicate_response_loss_and_restart()
 {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            fixture
                .state
                .surfaces
                .lock()
                .unwrap()
                .get_mut(&71)
                .unwrap()
                .actions[0]
                .contract
                .result = tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type":"object"}),
            };
            // Every thread is spawned before any is joined.
            #[expect(
                clippy::needless_collect,
                reason = "spawning all threads before joining is the concurrency under test"
            )]
            let requests = (0..8)
                .map(|_| {
                    let state = fixture.state.clone();
                    let caller = caller.clone();
                    let request = request.clone();
                    std::thread::spawn(move || {
                        state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>();
            let admitted = requests
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>();
            assert!(
                admitted
                    .iter()
                    .all(|row| row.wire_id == admitted[0].wire_id)
            );
            let (_, empty_rx) = mpsc::unbounded_channel();
            let rx = Rc::new(AsyncMutex::new(std::mem::replace(
                &mut fixture.invocation_rx,
                empty_rx,
            )));
            let stream = InvocationStreamServer {
                host_guard: None,
                rx: rx.clone(),
                delivered: None,
            };
            let delivered = stream.next().await.unwrap();
            let delivered_id = delivered.id;
            assert_eq!(delivered.invocation, request.invocation);
            assert!(
                rx.lock().await.try_recv().is_err(),
                "one actual application delivery"
            );
            effect(root.path());
            let terminal = tilo_actions::InvocationUpdate::Succeeded {
                invocation: request.invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(
                        serde_json::json!({"counter":1,"private":"private-result-token"}),
                    ),
                    resources: vec![],
                },
            };
            // Force receipt persistence to fail independently of the already opened journal.
            fixture.membership.as_ref().unwrap().write().persist_path =
                Some(root.path().to_owned());
            fixture
                .complete_invocation(delivered_id, &terminal)
                .await
                .unwrap();
            // Discard the original response and retry under freshly verified caller custody.
            let duplicate = fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            assert_eq!(duplicate.update, terminal);
            assert!(
                fixture
                    .state
                    .invocation_statuses(&caller.admitted.principal_id)
                    .is_empty(),
                "legacy principal-only lookup cannot reveal certified terminal output"
            );
            let mut changed = request.clone();
            changed.argument = Some(String::new());
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &changed, None)
                    .is_err()
            );
            let membership = fixture.membership.clone().unwrap();
            {
                let mut tree = membership.write();
                assert_eq!(tree.workspaces[0].effect_receipts.len(), 1);
                assert!(
                    !serde_json::to_string(&tree.workspaces[0].effect_receipts)
                        .unwrap()
                        .contains("private-result-token")
                );
                tree.persist_path = Some(root.path().join("state.json"));
            }
            let gone = Arc::downgrade(&fixture.state);
            drop(fixture);
            broker_gone(gone).await;
            let (mut restarted, caller, _) = durable_fixture(root.path(), Some(membership));
            assert_eq!(
                restarted
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .unwrap()
                    .update,
                terminal
            );
            assert!(restarted.invocation_rx.try_recv().is_err());
            assert_eq!(
                std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                "1"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_crash_boundaries_never_repeat_real_effect() {
    tokio::task::LocalSet::new().run_until(async {
        for boundary in [BarrierFailure::File, BarrierFailure::AfterAdmission, BarrierFailure::Terminal] {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            if boundary == BarrierFailure::Terminal {
                fixture.state.start_verified_invocation(&caller, &request, None).unwrap();
                let delivered = fixture.invocation_rx.try_recv().unwrap();
                effect(root.path());
                fixture.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(BarrierFailure::File);
                assert!(fixture.complete_invocation(delivered.id, &tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput::default() }).await.is_err());
            } else {
                fixture.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(boundary);
                assert!(fixture.state.start_verified_invocation(&caller, &request, None).is_err());
                assert!(fixture.invocation_rx.try_recv().is_err());
            }
            let membership = fixture.membership.clone().unwrap();
            let gone = Arc::downgrade(&fixture.state);
            drop(fixture);
            broker_gone(gone).await;
            let (mut restarted, caller, _) = durable_fixture(root.path(), Some(membership));
            if boundary == BarrierFailure::File {
                assert!(restarted.state.verified_invocation_status(&caller, request.invocation).unwrap().is_none());
                assert!(!root.path().join("effect-counter").exists());
            } else {
                let result = restarted.state.start_verified_invocation(&caller, &request, None).unwrap();
                assert!(result.uncertain);
                assert!(matches!(result.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable));
                assert!(restarted.invocation_rx.try_recv().is_err());
                if boundary == BarrierFailure::Terminal {
                    assert_eq!(std::fs::read_to_string(root.path().join("effect-counter")).unwrap(), "1");
                } else { assert!(!root.path().join("effect-counter").exists()); }
            }
        }
    }).await;
}

#[test]
fn invocation_journal_owner_is_device_scoped_and_live_authority_precedes_duplicate_disclosure()
 {
    let root = tempfile::tempdir().unwrap();
    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
    fixture
        .state
        .start_verified_invocation(&caller, &request, None)
        .unwrap();
    fixture.invocation_rx.try_recv().unwrap();
    let other_device = SigningKey::from_bytes(&[53; 32]);
    let other = fixture
        .state
        .verify_action_caller(
            &fixture.cluster_id,
            &certified_join(
                &fixture.cluster_id,
                &SigningKey::from_bytes(&[51; 32]),
                &other_device,
            )
            .encode()
            .unwrap(),
        )
        .unwrap();
    assert!(
        fixture
            .state
            .verified_invocation_status(&other, request.invocation)
            .unwrap()
            .is_none()
    );
    assert!(
        futures::executor::block_on(
            fixture
                .state
                .cancel_verified_invocation(&other, request.invocation)
        )
        .is_err()
    );
    assert!(
        fixture
            .state
            .invocation_statuses(&caller.admitted.principal_id)
            .is_empty(),
        "legacy root-scoped queries must not disclose another device's private results"
    );
    let mut bad = request.clone();
    bad.invocation = tilo_actions::InvocationId::new();
    bad.record_id = "different-record".into();
    assert!(
        fixture
            .state
            .start_verified_invocation(&caller, &bad, None)
            .is_err()
    );
    fixture.membership.as_ref().unwrap().write().workspaces[0].set_owner("new-owner");
    assert!(
        fixture
            .state
            .start_verified_invocation(&caller, &request, None)
            .is_err()
    );
    assert!(
        fixture
            .state
            .verified_invocation_status(&caller, request.invocation)
            .is_err()
    );
    assert!(fixture.invocation_rx.try_recv().is_err());
}

#[test]
fn invocation_journal_old_apps_wrong_rooms_and_silence_fail_safely() {
    for disconnected in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (mut fixture, caller, request) = durable_fixture(root.path(), None);
        fixture
            .state
            .stable_invocation_subscribers
            .lock()
            .unwrap()
            .clear();
        assert!(
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap_err()
                .contains("stable_identity_unavailable")
        );
        assert!(fixture.invocation_rx.try_recv().is_err());
        fixture
            .state
            .stable_invocation_subscribers
            .lock()
            .unwrap()
            .insert(71);
        let original_room = fixture
            .state
            .surfaces
            .lock()
            .unwrap()
            .get(&71)
            .unwrap()
            .route
            .cluster_id
            .clone();
        fixture
            .state
            .surfaces
            .lock()
            .unwrap()
            .get_mut(&71)
            .unwrap()
            .route
            .cluster_id = uuid::Uuid::new_v4().to_string();
        assert!(
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .is_err()
        );
        fixture
            .state
            .surfaces
            .lock()
            .unwrap()
            .get_mut(&71)
            .unwrap()
            .route
            .cluster_id = original_room;
        let admitted = fixture
            .state
            .start_verified_invocation(&caller, &request, None)
            .unwrap();
        fixture.invocation_rx.try_recv().unwrap();
        let cancelled = futures::executor::block_on(
            fixture
                .state
                .cancel_verified_invocation(&caller, request.invocation),
        )
        .unwrap();
        assert!(cancelled.cancellation_requested);
        // A durable cancellation request acknowledges receipt only; it
        // does not assert that the running external effect stopped.
        use tilo_delivery_protocol::{
            peer_actions::{ActionMessage, DirectedAction},
            transport_v2::{self, EnvelopeV2},
        };
        let cancel = DirectedAction {
            room: uuid::Uuid::parse_str(&caller.room).unwrap(),
            request_id: uuid::Uuid::new_v4(),
            source: [52; 32],
            target: [51; 32],
            message: ActionMessage::Cancel {
                invocation: request.invocation,
            },
        };
        // The broker's reply path answers exactly this cancel: same
        // room and request, reversed direction, the same invocation.
        let acknowledgement = crate::peer_actions::bounded_reply(
            &cancel,
            ActionMessage::CancellationRequested {
                invocation: request.invocation,
            },
        );
        assert_eq!(
            acknowledgement,
            DirectedAction {
                room: cancel.room,
                request_id: cancel.request_id,
                source: cancel.target,
                target: cancel.source,
                message: ActionMessage::CancellationRequested {
                    invocation: request.invocation,
                },
            }
        );
        // And it survives the private lane the relay carries.
        assert!(cancel.validate() && acknowledgement.validate());
        let reply = EnvelopeV2::Directed(acknowledgement);
        let encoded = transport_v2::encode(&reply).unwrap();
        assert_eq!(transport_v2::decode(&encoded[4..]).unwrap(), reply);
        if disconnected {
            fixture.state.remove_connection(71);
        } else {
            fixture.state.expire_stale_invocations_at(
                std::time::Instant::now(),
                Duration::ZERO,
                Duration::ZERO,
            );
        }
        let status = fixture
            .state
            .verified_invocation_status(&caller, request.invocation)
            .unwrap()
            .unwrap();
        assert!(status.uncertain);
        assert!(
            matches!(status.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown")
        );
        assert_eq!(
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap()
                .wire_id,
            admitted.wire_id
        );
        assert!(fixture.invocation_rx.try_recv().is_err());
    }
}

/// An agent, a device admin or another device drives an app through
/// its actions. The host learns of each invocation the app is handed,
/// so a window the app closes because of it is not taken for one it
/// lost.
#[tokio::test(flavor = "current_thread")]
async fn the_host_learns_each_invocation_an_app_is_handed() {
    struct Recording(Mutex<Vec<(String, String)>>);
    impl crate::services::LauncherServices for Recording {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("this host launches nothing".into())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
        fn note_acted_on(&self, app: &str, room: &str) {
            self.0.lock().unwrap().push((app.into(), room.into()));
        }
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let recording = Arc::new(Recording(Mutex::new(Vec::new())));
            let services = HostServices {
                launcher: Some(recording.clone()),
                ..Default::default()
            };
            let (tx, rx) = mpsc::unbounded_channel();
            let stream = InvocationStreamServer {
                host_guard: None,
                rx: Rc::new(AsyncMutex::new(rx)),
                delivered: invocation_delivery_notice(
                    &services,
                    "so.tilo.notes".into(),
                    "room-7".into(),
                ),
            };
            assert!(recording.0.lock().unwrap().is_empty());
            tx.send(InvocationEnvelope {
                id: 1,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(1)),
                resource: "window".into(),
                action: "close".into(),
                argument: None,
                actor: "agent:assistant".into(),
                context: None,
            })
            .unwrap();
            stream.next().await.unwrap();
            assert_eq!(
                *recording.0.lock().unwrap(),
                vec![("so.tilo.notes".to_owned(), "room-7".to_owned())]
            );
            assert!(
                invocation_delivery_notice(
                    &HostServices::default(),
                    "a".into(),
                    "r".into()
                )
                .is_none(),
                "a host without a launcher has no one to tell"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_native_stream_negotiates_uuid_capability_explicitly() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (fixture, caller, request) = durable_fixture(root.path(), None);
            let (connection, outgoing) = wire::WireConnection::new(None);
            let handle =
                connection.open(wire::ChannelSession::new(Rc::new(JoinedChannel {
                    as_app: None,
                    person_chat_views: RefCell::default(),
                    committed_read: Rc::new(RefCell::new(
                        committed_document::Slot::default(),
                    )),
                    state: fixture.state.clone(),
                    directories: fixture.directories.clone(),
                    channel_id: fixture.route.channel_key(),
                    peer_id: fixture.peer_id,
                    connection_id: fixture.peer_id,
                    verified_identity: caller.admitted.clone(),
                    record_kind: None,
                    route_access: RouteAccess::Own,
                    lease: None,
                    _hold: None,
                })));
            let app = wire_client(connection, outgoing);
            let _legacy = app
                .wire()
                .subscribe(
                    handle,
                    runtime_collab::wire::Topic::Invocations { stable_ids: false },
                )
                .await
                .unwrap();
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .unwrap_err()
                    .contains("stable_identity_unavailable")
            );
            let stream = app
                .channel(handle)
                .subscribe_stable_invocations()
                .await
                .unwrap();
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            let delivered = stream.read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *request.invocation.0.as_bytes());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_invalid_updates_never_persist_and_legacy_completion_still_works()
 {
    tokio::task::LocalSet::new().run_until(async {
        let root = tempfile::tempdir().unwrap();
        let (mut fixture, caller, request) = durable_fixture(root.path(), None);
        let channel = fixture.invocation_channel();
        let admitted = fixture.state.start_verified_invocation(&caller, &request, None).unwrap();
        fixture.invocation_rx.try_recv().unwrap();
        let foreign = wire_channel(JoinedChannel {
            as_app: None,
            person_chat_views: RefCell::default(),
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: fixture.state.clone(), directories: fixture.directories.clone(),
            channel_id: fixture.route.channel_key(), peer_id: 999, connection_id: 999,
            verified_identity: caller.admitted.clone(),
            record_kind: None,
            route_access: RouteAccess::Own,
            lease: None, _hold: None,
        });
        assert!(update_app(&foreign, admitted.wire_id, &tilo_actions::InvocationUpdate::Succeeded {
            invocation: request.invocation, output: tilo_actions::ActionOutput::default(),
        }).await.is_err());
        let invalid = [
            tilo_actions::InvocationUpdate::Accepted { invocation: request.invocation },
            tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: String::new(), completed: None, total: None, message: None },
            tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: "working".into(), completed: Some(2), total: Some(1), message: None },
            tilo_actions::InvocationUpdate::Item { invocation: request.invocation, sequence: 1, value: serde_json::json!({"private":"value"}) },
            tilo_actions::InvocationUpdate::Failed { invocation: request.invocation, error: tilo_actions::ActionFailure { code: "bad".into(), message: "failed".into(), details: None, retryable: true } },
            tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput { value: Some(serde_json::json!("undeclared")), resources: vec![] } },
        ];
        for update in invalid {
            assert!(update_app(&channel, admitted.wire_id, &update).await.is_err());
            assert!(matches!(fixture.state.verified_invocation_status(&caller, request.invocation).unwrap().unwrap().update, tilo_actions::InvocationUpdate::Accepted { .. }));
        }
        channel.complete_invocation(admitted.wire_id, "", "").await.unwrap();
        assert!(matches!(fixture.state.verified_invocation_status(&caller, request.invocation).unwrap().unwrap().update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == request.invocation));

        // The legacy numeric local caller still receives and completes its original handle.
        let legacy = fixture.state.start_invocation(71, &request.resource, &request.action, None, &caller.admitted.principal_id).unwrap();
        let delivered = fixture.invocation_rx.try_recv().unwrap();
        assert_eq!(delivered.invocation.0.as_u128(), u128::from(legacy.id));
        channel.complete_invocation(legacy.id, "", "").await.unwrap();
        assert!(legacy.completion.recv().unwrap().code.is_empty());
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
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            let channel = fixture.invocation_channel();
            let admitted = fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            fixture.invocation_rx.try_recv().unwrap();
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
                let id = admitted.wire_id;
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
                channel.invocation_cancellation(admitted.wire_id),
            )
            .await
            .unwrap();
            assert!(
                !completing.is_finished(),
                "the answer came only once the completion had written"
            );
            assert!(
                matches!(
                    fixture
                        .state
                        .verified_invocation_status(&caller, request.invocation)
                        .unwrap()
                        .unwrap()
                        .update,
                    tilo_actions::InvocationUpdate::Accepted { .. }
                ),
                "nobody hears of the outcome before it is durable"
            );
            release.send(()).unwrap();
            tilo_test_env::within(
                "the completion to finish once its write goes",
                completing,
            )
            .await
            .unwrap()
            .unwrap();
            assert!(matches!(
                fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .unwrap()
                    .update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_immediate_progress_and_malformed_stream_items_are_rejected() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for streaming in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                let channel = fixture.invocation_channel();
                fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get_mut(&71)
                    .unwrap()
                    .actions[0]
                    .contract
                    .execution = if streaming {
                    tilo_actions::ExecutionMode::Streaming {
                        cancellable: true,
                        item_schema: serde_json::json!({"type":"integer"}),
                    }
                } else {
                    tilo_actions::ExecutionMode::Immediate
                };
                if !streaming {
                    fixture
                        .state
                        .surfaces
                        .lock()
                        .unwrap()
                        .get_mut(&71)
                        .unwrap()
                        .actions[0]
                        .contract
                        .result = tilo_actions::ResultSpec::None;
                }
                let admitted = fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .unwrap();
                fixture.invocation_rx.try_recv().unwrap();
                let bad = if streaming {
                    tilo_actions::InvocationUpdate::Item {
                        invocation: request.invocation,
                        sequence: 1,
                        value: serde_json::json!("wrong type"),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Progress {
                        invocation: request.invocation,
                        sequence: 1,
                        phase: "working".into(),
                        completed: None,
                        total: None,
                        message: None,
                    }
                };
                assert!(update_app(&channel, admitted.wire_id, &bad).await.is_err());
                assert!(matches!(
                    fixture
                        .state
                        .verified_invocation_status(&caller, request.invocation)
                        .unwrap()
                        .unwrap()
                        .update,
                    tilo_actions::InvocationUpdate::Accepted { .. }
                ));
                if streaming {
                    let item = tilo_actions::InvocationUpdate::Item {
                        invocation: request.invocation,
                        sequence: 1,
                        value: serde_json::json!(7),
                    };
                    update_app(&channel, admitted.wire_id, &item).await.unwrap();
                    assert!(update_app(&channel, admitted.wire_id, &item).await.is_err());
                    assert_eq!(
                        fixture
                            .state
                            .verified_invocation_status(&caller, request.invocation)
                            .unwrap()
                            .unwrap()
                            .update,
                        item
                    );
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_journal_confirmation_uuid_and_lifecycle_remain_authoritative() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            let channel = fixture.invocation_channel();
            fixture
                .state
                .surfaces
                .lock()
                .unwrap()
                .get_mut(&71)
                .unwrap()
                .actions[0]
                .contract
                .confirmation = tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "erase exactly".into(),
            };
            let actor = &caller.admitted.principal_id;
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .is_err()
            );
            assert!(
                fixture
                    .state
                    .prepare_confirmation(
                        71,
                        &request.resource,
                        &request.action,
                        None,
                        actor,
                        Some("erase")
                    )
                    .is_err()
            );
            let token = fixture
                .state
                .prepare_confirmation(
                    71,
                    &request.resource,
                    &request.action,
                    None,
                    actor,
                    Some("erase exactly"),
                )
                .unwrap();
            let admitted = fixture
                .state
                .start_verified_invocation(&caller, &request, Some(&token))
                .unwrap();
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            let delivered = fixture.invocation_rx.try_recv().unwrap();
            assert_eq!(delivered.invocation, request.invocation);
            let mut new = request.clone();
            new.invocation = tilo_actions::InvocationId::new();
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &new, Some(&token))
                    .is_err()
            );
            let progress = tilo_actions::InvocationUpdate::Progress {
                invocation: request.invocation,
                sequence: 1,
                phase: "working".into(),
                completed: Some(0),
                total: Some(1),
                message: None,
            };
            update_app(&channel, delivered.id, &progress).await.unwrap();
            assert!(update_app(&channel, delivered.id, &progress).await.is_err());
            assert!(
                update_app(
                    &channel,
                    delivered.id,
                    &tilo_actions::InvocationUpdate::Succeeded {
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                            u128::from(delivered.id)
                        )),
                        output: tilo_actions::ActionOutput::default()
                    }
                )
                .await
                .is_err()
            );
            fixture
                .state
                .cancel_verified_invocation(&caller, request.invocation)
                .await
                .unwrap();
            assert!(
                !fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .unwrap()
                    .update
                    .is_terminal(),
                "request is not acknowledgement"
            );
            let cancelled = tilo_actions::InvocationUpdate::Cancelled {
                invocation: request.invocation,
                message: None,
            };
            update_app(&channel, delivered.id, &cancelled)
                .await
                .unwrap();
            assert_eq!(
                fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .unwrap()
                    .update,
                cancelled
            );
            assert_eq!(admitted.wire_id, delivered.id);
        })
        .await;
}
