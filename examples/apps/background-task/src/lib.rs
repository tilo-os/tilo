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
//! - Run and Cancel are effect actions with a job lifecycle, not document
//!   edits, so the palette, agents and scripts can start and cancel the task
//!   exactly as the buttons do.
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

/// Run and Cancel as effect actions on their own resource. Run takes no
/// arguments; Cancel takes a confirmation flag, so the action surface asks
/// before discarding work.
#[expect(
    clippy::expect_used,
    reason = "a boolean argument object always serializes"
)]
fn task_resource() -> tilo_actions::Resource {
    let mut resource = ui::widget::resource_actions::resource(
        "example.prime-count",
        "Prime count",
        "run",
        "Count primes",
        true,
    );
    let mut cancel = tilo_actions::ActionSpec::typed_operation(
        "cancel",
        "Cancel the count",
        "Confirm cancellation",
        tilo_actions::OperationKind::Effect,
        ui::widget::resource_actions::OPERATION_TYPE,
        vec![tilo_actions::ActionParam::required_owned(
            "cancel",
            tilo_actions::ParamType::Boolean,
            "Cancel the running count",
        )],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                ui::widget::resource_actions::OPERATION_TYPE,
                &args.to_json(),
            )
            .expect("a boolean argument object serializes")
        },
    );
    cancel.contract.discovery = tilo_actions::DiscoveryScope::Launchable;
    cancel.contract.execution = tilo_actions::ExecutionMode::Background { cancellable: false };
    cancel.contract.result = tilo_actions::ResultSpec::Job;
    resource.actions.push(cancel);
    resource
}

fn wants_cancel(payload: &[u8]) -> bool {
    ui::serde_json::from_slice::<ui::serde_json::Value>(payload)
        .ok()
        .and_then(|value| value.get("cancel").and_then(ui::serde_json::Value::as_bool))
        == Some(true)
}

pub struct App;

impl ResourceApp for App {
    type Resource = Search;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    #[expect(
        clippy::expect_used,
        reason = "the buttons bind to the actions task_resource just built"
    )]
    fn view(cx: &mut ViewCx<Search>) -> Element {
        install_catalog();
        #[expect(
            clippy::cast_precision_loss,
            reason = "the limit is at most MAX_LIMIT, far below 2^52"
        )]
        let limit = cx
            .control(
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
        let resource = task_resource();
        let run = Button::new("run-task", "task-run", None).on_action(
            tilo_actions::ActionBinding::unit_for(&resource, "run")
                .expect("the run button matches the exported action"),
        );
        let cancel = tilo_actions::ActionBinding::static_json_for(
            &resource,
            "cancel",
            ui::serde_json::Map::from_iter([("cancel".into(), ui::serde_json::json!(true))]),
        )
        .expect("the cancel button matches the exported action");

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
                        Button::new("cancel-task", "task-cancel", None).on_action(cancel.clone()),
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

        let root = Box::new(ui::column![
            Text::new("task-title").with_role(TextRole::Title),
            labeled_field("task-limit-label", limit),
            run,
            body,
        ]);
        let live = cx.live().clone();
        let cancel_task = task.clone();
        ui::widget::resource_actions::background(root, resource, move |payload| {
            let (sender, receiver) = std::sync::mpsc::channel();
            if wants_cancel(payload) {
                cancel_task.cancel(Progress::Idle);
                let _ = sender.send(Ok(tilo_actions::ActionOutput::default()));
                return Ok(receiver);
            }
            // The worker gets a copy of its input, read from the resource now.
            let limit = live.with(|search| search.limit);
            let completion = task.begin(Progress::Running);
            let unavailable = sender.clone();
            // A worker where the target has one; the page's executor otherwise.
            let spawned = ui::background::Background::current().spawn("count-primes", move || {
                let result = count_primes(limit, || completion.is_current());
                let outcome = match result {
                    Some(count) => {
                        completion.complete(Progress::Done { limit, count });
                        Ok(tilo_actions::ActionOutput::default())
                    }
                    None => Err(String::from("cancelled")),
                };
                let _ = sender.send(outcome);
            });
            if spawned.is_err() {
                task.cancel(Progress::Idle);
                let _ =
                    unavailable.send(Err(String::from("Background work is not available here.")));
            }
            Ok(receiver)
        })
    }
}

ui::dev_reload_entry!(App);
