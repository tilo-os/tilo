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
/// `apply` and `group` name the actions they repeat; the field-edit listing
/// names each editable path once with its operations; a parameter's schema
/// does not repeat its description, nor a description its label.
const BUDGET: usize = 64 * 1024;

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
    // Header controls remain real AT controls even where the artwork also
    // exposes their canonical commands. Suppressing their semantic leaves
    // creates orphan bindings; their public UI door is chiefly native focus.
    let header = [
        "canvas%2Dadd%2Dpicture",
        "canvas%2Dexport%2Dtitle",
        "canvas%2Dmenu",
        "canvas%2Dzoom%2Dfit",
        "canvas%2Dzoom%2Din",
        "canvas%2Dzoom%2Dout",
        "undo",
        "redo",
    ];
    let rows = surface.as_array().unwrap();
    let controls: std::collections::BTreeSet<_> = rows
        .iter()
        .filter(|row| row["resource_kind"] == "tilo.ui.control")
        .filter_map(|row| row["resource"].as_str())
        .filter(|resource| !resource.contains("nav%2Dshell%2D"))
        .collect();
    let expected: std::collections::BTreeSet<_> = header
        .iter()
        .map(|id| format!("ui:root%7Ek%3Acanvas%2Dshell%2Ewindow%7Ek%3A{id}"))
        .collect();
    assert_eq!(
        controls,
        expected
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        "exact native header inventory, with no unrelated controls"
    );
    for resource in &controls {
        assert_eq!(
            rows.iter()
                .filter(|row| row["resource"] == *resource
                    && row["action"] == "focus"
                    && row["operation"] == "View")
                .count(),
            1,
            "each header control has exactly one actual focus door: {resource}"
        );
    }
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
        "native header controls never mint alternate document mutation doors"
    );
    let bindings = ui::widget::controls::control_action_bindings(app.harness().root_widget());
    for command in ["undo", "redo"] {
        assert!(bindings.iter().any(|binding| binding.control_id == command && matches!(
            &binding.kind, ui::widget::ControlBindingKind::Action(address) if address.action == command
        )), "the actual header retains canonical {command}: {bindings:#?}");
    }
    let audit = app.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "the actual semantic leaves match their bindings: {audit:#?}"
    );
}
