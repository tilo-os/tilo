#![allow(
    clippy::significant_drop_tightening,
    reason = "every test here keeps its fixture and guards alive through its assertions"
)]
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
    /// The apps this session serves itself, by the app: the resource each is served at.
    served: Mutex<std::collections::BTreeMap<String, String>>,
}

impl Launcher {
    /// Report `app` as one the session serves, at `root`: its `open` is answered, never launched.
    fn served_with(&self, app: &str, root: &str) {
        self.served
            .lock()
            .unwrap()
            .insert(app.to_owned(), root.to_owned());
    }
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
        if workspace.id() != workspace_id()
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
    fn attest_process(&self, _pid: u32, _app: &str, _workspace: &str) -> bool {
        false
    }
    fn served(&self, app: &str) -> Option<String> {
        self.served.lock().unwrap().get(app).cloned()
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

fn fixture() -> (
    SignedLaunchFixture,
    tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let launcher = Arc::new(Launcher {
        launches: sender,
        custody: Mutex::new(None),
        workspaces: Mutex::new(Vec::new()),
        served: Mutex::new(std::collections::BTreeMap::new()),
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
            format!("cold-writer-{}", if allow { "allow" } else { "deny" }),
            Subject::agent("scribe"),
            TreePath::workspace(workspace_id().to_string()).app(WRITER),
            KindSet::READ.union(KindSet::UNDOABLE),
            if allow { Answer::Allow } else { Answer::Deny },
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
            tilo_action_policy::TreePath::workspace(workspace_id().to_string()).app(WRITER),
            tilo_action_policy::KindSet::READ.union(tilo_action_policy::KindSet::UNDOABLE),
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
        )
        .with_lifetime(tilo_action_policy::Lifetime::Once),
    );
}

async fn invoke_provider(
    agent: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
    provider: &str,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) -> Result<(u64, tilo_actions::InvocationUpdate), runtime_portal::NotAvailable> {
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
    agent: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
    provider: &str,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
    id: uuid::Uuid,
) -> Result<(u64, tilo_actions::InvocationUpdate), runtime_portal::NotAvailable> {
    let argument = arguments.to_string();
    let (id, update) = agent
        .invoke_task_action(
            run.run_id,
            &run.lease,
            runtime_collab::TaskAction {
                peer_id: 0,
                provider,
                resource,
                action,
                argument: Some(&argument),
            },
            &id.to_string(),
        )
        .await?;
    Ok((id, tilo_actions::decode_json(update.as_bytes()).unwrap()))
}

struct Writer {
    app: HeadlessApp<WordProcessorApp>,
    channel: runtime_collab::ChannelHandle,
    stream: InvocationStreamServer,
    peer: u64,
}
impl Writer {
    async fn mount(f: &Fixture, changed: bool) -> Self {
        let mut app = HeadlessApp::<WordProcessorApp>::new().unwrap();
        let descriptors = app.actions().unwrap();
        let peer = 210;
        let handle = Rc::new(f.app_handle(peer, WRITER, false));
        let channel = wire_channel_rc(Rc::clone(&handle));
        let actions = descriptors
            .iter()
            .map(|descriptor| {
                let mut action = runtime_collab::ExportedAction::from(descriptor);
                if changed && descriptor.address.action == "append-paragraph" {
                    action.contract.field = Some("comments".into());
                }
                action
            })
            .collect::<Vec<_>>();
        channel.publish_actions(&actions).await.unwrap();
        let stream = handle.invocation_feed(true).unwrap();
        Self {
            app,
            channel,
            stream,
            peer,
        }
    }

    async fn settle(&mut self, id: u64) {
        let delivered = tilo_test_env::within("the Writer delivery", self.stream.next())
            .await
            .unwrap();
        assert_eq!(delivered.id, id);
        assert_eq!(delivered.actor, "agent:scribe");
        let arguments: serde_json::Value =
            serde_json::from_str(delivered.argument.as_deref().unwrap()).unwrap();
        let context = delivered.context.clone().unwrap();
        assert_eq!(context.group.as_ref().unwrap().kind, "task");
        let invoker = tilo_actions::ActionInvoker::delivered(&delivered.actor, Some(&context));
        assert!(
            invoker.participant.is_some(),
            "the native host must supply the task's own view"
        );
        let output = self
            .app
            .call_by(invoker, &delivered.resource, &delivered.action, arguments)
            .unwrap();
        self.app.settle();
        self.channel
            .update_invocation(
                id,
                &serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivered.invocation,
                    output,
                })
                .unwrap(),
            )
            .await
            .unwrap();
    }

