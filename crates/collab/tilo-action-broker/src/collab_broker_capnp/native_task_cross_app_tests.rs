//! One deterministic semantic acceptance journey, not a model or screenshot test.
//! Uses the enrolled native owner, delegated Capnp task executor and real Writer
//! and Settings resource vocabularies/action door. The small worker adapter below
//! replaces window/process mounting, not the broker, policy or action executor.
//! Worker records are saved and reopened; this does not prove session document
//! replication or the Settings host's preference-file projection.
use super::*;
use settings::panes::keyboard_shortcuts::DesktopControls;
use std::marker::PhantomData;
use tilo_actions::door::InvocationProvenance;
use tilo_resource::{Invoke, Resource};
use word_processor::model::Document;

struct Worker<R: Resource> {
    channel: channel_handle::Client,
    stream: invocation_stream::Client,
    peer: u64,
    app: &'static str,
    action: String,
    doc: automerge::AutoCommit,
    path: std::path::PathBuf,
    marker: PhantomData<R>,
}

impl<R: Resource> Worker<R> {
    async fn mount(f: &Fixture, peer: u64, app: &'static str, action: &str) -> Self {
        let mut doc = automerge::AutoCommit::new();
        R::initialize(&mut doc).unwrap();
        doc.commit();
        let tree =
            tilo_resource::resource::publish::<R>(R::KIND, R::LABEL, &R::read(&doc).unwrap());
        let spec = tree.actions.iter().find(|spec| spec.id == action).unwrap();
        let channel = f.app_channel(peer, app, false);
        let mut publish = channel.publish_actions_request();
        publish.get().set_app_id(app);
        {
            let mut row = publish.get().init_actions(1).get(0);
            row.set_resource(&tree.id.0);
            row.set_resource_kind(&tree.kind);
            row.set_resource_label(&tree.label);
            row.set_action(&spec.id);
            row.set_action_label(&spec.label);
            row.set_operation_kind(spec.kind.as_str());
            row.set_params_json(serde_json::to_string(spec.params()).unwrap());
            row.set_contract_json(serde_json::to_string(&spec.contract).unwrap());
        }
        publish.send().promise.await.unwrap();
        let mut subscribe = channel.subscribe_invocations_request();
        subscribe.get().set_stable_invocation_ids(true);
        let stream = subscribe
            .send()
            .promise
            .await
            .unwrap()
            .get()
            .unwrap()
            .get_stream()
            .unwrap();
        Self {
            channel,
            stream,
            peer,
            app,
            action: action.into(),
            doc,
            path: f.directory.path().join(format!("{app}.automerge")),
            marker: PhantomData,
        }
    }

    // Resolve the broker-delivered address and arguments against the app's fresh
    // publication, decode in its real registry and execute with task provenance.
    async fn settle(&mut self, wire_id: u64, invocation: uuid::Uuid, group: &str) {
        let response = tilo_test_env::within(
            "the worker delivery",
            self.stream.read_request().send().promise,
        )
        .await
        .unwrap();
        let wire = response.get().unwrap();
        assert_eq!(wire.get_id(), wire_id);
        assert_eq!(wire.get_invocation_uuid().unwrap(), invocation.as_bytes());
        let actor = wire.get_actor().unwrap().to_str().unwrap();
        assert_eq!(actor, "agent:scribe");
        let context: tilo_actions::ActionContext =
            serde_json::from_str(wire.get_context_json().unwrap().to_str().unwrap()).unwrap();
        assert_eq!(context.group.as_ref().unwrap().id, group);
        let address = tilo_actions::ActionAddress::new(
            wire.get_resource().unwrap().to_str().unwrap(),
            wire.get_action().unwrap().to_str().unwrap(),
        );
        let arguments: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
        let tree =
            tilo_resource::resource::publish::<R>(R::KIND, R::LABEL, &R::read(&self.doc).unwrap());
        let spec = tilo_actions::door::resolve(std::slice::from_ref(&tree), &address)
            .unwrap()
            .clone();
        let before = self.doc.get_heads();
        let registry = tilo_actions::ActionRegistry::with(R::register);
        let dispatched = tilo_actions::door::invoke_json(
            &mut self.doc,
            &[tree],
            &address,
            &arguments,
            InvocationProvenance::caller(actor.as_bytes()).within(&context),
            &registry,
            &mut tilo_actions::LocalTransport::new(),
        )
        .unwrap();
        assert_eq!(
            dispatched
                .envelope()
                .context
                .as_ref()
                .unwrap()
                .group
                .as_ref()
                .unwrap()
                .id,
            group
        );
        assert!(dispatched.mutated_local());
        let after = self.doc.get_heads();
        let changes = self.doc.diff(&before, &after);
        // Match the runtime's document-result path: ask the real resource
        // what changed, with null only when a declared JSON answer is absent.
        let value =
            matches!(spec.contract.result, tilo_actions::ResultSpec::Json { .. }).then(|| {
                R::answer(&self.doc, &address.action, &changes).unwrap_or(serde_json::Value::Null)
            });
        let output = tilo_actions::ActionOutput {
            value,
            resources: tilo_resource::take_minted(),
        };
        spec.contract.result.validate(&output).unwrap();
        std::fs::write(&self.path, self.doc.save()).unwrap();
        let terminal = tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId(invocation),
            output,
        };
        let mut update = self.channel.update_invocation_request();
        update.get().set_id(wire_id);
        update
            .get()
            .set_update_json(serde_json::to_string(&terminal).unwrap());
        update.send().promise.await.unwrap();
    }

    fn reopened(&self) -> automerge::AutoCommit {
        automerge::AutoCommit::load(&std::fs::read(&self.path).unwrap()).unwrap()
    }

    fn assert_no_delivery(&self) {
        assert_eq!(
            crate::collab_broker_capnp::unread_invocations(self.peer),
            0,
            "a fenced action reached {}",
            self.app
        );
    }
}

