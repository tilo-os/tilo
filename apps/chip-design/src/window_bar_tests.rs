use ui::framework::testing::TestApp;
use ui::window_toolbar::{ToolbarEvent, declared};

use crate::ChipDesignShell;

#[test]
fn the_projects_commands_are_window_bar_actions_and_the_window_draws_no_header_row() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.harness().present_in_window_bar();
    app.settle();
    let bar = declared().expect("the shell declares the window's slots");
    let names: Vec<&str> = bar
        .actions
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    for command in ["Operator graph", "All reports", "Import report"] {
        assert!(
            names.contains(&command),
            "{command} is a bar action: {names:?}"
        );
    }
    assert!(
        !names.iter().any(|name| name.contains("Search")),
        "the bar's own search is the only one: {names:?}"
    );
    let drawn: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    for command in ["Operator graph", "All reports", "Import report"] {
        assert!(
            !drawn.iter().any(|label| label == command),
            "the window draws no {command} button of its own: {drawn:?}"
        );
    }

    let graph = bar
        .actions
        .iter()
        .find(|action| action.name == "Operator graph")
        .expect("the graph's action")
        .id
        .clone();
    app.harness()
        .press_window_slot(ToolbarEvent::Action { id: graph });
    app.settle();
    assert!(
        app.harness().targets().contains_key("chip-graph-add"),
        "the bar's action opened the operator graph"
    );
}