    fn assert_no_delivery(&self) {
        assert_eq!(
            crate::collab_app_broker::unread_invocations(self.peer),
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
            let paths =
                tilo_user_identity::IdentityPaths::under(f.directory.path().join("identity"));
            std::fs::remove_file(paths.device_privkey).unwrap();
            let result = invoke_provider(
                &agent,
                &run,
                &tilo_actions::remote::workspace_provider(&workspace_id().to_string()),
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
async fn a_task_open_of_an_app_the_session_serves_is_refused_before_any_wait() {
    // An app whose model the session runs itself has no window process: a task's `open` of it is
    // refused at once, naming the resource to open, rather than left to the delivery wait for a
    // process that never publishes (`docs/designs/app-language/one-app-model.md` §3). Before this,
    // the request waited the whole bounded delivery and the agent heard nothing for ten seconds.
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, mut launches) = fixture();
            // The launcher reports the Writer as served by the session, at its root.
            f.launcher
                .served_with(WRITER, "tl:so.tilo.word-processor:root-main");
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let started = std::time::Instant::now();
            let result = invoke_provider(
                &agent,
                &run,
                &tilo_actions::remote::workspace_provider(&workspace_id().to_string()),
                &format!("app.{WRITER}"),
                "open",
                serde_json::json!({}),
            )
            .await;
            let error = result.expect_err("the served app has no window to open");
            assert!(
                error.to_string().contains("served by this session"),
                "{error}"
            );
            assert!(
                error
                    .to_string()
                    .contains("tl:so.tilo.word-processor:root-main"),
                "the refusal names the resource to open instead: {error}"
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "it is answered at once, not left to the delivery wait: {:?}",
                started.elapsed()
            );
            assert!(
                launches.try_recv().is_err(),
                "no launcher effect is asked for a served app"
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
        let workspace_provider = tilo_actions::remote::workspace_provider(&workspace_id().to_string());
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
        let provider = tilo_actions::remote::application_action_target(&workspace_id().to_string(), WRITER, Document::KIND, "append-paragraph").provider;
        let appended = invoke_provider(&agent, &run, &provider, Document::KIND, "append-paragraph",
            serde_json::json!({"text":"Visible governed paragraph"})).await.unwrap();
        writer.settle(appended.0).await;
        assert!(writer.app.harness().inspection_json().unwrap().contains("Visible governed paragraph"),
            "the actual Writer semantic UI must expose the appended paragraph");
        writer.app.call_on(Document::KIND, "select-all", serde_json::json!({})).unwrap();
        writer.app.settle();
        let person = writer.app.with_surface::<WordProcessorWidget, _>(|widget| widget.state().local_participant_view()).unwrap();
        assert!(person.anchor.is_some(), "the person must have a real selection before agent navigation");
        assert_ne!(person.anchor.as_ref().unwrap(), &person.caret, "the selected range must be nonempty");
        assert!(person.caret.offset > 0, "the person's caret must not be the default start");
        let found = invoke_provider(&agent, &run, &provider, Document::KIND, "find-text",
            serde_json::json!({"query":"governed"})).await.unwrap();
        writer.settle(found.0).await;
        let doc = writer.app.peer_document();
        writer.app.with_surface::<WordProcessorWidget, _>(|widget| {
            let widget = widget.state();
            let agent = widget.participant_view_or_default(&tilo_actions::ParticipantId::for_agent_task(&run.group()).unwrap(), &doc).unwrap();
            assert_eq!(agent.find.query, "governed");
            assert!(agent.anchor.is_some(), "navigation must select the visible match");
            assert_eq!(widget.local_participant_view(), person, "agent navigation must preserve the person's caret");
        }).unwrap();
        drop(writer);
        assert!(launches.try_recv().is_err(), "fresh resource actions must not launch a second copy");
    })).await;
}

/// A task's step that shows nothing asks the workspace to start the closed
/// app first: the host runs it with no window, nothing is launched, and the
/// start answers with that instance, as an `open` names the one it started.
#[tokio::test(flavor = "current_thread")]
async fn a_task_start_of_a_closed_app_runs_it_without_a_window() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            f.approve();
            let (asked, mut asks) = tokio::sync::mpsc::unbounded_channel();
            f.state
                .launchable
                .replace_windowless_for_test(Arc::new(move |app, workspace| {
                    asked
                        .send((app.to_owned(), workspace.to_owned()))
                        .map_err(|error| error.to_string())?;
                    let (started, start) = tokio::sync::oneshot::channel();
                    let _ = started.send(Ok(()));
                    Ok(Some(start))
                }));
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let workspace_provider =
                tilo_actions::remote::workspace_provider(&workspace_id().to_string());
            let app_resource = format!("app.{WRITER}");
            let start = invoke_provider(
                &agent,
                &run,
                &workspace_provider,
                &app_resource,
                tilo_actions::remote::APPLICATION_START_ACTION,
                serde_json::json!({}),
            );
            tokio::pin!(start);
            let (app, workspace) = tokio::select! {
                ask = asks.recv() => ask.unwrap(),
                answer = &mut start => panic!("the start answered before the app ran: {answer:?}"),
            };
            assert_eq!(
                (app.as_str(), workspace),
                (WRITER, workspace_id().to_string())
            );
            f.state
                .launchable
                .windowless_peers
                .lock()
                .unwrap()
                .insert(210);
            let _writer = Box::pin(Writer::mount(&f, false)).await;
            let (_, update) = start.await.unwrap();
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
                panic!("the start did not succeed: {update:?}");
            };
            assert_eq!(output.value, Some(serde_json::json!({"peer_id": 210})));
            assert!(launches.try_recv().is_err(), "no window was launched");
        }))
        .await;
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
        route: CrdtRoute::record(WRITER, workspace_id().to_string(), "main"),
        live: true,
        actions: vec![PublishedAction {
            state: tilo_actions::ActionState::default(),
            semantic_identity: descriptor.semantic_identity,
            resource: descriptor.address.resource,
            action: descriptor.address.action,
            resource_kind: descriptor.resource_kind,
            resource_label: descriptor.resource_label,
            action_label: descriptor.action_label,
            keywords: Vec::new(),
            icon: None,
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
                &workspace_id().to_string(),
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
        CrdtRoute::record(WRITER, workspace_id().to_string(), "main")
    );
    assert!(
        !retained[0].live,
        "retention must not invent a live publisher"
    );
    assert!(
        !f.state.has_live_published_action(
            WRITER,
            &workspace_id().to_string(),
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
            drop(writer);
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
                let provider = tilo_actions::remote::application_action_target(
                    &workspace_id().to_string(),
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
                drop(writer);
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
                    tilo_action_policy::TreePath::workspace(workspace_id().to_string()).app(WRITER),
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
            let provider = tilo_actions::remote::workspace_provider(&workspace_id().to_string());
            let resource = format!("app.{WRITER}");
            let issue_run = f.native.runs.lock().unwrap()[0].clone();
            crate::services::TrackerRuns::authorize_action(
                f.native.as_ref(),
                &workspace_id().to_string(),
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
            // The workspace holds the agent to the changes it is given app by app,
            // so beyond the one-use grant its run may not open Writer.
            put_grant(
                &f,
                tilo_action_policy::Grant::new(
                    "cold-writer-workspace-held",
                    tilo_action_policy::Subject::agent("scribe"),
                    tilo_action_policy::TreePath::workspace(workspace_id().to_string()),
                    tilo_action_policy::KindSet::UNDOABLE,
                    tilo_action_policy::Answer::Deny,
                    tilo_action_policy::Subject::person(hex::encode(
                        f.native.certificate.user_public_key,
                    )),
                ),
            );
            once_permission(&f);
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let provider = tilo_actions::remote::workspace_provider(&workspace_id().to_string());
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
            // while the original activation is waiting for its publisher.
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
                .is_err()
            );
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
        let provider = tilo_actions::remote::workspace_provider(&workspace_id().to_string());
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
            tilo_action_policy::TreePath::workspace(workspace_id().to_string()).app(WRITER),
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
                        tilo_action_policy::TreePath::workspace(workspace_id().to_string())
                            .app(WRITER),
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
                                            tilo_action_policy::TreePath::workspace(
                                                workspace_id().to_string(),
                                            )
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
                        &tilo_actions::remote::workspace_provider(&workspace_id().to_string()),
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
                    workspace: workspace_id().to_string(),
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
            let provider = tilo_actions::remote::workspace_provider(&workspace_id().to_string());
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
            tilo_action_policy::TreePath::workspace(workspace_id().to_string()).app(WRITER),
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
            drop(writer);
        }))
        .await;
}

