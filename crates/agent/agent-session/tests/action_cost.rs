//! What an action on a tracker nobody watches costs: its own work, never a
//! frame of the app's view.
//!
//! The times are printed for reading (`cargo test -p tilo-agent-session
//! --test it action_cost -- --nocapture`), never asserted: on a shared
//! machine they vary with the load. The counts are asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use ui::tilo_resource::profiling::observe;

/// The issues a tracker holds before its actions are timed.
const ISSUES: usize = 200;

/// The actions timed on it.
const ACTIONS: usize = 30;

/// The address the tracker publishes its issues under.
fn issues() -> String {
    ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ))
}

/// File one issue through the tracker's own action.
fn file(page: &mut Replica, address: &ActionAddress, number: usize) {
    page.invoke(
        address,
        serde_json::json!({"title": format!("Issue {number}")})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the issue is filed");
}

/// Make one project on the tracker itself, which changes none of its issues:
/// what an agent does to a tracker between the issues it works.
fn project(page: &mut Replica, address: &ActionAddress, number: usize) {
    page.invoke(
        address,
        serde_json::json!({"name": format!("Project {number}")})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the project is made");
}

fn median(mut took: Vec<Duration>) -> Duration {
    took.sort();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

/// A tracker nobody watches answers its actions without laying its view out:
/// the work of the action lands before it answers, and a tracker holding many
/// issues costs what the one action does, not a frame of all of them.
#[test]
fn an_action_on_a_tracker_nobody_watches_lays_nothing_out() {
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let address = ActionAddress::new(issues(), "create");
    for number in 0..ISSUES {
        file(&mut page, &address, number);
    }
    assert!(
        page.survey_rows().len() >= ISSUES,
        "the tracker holds every issue filed through it"
    );

    // The first actions of a view pay what opening it left owed; the
    // steady state an agent moving a tracker in sees is what is timed.
    let tracker = page
        .resources()
        .into_iter()
        .find(|resource| resource.kind == "tasks.tracker")
        .map(|resource| ActionAddress::new(resource.id.as_str(), "create-area"))
        .expect("the tracker is the only one it holds");
    let rows_before = page.survey_rows().len();
    let mut took = Vec::with_capacity(ACTIONS);
    let ((), acting) = observe(|| {
        for number in 0..5 {
            project(&mut page, &tracker, number);
        }
        for number in 0..ACTIONS {
            let started = Instant::now();
            project(&mut page, &tracker, 5 + number);
            took.push(started.elapsed());
        }
    });
    let layouts: usize = acting
        .iter()
        .filter(|phase| phase.phase == "ui.layout")
        .map(|phase| phase.calls)
        .sum();
    assert_eq!(
        layouts, 0,
        "an action on a tracker nobody watches builds no frame: {acting:?}"
    );
    println!(
        "a tracker of {ISSUES} issues: {ACTIONS} actions, median {:?}, total {:?}",
        median(took.clone()),
        took.iter().sum::<Duration>()
    );
    let rows_after = page.survey_rows().len();
    println!("rows before {rows_before}, after {rows_after}");
    assert!(
        rows_after > rows_before,
        "every action left what it made ({rows_before} then {rows_after})"
    );
}
