use ui::widget::{Banner, Button, Column, Spinner, Text, Widget};
use ui::{LatestTask, ModelView};

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

/// A complete button → worker → typed state → retained subtree loop.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let task = LatestTask::new(LoadState::Idle);
    let result = task.state();
    let run_task = task.clone();

    let run = Button::new("run-task", "task-run", None).on_click(move || {
        let completion = run_task.begin(LoadState::Loading);
        std::thread::spawn(move || {
            let result = std::fs::read_dir(".")
                .map(|entries| entries.filter_map(Result::ok).count())
                .map_or_else(
                    |error| LoadState::Failed(error.to_string()),
                    LoadState::Ready,
                );
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
                    Box::new(
                        Column::new()
                            .push(Box::new(Spinner::new()))
                            .push(Box::new(Text::new("task-loading")))
                            .push(Box::new(
                                Button::new("cancel-task", "task-cancel", None)
                                    .on_click(move || cancel_task.cancel(LoadState::Idle)),
                            )),
                    )
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

    Box::new(
        Column::new()
            .push(Box::new(Text::new("task-title").with_size(28.0)))
            .push(Box::new(run))
            .push(Box::new(body)),
    )
}
