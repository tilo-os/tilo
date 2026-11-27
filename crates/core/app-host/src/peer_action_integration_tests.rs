// The person's other device acting on this one: the engine's action service
// hands such a request to this broker's api as an `ActionProvider`, and what it
// gets is the same durable log an application's own caller gets.

use super::*;
use tilo_actions::remote::{
    ActionProvider as _, Caller, CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest,
    PendingPoll, ReceiptStatus, RemoteActionTarget,
};

/// The root every device in these fixtures is certified under.
const ROOT_SEED: u8 = 51;

/// The device the fixture's workspace belongs to, as the engine's action service
/// hands a request over: verified by the action service, which is this person.
fn a_device(fixture: &LocalDirectoryFixture, seed: u8) -> Caller {
    Caller::authenticated(
        SigningKey::from_bytes(&[ROOT_SEED; 32])
            .verifying_key()
            .to_bytes(),
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
    .in_workspace(fixture.cluster_id.clone())
}

fn device_api(
    fixture: &LocalDirectoryFixture,
) -> Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> {
    crate::collab_app_broker::device_host(Arc::clone(&fixture.state))
}

/// The fixture application's own action, invoked under `invocation`.
fn invoke_request(invocation: tilo_actions::InvocationId) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        invocation,
        target: RemoteActionTarget {
            provider: "71".into(),
            address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

async fn answered<T>(
    fixture: &LocalDirectoryFixture,
    mut pending: tilo_actions::remote::Pending<T>,
) -> T {
    tilo_test_env::within("the broker api answers", async {
        loop {
            match pending.poll_ready() {
                PendingPoll::Ready(value) => return Some(value),
                PendingPoll::Waiting => {
                    // What the API asks a live directory is answered on this
                    // local set, as the shell's accept loop answers it.
                    fixture.state.take_turn(&fixture.directories);
                    tokio::task::yield_now().await;
                }
                PendingPoll::Abandoned => return None,
            }
        }
    })
    .await
    .expect("the broker api keeps answering")
}

/// The broker refused `request`: either the admission was refused as a
/// receipt, or the API refused it outright.
async fn refused(
    fixture: &LocalDirectoryFixture,
    api: &Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    caller: &Caller,
    request: &InvokeRequest,
) {
    match answered(fixture, api.invoke(caller, request.clone())).await {
        Err(_) => {}
        Ok(receipt) => assert!(
            matches!(receipt.status, ReceiptStatus::Rejected { .. }),
            "expected a refusal, got {receipt:?}"
        ),
    }
}

/// A phrase-confirmed action answers only to the phrase its contract names,
/// and a caller's own device reaches it exactly once.
#[tokio::test(flavor = "current_thread")]
async fn a_devices_action_runs_once_and_follows_its_own_owner() {
    use tilo_actions::{ConfirmationPolicy, ExecutionMode, InvocationUpdate};
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, _) = durable_fixture(root.path(), None);
            let api = device_api(&fixture);
            let caller = a_device(&fixture, 52);
            let other = a_device(&fixture, 53);
            let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID));
            let request = invoke_request(invocation);
            // The catalogue this device is offered names the action it may take.
            let page = answered(
                &fixture,
                api.query(
                    &caller,
                    CatalogueQuery {
                        provider: Some("71".into()),
                        max_results: 32,
                        ..CatalogueQuery::default()
                    },
                ),
            )
            .await
            .expect("a catalogue page");
            assert!(
                page.rows.iter().any(|row| {
                    row.provider == "71"
                        && row.action.as_ref().is_some_and(|action| {
                            action.address.action == "open"
                                && action.address.resource == "workspace:atlas"
                        })
                }),
                "the catalogue offers the fixture application's action: {:?}",
                page.rows
            );
            // The contract now asks for a phrase, and streams items.
            {
                let mut surfaces = fixture.state.publishers.surfaces.lock().unwrap();
                let action = &mut surfaces.get_mut(&71).unwrap().actions[0];
                action.contract.confirmation = ConfirmationPolicy::Phrase {
                    phrase: "DO IT".into(),
                };
                action.contract.execution = ExecutionMode::Streaming {
                    cancellable: true,
                    item_schema: serde_json::json!({"type": "string"}),
                };
            }
            refused(&fixture, &api, &caller, &request).await;
            let mut wrong = request.clone();
            wrong.confirmation = Some(Confirmation::Phrase("wrong".into()));
            refused(&fixture, &api, &caller, &wrong).await;
            assert!(
                fixture.invocation_rx.try_recv().is_err(),
                "an unconfirmed or mis-confirmed phrase never reaches the application"
            );
            let mut confirmed = request.clone();
            confirmed.confirmation = Some(Confirmation::Phrase("DO IT".into()));
            let first = answered(&fixture, api.invoke(&caller, confirmed))
                .await
                .unwrap();
            assert_eq!(first.invocation, invocation);
            assert!(matches!(first.status, ReceiptStatus::Accepted));
            // The same id from the same device is the same invocation, whatever
            // confirmation the retry carries: the effect is not handed over twice.
            let duplicate = answered(&fixture, api.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(duplicate, first);
            let delivered = fixture
                .invocation_rx
                .try_recv()
                .expect("the confirmed action is handed to the application");
            assert_eq!(delivered.invocation, invocation);
            let channel = fixture.invocation_channel();
            update_app(
                &channel,
                delivered.id,
                &InvocationUpdate::Progress {
                    invocation,
                    sequence: 1,
                    phase: "working".into(),
                    completed: Some(1),
                    total: Some(2),
                    message: Some("progress".into()),
                },
            )
            .await
            .unwrap();
            for sequence in 2..=3 {
                update_app(
                    &channel,
                    delivered.id,
                    &InvocationUpdate::Item {
                        invocation,
                        sequence,
                        value: serde_json::json!(format!("item-{sequence}")),
                    },
                )
                .await
                .unwrap();
            }
            // Progress a person already had cannot be replayed over newer items.
            assert!(
                update_app(
                    &channel,
                    delivered.id,
                    &InvocationUpdate::Progress {
                        invocation,
                        sequence: 1,
                        phase: "invalid replay".into(),
                        completed: None,
                        total: None,
                        message: None,
                    },
                )
                .await
                .is_err()
            );
            let observed = api
                .observe(&caller, &invocation)
                .expect("the invoker follows its own invocation");
            assert_eq!(
                observed.update,
                InvocationUpdate::Item {
                    invocation,
                    sequence: 3,
                    value: serde_json::json!("item-3"),
                }
            );
            // Another of the person's devices neither sees this invocation nor
            // runs its own effect under the same id.
            assert!(api.observe(&other, &invocation).is_none());
            refused(&fixture, &api, &other, &request).await;
            // Cancelling is a request the application answers.
            answered(&fixture, api.cancel(&caller, &invocation))
                .await
                .expect("a running invocation is cancellable");
            answered(&fixture, api.cancel(&caller, &invocation))
                .await
                .expect("a repeated cancel is the same request");
            assert!(matches!(
                api.observe(&caller, &invocation).unwrap().update,
                InvocationUpdate::Item { sequence: 3, .. }
            ));
            effect(root.path());
            let terminal = InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            update_app(&channel, delivered.id, &terminal).await.unwrap();
            // The id answers with what it finished as, on every later ask.
            assert_eq!(api.observe(&caller, &invocation).unwrap().update, terminal);
            let replayed = answered(&fixture, api.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(
                replayed.status,
                ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default()
                }
            );
            assert!(fixture.invocation_rx.try_recv().is_err());
            assert_eq!(
                std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                "1"
            );
        })
        .await;
}

