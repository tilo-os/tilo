//! The layer list stands as a tree: grouped layers under their folder, folded away and
//! opened again, each row choosing what the brush paints.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "in a test a panic is the assertion"
)]

use canvas::App;
use serde_json::json;
use ui::a11y::Action;
use ui::framework::testing::TestApp;

/// The layer list's rows as a reader hears them: each row's name and the depth it stands at.
fn rows(app: &mut TestApp<App>) -> Vec<(String, Option<usize>)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::TreeItem)
        .filter_map(|(_, node)| Some((node.label()?.to_owned(), node.level())))
        .collect()
}

/// Two layers filed in one folder, and their ids: the newer, `Hills`, on top.
fn grouped() -> (TestApp<App>, String, String) {
    let mut app = TestApp::<App>::new().unwrap();
    for name in ["Sky", "Hills"] {
        app.call("add-layer", json!({"name": name})).unwrap();
    }
    let ids: Vec<String> = app
        .state()
        .layers
        .iter()
        .map(|layer| layer.id.clone())
        .collect();
    let [.., sky, hills] = ids.as_slice() else {
        panic!("two layers were added: {ids:?}");
    };
    for id in [sky, hills] {
        app.call("edit-layer", json!({"id": id, "group": "Foreground"}))
            .unwrap();
    }
    (app, sky.clone(), hills.clone())
}

#[test]
fn grouped_layers_stand_under_their_folder_and_fold_away() {
    let (mut app, _, _) = grouped();
    assert_eq!(
        rows(&mut app),
        [
            ("Foreground".to_owned(), Some(1)),
            ("Hills".to_owned(), Some(2)),
            ("Sky".to_owned(), Some(2)),
            ("Layer 1".to_owned(), Some(1)),
        ],
        "both layers stand in the folder, top first"
    );
    // The folder's own row folds its layers away, as its disclosure does.
    app.harness()
        .perform_semantic_named("canvas-layer-tree", Some("key:4"), &Action::Collapse)
        .unwrap();
    assert_eq!(
        rows(&mut app),
        [
            ("Foreground".to_owned(), Some(1)),
            ("Layer 1".to_owned(), Some(1))
        ]
    );
    app.harness()
        .perform_semantic_named("canvas-layer-tree", Some("key:4"), &Action::Expand)
        .unwrap();
    assert_eq!(rows(&mut app).len(), 4, "the folder opens again");
}

#[test]
fn choosing_a_layers_row_draws_on_that_layer() {
    let (mut app, sky, _) = grouped();
    let row = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::TreeItem && node.label() == Some("Sky")
        })
        .and_then(|(_, node)| node.bounds())
        .expect("the layer's row is on screen");
    app.harness()
        .click_at(f64::midpoint(row.x0, row.x1), f64::midpoint(row.y0, row.y1));
    app.settle();
    assert!(
        app.harness()
            .targets()
            .contains_key(&format!("canvas-layer-name-{sky}")),
        "the chosen layer's own controls stand under the list: {sky}"
    );
}
