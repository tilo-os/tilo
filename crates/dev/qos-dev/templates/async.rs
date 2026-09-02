use ui::widget::{Banner, Button, Spinner, Text, Widget};
use ui::{LatestTask, ModelView, qos_actions, tokens::TextRole};

enum LoadState {
    Idle,
    Loading,
    Ready(usize),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadScreen {
    Idle,
    Loading,
    Ready,
    Failed,
}

fn count_entries() -> Result<usize, String> {
    std::fs::read_dir(".")
        .map(|entries| entries.filter_map(Result::ok).count())
        .map_err(|error| error.to_string())
}

/// A complete button → worker → typed state → retained subtree loop.
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let task = LatestTask::new(LoadState::Idle);
    let result = task.state();
    let mut resource = ui::widget::resource_actions::resource(
        "generated.background-task",
        "Background task",
        "run",
        "Run task",
        true,
    );
    let mut cancel_action = qos_actions::ActionSpec::typed_operation(
        "cancel",
        "Cancel task",
        "Confirm cancellation",
        qos_actions::OperationKind::Effect,
        ui::widget::resource_actions::OPERATION_TYPE,
        vec![qos_actions::ActionParam::required_owned(
            "cancel",
            qos_actions::ParamType::Boolean,
            "Cancel the active task",
        )],
        |args| {
            qos_actions::EncodedAction::encode_operation(
                ui::widget::resource_actions::OPERATION_TYPE,
                &args.to_json(),
            )
            .expect("generated cancellation is serializable")
        },
    );
    cancel_action.contract.discovery = qos_actions::DiscoveryScope::Launchable;
    cancel_action.contract.execution =
        qos_actions::ExecutionMode::Background { cancellable: false };
    cancel_action.contract.result = qos_actions::ResultSpec::Job;
    resource.actions.push(cancel_action);
    let run_binding = qos_actions::ActionBinding::unit_for(&resource, "run")
        .expect("generated run button matches the exported action");
    let run = Button::new("run-task", "task-run", None).on_action(run_binding);
    let cancel_binding = qos_actions::ActionBinding::static_json_for(
        &resource,
        "cancel",
        serde_json::Map::from_iter([("cancel".into(), serde_json::json!(true))]),
    )
    .expect("generated cancel button matches the exported action");

    let body = ModelView::from_state_by(
        &result,
        |result| match result {
            LoadState::Idle => LoadScreen::Idle,
            LoadState::Loading => LoadScreen::Loading,
            LoadState::Ready(_) => LoadScreen::Ready,
            LoadState::Failed(_) => LoadScreen::Failed,
        },
        move |result| -> Box<dyn Widget> {
            match result {
                LoadState::Idle => Box::new(Text::new("task-idle")),
                LoadState::Loading => Box::new(ui::column![
                    Spinner::new(),
                    Text::new("task-loading"),
                    Button::new("cancel-task", "task-cancel", None)
                        .on_action(cancel_binding.clone()),
                ]),
                LoadState::Ready(count) => Box::new(
                    Text::new("task-ready")
                        .with_arg("count", count.to_string())
                        .with_id("task-result"),
                ),
                LoadState::Failed(message) => Box::new(Banner::error(message)),
            }
        },
    );

    let root = Box::new(ui::column![
        Text::new("task-title").with_role(TextRole::Title),
        run,
        body,
    ]);
    let cancel_task = task.clone();
    ui::widget::resource_actions::background(root, resource, move |payload| {
        if serde_json::from_slice::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| value.get("cancel").and_then(serde_json::Value::as_bool))
            == Some(true)
        {
            cancel_task.cancel(LoadState::Idle);
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = sender.send(Ok(qos_actions::ActionOutput::default()));
            return Ok(receiver);
        }
        let completion = task.begin(LoadState::Loading);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = count_entries();
            let state = result.as_ref().map_or_else(
                |error| LoadState::Failed(error.clone()),
                |count| LoadState::Ready(*count),
            );
            let _ = completion.complete(state);
            let result = result.map(|_| qos_actions::ActionOutput::default());
            let _ = sender.send(result);
        });
        Ok(receiver)
    })
}
