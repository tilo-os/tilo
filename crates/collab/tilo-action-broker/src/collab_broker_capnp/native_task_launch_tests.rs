//! Cold activation exercises the host launcher service and the real Writer UI owner.
use super::*;
use crate::services::{LaunchPlace, LaunchableApplication, LauncherServices};
use app_framework::framework::testing::HeadlessApp;
use tilo_resource::Resource;
use word_processor::{WordProcessorWidget, framework::WordProcessorApp, model::Document};

const WRITER: &str = "so.tilo.word-processor";

struct Launcher {
    launches: tokio::sync::mpsc::UnboundedSender<String>,
    custody: Mutex<Option<std::path::PathBuf>>,
}
impl LauncherServices for Launcher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        vec![LaunchableApplication {
            package_id: WRITER.into(),
            display_name: "Writer".into(),
            unconfined: false,
            offers: Vec::new(),
        }]
    }
    fn launch(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        record: Option<&str>,
        _place: &LaunchPlace,
    ) -> Result<(), String> {
        if workspace.room != room()
            || app != WRITER
            || !(record.is_none() || record == Some("main"))
        {
            return Err("host activation named the wrong workspace, app or record".into());
        }
        let custody = self.custody.lock().unwrap().clone().unwrap();
        let retained = std::fs::read_to_string(custody).unwrap();
        if !retained.contains("\"host_activation\":true") {
            return Err("the actual host call requires custody already on disk".into());
        }
        if !retained.contains("AuthenticatedLocal") {
            return Err("custody must retain the real signed caller identity".into());
        }
        self.launches
            .send(app.into())
            .map_err(|error| error.to_string())
    }
    fn attest_process(&self, _pid: u32, _app: &str, _room: &str) -> bool {
        false
    }
}

fn fixture() -> (Fixture, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let launcher = Arc::new(Launcher {
        launches: sender,
        custody: Mutex::new(None),
    });
    let f = Fixture::with_launcher(Some(launcher.clone()));
    *launcher.custody.lock().unwrap() = Some(f.directory.path().join("journal/records.json"));
    permission(&f, true);
    (f, receiver)
}

fn permission(f: &Fixture, allow: bool) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    put_grant(
        f,
        Grant::new(
            format!("cold-writer-{}", if allow { "allow" } else { "deny" }),
            Subject::agent("scribe"),
            TreePath::room(room().to_string()).app(WRITER),
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

struct Writer {
    app: HeadlessApp<WordProcessorApp>,
    channel: channel_handle::Client,
    stream: invocation_stream::Client,
}
impl Writer {
    async fn mount(f: &Fixture, changed: bool) -> Self {
        let mut app = HeadlessApp::<WordProcessorApp>::new().unwrap();
        let descriptors = app.actions().unwrap();
        let channel = f.app_channel(210, WRITER, false);
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
        }
    }

    async fn settle(&mut self, id: u64) {
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            self.stream.read_request().send().promise,
        )
        .await
        .unwrap()
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

    async fn assert_no_delivery(&self) {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                self.stream.read_request().send().promise
            )
            .await
            .is_err(),
            "a refused cold action reached the real UI owner"
        );
    }
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
            let agent = widget.participant_view_or_default(&tilo_actions::ParticipantId::for_agent_task(&f.task).unwrap(), &doc).unwrap();
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
            let pending = invoke_provider(&agent, &run, &provider, Document::KIND, "append-paragraph",
                serde_json::json!({"text":"Retained request visible only under its fresh contract"}));
            tokio::pin!(pending);
            tokio::select! {
                app = launches.recv() => assert_eq!(app.unwrap(), WRITER),
                result = &mut pending => panic!("retained request answered before host launch: {result:?}"),
            }
            let mut writer = Box::pin(Writer::mount(&f, changed)).await;
            let result = tokio::time::timeout(Duration::from_secs(2), pending).await.unwrap();
            if changed {
                assert!(result.unwrap_err().to_string().contains("contract changed"));
                writer.assert_no_delivery().await;
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
                    f.person_command(WorkspaceTaskCommand::CancelForRun {
                        task_id: f.task.clone(),
                        run_epoch: 1,
                        after_revision: 0,
                    })
                    .unwrap();
                } else {
                    permission(&f, false);
                }
                let mut writer = Box::pin(Writer::mount(&f, false)).await;
                assert!(
                    tokio::time::timeout(Duration::from_secs(2), pending)
                        .await
                        .unwrap()
                        .is_err()
                );
                writer.assert_no_delivery().await;
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
            let provider = tilo_actions::remote::workspace_provider(&room().to_string());
            let resource = format!("app.{WRITER}");
            f.native
                .authorize_action(
                    &room().to_string(),
                    "scribe",
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

#[tokio::test(flavor = "current_thread")]
async fn once_activation_is_spent_and_completed_retry_after_close_never_relaunches() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let (f, mut launches) = fixture();
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
            let retry = tokio::time::timeout(
                Duration::from_millis(250),
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
            .expect("a certain terminal receipt must not wait for a closed publisher")
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
