//! An agent's answer offers the person who asked a way to stop the turn while the agent works:
//! the reply draws the agent package's own Stop, and stopping moves the ask on so the control is
//! gone. The check runs through the app's actions and the view a person reads.

use std::rc::Rc;

use tilolang_runtime::members::{Member, Reads};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value};

use crate::fixtures::{Replica, Workspace, target};
use crate::legibility::call;

const AGENT: (&str, &str) = (
    include_str!("../../../../apps/tl/agent/app.tl"),
    include_str!("../../../../apps/tl/agent/app.ids"),
);
const AGENT_APP: &str = "so.tilo.agent";

/// The agent the test asks.
fn helper() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::agent("helper"))
}

/// Ada's workspace with the agent package, the helper agent a member of it.
fn workspace() -> Replica {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[AGENT]);
    ada.host.set_clock(|| "2026-11-21T09:00:00Z".to_owned());
    let grants: Rc<Reads> = Rc::new(|_: &Actor, _: &ResourceRef| true);
    ada.host.runtime().set_members(
        vec![
            Member::person("ada", tilo_action_policy::Role::Owner),
            Member::agent("helper"),
        ],
        Some(grants),
    );
    ada.commit();
    ada
}

/// Commit everything the runtime committed, the asks its mentions made included.
fn settle(r: &Replica) {
    while r.commit() > 0 {}
}

/// The state `ask` stands in, by the enum variant's name.
fn state(r: &Replica, ask: &ResourceRef) -> String {
    let Value::Enum(state) = r.read(ask, "state") else {
        panic!("an ask's state is an enum");
    };
    let meta = r.host.package(AGENT_APP).unwrap().meta;
    meta.enums
        .iter()
        .flat_map(|e| e.variants.iter())
        .find(|v| v.id == state.variant)
        .map_or_else(|| state.variant.clone(), |v| v.name.clone())
}

/// The key of the `ui.Button` labelled `label` under `node`, where it draws one.
fn button(node: &UiNode, label: &str) -> Option<String> {
    node.of_kind("ui.Button")
        .into_iter()
        .find(|b| b.props.get("label") == Some(&Value::text(label)))
        .map(|b| b.key.clone())
}

#[test]
fn a_working_answer_offers_its_asker_stop_and_stopping_moves_the_ask_on() {
    let ada = workspace();
    let agents = Value::resource(&ada.host.root_of(AGENT_APP).unwrap());
    let ask = call(
        &ada,
        "ada",
        AGENT_APP,
        "ask",
        vec![
            agents,
            Value::Actor(helper()),
            Value::Rich(RichValue::from_text("What is due this week?")),
        ],
    );
    settle(&ada);
    let ask_at = target(&ask);
    let reply = call(&ada, &helper(), AGENT_APP, "answer", vec![ask]);
    settle(&ada);
    assert_eq!(state(&ada, &ask_at), "working", "the agent took the turn");

    let reply_at = target(&reply);
    let view = ada
        .host
        .open_resource(&reply_at, None)
        .expect("the reply's own view");
    let stop = button(view.tree(), "Stop").expect("a working answer offers Stop");
    view.press(&stop).expect("the press runs the agent's stop");
    settle(&ada);
    assert_eq!(
        state(&ada, &ask_at),
        "stopped",
        "stopping the turn moves its ask on"
    );

    let again = ada
        .host
        .open_resource(&reply_at, None)
        .expect("the reply's own view");
    assert!(
        button(again.tree(), "Stop").is_none(),
        "a stopped turn offers Stop no more"
    );
}
