//! Native Store submissions keep task custody and require exact host disclosure consent.
use super::*;
use crate::app_actions::{AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest};
use crate::services::TrackerAnswer;

const STORE: &str = "so.tilo.store";
const RESOURCE: &str = "store:package";

pub(super) struct Sheet(
    pub(super)  tokio::sync::mpsc::UnboundedSender<(
        AppActionConsentRequest,
        tokio::sync::oneshot::Sender<AppActionConsent>,
    )>,
);
impl AppActionConsentPrompter for Sheet {
    fn ask(
        &self,
        request: AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionConsent>> {
        let (answer, receive) = tokio::sync::oneshot::channel();
        self.0.send((request, answer)).ok()?;
        Some(receive)
    }
}

fn operation(action: &str, content: &str) -> String {
    use registry_types::{ExternalModerationOperation, ModerationDestination, ModerationMutation};
    // Keep repeated edits byte-identical while exercising the live validity gate.
    static ISSUED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let issued = *ISSUED.get_or_init(tilo_clock::unix_now);
    serde_json::to_string(&ExternalModerationOperation {
        request_id: uuid::Uuid::from_u128(9201).to_string(),
        issued_at_unix: issued,
        expires_at_unix: issued.saturating_add(3600),
        destination: ModerationDestination {
            base_url: "https://registry.example".into(),
            owner_pubkey_hex: "ab".repeat(32),
        },
        mutation: if action == "appeal" {
            ModerationMutation::PublisherAppeal {
                case_id: "case-one".into(),
                reason: content.into(),
                evidence_digest_hex: Some("cd".repeat(32)),
            }
        } else {
            ModerationMutation::FileReport {
                package_id: "org.example.app".into(),
                category: "policy".into(),
                summary: content.into(),
                evidence_digest_hex: "cd".repeat(32),
            }
        },
    })
    .unwrap()
}

async fn target(f: &Fixture, action: &str) -> (channel_handle::Client, invocation_stream::Client) {
    let target = f.app_channel(200, STORE, false);
    let mut publish = target.publish_actions_request();
    publish.get().set_app_id(STORE);
    {
        let mut row = publish.get().init_actions(1).get(0);
        row.set_resource(RESOURCE);
        row.set_action(action);
        row.set_resource_kind("store.package");
        row.set_resource_label("Package");
        row.set_action_label("Submit");
        row.set_operation_kind("effect");
        row.set_params_json(
            serde_json::to_string(&vec![
                tilo_actions::ActionParam::required(
                    "app",
                    tilo_actions::ParamType::Text,
                    "Package",
                ),
                tilo_actions::ActionParam::required(
                    "operation",
                    tilo_actions::ParamType::Text,
                    "Report",
                ),
            ])
            .unwrap(),
        );
        row.set_contract_json(
            serde_json::to_string(&tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                result: tilo_actions::ResultSpec::Job,
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                ..Default::default()
            })
            .unwrap(),
        );
    }
    publish.send().promise.await.unwrap();
    f.state
        .launchable
        .retain(&f.state.surfaces.lock().unwrap()[&200]);
    let mut subscribe = target.subscribe_invocations_request();
    subscribe.get().set_stable_invocation_ids(true);
    let response = subscribe.send().promise.await.unwrap();
    let stream = response.get().unwrap().get_stream().unwrap();
    (target, stream)
}

/// The agent asks its person about the Store `action` it was answered asks
/// first, on the issue.
fn ask(f: &Fixture, action: &str) -> crate::services::TrackerQuestion {
    let arguments =
        serde_json::json!({"app": "org.example.app", "operation": operation(action, "Original")});
    confirmation::ask(f, (RESOURCE, action), arguments.as_object().unwrap())
}

/// The person confirms the run's question, changing the operation to `text`.
fn confirm(f: &Fixture, text: String) {
    confirmation::answer(
        f,
        TrackerAnswer::Confirmed {
            edits: serde_json::to_value(vec![tilo_actions::DraftEdit {
                path: vec!["operation".into()],
                text,
            }])
            .unwrap(),
        },
    );
}

