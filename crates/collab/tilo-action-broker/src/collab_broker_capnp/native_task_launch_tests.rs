use super::*;
use crate::services::{LaunchPlace, LaunchableApplication, LauncherServices};
use app_framework::framework::testing::HeadlessApp;
use tilo_resource::Resource;
use word_processor::{WordProcessorWidget, framework::WordProcessorApp, model::Document};

const WRITER: &str = "so.tilo.word-processor";

struct Launcher {
    launches: tokio::sync::mpsc::UnboundedSender<String>,
    custody: Mutex<Option<std::path::PathBuf>>,
    workspaces: Mutex<Vec<tilo_desktop_model::Workspace>>,
}
impl LauncherServices for Launcher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        vec![LaunchableApplication {
            capabilities: Vec::new(),
            package_id: WRITER.into(),
            display_name: "Writer".into(),
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
        _place: &LaunchPlace,
    ) -> Result<(), String> {
        if workspace.room() != room()
            || app != WRITER
            || !(record.is_none() || record == Some("main"))
        {
            return Err("host activation named the wrong workspace, app or record".into());
        }
        let custody = self.custody.lock().unwrap().clone().unwrap();
        let snapshot = super::super::super::invocation_journal::durable_snapshot(&custody);
        let retained = serde_json::to_string(&snapshot).unwrap();
        if !retained.contains("\"host_activation\":true") {
            return Err("the actual host call requires custody already on disk".into());
        }
        if !retained.contains("AuthenticatedLocal") {
            return Err("custody must retain the real signed caller identity".into());
        }
        let activation = snapshot["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["intent"]["host_activation"] == true)
            .expect("the host effect has a persisted activation");
        let evidence: crate::action_evidence::AdmissionEvidence =
            serde_json::from_value(activation["admission"].clone())
                .expect("activation must be signed before the host launcher runs");
        evidence.validate(&evidence.receipt.facts).unwrap();
        let facts = &evidence.receipt.facts;
        assert_eq!(facts.app_id, WRITER);
        assert_eq!(facts.resource, format!("app.{WRITER}"));
        assert_eq!(facts.action, "open");
        let crate::action_evidence::VerifiedActor::NativeTask { root, device, .. } = &facts.actor
        else {
            panic!("host activation must retain task identity");
        };
        let owner = &activation["intent"]["caller"]["AuthenticatedLocal"];
        assert_eq!(serde_json::to_value(root).unwrap(), owner["root"]);
        assert_eq!(serde_json::to_value(device).unwrap(), owner["device"]);
        self.workspaces.lock().unwrap().push(workspace.clone());
        self.launches
            .send(app.into())
            .map_err(|error| error.to_string())
    }
    fn attest_process(&self, _pid: u32, _app: &str, _room: &str) -> bool {
        false
    }
}

struct SignedLaunchFixture {
    fixture: Fixture,
    launcher: Arc<Launcher>,
}
impl std::ops::Deref for SignedLaunchFixture {
    type Target = Fixture;
    fn deref(&self) -> &Fixture {
        &self.fixture
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
fn fixture() -> (
    SignedLaunchFixture,
    tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let launcher = Arc::new(Launcher {
        launches: sender,
        custody: Mutex::new(None),
        workspaces: Mutex::new(Vec::new()),
    });
    let f = Fixture::with_launcher(Some(launcher.clone()));
    *launcher.custody.lock().unwrap() = Some(f.directory.path().join("journal"));
    permission(&f, true);
    (
        SignedLaunchFixture {
            fixture: f,
            launcher,
        },
        receiver,
    )
}

fn permission(f: &Fixture, allow: bool) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    put_grant(
        f,
        Grant::new(
            // One id, so restoring the permission replaces the refusal
            // instead of standing beside it.
            "cold-writer-scope",
            Subject::agent("scribe"),
            TreePath::room(room().to_string()).app(WRITER),
            KindSet::READ.union(KindSet::UNDOABLE),
            if allow { Answer::Allow } else { Answer::Deny },
            Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

/// Answer the issue's work for both reading and editing Writer: a run is
/// answered for the person's decision to start it and by what the issue says
/// it may do, so grants alone leave it unanswered.
fn answer_issue_for_writer(f: &Fixture) {
    answer_issue_kind(f, tilo_action_policy::ActionKind::Read, Answer::Allow);
    answer_issue_kind(f, tilo_action_policy::ActionKind::Undoable, Answer::Allow);
}

/// Answer the issue's work for one kind of action. The issue's answer
/// decides for a run beside the agent's own grants, so a run holds the lesser
/// of the two.
fn answer_issue_kind(
    f: &Fixture,
    kind: tilo_action_policy::ActionKind,
    answer: tilo_action_policy::Answer,
) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    put_grant(
        f,
        agent_answers::answer_grant(
            &room().to_string(),
            Subject::Task(f.task.clone()),
            (&AnswerScope::app(WRITER), kind),
            answer,
            Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

fn once_permission(f: &Fixture) {
    put_grant(
        f,
        tilo_action_policy::Grant::new(
            "cold-writer-allow",
            tilo_action_policy::Subject::agent("scribe"),
            tilo_action_policy::TreePath::room(room().to_string()).app(WRITER),
            tilo_action_policy::KindSet::READ.union(tilo_action_policy::KindSet::UNDOABLE),
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
        )
        .with_lifetime(tilo_action_policy::Lifetime::Once),
    );
}

async fn invoke_provider(
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    provider: &str,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    invoke_provider_id(
        agent,
        run,
        provider,
        resource,
        action,
        arguments,
        uuid::Uuid::new_v4(),
    )
    .await
}

async fn invoke_provider_id(
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    provider: &str,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
    id: uuid::Uuid,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut request = agent.invoke_task_action_request();
    {
        let mut p = request.get();
        p.set_run_id(run.run_id);
        p.set_lease(&run.lease);
        p.set_provider(provider);
        p.set_resource(resource);
        p.set_action(action);
        p.set_argument(arguments.to_string());
        p.set_has_argument(true);
        p.set_invocation(id.to_string());
    }
    let response = request.send().promise.await?;
    let value = response.get()?;
    Ok((
        value.get_id(),
        tilo_actions::decode_json(value.get_update_json()?.to_str()?.as_bytes()).unwrap(),
    ))
}

/// The action a running application publishes for being opened, as the
/// broker describes it when it offers an installed app.
fn open_descriptor() -> tilo_actions::ActionDescriptor {
    tilo_actions::ActionDescriptor {
        address: tilo_actions::ActionAddress::new(format!("app.{WRITER}"), "open"),
        semantic_identity: None,
        resource_kind: "applications.app".into(),
        resource_label: "Writer".into(),
        action_label: "Open Writer".into(),
        description: None,
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: false },
            result: tilo_actions::ResultSpec::Job,
            ..tilo_actions::ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

struct Writer {
    app: HeadlessApp<WordProcessorApp>,
    channel: channel_handle::Client,
    stream: invocation_stream::Client,
    peer: u64,
}
impl Writer {
    async fn mount(f: &Fixture, changed: bool) -> Self {
        let mut app = HeadlessApp::<WordProcessorApp>::new().unwrap();
        let mut descriptors = app.actions().unwrap();
        // A live application also offers the activation that opened it, so a
        // waiting activation finds the instance it asked for.
        descriptors.push(open_descriptor());
        let peer = 210;
        let channel = f.app_channel(peer, WRITER, false);
        let mut publish = channel.publish_actions_request();
        publish.get().set_app_id(WRITER);
        {
            let mut rows = publish
                .get()
                .init_actions(u32::try_from(descriptors.len()).unwrap());
            for (index, descriptor) in descriptors.into_iter().enumerate() {
                let mut row = rows.reborrow().get(u32::try_from(index).unwrap());
                row.set_resource(&descriptor.address.resource);
                row.set_resource_kind(&descriptor.resource_kind);
                row.set_resource_label(&descriptor.resource_label);
                row.set_action(&descriptor.address.action);
                row.set_action_label(&descriptor.action_label);
                row.set_operation_kind(descriptor.kind.as_str());
                row.set_params_json(serde_json::to_string(&descriptor.params).unwrap());
                let mut contract = descriptor.contract;
                if changed && descriptor.address.action == "append-paragraph" {
                    contract.field = Some("comments".into());
                }
                row.set_contract_json(serde_json::to_string(&contract).unwrap());
            }
        }
        publish.send().promise.await.unwrap();
        let mut subscribe = channel.subscribe_invocations_request();
        subscribe.get().set_stable_invocation_ids(true);
        let response = subscribe.send().promise.await.unwrap();
        let stream = response.get().unwrap().get_stream().unwrap();
        Self {
            app,
            channel,
            stream,
            peer,
        }
    }

    async fn settle(&mut self, id: u64) {
        let response = tilo_test_env::within(
            "the Writer delivery",
            self.stream.read_request().send().promise,
        )
        .await
        .unwrap();
        let wire = response.get().unwrap();
        assert_eq!(wire.get_id(), id);
        let actor = wire.get_actor().unwrap().to_str().unwrap();
        assert_eq!(actor, "agent:scribe");
        let resource = wire.get_resource().unwrap().to_str().unwrap();
        let action = wire.get_action().unwrap().to_str().unwrap();
        let arguments: serde_json::Value =
            serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
        let invocation = tilo_actions::InvocationId(
            uuid::Uuid::from_slice(wire.get_invocation_uuid().unwrap()).unwrap(),
        );
        let context: tilo_actions::ActionContext =
            serde_json::from_str(wire.get_context_json().unwrap().to_str().unwrap()).unwrap();
        assert_eq!(context.group.as_ref().unwrap().kind, "task");
        let invoker = tilo_actions::ActionInvoker::delivered(actor, Some(&context));
        assert!(
            invoker.participant.is_some(),
            "the native host must supply the task's own view"
        );
        let output = self
            .app
            .call_by(invoker, resource, action, arguments)
            .unwrap();
        self.app.settle();
        let mut update = self.channel.update_invocation_request();
        update.get().set_id(id);
        update.get().set_update_json(
            serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output,
            })
            .unwrap(),
        );
        update.send().promise.await.unwrap();
    }

    fn assert_no_delivery(&self) {
        assert_eq!(
            crate::collab_broker_capnp::unread_invocations(self.peer),
            0,
            "a refused cold action reached the real UI owner"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_task_activation_refuses_missing_host_signer_before_launching() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, mut launches) = fixture();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            answer_issue_for_writer(&f);
            let paths =
                tilo_user_identity::IdentityPaths::under(f.directory.path().join("identity"));
            std::fs::remove_file(paths.device_privkey).unwrap();
            let result = invoke_provider(
                &agent,
                &run,
                &tilo_actions::remote::workspace_provider(&room().to_string()),
                &format!("app.{WRITER}"),
                "open",
                serde_json::json!({}),
            )
            .await;
            let error = result.expect_err("host activation requires the original host signer");
            assert!(
                error.to_string().contains("action_evidence_unavailable"),
                "{error}"
            );
            assert!(
                launches.try_recv().is_err(),
                "no launcher effect precedes signed admission"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_cold_package_activates_before_fresh_resource_actions_move_the_writer_view() {
    tokio::task::LocalSet::new().run_until(Box::pin(async {
        let (f, mut launches) = fixture();
        assert!(f.state.published_surfaces().is_empty());
        assert!(f.state.launchable.surfaces().is_empty());
        f.approve();
        let agent = f.channel(100, true);
        let run = delivered(&agent).await;
        answer_issue_for_writer(&f);
        let workspace_provider = tilo_actions::remote::workspace_provider(&room().to_string());
        let app_resource = format!("app.{WRITER}");
        let open = invoke_provider(&agent, &run, &workspace_provider,
            &app_resource, "open", serde_json::json!({}));
        tokio::pin!(open);
        let activated_app = tokio::select! {
            app = launches.recv() => app.unwrap(),
            answer = &mut open => panic!("cold activation answered before host launch: {answer:?}"),
        };
        assert_eq!(activated_app, WRITER);
        let mut writer = Box::pin(Writer::mount(&f, false)).await;
        assert!(matches!(open.await.unwrap().1, tilo_actions::InvocationUpdate::Succeeded { .. }));
        // Deterministic planner: append through the fresh resource publication,
        // then invoke Writer's registered view operation, never pointer input.
        let provider = tilo_actions::remote::application_action_target(&room().to_string(), WRITER, Document::KIND, "append-paragraph").provider;
        let appended = invoke_provider(&agent, &run, &provider, Document::KIND, "append-paragraph",
            serde_json::json!({"text":"Visible governed paragraph"})).await.unwrap();
        writer.settle(appended.0).await;
        assert!(writer.app.harness().inspection_json().unwrap().contains("Visible governed paragraph"),
            "the actual Writer semantic UI must expose the appended paragraph");
        writer.app.call_on(Document::KIND, "select-all", serde_json::json!({})).unwrap();
        writer.app.settle();
        let person = writer.app.with_surface::<WordProcessorWidget, _>(|widget| widget.local_participant_view()).unwrap();
        assert!(person.anchor.is_some(), "the person must have a real selection before agent navigation");
        assert_ne!(person.anchor.as_ref().unwrap(), &person.caret, "the selected range must be nonempty");
        assert!(person.caret.offset > 0, "the person's caret must not be the default start");
        let found = invoke_provider(&agent, &run, &provider, Document::KIND, "find-text",
            serde_json::json!({"query":"governed"})).await.unwrap();
        writer.settle(found.0).await;
        let doc = writer.app.peer_document();
        writer.app.with_surface::<WordProcessorWidget, _>(|widget| {
            let agent = widget.participant_view_or_default(&tilo_actions::ParticipantId::for_agent_task(&run.group()).unwrap(), &doc).unwrap();
            assert_eq!(agent.find.query, "governed");
            assert!(agent.anchor.is_some(), "navigation must select the visible match");
            assert_eq!(widget.local_participant_view(), person, "agent navigation must preserve the person's caret");
        }).unwrap();
        assert!(launches.try_recv().is_err(), "fresh resource actions must not launch a second copy");
    })).await;
}

fn retain_writer(f: &Fixture) -> String {
    let mut app = HeadlessApp::<WordProcessorApp>::new().unwrap();
    let descriptor = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|descriptor| descriptor.address.action == "append-paragraph")
        .unwrap();
    let surface = PublishedSurface {
        peer_id: 209,
        app_id: WRITER.into(),
        route: CrdtRoute::record(WRITER, room().to_string(), "main"),
        live: true,
        actions: vec![PublishedAction {
            semantic_identity: descriptor.semantic_identity,
            resource: descriptor.address.resource,
            action: descriptor.address.action,
            resource_kind: descriptor.resource_kind,
            resource_label: descriptor.resource_label,
            action_label: descriptor.action_label,
            description: descriptor.description,
            operation_kind: descriptor.kind,
            params: descriptor.params,
            contract: descriptor.contract,
            view: descriptor.view,
            input: PublishedInput::None,
        }],
    };
    let provider = launchable::launchable_provider(&surface);
    f.state.launchable.retain(&surface);
    assert_closed_writer(f);
    provider
}

fn assert_closed_writer(f: &Fixture) {
    let retained = f.state.published_surfaces();
    assert_eq!(retained.len(), 1, "the closed Writer remains discoverable");
    assert_eq!(retained[0].app_id, WRITER);
    let catalogue: serde_json::Value = serde_json::from_str(
        &f.state
            .action_catalogue_json(
                &room().to_string(),
                &hex::encode(f.native.certificate.user_public_key),
            )
            .unwrap(),
    )
    .unwrap();
    let closed = catalogue["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|surface| surface["record_id"] == "main")
        .collect::<Vec<_>>();
    assert_eq!(
        closed.len(),
        1,
        "retained discovery must not duplicate a route"
    );
    assert_eq!(closed[0]["live"], false);
    assert_eq!(
        closed[0]["provider"],
        launchable::launchable_provider(&retained[0])
    );
    assert_eq!(closed[0]["actions"][0]["action"], "append-paragraph");
    assert!(
        f.state
            .invoke(209, Document::KIND, "append-paragraph", None, "scribe")
            .is_err(),
        "stable catalogue discovery cannot revive ordinary publisher invocation"
    );
    assert_eq!(
        retained[0].route,
        CrdtRoute::record(WRITER, room().to_string(), "main")
    );
    assert!(
        !retained[0].live,
        "retention must not invent a live publisher"
    );
    assert!(
        !f.state.has_live_published_action(
            WRITER,
            &room().to_string(),
            "main",
            Document::KIND,
            "append-paragraph",
        ),
        "the retained action has no live UI publisher"
    );
    assert!(
        f.state.surfaces.lock().unwrap().is_empty(),
        "retention must not activate any live UI owner"
    );
    assert!(
        f.state.invocation_subscribers.lock().unwrap().is_empty(),
        "retention must not create an invocation stream for a UI owner"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn retained_actions_activate_the_host_but_changed_fresh_contracts_have_no_ui_effect() {
    tokio::task::LocalSet::new().run_until(Box::pin(async {
        for changed in [false, true] {
            let (f, mut launches) = fixture();
            let provider = retain_writer(&f);
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            answer_issue_for_writer(&f);
            let pending = invoke_provider(&agent, &run, &provider, Document::KIND, "append-paragraph",
                serde_json::json!({"text":"Retained request visible only under its fresh contract"}));
            tokio::pin!(pending);
            tokio::select! {
                app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                result = &mut pending => panic!("retained request answered before host launch: {result:?}"),
            }
            let mut writer = Box::pin(Writer::mount(&f, changed)).await;
            let result = tilo_test_env::within("the retained request answer", pending).await;
            if changed {
                assert!(result.unwrap_err().to_string().contains("contract changed"));
                writer.assert_no_delivery();
                assert!(!writer.app.harness().inspection_json().unwrap().contains("Retained request visible"));
            } else {
                writer.settle(result.unwrap().0).await;
                assert!(writer.app.harness().inspection_json().unwrap().contains("Retained request visible"),
                    "a retained call must reach the mounted Writer's actual semantic UI");
            }
            assert!(launches.try_recv().is_err(), "one retained request activates once");
        }
    })).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_revoked_cold_scope_is_refused_before_any_host_activation() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            let provider = retain_writer(&f);
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            answer_issue_for_writer(&f);
            let projected = WorkspaceAuthority::snapshot(&*f.native).unwrap();
            *f.native.cached_projection.lock().unwrap() = Some(projected);
            permission(&f, false);
            assert!(
                invoke_provider(
                    &agent,
                    &run,
                    &provider,
                    Document::KIND,
                    "append-paragraph",
                    serde_json::json!({"text":"Denied before activation"})
                )
                .await
                .is_err()
            );
            assert!(
                launches.try_recv().is_err(),
                "a stale catalogue is not authority to launch"
            );
            assert_closed_writer(&f);
        }))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stop_and_revocation_while_a_cold_app_is_waiting_deliver_no_action() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            for stop in [true, false] {
                let (f, mut launches) = fixture();
                f.approve();
                let agent = f.channel(100, true);
                let run = delivered(&agent).await;
                answer_issue_for_writer(&f);
                let provider = tilo_actions::remote::application_action_target(
                    &room().to_string(),
                    WRITER,
                    Document::KIND,
                    "append-paragraph",
                )
                .provider;
                let pending = invoke_provider(
                    &agent,
                    &run,
                    &provider,
                    Document::KIND,
                    "append-paragraph",
                    serde_json::json!({"text":"Must remain absent"}),
                );
                tokio::pin!(pending);
                tokio::select! {
                    app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                    result = &mut pending => panic!("answered before activation: {result:?}"),
                }
                if stop {
                    f.stop();
                } else {
                    permission(&f, false);
                }
                let mut writer = Box::pin(Writer::mount(&f, false)).await;
                assert!(
                    tilo_test_env::within("the waiting request answer", pending)
                        .await
                        .is_err()
                );
                writer.assert_no_delivery();
                assert!(
                    !writer
                        .app
                        .harness()
                        .inspection_json()
                        .unwrap()
                        .contains("Must remain absent")
                );
            }
        }))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_capped_authenticated_task_cannot_activate_outside_its_connection_ceiling() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            put_grant(
                &f,
                tilo_action_policy::Grant::new(
                    "foreign-writer-denied",
                    tilo_action_policy::Subject::ForeignDevice,
                    tilo_action_policy::TreePath::room(room().to_string()).app(WRITER),
                    tilo_action_policy::KindSet::UNDOABLE,
                    tilo_action_policy::Answer::Deny,
                    tilo_action_policy::Subject::person(hex::encode(
                        f.native.certificate.user_public_key,
                    )),
                ),
            );
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            answer_issue_for_writer(&f);
            let provider = tilo_actions::remote::workspace_provider(&room().to_string());
            let resource = format!("app.{WRITER}");
            let issue_run = f.native.runs.lock().unwrap()[0].clone();
            crate::services::TrackerRuns::authorize_action(
                f.native.as_ref(),
                &room().to_string(),
                &issue_run,
                &crate::services::NativeTaskActionRequirement {
                    binding: f
                        .state
                        .task_run_for(100, "scribe", run.run_id, &run.lease)
                        .unwrap()
                        .native,
                    app: WRITER.into(),
                    resource_kind: "applications.app".into(),
                    resource: resource.clone(),
                    action: "open".into(),
                    need: tilo_actions::ActionNeed::of(tilo_actions::ActionKind::Undoable),
                    consequential: false,
                    attended: false,
                    confirmed: false,
                },
            )
            .expect("canonical starter/task authority alone would permit this activation");
            f.cap_connection();
            assert!(
                invoke_provider(
                    &agent,
                    &run,
                    &provider,
                    &resource,
                    "open",
                    serde_json::json!({})
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("certified agent session is no longer admitted")
            );
            assert!(
                launches.try_recv().is_err(),
                "the actual capped caller must not reach the host launcher"
            );
            assert!(f.state.published_surfaces().is_empty());
        }))
        .await;
}

#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
#[tokio::test(flavor = "current_thread")]
async fn once_activation_is_spent_and_completed_retry_after_close_never_relaunches() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            // The room holds the agent to the changes it is given app by app,
            // so beyond the one-use grant its run may not open Writer.
            put_grant(
                &f,
                tilo_action_policy::Grant::new(
                    "cold-writer-room-held",
                    tilo_action_policy::Subject::agent("scribe"),
                    tilo_action_policy::TreePath::room(room().to_string()),
                    tilo_action_policy::KindSet::UNDOABLE,
                    tilo_action_policy::Answer::Deny,
                    tilo_action_policy::Subject::person(hex::encode(
                        f.native.certificate.user_public_key,
                    )),
                ),
            );
            // The issue's answer stands before the one-use grant is given, so
            // the grant is the only new authority this activation sees.
            answer_issue_for_writer(&f);
            once_permission(&f);
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let provider = tilo_actions::remote::workspace_provider(&room().to_string());
            let resource = format!("app.{WRITER}");
            let id = uuid::Uuid::new_v4();
            let open = invoke_provider_id(
                &agent,
                &run,
                &provider,
                &resource,
                "open",
                serde_json::json!({}),
                id,
            );
            tokio::pin!(open);
            tokio::select! {
                app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                answer = &mut open => panic!("activation answered before launch: {answer:?}"),
            }
            // A separate invocation cannot borrow the spent permission, even
            // while the original activation is waiting for its publisher. The
            // issue still lets the work edit Writer, so that grant is what
            // stands between its own call and the person's one use.
            permission(&f, false);
            let refused = invoke_provider(
                &agent,
                &run,
                &provider,
                &resource,
                "open",
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
            assert!(refused.to_string().contains("not allowed"));
            assert!(launches.try_recv().is_err());
            // Live authority is required even for a retained receipt. Restore it
            // without a one-use grant before observing the admitted invocation.
            permission(&f, true);
            let writer = Box::pin(Writer::mount(&f, false)).await;
            let first = open.await.unwrap();
            assert!(matches!(
                first.1,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            f.state.surfaces.lock().unwrap().remove(&210);
            f.state.invocation_subscribers.lock().unwrap().remove(&210);
            drop(writer);
            once_permission(&f);
            // A certain terminal receipt must not wait for the closed publisher.
            let retry = tilo_test_env::within(
                "the retry's terminal receipt",
                invoke_provider_id(
                    &agent,
                    &run,
                    &provider,
                    &resource,
                    "open",
                    serde_json::json!({}),
                    id,
                ),
            )
            .await
            .unwrap();
            assert_eq!(retry, first);
            assert!(
                f.native.spent_grants.lock().unwrap().is_empty(),
                "observing an exact retry must not spend a newly supplied one-use grant"
            );
            assert!(
                launches.try_recv().is_err(),
                "an exact retry must not replay activation"
            );
            permission(&f, false);
            assert!(
                invoke_provider_id(
                    &agent,
                    &run,
                    &provider,
                    &resource,
                    "open",
                    serde_json::json!({}),
                    id
                )
                .await
                .is_err(),
                "retained receipts still require current live authority"
            );
            assert!(launches.try_recv().is_err());
        }))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_terminal_activation_receipt_rechecks_the_authenticated_connection_ceiling() {
    tokio::task::LocalSet::new().run_until(Box::pin(async {
        let (f, mut launches) = fixture();
        f.approve();
        let agent = f.channel(100, true);
        let run = delivered(&agent).await;
        answer_issue_for_writer(&f);
        let provider = tilo_actions::remote::workspace_provider(&room().to_string());
        let resource = format!("app.{WRITER}");
        let id = uuid::Uuid::new_v4();
        let open = invoke_provider_id(&agent, &run, &provider, &resource, "open", serde_json::json!({}), id);
        tokio::pin!(open);
        tokio::select! {
            app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
            answer = &mut open => panic!("activation answered before launch: {answer:?}"),
        }
        let writer = Box::pin(Writer::mount(&f, false)).await;
        assert!(matches!(open.await.unwrap().1, tilo_actions::InvocationUpdate::Succeeded { .. }));
        f.state.surfaces.lock().unwrap().remove(&210);
        f.state.invocation_subscribers.lock().unwrap().remove(&210);
        drop(writer);
        put_grant(&f, tilo_action_policy::Grant::new(
            "foreign-writer-denied-after-close", tilo_action_policy::Subject::ForeignDevice,
            tilo_action_policy::TreePath::room(room().to_string()).app(WRITER),
            tilo_action_policy::KindSet::UNDOABLE, tilo_action_policy::Answer::Deny,
            tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ));
        f.cap_connection();
        assert!(invoke_provider_id(&agent, &run, &provider, &resource, "open", serde_json::json!({}), id)
            .await.unwrap_err().to_string().contains("certified agent session is no longer admitted"),
            "a retained terminal receipt must not bypass the actual connection's current ceiling");
        assert!(launches.try_recv().is_err(), "receipt observation must not replay the host side effect");
    })).await;
}

#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
/// Durable custody must not turn an expired connection or workspace decision
/// into permission to perform the host effect. Mutate only the live projection
/// here: canonical task mutations are serialized behind this admission turn.
#[tokio::test(flavor = "current_thread")]
async fn activation_rechecks_original_caller_and_live_policy_after_custody() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            for change in ["connection", "policy", "workspace", "broker", "run"] {
                let (f, mut launches) = fixture();
                put_grant(
                    &f,
                    tilo_action_policy::Grant::new(
                        "foreign-writer-barrier-denied",
                        tilo_action_policy::Subject::ForeignDevice,
                        tilo_action_policy::TreePath::room(room().to_string()).app(WRITER),
                        tilo_action_policy::KindSet::UNDOABLE,
                        tilo_action_policy::Answer::Deny,
                        tilo_action_policy::Subject::person(hex::encode(
                            f.native.certificate.user_public_key,
                        )),
                    ),
                );
                f.approve();
                let agent = f.channel(100, true);
                let run = delivered(&agent).await;
                answer_issue_for_writer(&f);
                let id = uuid::Uuid::new_v4();
                let native = f.native.clone();
                let state = f.state.clone();
                let run_id = run.run_id;
                let crossed = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let observed = crossed.clone();
                super::super::super::native_task_launch::test_barrier::install(
                    tilo_actions::InvocationId(id),
                    Box::new(move || {
                        observed.store(true, std::sync::atomic::Ordering::SeqCst);
                        match change {
                            "connection" => native
                                .capped_connection
                                .store(true, std::sync::atomic::Ordering::SeqCst),
                            "broker" => state
                                .accepting
                                .store(false, std::sync::atomic::Ordering::SeqCst),
                            "run" => {
                                state
                                    .pending_task_runs
                                    .lock()
                                    .unwrap()
                                    .get_mut(&run_id)
                                    .unwrap()
                                    .terminal = true;
                            }
                            _ => {
                                let mut tree = WorkspaceAuthority::snapshot(&*native).unwrap();
                                if change == "workspace" {
                                    tree.workspaces.clear();
                                } else {
                                    tree.workspaces[0].policy.grants.push(
                                        tilo_action_policy::Grant::new(
                                            "writer-denied-after-custody",
                                            tilo_action_policy::Subject::agent("scribe"),
                                            tilo_action_policy::TreePath::room(room().to_string())
                                                .app(WRITER),
                                            tilo_action_policy::KindSet::UNDOABLE,
                                            tilo_action_policy::Answer::Deny,
                                            tilo_action_policy::Subject::person(hex::encode(
                                                native.certificate.user_public_key,
                                            )),
                                        ),
                                    );
                                }
                                *native.cached_projection.lock().unwrap() = Some(tree);
                            }
                        }
                    }),
                );
                let result = tilo_test_env::within(
                    "post-custody checks must not re-enter task mutation admission",
                    invoke_provider_id(
                        &agent,
                        &run,
                        &tilo_actions::remote::workspace_provider(&room().to_string()),
                        &format!("app.{WRITER}"),
                        "open",
                        serde_json::json!({}),
                        id,
                    ),
                )
                .await;
                assert!(result.is_err(), "{change} must revoke activation");
                assert!(crossed.load(std::sync::atomic::Ordering::SeqCst));
                assert!(
                    launches.try_recv().is_err(),
                    "{change} reached the launcher"
                );
                assert!(f.state.surfaces.lock().unwrap().is_empty());
                assert!(f.state.invocation_subscribers.lock().unwrap().is_empty());
                let digest = blake3::hash(format!("native-host-activation:{id}").as_bytes());
                let key = InvocationKey {
                    room: room().to_string(),
                    host: f.state.invocation_host().unwrap(),
                    owner: InvocationOwner::NativeTask {
                        agent: "scribe".into(),
                        task: f.task.clone(),
                        epoch: 1,
                    },
                    invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                        digest.as_bytes()[..16].try_into().unwrap(),
                    )),
                };
                let activation = f
                    .state
                    .journal
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .lookup(&key)
                    .unwrap()
                    .unwrap();
                assert!(
                    matches!(activation.update, tilo_actions::InvocationUpdate::Failed { error, .. }
                    if error.code == "host_activation_failed")
                );
                assert!(!activation.uncertain);
                let admission = activation
                    .admission
                    .expect("signed host activation admission");
                admission.validate(&admission.receipt.facts).unwrap();
                crate::action_evidence::verify_terminal(
                    &activation.terminal.expect("signed host activation failure"),
                    &admission.receipt,
                )
                .unwrap();
            }
        }))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn activation_uses_the_current_workspace_after_custody() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            answer_issue_for_writer(&f);
            let id = uuid::Uuid::new_v4();
            let native = f.native.clone();
            super::super::super::native_task_launch::test_barrier::install(
                tilo_actions::InvocationId(id),
                Box::new(move || {
                    let mut tree = WorkspaceAuthority::snapshot(&*native).unwrap();
                    tree.workspaces[0].name = "Current launch workspace".into();
                    *native.cached_projection.lock().unwrap() = Some(tree);
                }),
            );
            let provider = tilo_actions::remote::workspace_provider(&room().to_string());
            let resource = format!("app.{WRITER}");
            let open = invoke_provider_id(
                &agent,
                &run,
                &provider,
                &resource,
                "open",
                serde_json::json!({}),
                id,
            );
            tokio::pin!(open);
            tokio::select! {
                app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                result = &mut open => panic!("answered before activation: {result:?}"),
            }
            assert_eq!(
                f.launcher.workspaces.lock().unwrap()[0].name,
                "Current launch workspace"
            );
            let _writer = Box::pin(Writer::mount(&f, false)).await;
            assert!(tilo_test_env::within("the activation", open).await.is_ok());
            assert!(launches.try_recv().is_err());
        }))
        .await;
}