fn permission(f: &Fixture, app: &str, allow: bool) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    put_grant(
        f,
        Grant::new(
            // One answer per app: setting it again replaces it.
            format!("acceptance-{app}"),
            Subject::agent("scribe"),
            TreePath::room(room().to_string()).app(app),
            KindSet::UNDOABLE,
            if allow { Answer::Allow } else { Answer::Deny },
            Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

async fn plan_step<R: Resource>(
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    worker: &Worker<R>,
    invocation: uuid::Uuid,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut request = agent.invoke_task_action_request();
    {
        let mut p = request.get();
        p.set_run_id(run.run_id);
        p.set_lease(&run.lease);
        p.set_peer_id(worker.peer);
        p.set_resource(R::KIND);
        p.set_action(&worker.action);
        p.set_argument(serde_json::to_string(arguments).unwrap());
        p.set_has_argument(true);
        p.set_invocation(invocation.to_string());
    }
    let response = request.send().promise.await?;
    let value = response.get()?;
    Ok((
        value.get_id(),
        tilo_actions::decode_json(value.get_update_json()?.to_str().unwrap().as_bytes()).unwrap(),
    ))
}

#[tokio::test(flavor = "current_thread")]
async fn native_cross_app_approval_steering_stop_and_grants() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let f = Fixture::new();
            let mut writer =
                Worker::<Document>::mount(&f, 210, "so.tilo.word-processor", "append-paragraph")
                    .await;
            let mut settings = Worker::<DesktopControls>::mount(
                &f,
                211,
                "so.tilo.settings",
                tilo_resource::FIELD_EDIT,
            )
            .await;
            let agent = f.channel(100, true);
            let mut run = approve_journey(&f, &agent, &writer, &settings).await;
            // Deterministic planner: one Writer operation, a person direction, then
            // a Settings edit on the revised lease. No model credentials or server.
            let first_args = serde_json::json!({
                "text": "Release checklist: preserve compatibility examples."
            })
            .as_object()
            .unwrap()
            .clone();
            let edit =
                Invoke::<DesktopControls>::edit(&DesktopControls::paths().vim_mode.set(true))
                    .unwrap();
            writer_journey(
                &f,
                &agent,
                &run,
                &mut writer,
                &settings,
                &first_args,
                &edit.arguments,
            )
            .await;
            steering_journey(&f, &agent, &mut run, &mut settings, &edit.arguments).await;
            assert_reopened_resources(&writer, &settings);
            stop_journey(
                &f,
                &agent,
                &run,
                &writer,
                &settings,
                &first_args,
                &edit.arguments,
            )
            .await;
            drop(f);
        }))
        .await;
}

async fn approve_journey(
    f: &Fixture,
    agent: &channel_handle::Client,
    writer: &Worker<Document>,
    settings: &Worker<DesktopControls>,
) -> runtime_collab::TaskRunEnvelope {
    // Nothing runs before the person approves.
    writer.assert_no_delivery();
    settings.assert_no_delivery();

    f.approve(); // The person's approval on the issue, independent of any tool grant.
    let run = delivered(agent).await;
    assert_eq!(run.task_id, f.task);
    assert!(run.instruction().contains("blocking inconsistency"));
    report(agent, &run, runtime_collab::TaskRunUpdate::Accepted)
        .await
        .unwrap();
    run
}

