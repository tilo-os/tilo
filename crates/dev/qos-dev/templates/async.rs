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
    let resource = ui::widget::resource_actions::resource(
        "generated.background-task",
        "Background task",
        "run",
        "Run task",
        true,
    );
    let run_binding = qos_actions::ActionBinding::unit_for(&resource, "run")
        .expect("generated run button matches the exported action");
    let run = Button::new("run-task", "task-run", None).on_action(run_binding);

    let body_task = task.clone();
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
                LoadState::Loading => {
                    let cancel_task = body_task.clone();
                    Box::new(ui::column![
                        Spinner::new(),
                        Text::new("task-loading"),
                        Button::new("cancel-task", "task-cancel", None)
                            .on_click(move || cancel_task.cancel(LoadState::Idle)),
                    ])
                }
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
    ui::widget::resource_actions::background(
        root,
        resource,
        move |_| {
            let completion = task.begin(LoadState::Loading);
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = count_entries();
                let state = result
                    .as_ref()
                    .map_or_else(|error| LoadState::Failed(error.clone()), |count| LoadState::Ready(*count));
                let _ = completion.complete(state);
                let result = result.map(|_| qos_actions::ActionOutput::default());
                let _ = sender.send(result);
            });
            Ok(receiver)
        },
    )
}
