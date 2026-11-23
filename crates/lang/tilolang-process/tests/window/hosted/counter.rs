//! The runtime with the Counter package installed: a person's click runs the action, and history
//! names them.

use crate::change_marks::{Driven as _, settle};
use tilolang_tilo::runtime::Value;
use tilolang_tilo::runtime::store::Store as _;

const COUNTER: &str = "so.tilo.lang.counter";

/// The id of the control whose accessible name is `name`.
fn control_named(app: &mut super::Session, name: &str) -> String {
    let named = |node: &&ui::headless::InspectionNode| {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(name))
    };
    (app.harness().inspection().root.nodes().into_iter())
        .filter(named)
        .find_map(|node| node.test_id.clone())
        .unwrap_or_else(|| panic!("no control named `{name}`"))
}

#[test]
fn a_persons_click_on_a_button_runs_the_action_and_history_names_them() {
    let mut app = super::Session::new("so.tilo.lang.counter", "ada");
    settle(&mut app);
    for _ in 0..3 {
        let plus = control_named(&mut app, "Increment");
        app.harness().click_named(&plus).unwrap();
        settle(&mut app);
    }
    let root = app.instance_host().root_of(COUNTER).expect("the counter");
    let counter = match app
        .instance_host()
        .runtime()
        .read(&root, "value")
        .expect("the counter reads")
    {
        Value::Int(n) => n,
        other => panic!("a counter: {other:?}"),
    };
    assert_eq!(counter, 3, "each click counted once");
    let versions = app
        .instance_host()
        .store()
        .versions(&root)
        .expect("the counter's history");
    let presses = versions
        .iter()
        .filter(|version| {
            version
                .who
                .as_ref()
                .is_some_and(|who| format!("{who:?}").contains("ada"))
        })
        .count();
    assert!(presses >= 3, "the presses name the person: {versions:#?}");
    for version in &versions {
        assert!(
            version.run.is_none(),
            "a person's own change is ungrouped: {version:#?}"
        );
    }
}