async fn invoke(
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    action: &str,
    provider: u8,
) -> Result<(), capnp::Error> {
    let mut request = agent.invoke_task_action_request();
    {
        let mut p = request.get();
        p.set_run_id(run.run_id);
        p.set_lease(&run.lease);
        p.set_peer_id(if provider == 0 { 200 } else { 0 });
        match provider {
            1 => p.set_provider(format!("launch:{}:{STORE}:main", room())),
            2 => p.set_provider(
                tilo_actions::remote::application_action_target(
                    &room().to_string(),
                    STORE,
                    RESOURCE,
                    action,
                )
                .provider,
            ),
            _ => {}
        }
        p.set_resource(RESOURCE);
        p.set_action(action);
        p.set_argument(serde_json::json!({"app": "org.example.app", "operation": operation(action, "Original")}).to_string());
        p.set_has_argument(true);
        p.set_invocation(uuid::Uuid::from_u128(9200).to_string());
    }
    request.send().promise.await.map(|_| ())
}

fn provision_receipt_signer(
    identity: &std::path::Path,
    environment: &mut tilo_test_env::ScopedEnv,
) {
    environment
        .set("XDG_CONFIG_HOME", identity.join("config"))
        .set("TILO_IDENTITY_ROOT", identity.join("identity"))
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identity.join("config/tilo/identity"),
        &tilo_user_identity::IdentityPaths::under(identity.join("identity")),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
}

