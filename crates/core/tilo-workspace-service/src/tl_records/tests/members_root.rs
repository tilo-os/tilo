//! An action run by the workspace service reads who is in the workspace through the Workspace
//! package's `members` root, as a window does: the record's host serves the root from the
//! workspace's policy it was opened with, the one list every host reads.

use std::rc::Rc;
use std::sync::Arc;

use tilo_action_policy::{Role, WorkspacePolicy};

use super::super::{LocalPort, Opened, Packages, ResourceStore, member, runtime::Value};

/// A roll call: an action that counts and names everyone in the workspace.
const ROLL: &str = r#"app test.roll "Roll"

use tilo.workspace as workspace

type Roll {
    called: Text = ""
}

root Roll

default view roll for Roll

view roll(r: Roll) {
    text(r.called)
}

/// Calls everyone in the workspace, by their id, and answers how many it called.
action call_roll(r: Roll) -> Int {
    r.called = workspace.members.people.map(p -> p.id()).join(", ")
    workspace.members.people.len()
}
"#;

#[test]
fn an_action_the_service_runs_reads_the_members_through_the_workspaces_root() {
    let packages = Packages::compile(&[(ROLL, "")]).expect("the roll call compiles");
    let mut policy = WorkspacePolicy::new("workspace", "ada");
    policy.members.insert("bo".to_owned(), Role::Editor);
    let ada = member(tilo_actions::MemberId::person("ada"));
    let mut opened = Opened::open(
        ResourceStore::new(Rc::new(LocalPort::in_memory())),
        &packages,
        ada.clone(),
        Some(Arc::new(policy)),
    )
    .expect("the record opens with the workspace's policy");
    let roll = opened.open_root().expect("the roll is made");
    let called = opened
        .invoke(
            &ada,
            ("test.roll", "call_roll"),
            vec![Value::resource(&roll)],
            None,
        )
        .expect("the roll is called");
    assert_eq!(called, Value::Int(2), "the owner and the member are called");
    let names = opened
        .host()
        .runtime()
        .read(&roll, "called")
        .expect("what the roll called reads back");
    // An actor's id is what names it on every device: the member it is, as text.
    assert_eq!(
        names,
        Value::text(r#"{"member":{"person":"ada"}}, {"member":{"person":"bo"}}"#)
    );
}
