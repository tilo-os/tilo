//! What an action on tasks nobody has open costs: its own work, never a view of the app.
//!
//! The times are printed for reading (`cargo test -p tilo-workspace-service action_cost --
//! --nocapture`), never asserted as exact durations: what is asserted is that an action renders
//! no view and lays nothing out, whether the workspace holds two hundred tasks or one.

use std::time::{Duration, Instant};

use tilo_actions::ActionAddress;
use tilo_resource::profiling::{Sample, observe};

use super::Tasks;

/// The tasks the workspace holds before its actions are timed.
const TASKS: usize = 200;

/// The actions timed on it.
const ACTIONS: usize = 30;

/// The tasks one `new_tasks` lands.
const LIST: usize = 100;

fn median(mut took: Vec<Duration>) -> Duration {
    took.sort();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

/// The views `observed` rendered and the frames it laid out.
fn drawn(observed: &[Sample]) -> usize {
    observed
        .iter()
        .filter(|phase| phase.phase == "tl.view.render" || phase.phase == "ui.layout")
        .map(|phase| phase.calls)
        .sum()
}

/// Tasks nobody has open answer their actions without rendering a view: the work of the action
/// lands before it answers, and a workspace holding many tasks costs what the one action does,
/// not a view of all of them.
#[test]
fn an_action_on_tasks_nobody_has_open_renders_nothing() {
    let mut tasks = Tasks::founded();
    for start in (0..TASKS).step_by(LIST) {
        let titles: Vec<String> = (start..(start + LIST).min(TASKS))
            .map(|at| format!("Task {at}"))
            .collect();
        tasks.new_tasks(&titles);
    }
    assert!(
        tasks.under_tasks() >= TASKS,
        "the record holds every task landed through it"
    );

    // Start one project on the tasks themselves, which changes none of them: what an agent does
    // to a workspace between the tasks it reads. The first actions pay what opening the record
    // left owed; the steady state an agent moving tasks in sees is what is timed.
    let new_project = ActionAddress::new(tasks.root.clone(), "new_project");
    let rows_before = tasks.opened.rows().len();
    let mut took = Vec::with_capacity(ACTIONS);
    let ((), acting) = observe(|| {
        for _ in 0..5 {
            tasks.invoke(&new_project, &serde_json::json!({}));
        }
        for _ in 0..ACTIONS {
            let started = Instant::now();
            tasks.invoke(&new_project, &serde_json::json!({}));
            took.push(started.elapsed());
        }
    });
    assert_eq!(
        drawn(&acting),
        0,
        "an action on tasks nobody has open renders no view: {acting:?}"
    );
    println!(
        "a workspace of {TASKS} tasks: {ACTIONS} actions, median {:?}, total {:?}",
        median(took.clone()),
        took.iter().sum::<Duration>()
    );
    let rows_after = tasks.opened.rows().len();
    println!("rows before {rows_before}, after {rows_after}");
    assert!(
        rows_after > rows_before,
        "every action left what it made ({rows_before} then {rows_after})"
    );
}