/// Answer for the issue's work, as Tasks records it when a person starts a
/// run: reading and undoable changes allowed within the room, reaching
/// outside it asked first.
fn answer_issue(f: &Fixture, answer: tilo_action_policy::Answer) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let given_by =
        tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key));
    let grants = [
        tilo_action_policy::ActionKind::Read,
        tilo_action_policy::ActionKind::Undoable,
        tilo_action_policy::ActionKind::Outside,
    ]
    .map(|kind| {
        agent_answers::answer_grant(
            &room().to_string(),
            tilo_action_policy::Subject::Task(f.task.clone()),
            (&AnswerScope::app(STORE), kind),
            if kind == tilo_action_policy::ActionKind::Outside {
                tilo_action_policy::Answer::Ask
            } else {
                answer
            },
            given_by.clone(),
        )
    });
    for grant in grants {
        put_grant(f, grant);
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
#[tokio::test(flavor = "current_thread")]
async fn native_store_host_sheet_uses_final_task_edits_and_rechecks_authority() {
    let identity = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    provision_receipt_signer(identity.path(), &mut environment);
    tokio::task::LocalSet::new().run_until(async {
        for provider in [0, 1, 2] {
            for action in ["submit-report", "appeal"] {
                for outcome in [0, 1, 2, 3] {
                    let f = Fixture::new();
                    answer_issue(&f, tilo_action_policy::Answer::Allow);
                    put_grant(&f, tilo_action_policy::Grant::new(
                        "store-allow", tilo_action_policy::Subject::agent("scribe"),
                        tilo_action_policy::TreePath::room(room().to_string()).app(STORE),
                        tilo_action_policy::KindSet::ALL, tilo_action_policy::Answer::Allow,
                        tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
                    ));
                    f.approve();
                    let agent = f.channel(100, true);
                    let run = delivered(&agent).await;
                    let (_target, stream) = target(&f, action).await;
                    let waiting = invoke(&agent, &run, action, provider).await.unwrap_err();
                    assert!(waiting.to_string().contains(tilo_actions::draft::AWAITING_CONFIRMATION), "{waiting}");
                    ask(&f, action);
                    confirm(&f, operation(action, "Reviewed"));
                    let unavailable = invoke(&agent, &run, action, provider).await.unwrap_err();
                    assert!(unavailable.to_string().contains("host confirmation unavailable"), "{unavailable}");
                    assert!(confirmation::nothing_delivered());
                    let (send, mut sheets) = tokio::sync::mpsc::unbounded_channel();
                    f.state.set_app_action_consent(Arc::new(Sheet(send)));
                    let call = invoke(&agent, &run, action, provider);
                    tokio::pin!(call);
                    let (question, answer) = tokio::select! {
                        result = &mut call => panic!("dispatched without a host sheet: {result:?}"),
                        sheet = sheets.recv() => sheet.unwrap(),
                    };
                    assert_eq!(question.target_app, STORE);
                    assert_eq!(question.person, principal_id_for_root(f.native.certificate.user_public_key));
                    let preview = question.moderation.expect("typed disclosure, not a generic approval");
                    assert_eq!(preview.content, "Reviewed");
                    assert_eq!(preview.signing_root, hex::encode(f.native.certificate.user_public_key));
                    assert!(confirmation::nothing_delivered());
                    if outcome == 1 { f.cap_connection(); }
                    if outcome == 3 {
                        f.state.surfaces.lock().unwrap().get_mut(&200).unwrap().actions[0].contract.field = Some("changed".into());
                    }
                    answer.send(if outcome == 2 { AppActionConsent::Deny } else { AppActionConsent::Once }).unwrap();
                    let result = call.await;
                    if outcome != 0 {
                        assert!(result.is_err(), "declined or revoked disclosure ran");
                        assert!(confirmation::nothing_delivered());
                    } else {
                        result.unwrap();
                        let delivered = stream.read_request().send().promise.await.unwrap();
                        let wire = delivered.get().unwrap();
                        let arguments: serde_json::Value = serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
                        assert_eq!(arguments["operation"], operation(action, "Reviewed"));
                    }
                }
            }
        }
    }).await;
}

struct StoreLauncher(tokio::sync::mpsc::UnboundedSender<String>);

impl crate::services::LauncherServices for StoreLauncher {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        vec![crate::services::LaunchableApplication {
            capabilities: Vec::new(),
            package_id: STORE.into(),
            display_name: "Store".into(),
            unconfined: false,
            offers: Vec::new(),
        }]
    }

    #[allow(
        clippy::panic_in_result_fn,
        reason = "a test double asserts what it is asked to launch; the Result is the launcher contract's"
    )]
    fn launch(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        record: Option<&str>,
        _place: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        assert_eq!(app, STORE);
        assert_eq!(workspace.room(), room());
        assert!(record.is_none() || record == Some("main"));
        self.0.send(app.into()).map_err(|error| error.to_string())
    }

    fn attest_process(&self, _pid: u32, _app: &str, _room: &str) -> bool {
        false
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
#[tokio::test(flavor = "current_thread")]
async fn cold_store_edited_task_draft_launches_then_discloses_without_a_second_draft() {
    let identity = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    provision_receipt_signer(identity.path(), &mut environment);
    tokio::task::LocalSet::new().run_until(async {
        for action in ["submit-report", "appeal"] {
            let (launch, mut launches) = tokio::sync::mpsc::unbounded_channel();
            let f = Fixture::with_launcher(Some(Arc::new(StoreLauncher(launch))));
            answer_issue(&f, tilo_action_policy::Answer::Allow);
            put_grant(&f, tilo_action_policy::Grant::new(
                "store-allow", tilo_action_policy::Subject::agent("scribe"),
                tilo_action_policy::TreePath::room(room().to_string()).app(STORE),
                tilo_action_policy::KindSet::ALL, tilo_action_policy::Answer::Allow,
                tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
            ));
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (old_target, old_stream) = target(&f, action).await;
            f.state.surfaces.lock().unwrap().remove(&200);
            f.state.invocation_subscribers.lock().unwrap().remove(&200);
            drop((old_target, old_stream));
            assert!(!f.state.published_surfaces()[0].live);
            let (send, mut sheets) = tokio::sync::mpsc::unbounded_channel();
            f.state.set_app_action_consent(Arc::new(Sheet(send)));

            let waiting = invoke(&agent, &run, action, 1).await.unwrap_err();
            assert!(waiting.to_string().contains(tilo_actions::draft::AWAITING_CONFIRMATION), "{waiting}");
            let asked = ask(&f, action);
            assert_eq!(asked.app, STORE);
            assert!(launches.try_recv().is_err(), "an unconfirmed draft cannot launch Store");
            assert!(sheets.try_recv().is_err(), "disclosure needs a live provider");
            confirm(&f, operation(action, "Reviewed while Store was closed"));

            let call = invoke(&agent, &run, action, 1);
            tokio::pin!(call);
            // Other tests deliberately occupy the process-wide launch pool.
            // Wait for the actual host launch, not a two-second scheduling
            // budget; a completed invocation before that event is still wrong.
            tilo_test_env::within("confirmed Store draft to reach the host launcher", async {
                tokio::select! {
                    result = &mut call => panic!("confirmed draft failed before launch: {result:?}"),
                    app = launches.recv() => assert_eq!(app.unwrap(), STORE),
                }
            }).await;
            assert!(futures::poll!(&mut call).is_pending(), "the launch alone completes nothing");
            assert!(sheets.try_recv().is_err(), "launch alone cannot authorize disclosure");
            let custody = crate::collab_broker_capnp::invocation_journal::durable_snapshot(&f.directory.path().join("journal"));
            let launch = custody["records"].as_array().unwrap().iter()
                .find(|record| record["intent"]["host_activation"] == true).unwrap();
            let edited_argument = serde_json::json!({
                "app": "org.example.app",
                "operation": operation(action, "Reviewed while Store was closed"),
            }).to_string();
            let edited_request = serde_json::json!({
                "provider": format!("launch:{}:{STORE}:main", room()),
                "resource": RESOURCE,
                "action": action,
                "argument": edited_argument,
            });
            assert_eq!(launch["request_digest"], hex::encode(crate::collab_broker_capnp::invocation_journal::admission_request_digest(&edited_request)),
                "host launch custody binds the edit without retaining disclosure text");

            let (_target, stream) = target(&f, action).await;
            let (question, answer) = tilo_test_env::within("live Store publication to request disclosure", async {
                tokio::select! {
                    result = &mut call => panic!("edited retained action asked again or bypassed disclosure: {result:?}"),
                    sheet = sheets.recv() => sheet.unwrap(),
                }
            }).await;
            assert_eq!(question.target_app, STORE);
            assert_eq!(question.person, principal_id_for_root(f.native.certificate.user_public_key));
            let preview = question.moderation.expect("typed host disclosure");
            assert_eq!(preview.content, "Reviewed while Store was closed");
            assert_eq!(preview.signing_root, hex::encode(f.native.certificate.user_public_key));
            let confirmed = confirmation::held(&f).unwrap();
            assert_eq!(confirmed.id, asked.id, "delivery must not create a second task draft");
            assert!(!matches!(confirmed.answer, TrackerAnswer::Waiting));
            assert!(confirmation::nothing_delivered());
            answer.send(AppActionConsent::Once).unwrap();
            tilo_test_env::within("confirmed Store invocation to be admitted", &mut call).await.unwrap();
            let delivered = tilo_test_env::within("Store stream to receive the confirmed invocation", stream.read_request().send().promise)
                .await.unwrap();
            let wire = delivered.get().unwrap();
            assert_eq!(wire.get_action().unwrap().to_str().unwrap(), action);
            let arguments: serde_json::Value = serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
            assert_eq!(arguments["operation"], operation(action, "Reviewed while Store was closed"));
            assert!(confirmation::held(&f).is_none_or(|held| held.id == asked.id && !matches!(held.answer, TrackerAnswer::Waiting)));
            assert!(launches.try_recv().is_err(), "one confirmed draft launches once");
            assert!(sheets.try_recv().is_err(), "one dispatch needs one disclosure sheet");
            assert!(confirmation::nothing_delivered());
        }
    }).await;
}
