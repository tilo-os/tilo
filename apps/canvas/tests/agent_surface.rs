//! The agent's view of Canvas, kept as a golden file: every published
//! action, its description and parameter schema. A change to the
//! vocabulary shows up here as a diff to review, with its size in context.
//!
//! Update after an intended change:
//! `TILO_UPDATE_GOLDEN=1 cargo test -p canvas --test agent_surface`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::TestApp;

/// The most the whole surface may weigh, in bytes of JSON: what an agent
/// reads before it can act. Growing past it is a design decision, not drift.
/// Vector design took it to 89 KB, most of that `apply` and `group`
/// repeating every named action's schema; they now name those actions
/// instead, and it fits the original budget again.
const BUDGET: usize = 64 * 1024;

#[test]
fn the_agent_surface_matches_its_golden_file_and_budget() {
    let mut app = TestApp::<App>::new().expect("the app opens");
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
        "diff",
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
    let controls: Vec<&str> = surface
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["resource_kind"] == "tilo.ui.control")
        .filter_map(|row| row["resource"].as_str())
        .collect();
    for control in ["canvas-brush-size", "canvas-layer", "add-layer"] {
        let encoded = control.replace('-', "%2D");
        assert!(
            controls.iter().any(|id| id.contains(&encoded)),
            "the inspector's {control} control is reachable: {controls:?}"
        );
    }
}
