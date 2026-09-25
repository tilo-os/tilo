use ui::prelude::*;

/// The durable part of the app: what to scan. Stored, synced and undoable.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Scan {
    /// The folder whose entries the background task counts.
    pub folder: String,
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            folder: String::from("."),
        }
    }
}

#[resource(kind = "generated.scan", label = "Scan")]
impl Scan {
    /// Choose the folder to scan.
    #[action]
    pub fn set_folder(&mut self, #[param("Folder to scan")] folder: String) {
        self.folder = folder;
    }
}

/// The transient part: the task's progress. View state, never stored,
/// shared or undone.
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

fn count_entries(folder: &str) -> Result<usize, String> {
    std::fs::read_dir(folder)
        .map(|entries| entries.filter_map(Result::ok).count())
        .map_err(|error| error.to_string())
}

/// The background task as an effect: an action with a job lifecycle rather
/// than a document edit, so the palette, agents and scripts can start and
/// cancel it too.
#[expect(
    clippy::expect_used,
    reason = "the generated cancellation arguments are always serializable"
)]
fn task_resource() -> tilo_actions::Resource {
    let mut resource = ui::widget::resource_actions::resource(
        "generated.background-task",
        "Background task",
        "run",
        "Run task",
        true,
    );
    let mut cancel_action = tilo_actions::ActionSpec::typed_operation(
        "cancel",
        "Cancel task",
        "Confirm cancellation",
        tilo_actions::OperationKind::Effect,
        ui::widget::resource_actions::OPERATION_TYPE,
        vec![tilo_actions::ActionParam::required_owned(
            "cancel",
            tilo_actions::ParamType::Boolean,
            "Cancel the active task",
        )],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                ui::widget::resource_actions::OPERATION_TYPE,
                &args.to_json(),
            )
            .expect("generated cancellation is serializable")
        },
    );
    cancel_action.contract.discovery = tilo_actions::DiscoveryScope::Launchable;
    cancel_action.contract.execution =
        tilo_actions::ExecutionMode::Background { cancellable: false };
    cancel_action.contract.result = tilo_actions::ResultSpec::Job;
    resource.actions.push(cancel_action);
    resource
}

/// A complete button → worker → typed state → retained subtree loop, reading
/// its input from the resource.
pub struct App;

impl ResourceApp for App {
    type Resource = Scan;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    #[expect(
        clippy::expect_used,
        reason = "the bindings use the task actions constructed immediately above"
    )]
    fn view(cx: &mut ViewCx<Scan>) -> Element {
        crate::install_catalog();
        let folder = cx
            .input(|scan| scan.folder.clone(), Scan::SET_FOLDER)
            .with_id("folder-input");
        let task = LatestTask::new(LoadState::Idle);
        let result = task.state();
        let resource = task_resource();
        let run = Button::new("run-task", "task-run", None).on_action(
            tilo_actions::ActionBinding::unit_for(&resource, "run")
                .expect("the run button matches the exported action"),
        );
        let cancel_binding = tilo_actions::ActionBinding::static_json_for(
            &resource,
            "cancel",
            serde_json::Map::from_iter([("cancel".into(), serde_json::json!(true))]),
        )
        .expect("the cancel button matches the exported action");

        let body = ModelView::from_state_by(
            &result,
            |result| match result {
                LoadState::Idle => LoadScreen::Idle,
                LoadState::Loading => LoadScreen::Loading,
                LoadState::Ready(_) => LoadScreen::Ready,
                LoadState::Failed(_) => LoadScreen::Failed,
            },
            move |result| -> Element {
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
            labeled_field("task-folder-label", folder),
            run,
            body,
        ]);
        let live = cx.live().clone();
        let cancel_task = task.clone();
        ui::widget::resource_actions::background(root, resource, move |payload| {
            let (sender, receiver) = std::sync::mpsc::channel();
            if serde_json::from_slice::<serde_json::Value>(payload)
                .ok()
                .and_then(|value| value.get("cancel").and_then(serde_json::Value::as_bool))
                == Some(true)
            {
                cancel_task.cancel(LoadState::Idle);
                let _ = sender.send(Ok(tilo_actions::ActionOutput::default()));
                return Ok(receiver);
            }
            // The worker gets a copy of the input, read from the resource now.
            let folder = live.with(|scan| scan.folder.clone());
            let completion = task.begin(LoadState::Loading);
            std::thread::spawn(move || {
                let result = count_entries(&folder);
                let state = result.as_ref().map_or_else(
                    |error| LoadState::Failed(error.clone()),
                    |count| LoadState::Ready(*count),
                );
                let _ = completion.complete(state);
                let _ = sender.send(result.map(|_| tilo_actions::ActionOutput::default()));
            });
            Ok(receiver)
        })
    }
}
