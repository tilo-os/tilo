//! A windowless host-owned private result resource. Only this adapter is fake:
//! publication, native child admission, result validation and receipt retention
//! are the production broker paths. There is no placeholder application window.
use super::*;

pub(super) const APP: &str = "so.tilo.notes";
pub(super) fn resource(task: &str) -> String {
    format!("task-output:{task}")
}

pub(super) struct Outputs {
    worker: tokio::task::JoinHandle<Vec<(String, String)>>,
    // Keep the real publication alive until the broker has retained the final
    // output link, not merely until the app sent its terminal update.
    _channel: channel_handle::Client,
}
impl Drop for Outputs {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
impl Outputs {
    pub(super) async fn finish(mut self, expected: usize) {
        let written = (&mut self.worker).await.unwrap();
        assert_eq!(written.len(), expected);
        assert_eq!(
            written
                .iter()
                .map(|(task, _)| task)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            expected
        );
    }
}

async fn publish(
    channel: &channel_handle::Client,
    tasks: &[String; 2],
    params: &[tilo_actions::ActionParam],
    write: &tilo_actions::ActionContract,
) {
    let mut publish = channel.publish_actions_request();
    publish.get().set_app_id(APP);
    {
        let mut rows = publish.get().init_actions(4);
        for (index, task) in tasks.iter().enumerate() {
            let mut row = rows.reborrow().get(u32::try_from(index * 2).unwrap());
            row.set_resource(resource(task));
            row.set_resource_kind("task-output");
            row.set_resource_label("Private task result");
            row.set_action("write-private-result");
            row.set_action_label("Save private result");
            row.set_operation_kind("document");
            row.set_params_json(serde_json::to_string(&params).unwrap());
            row.set_contract_json(serde_json::to_string(&write).unwrap());
            let mut row = rows.reborrow().get(u32::try_from(index * 2 + 1).unwrap());
            row.set_resource(resource(task));
            row.set_resource_kind("task-output");
            row.set_resource_label("Private task result");
            row.set_action("open");
            row.set_action_label("Read private result");
            row.set_operation_kind("view");
            row.set_params_json("[]");
            row.set_contract_json(
                serde_json::to_string(&tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Read,
                    ..Default::default()
                })
                .unwrap(),
            );
            let mut view = row.init_view();
            view.set_id("private-result");
            view.set_label("Private result");
            view.set_open_action("open");
            view.set_primary(true);
        }
    }
    publish.send().promise.await.unwrap();
}

async fn serve(
    channel: channel_handle::Client,
    stream: invocation_stream::Client,
    tasks: [String; 2],
    expected: usize,
    params: Vec<tilo_actions::ActionParam>,
    write: tilo_actions::ActionContract,
) -> Vec<(String, String)> {
    let mut written = Vec::new();
    loop {
        let response = stream.read_request().send().promise.await.unwrap();
        let wire = response.get().unwrap();
        assert_eq!(wire.get_actor().unwrap().to_str().unwrap(), "agent:scribe");
        assert_eq!(
            wire.get_action().unwrap().to_str().unwrap(),
            "write-private-result"
        );
        let context: tilo_actions::ActionContext =
            serde_json::from_str(wire.get_context_json().unwrap().to_str().unwrap()).unwrap();
        let task = &context.group.as_ref().unwrap().id;
        assert!(tasks.contains(task));
        assert_eq!(
            wire.get_resource().unwrap().to_str().unwrap(),
            resource(task)
        );
        let argument = wire.get_argument().unwrap().to_str().unwrap();
        let arguments: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(argument),
        )
        .unwrap();
        tilo_actions::ActionArgs::bind_json(&params, &arguments).unwrap();
        let text = arguments["text"].as_str().unwrap();
        assert_eq!(
            text,
            if *task == tasks[0] {
                ANSWER_A
            } else {
                ANSWER_B
            }
        );
        assert!(
            !written.iter().any(|(id, _)| id == task),
            "task result was dispatched twice"
        );
        written.push((task.clone(), text.to_owned()));
        let output = tilo_actions::ActionOutput {
            value: None,
            resources: vec![resource(task)],
        };
        write.result.validate(&output).unwrap();
        let invocation = uuid::Uuid::from_slice(wire.get_invocation_uuid().unwrap()).unwrap();
        let mut update = channel.update_invocation_request();
        update.get().set_id(wire.get_id());
        update.get().set_update_json(
            serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId(invocation),
                output,
            })
            .unwrap(),
        );
        update.send().promise.await.unwrap();
        // The model marks its final write with a private reply; no further
        // adapter work is needed once B has written in the Stop journey.
        if written.len() == expected {
            return written;
        }
    }
}

pub(super) async fn mount(f: &Fixture, tasks: [String; 2], expected: usize) -> Outputs {
    let channel = f.app_channel(7200, APP, false);
    let params = vec![tilo_actions::ActionParam::required(
        "text",
        tilo_actions::ParamType::Text,
        "Private answer",
    )];
    let write = tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        field: Some("body".into()),
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Resource {
            resource_kind: "task-output".into(),
        },
        ..Default::default()
    };
    publish(&channel, &tasks, &params, &write).await;
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
    let worker = tokio::task::spawn_local(serve(
        channel.clone(),
        stream,
        tasks,
        expected,
        params,
        write,
    ));
    Outputs {
        worker,
        _channel: channel,
    }
}
