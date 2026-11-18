//! What reaches a view of the shapes an app publishes beyond plain values, seen in a view over a
//! workspace, and that none of it reaches a viewer who may not read the app.

use serde_json::{Value as Json, json};
use ui::written_view::ViewState as _;

use super::{TestWorkspace, ada, shown_reading, texts};

const SHEETS: &str = "so.tilo.sheets";

/// A workbook of Sheets with the remarks `remarks`, each its words and the name of who made it,
/// as a read answers what Sheets stores: a collection is its items by id, each under its place,
/// and who made a remark is a case that carries a value, its name under `$variant` beside the
/// value under the number the case gives it.
fn workbook(remarks: &[(&str, &str)]) -> Json {
    let mut stored = serde_json::Map::new();
    for (at, (words, by)) in remarks.iter().enumerate() {
        stored.insert(
            format!("c{at}"),
            json!({ "k": format!("a{at}"), "v": {
                "anchor": {
                    "sheet": "s1",
                    "area": { "top": "1", "left": "A", "bottom": "1", "right": "A" },
                },
                "author": { "$variant": "named", "0": by },
                "date": "2026-11-18",
                "text": words,
                "body": [],
                "resolved": false,
                "replies": {},
            } }),
        );
    }
    json!({ "comments": stored })
}

/// Each remark of the workbook: who made it, then its words.
const REMARKS: &str = "view remarks() {\n    for c in sheets.workbook.comments {\n        match c.author {\n            .member(_) => text(\"a member\"),\n            .named(name) => text(name),\n        }\n        text(c.text)\n    }\n}";

#[test]
fn a_case_that_carries_a_value_reaches_a_view_with_its_value() {
    let workspace = TestWorkspace::default();
    workspace.write(SHEETS, workbook(&[("Check the sum", "Ada")]));
    let (mut surface, state) = shown_reading(REMARKS, ada(), &["sheets"], &workspace);
    assert_eq!(
        texts(&surface),
        ["Ada", "Check the sum"],
        "who made the remark, by the value its case carries, then its words: {:?}",
        surface.nodes("error")
    );

    // Someone adds a remark in Sheets: the view follows, case and value.
    workspace.write(
        SHEETS,
        workbook(&[("Check the sum", "Ada"), ("Done", "Bo")]),
    );
    surface.turn();
    assert_eq!(texts(&surface), ["Ada", "Check the sum", "Bo", "Done"]);

    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn a_case_and_its_value_stay_unread_by_a_viewer_who_may_not_read_the_app() {
    let workspace = TestWorkspace::default();
    workspace.write(SHEETS, workbook(&[("Check the sum", "Ada")]));
    workspace.refuse(SHEETS);
    let (mut surface, state) = shown_reading(REMARKS, ada(), &["sheets"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Sheets"]);
    assert!(
        texts(&surface).is_empty(),
        "neither the remark nor who made it is shown: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}