/// An agent door connection's `open` of an app (the workspace's host action, what `notes#open`
/// is) is read back by the connection as it went: the launch is kept under the activation's own
/// identity, and the run that asked for it answers for it.
#[tokio::test(flavor = "current_thread")]
async fn a_door_connections_open_of_an_app_is_read_back_as_it_went() {
    use tilo_actions::remote::ActionHost as _;
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
            let agent = super::task_run_host::admit(&f);
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = super::task_run_host::connection(&host);
            let request = tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::workspace_provider(&workspace_id().to_string()),
                    address: tilo_actions::ActionAddress::new(format!("app.{WRITER}"), "open"),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new()),
                context: None,
                confirmation: None,
                revision: None,
                appearance: None,
                wait_ms: None,
            };
            let id = request.invocation;
            let open = host.invoke(&caller, request);
            tokio::pin!(open);
            tokio::select! {
                app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                answer = &mut open => panic!("the open answered before the launch: {answer:?}"),
            }
            let writer = Box::pin(Writer::mount(&f, false)).await;
            let receipt = open.await.expect("answered").unwrap();
            assert!(
                !matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Rejected { .. }
                ),
                "{receipt:?}"
            );
            let state = host
                .observe(&caller, &id)
                .expect("the connection reads its open back");
            assert!(
                matches!(
                    state.update,
                    tilo_actions::InvocationUpdate::Succeeded { .. }
                ),
                "{:?}",
                state.update
            );
            drop(writer);
            drop(f);
        }))
        .await;
}
