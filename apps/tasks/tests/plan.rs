//! Planning an issue is anyone's work: a person's and an agent's.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use common::{chain, held};
use serde_json::json;
use tasks::App;
use tasks::model::issue_resource;
use ui::prelude::*;

#[test]
fn an_agent_is_offered_plan_and_moves_a_due_time_with_it() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    app.call_on(
        &chain(),
        "create",
        json!({ "title": "Ship it", "due_ms": 1_000 }),
    )
    .expect("create");
    let id = held(&app).ids().into_iter().next().expect("the issue");
    let issue = issue_resource(&id);

    let plan = app
        .actions()
        .expect("the actions")
        .into_iter()
        .find(|action| action.address.resource == issue && action.address.action == "plan")
        .expect("the issue offers plan");
    assert!(
        plan.contract.reaches_agents(),
        "an agent's listing offers it"
    );
    assert!(plan.contract.reaches_people(), "a person's does too");

    let scribe = ui::tilo_actions::ActionInvoker::agent("scribe");
    app.call_by(
        scribe,
        &issue,
        "plan",
        json!({ "due_ms": 2_000, "priority": "high" }),
    )
    .expect("an agent plans the issue");
    let planned = held(&app).get(&id).expect("the issue");
    assert_eq!(planned.due_ms, Some(2_000), "wanted later");
    assert_eq!(planned.priority, Some(2));
}
