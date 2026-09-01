use ui::widget::{Banner, Button, Spinner, Text, Widget};
use ui::{LatestTask, ModelView, tokens::TextRole};

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
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let task = LatestTask::new(LoadState::Idle);
    let result = task.state();
    let run_task = task.clone();

    let run = Button::new("run-task", "task-run", None).on_click(move || {
        let completion = run_task.begin(LoadState::Loading);
        std::thread::spawn(move || {
            let result = count_entries().map_or_else(LoadState::Failed, LoadState::Ready);
            let _ = completion.complete(result);
        });
    });

    let body_task = task;
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
    crate::resource_actions::background(
        root,
        crate::resource_actions::resource(
            "generated.background-task",
            "Background task",
            "run",
            "Run task",
            true,
        ),
        |_| {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = count_entries().map(|_| Default::default());
                let _ = sender.send(result);
            });
            Ok(receiver)
        },
    )
}