/// A directory that refuses the target it is resolved against refuses the
/// invocation, before anything is admitted.
#[tokio::test(flavor = "current_thread")]
async fn a_resolved_target_that_is_gone_or_stale_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, _) = durable_fixture(root.path(), None);
            let api = device_api(&fixture);
            let caller = a_device(&fixture, 52);
            // What the caller names is resolved by the directory that publishes
            // it rather than answered from an eager surface.
            fixture
                .state
                .publishers
                .surfaces
                .lock()
                .unwrap()
                .get_mut(&71)
                .unwrap()
                .live = false;
            let mut request =
                invoke_request(tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID)));
            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::NotFound;
            refused(&fixture, &api, &caller, &request).await;
            request.revision = Some("stale-directory".into());
            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Stale;
            refused(&fixture, &api, &caller, &request).await;
            assert!(fixture.invocation_rx.try_recv().is_err());
            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Found;
            let admitted = answered(&fixture, api.invoke(&caller, request))
                .await
                .unwrap();
            assert!(matches!(admitted.status, ReceiptStatus::Accepted));
            assert_eq!(
                fixture.invocation_rx.try_recv().unwrap().invocation,
                admitted.invocation
            );
        })
        .await;
}

/// What one invocation did is durable: a restarted broker on the same journal
/// answers with it instead of running it again, an outcome it could not
/// confirm stays unknown, and a write that reached no durable byte admits
/// nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_devices_invocation_answers_across_restart_and_a_failed_write() {
    use tilo_actions::InvocationUpdate;
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, _) = durable_fixture(root.path(), None);
            let membership = fixture.membership.as_ref().unwrap().clone();
            let caller = a_device(&fixture, 52);
            let host = fixture.state.invocation_host().unwrap();
            let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID));
            let request = invoke_request(invocation);
            let api = device_api(&fixture);
            answered(&fixture, api.invoke(&caller, request.clone()))
                .await
                .unwrap();
            let delivered = fixture.invocation_rx.try_recv().unwrap();
            effect(root.path());
            let terminal = InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            update_app(&fixture.invocation_channel(), delivered.id, &terminal)
                .await
                .unwrap();
            // Silence after a delivery leaves the outcome unknown; it is never
            // re-admitted as a new effect.
            let silent = tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID + 1));
            let silent_request = invoke_request(silent);
            answered(&fixture, api.invoke(&caller, silent_request.clone()))
                .await
                .unwrap();
            let silent_wire = fixture.invocation_rx.try_recv().unwrap().id;
            fixture
                .state
                .expire_stale_invocations_at(
                    std::time::Instant::now(),
                    Duration::ZERO,
                    Duration::ZERO,
                );
            let unknown = api.observe(&caller, &silent).unwrap().update;
            assert!(
                matches!(&unknown, InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown"),
                "{unknown:?}"
            );
            let replayed = answered(&fixture, api.invoke(&caller, silent_request.clone()))
                .await
                .unwrap();
            assert!(matches!(replayed.status, ReceiptStatus::Failed { .. }));
            assert!(fixture.invocation_rx.try_recv().is_err());
            let _ = silent_wire;
            // Restart on the same journal and workspace tree.
            let gone = fixture.state.gone();
            drop(api);
            drop(fixture);
            broker_gone(gone).await;
            let (mut reopened, _) = durable_fixture(root.path(), Some(membership));
            assert_eq!(reopened.state.invocation_host().unwrap(), host);
            let api = device_api(&reopened);
            assert_eq!(
                answered(&reopened, api.invoke(&caller, request)).await.unwrap().status,
                ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default()
                }
            );
            assert!(matches!(
                answered(&reopened, api.invoke(&caller, silent_request)).await.unwrap().status,
                ReceiptStatus::Failed { .. }
            ));
            assert!(reopened.invocation_rx.try_recv().is_err());
            assert_eq!(
                std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                "1"
            );
            // A write that reaches no durable byte is taken back: nothing is
            // admitted and the application is handed nothing.
            reopened
                .state
                .journal
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .fail_at = Some(BarrierFailure::File);
            let blocked = invoke_request(

                tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID + 2)),
            );
            refused(&reopened, &api, &caller, &blocked).await;
            assert!(reopened.invocation_rx.try_recv().is_err());
            // A write that may be durable although unconfirmed withdraws the
            // broker: nothing more is admitted until the store is recovered.
            reopened
                .state
                .journal
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .fail_at = Some(BarrierFailure::Directory);
            let unconfirmed = invoke_request(

                tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID + 3)),
            );
            refused(&reopened, &api, &caller, &unconfirmed).await;
            assert!(
                reopened.state.invocation_host().is_err(),
                "a possibly-durable failed write withdraws the host until recovery"
            );
            let after = invoke_request(

                tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID + 4)),
            );
            refused(&reopened, &api, &caller, &after).await;
            assert!(reopened.invocation_rx.try_recv().is_err());
            assert_eq!(
                std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                "1"
            );
        })
        .await;
}

