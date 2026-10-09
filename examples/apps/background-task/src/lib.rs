//! Background task: slow work off the UI thread, startable and cancellable by
//! anyone.
//!
//! Pressing Run counts the primes below a limit on a worker thread. The UI
//! stays responsive, shows a spinner while it runs, and replaces it with the
//! result when the worker finishes. Three ideas carry the example:
//!
//! - The input (the limit) is resource state: durable, synced and undoable.
//!   The task's progress is view state held by a `LatestTask`: never stored,
//!   shared or undone.
//! - `LatestTask` hands each run a `TaskCompletion`. Starting again or
//!   cancelling makes the older completion stale, so a slow answer can never
//!   overwrite a newer one, and the worker polls `is_current` between chunks
//!   of work to stop early.
//! - Run and Cancel are the app's effects: work with no document delta,
//!   published beside the resource's actions, so the palette, agents and
//!   scripts start and cancel the task exactly as the buttons do. Run is a
//!   job: its caller learns the count when the worker finishes.
//!
//! The worker runs through `ui::background`: on a thread where the target has
//! them, on the page's executor on the web.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.background-task";
/// The window and launcher title.
pub const APP_TITLE: &str = "Background Task";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use ui::prelude::*;

/// The largest limit accepted.
pub const MAX_LIMIT: u64 = 50_000_000;
/// How many numbers the worker tests between cancellation checks.
const CHUNK: u64 = 4096;

/// What to compute. Stored, synced and undoable.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Search {
    /// Count the primes below this number.
    pub limit: u64,
}

impl Default for Search {
    fn default() -> Self {
        Self { limit: 1_000_000 }
    }
}

#[resource(kind = "example.prime-search", label = "Prime search")]
impl Search {
    /// Choose how far to search.
    #[action]
    pub fn set_limit(
        &mut self,
        #[param("Count the primes below this number")] limit: u64,
    ) -> Result<(), &'static str> {
        if !(2..=MAX_LIMIT).contains(&limit) {
            return Err("the limit is 2 to 50,000,000");
        }
        self.limit = limit;
        Ok(())
    }
}

/// Whether `n` is prime, by trial division.
#[must_use]
pub fn is_prime(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    if n.checked_rem(2) == Some(0) {
        return n == 2;
    }
    (3..)
        .step_by(2)
        .take_while(|d: &u64| d.checked_mul(*d).is_some_and(|square| square <= n))
        .all(|d| n.checked_rem(d) != Some(0))
}

/// Count the primes below `limit`, stopping early (with `None`) as soon as
/// `keep_going` says the answer is no longer wanted.
pub fn count_primes(limit: u64, keep_going: impl Fn() -> bool) -> Option<usize> {
    let mut count = 0_usize;
    for n in 2..limit {
        if n.checked_rem(CHUNK) == Some(0) && !keep_going() {
            return None;
        }
        if is_prime(n) {
            count = count.saturating_add(1);
        }
    }
    Some(count)
}

/// The task's progress: view state, never stored, shared or undone.
enum Progress {
    Idle,
    Running,
    Done { limit: u64, count: usize },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Idle,
    Running,
    Done,
}

/// What anyone can do with the task.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "example.prime-search.task")]
pub enum Task {
    /// Count primes.
    #[effect(job, cancellable)]
    Run,
    /// Cancel the count.
    Cancel,
}

pub struct App;

impl ResourceApp for App {
    type Resource = Search;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Search>) -> Element {
        install_catalog();
        #[expect(
            clippy::cast_precision_loss,
            reason = "the limit is at most MAX_LIMIT, far below 2^52"
        )]
        let limit = cx
            .control_with(
                |value| {
                    NumberInput::from_state(value)
                        .with_range(2.0, MAX_LIMIT as f64)
                        .with_step(100_000.0)
                },
                |search| search.limit as f64,
                Search::SET_LIMIT.map(|limit: f64| {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "the control's range keeps the value within 2..=MAX_LIMIT"
                    )]
                    let limit = limit.round() as u64;
                    limit
                }),
            )
            .with_id("limit-input");

        let task = LatestTask::new(Progress::Idle);
        let progress = task.state();
        cx.effects(move |effect: Task, cx| match effect {
            Task::Cancel => {
                task.cancel(Progress::Idle);
                Ok(Effected::done())
            }
            Task::Run => {
                // The worker gets a copy of its input, read from the resource now.
                let limit = cx.live().with(|search| search.limit);
                let completion = task.begin(Progress::Running);
                let reply = cx.reply();
                // A worker where the target has one; the page's executor otherwise.
                let spawned =
                    ui::background::Background::current().spawn("count-primes", move || {
                        let result = count_primes(limit, || {
                            completion.is_current() && !reply.is_cancelled()
                        });
                        if let Some(count) = result {
                            completion.complete(Progress::Done { limit, count });
                            reply.finish(Ok(tilo_actions::ActionOutput {
                                value: Some(ui::serde_json::json!(count)),
                                resources: Vec::new(),
                            }));
                        } else {
                            // Stale once Cancel ran; current when the caller
                            // cancelled the job itself.
                            completion.complete(Progress::Idle);
                            reply.finish(Err(String::from("cancelled")));
                        }
                    });
                if spawned.is_err() {
                    task.cancel(Progress::Idle);
                    return Err(String::from("Background work is not available here."));
                }
                Ok(Effected::Pending)
            }
        });

        let actions = cx.actions();
        let run = actions.bind_effect_invocation(Button::new("run-task", "task-run"), &Task::Run);
        let body = ModelView::from_state_by(
            &progress,
            |progress| match progress {
                Progress::Idle => Screen::Idle,
                Progress::Running => Screen::Running,
                Progress::Done { .. } => Screen::Done,
            },
            move |progress| -> Element {
                match progress {
                    Progress::Idle => Box::new(Text::new("task-idle").with_id("task-status")),
                    Progress::Running => Box::new(ui::column![
                        Spinner::new(),
                        Text::new("task-running").with_id("task-status"),
                        actions.bind_effect_invocation(
                            Button::new("cancel-task", "task-cancel"),
                            &Task::Cancel
                        ),
                    ]),
                    Progress::Done { limit, count } => Box::new(
                        Text::new("task-done")
                            .with_arg("count", *count)
                            .with_arg("limit", *limit)
                            .with_id("task-result"),
                    ),
                }
            },
        );

        Box::new(ui::column![
            Text::new("task-title").with_role(TextRole::Title),
            labeled_field("task-limit-label", limit),
            run,
            body,
        ])
    }
}

ui::dev_reload_entry!(App);