async fn writer_journey(
    f: &Fixture,
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    writer: &mut Worker<Document>,
    settings: &Worker<DesktopControls>,
    first_args: &serde_json::Map<String, serde_json::Value>,
    settings_args: &serde_json::Map<String, serde_json::Value>,
) {
    permission(f, writer.app, true);
    // The person told the agent it may not change Settings: approving the
    // run does not lift that, and nothing reaches it.
    permission(f, settings.app, false);
    assert!(
        plan_step(
            agent,
            run,
            settings,
            uuid::Uuid::from_u128(4100),
            settings_args
        )
        .await
        .is_err()
    );
    settings.assert_no_delivery();
    let first = uuid::Uuid::from_u128(4101);
    let accepted = plan_step(agent, run, writer, first, first_args)
        .await
        .unwrap();
    writer.settle(accepted.0, first, &run.group()).await;
    assert!(matches!(
        plan_step(agent, run, writer, first, first_args)
            .await
            .unwrap()
            .1,
        tilo_actions::InvocationUpdate::Succeeded { .. }
    ));
    writer.assert_no_delivery();
}

async fn steering_journey(
    f: &Fixture,
    agent: &channel_handle::Client,
    run: &mut runtime_collab::TaskRunEnvelope,
    settings: &mut Worker<DesktopControls>,
    settings_args: &serde_json::Map<String, serde_json::Value>,
) {
    let direction = "Keep the compatibility checklist; enable Vim desktop controls for review.";
    f.direct(direction);
    let control = observed_control(agent, run).await;
    assert_eq!(control.directions[0].text, direction);
    assert_eq!(control.applied_revision, 0);
    permission(f, settings.app, true);
    let second = uuid::Uuid::from_u128(4102);
    assert!(
        plan_step(agent, run, settings, second, settings_args)
            .await
            .is_err(),
        "saved direction fences the old plan"
    );
    settings.assert_no_delivery();
    // The agent takes the message in, on the issue.
    f.on_run(|run| run.directions[0].incorporated = true);
    assert_eq!(observed_control(agent, run).await.applied_revision, 1);
    let original_id = run.run_id;
    run.lease = format!("{}:1", run.lease.rsplit_once(':').unwrap().0);
    let accepted = plan_step(agent, run, settings, second, settings_args)
        .await
        .unwrap();
    settings.settle(accepted.0, second, &run.group()).await;
    assert!(matches!(
        plan_step(agent, run, settings, second, settings_args)
            .await
            .unwrap()
            .1,
        tilo_actions::InvocationUpdate::Succeeded { .. }
    ));
    assert_eq!(run.run_id, original_id, "steering must not start a new run");
}

fn assert_reopened_resources(writer: &Worker<Document>, settings: &Worker<DesktopControls>) {
    // Inspect actual app-owned resource content, reopened from CRDT bytes,
    // not claimed success text, receipts alone, or staged screenshots.
    let writer_doc = writer.reopened();
    let mut document = Document::read(&writer_doc).unwrap();
    Document::load(
        &mut document,
        &writer_doc,
        &tilo_resource::Demand::range(0..64),
    )
    .unwrap();
    let body = document.text().unwrap();
    assert!(
        body.blocks(0..body.body().len())
            .any(|block| block.text == "Release checklist: preserve compatibility examples.")
    );
    assert!(
        DesktopControls::read(&settings.reopened())
            .unwrap()
            .vim_mode
    );
}

async fn stop_journey(
    f: &Fixture,
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    writer: &Worker<Document>,
    settings: &Worker<DesktopControls>,
    first_args: &serde_json::Map<String, serde_json::Value>,
    settings_args: &serde_json::Map<String, serde_json::Value>,
) {
    let second = uuid::Uuid::from_u128(4102);
    // A live explicit denial also fences retained successes, not just new ids.
    permission(f, settings.app, false);
    assert!(
        plan_step(agent, run, settings, second, settings_args)
            .await
            .is_err()
    );
    assert!(
        plan_step(
            agent,
            run,
            settings,
            uuid::Uuid::from_u128(4103),
            settings_args
        )
        .await
        .is_err()
    );
    settings.assert_no_delivery();
    let before_writer = std::fs::read(&writer.path).unwrap();
    let before_settings = std::fs::read(&settings.path).unwrap();
    f.stop();
    let mut cancelled = agent.task_run_cancelled_request();
    cancelled.get().set_run_id(run.run_id);
    cancelled.get().set_lease(&run.lease);
    assert!(
        cancelled
            .send()
            .promise
            .await
            .unwrap()
            .get()
            .unwrap()
            .get_cancelled()
    );
    assert!(
        plan_step(agent, run, writer, uuid::Uuid::from_u128(4104), first_args)
            .await
            .is_err()
    );
    assert!(
        plan_step(
            agent,
            run,
            settings,
            uuid::Uuid::from_u128(4105),
            settings_args
        )
        .await
        .is_err()
    );
    writer.assert_no_delivery();
    settings.assert_no_delivery();
    assert_eq!(std::fs::read(&writer.path).unwrap(), before_writer);
    assert_eq!(std::fs::read(&settings.path).unwrap(), before_settings);
}