/// A host whose publisher records effects but has no owner to commit them to
/// advertises no durable invocation host, and opens the journal it was given
/// rather than the workspace tree's own file.
#[test]
fn a_recording_publisher_cannot_advertise_a_durable_invocation_host() {
    let root = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure broker fixture root");
    }
    let mut tree = WorkspaceTree::default();
    let mut published = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(71),
        "Explicit native peer",
    );
    published.policy.set_owner(person_id_for_root(
        SigningKey::from_bytes(&[ROOT_SEED; 32])
            .verifying_key()
            .to_bytes(),
    ));
    let workspace_id = uuid::Uuid::parse_str(&published.id().to_string()).unwrap();
    tree.workspaces.push(published);
    tree.persist_path = Some(root.path().join("must-not-be-used/state.json"));
    let journal_root = root.path().join("explicit-journal");
    let config = crate::NativePeerBrokerConfig {
        journal_root: journal_root.clone(),
        certificate_authority: Arc::new(RecordingAuthority {
            workspace: workspace_id,
        }),
    };
    let broker = crate::collab_broker::spawn_native_peer(
        root.path().join("app.sock"),
        tree.shared().authority(),
        PeerPublisher::disabled(),
        config,
    )
    .unwrap();
    assert!(
        broker
            .app_action_broker()
            .unwrap()
            .invocation_host()
            .is_err(),
        "a publisher with nothing to commit to is not a durable invocation host"
    );
    assert!(journal_root.join("records.json").is_file());
    assert!(!root.path().join("must-not-be-used").exists());
    drop(broker);
}

struct RecordingAuthority {
    workspace: uuid::Uuid,
}

impl crate::certificate_authority::CertificateAuthority for RecordingAuthority {
    fn authorize(
        &self,
        workspace: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if workspace != self.workspace
            || certificate.user_public_key
                != SigningKey::from_bytes(&[ROOT_SEED; 32])
                    .verifying_key()
                    .to_bytes()
        {
            return Err("current certified authority denied".into());
        }
        Ok(())
    }
}
