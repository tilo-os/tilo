//! The agent's view of Canvas, kept as a golden file: every published
//! action, its description and parameter schema. A change to the
//! vocabulary shows up here as a diff to review, with its size in context.
//!
//! Update after an intended change:
//! `TILO_UPDATE_GOLDEN=1 cargo test -p canvas --test agent_surface`.
#![allow(
    clippy::unwrap_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::TestApp;

/// The most the whole surface may weigh, in bytes of JSON: what an agent
/// reads before it can act. Growing past it is a design decision, not drift.
/// `apply` and `group` name the actions they repeat; the field-edit listing
/// names each editable path once with its operations; a parameter's schema
/// does not repeat its description, nor a description its label. Raised
/// from 64 KiB when every document resource gained `review-restore` and
/// `restore-reviewed` (exact-item restore, #4581).
const BUDGET: usize = 68 * 1024;

#[test]
fn the_agent_surface_matches_its_golden_file_and_budget() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    // The inspector slides in as the window opens; its controls are part of
    // what an agent sees once it has.
    let _ = app.harness().render_rgba();
    assert!(
        app.harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the window settles"
    );
    let surface = app.agent_surface();
    let text = serde_json::to_string_pretty(&surface).unwrap() + "\n";
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/agent-surface.json");
    if std::env::var_os("TILO_UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    // The current surface beside the build output, to review or promote
    // when the test ran on another machine.
    let current =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/agent-surface");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("canvas.json"), &text).unwrap();
    let compact = serde_json::to_string(&surface).unwrap().len();
    assert!(
        compact <= BUDGET,
        "the agent surface is {compact} bytes, over the {BUDGET}-byte budget"
    );
    assert_header_inventory(&surface, &mut app);
    let expected = std::fs::read_to_string(&golden).unwrap_or_default();
    assert!(
        expected == text,
        "the agent surface changed ({compact} bytes); review the diff and update with TILO_UPDATE_GOLDEN=1:\n{text}"
    );
    let actions: Vec<&str> = surface
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["resource_kind"] == "canvas.artwork")
        .filter_map(|row| row["action"].as_str())
        .collect();
    for action in [
        "paint",
        "draw",
        "edit-marks",
        "look",
        "inspect",
        "media",
        "apply",
        "history",
        "compare-versions",
        "undo",
        "redo",
        "restore",
        "checkpoint",
        "render",
    ] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }
    assert!(
        surface
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["resource_kind"] == "tilo.control.segmented"),
        "the tool rail is reachable"
    );
}

fn assert_header_inventory(surface: &serde_json::Value, app: &mut TestApp<App>) {
    // The artwork's commands are the window bar's slots now, not a row of
    // buttons inside the window: the app's own surface holds no header
    // control, so none of them mints a second door onto a document edit.
    let rows = surface.as_array().unwrap();
    let controls: Vec<_> = rows
        .iter()
        .filter(|row| row["resource_kind"] == "tilo.ui.control")
        .filter_map(|row| row["resource"].as_str())
        .filter(|resource| !resource.contains("nav%2Dshell%2D"))
        .collect();
    assert_eq!(
        controls,
        Vec::<&str>::new(),
        "the window bar holds the artwork's commands, not the body"
    );
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    let ids: Vec<&str> = declared
        .actions
        .iter()
        .flat_map(|action| {
            std::iter::once(action.id.as_str())
                .chain(action.items.iter().map(|item| item.id.as_str()))
        })
        .collect();
    for wanted in [
        "canvas.menu",
        "canvas.undo",
        "canvas.redo",
        "canvas.add-picture",
        "canvas.export",
        "canvas.zoom",
        "canvas.zoom-in",
        "canvas.zoom-out",
        "canvas.zoom-fit",
        "canvas.zoom-actual",
        "canvas.tools",
    ] {
        assert!(ids.contains(&wanted), "{wanted} is a slot in {ids:?}");
    }
    let location = declared.location.expect("the artwork is the location");
    assert!(
        location.editable,
        "the artwork's title is renamed in place, as the header's field did"
    );
    let documents: Vec<_> = rows
        .iter()
        .filter(|row| row["operation"] == "Document")
        .collect();
    let addresses: std::collections::BTreeSet<_> = documents
        .iter()
        .map(|row| {
            (
                row["resource"].as_str().unwrap(),
                row["action"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        addresses.len(),
        documents.len(),
        "document commands are never duplicated"
    );
    assert!(
        documents
            .iter()
            .all(|row| row["resource_kind"] != "tilo.ui.control"),
        "bar slots never mint alternate document mutation doors"
    );
    let audit = app.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "the actual semantic leaves match their bindings: {audit:#?}"
    );
}