fn ask_first_for_writer(f: &Fixture) {
    put_grant(
        f,
        tilo_action_policy::Grant::new(
            "cold-writer-ask",
            tilo_action_policy::Subject::agent("scribe"),
            tilo_action_policy::TreePath::room(room().to_string()).app(WRITER),
            tilo_action_policy::KindSet::UNDOABLE,
            tilo_action_policy::Answer::Ask,
            tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

/// A retained action whose grants ask first pauses at a reviewable draft
/// before anything is launched, as a running application's action does; once
/// the person confirms, the application is launched for exactly what they
/// confirmed, opening it included.
#[tokio::test(flavor = "current_thread")]
async fn a_retained_action_that_asks_first_pauses_at_a_draft_before_launching() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            ask_first_for_writer(&f);
            let provider = retain_writer(&f);
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            answer_issue_for_writer(&f);
            let id = uuid::Uuid::new_v4();
            let arguments = serde_json::json!({"text":"Drafted while Writer was closed"});
            let waiting = invoke_provider_id(
                &agent,
                &run,
                &provider,
                Document::KIND,
                "append-paragraph",
                arguments.clone(),
                id,
            )
            .await
            .unwrap_err();
            assert!(
                waiting
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "{waiting}"
            );
            assert!(launches.try_recv().is_err(), "nothing launched for a draft");
            let asked = super::confirmation::ask(
                &f,
                (Document::KIND, "append-paragraph"),
                arguments.as_object().unwrap(),
            );
            assert_eq!(asked.app, WRITER);
            super::confirmation::answer(
                &f,
                crate::services::TrackerAnswer::Confirmed {
                    edits: serde_json::Value::Null,
                },
            );
            let pending = invoke_provider_id(
                &agent,
                &run,
                &provider,
                Document::KIND,
                "append-paragraph",
                arguments,
                id,
            );
            tokio::pin!(pending);
            tokio::select! {
                app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                result = &mut pending => panic!("confirmed request answered before host launch: {result:?}"),
            }
            let mut writer = Box::pin(Writer::mount(&f, false)).await;
            let result = tilo_test_env::within("the cold request answer", pending).await;
            writer.settle(result.unwrap().0).await;
            assert!(
                writer
                    .app
                    .harness()
                    .inspection_json()
                    .unwrap()
                    .contains("Drafted while Writer was closed"),
                "the confirmed draft reached the launched Writer"
            );
        }))
        .await;
}
