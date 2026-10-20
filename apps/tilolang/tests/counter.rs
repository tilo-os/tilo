//! The runtime with the Counter package installed: a person's click runs the action, and history
//! names them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;

const COUNTER: &str = "so.tilo.lang.counter";

fn settle(app: &mut HostedApp) {
    for _ in 0..12 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

/// The id of the control whose accessible name is `name`.
fn control_named(app: &mut HostedApp, name: &str) -> String {
    fn find(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
        let named = node
            .semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(name));
        if named && let Some(id) = &node.test_id {
            return Some(id.clone());
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    find(&app.harness().inspection().root, name)
        .unwrap_or_else(|| panic!("no control named `{name}`"))
}

#[test]
fn a_persons_click_on_a_button_runs_the_action_and_history_names_them() {
    let mut app = HostedApp::open(support::installed("counter"), "ada").unwrap();
    settle(&mut app);
    for _ in 0..3 {
        let plus = control_named(&mut app, "+1");
        app.harness().click_named(&plus).unwrap();
        settle(&mut app);
    }
    let counter = app.read(|doc| {
        let id = resource_ids(doc, COUNTER).remove(0);
        let record = tilolang_tilo::action::read_resource(doc, COUNTER, &id).unwrap();
        record
            .schema()
            .fields
            .iter()
            .find_map(|field| match record.get(&field.id) {
                Some(tilolang_tilo::DynValue::Counter(counter)) => Some(counter.get()),
                _ => None,
            })
            .unwrap()
    });
    assert_eq!(counter, 3, "each click counted once");
    let versions = app.read(ui::tilo_resource::history::versions);
    let presses = versions
        .iter()
        .filter(|version| {
            version.context.as_ref().is_some_and(|context| {
                context
                    .what
                    .as_ref()
                    .is_some_and(|did| format!("{did:?}").contains("Verb"))
            })
        })
        .count();
    assert!(presses >= 3, "the presses are in history: {versions:#?}");
    for version in &versions {
        let context = version.context.as_ref();
        assert!(
            context.is_none_or(|context| context.group.is_none()),
            "a person's own change is ungrouped"
        );
    }
}
