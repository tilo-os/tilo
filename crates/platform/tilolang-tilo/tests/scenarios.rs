//! Every app's scenarios run against its root resource and fill its window: the seed data shots,
//! tests and benchmarks start from.

use automerge::AutoCommit;
use tilolang_runtime::{Principal, UiNode};

use crate::fixtures::Replica;

macro_rules! app {
    ($name:literal) => {
        (
            include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
            include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
        )
    };
}

/// Each app's package with the packages it imports before it, and the app it is.
fn apps() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("so.tilo.counter", vec![app!("counter")]),
        ("so.tilo.todo", vec![app!("todo")]),
        ("so.tilo.notes", vec![app!("notes")]),
        ("so.tilo.letters", vec![app!("letters")]),
        ("so.tilo.chat", vec![app!("chat")]),
        (
            "so.tilo.board",
            vec![app!("notes"), app!("todo"), app!("chat"), app!("board")],
        ),
    ]
}

fn nodes<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        nodes(child, out);
    }
}

fn count(node: &UiNode, kind: &str) -> usize {
    let mut all = Vec::new();
    nodes(node, &mut all);
    all.iter().filter(|n| n.kind == kind).count()
}

#[test]
fn every_scenario_of_every_app_seeds_its_root_and_the_first_fills_a_list_past_a_screen() {
    for (app, packages) in apps() {
        let meta = {
            let (replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
            drop(replica);
            loaded.last().unwrap().meta.clone()
        };
        assert!(
            (1..=3).contains(&meta.scenarios.len()),
            "{app} declares one to three scenarios"
        );
        for (i, scenario) in meta.scenarios.iter().enumerate() {
            // Each scenario in a room of its own, as a shot starts from one.
            let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
            // A scenario may attach the roots of the packages it uses (a board's apps).
            for package in &loaded {
                if package.meta.app != app {
                    let _ = replica.host.root_of(&package.meta.app);
                }
            }
            replica.host.root_of(app).unwrap();
            replica.commit();
            replica
                .host
                .runtime()
                .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
                .unwrap_or_else(|e| panic!("{app} {:?}: {e}", scenario.name));
            replica.commit();
            let view = replica.view(app);
            let rows = count(view.tree(), "list_row");
            if i == 0 && view.tree().kind == "list" {
                assert!(
                    rows >= 12,
                    "{app}'s first scenario has enough rows to scroll: {rows}"
                );
            }
        }
    }
}
