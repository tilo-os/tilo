//! The installed-image AT-SPI scenario in `tilo-app.toml`, run headlessly.
//!
//! The release gate opens Photos on a freshly installed profile, finds the
//! manifest's `required` names, activates `action` and waits for every
//! `after_action` name. This drives the same steps through the exported
//! accessibility tree — the one the AT-SPI bridge serves — so a manifest that
//! names something the app does not expose fails here, not in a booted VM.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use photos::PhotosShell;
use ui::accesskit::Action as AkAction;
use ui::framework::testing::TestApp;

/// A tiled desktop window on the gate's 1280x800 display.
const WINDOW: (u32, u32) = (664, 760);

struct Acceptance {
    required: Vec<String>,
    action: String,
    after_action: Vec<String>,
}

/// The `[accessibility.atspi_acceptance]` strings, read from the manifest
/// the package ships.
fn acceptance() -> Acceptance {
    let manifest = include_str!("../tilo-app.toml");
    let section = manifest
        .split("[accessibility.atspi_acceptance]")
        .nth(1)
        .expect("the manifest declares an AT-SPI acceptance scenario");
    let section = section.split("\n[").next().unwrap_or(section);
    let value = |key: &str| -> Vec<String> {
        let line = section
            .lines()
            .find(|line| {
                line.split('=')
                    .next()
                    .is_some_and(|name| name.trim() == key)
            })
            .unwrap_or_else(|| panic!("the scenario sets {key}"));
        line.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    };
    Acceptance {
        required: value("required"),
        action: value("action").remove(0),
        after_action: value("after_action"),
    }
}

fn names(app: &mut TestApp<PhotosShell>) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

#[test]
fn the_manifest_scenario_holds_on_a_fresh_library() {
    let scenario = acceptance();
    let mut app = TestApp::<PhotosShell>::new().expect("photos opens headlessly");
    app.settle();
    app.harness().resize(WINDOW).expect("photos lays out");
    app.settle();

    let before = names(&mut app);
    for name in &scenario.required {
        assert!(
            before.contains(name),
            "{name:?} is exposed on open: {before:?}"
        );
    }
    for name in &scenario.after_action {
        assert!(
            !before.contains(name),
            "{name:?} must only appear once {:?} acts: {before:?}",
            scenario.action
        );
    }

    let action = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some(scenario.action.as_str()) && node.supports_action(AkAction::Click)
        })
        .map(|(id, _)| id)
        .expect("the scenario's action is click-capable");
    app.harness()
        .perform_a11y_action(action, &ui::a11y::Action::Activate)
        .expect("the action is performed");
    app.settle();
    app.harness().relayout();
    app.settle();

    let after = names(&mut app);
    for name in &scenario.after_action {
        assert!(
            after.contains(name),
            "{name:?} is exposed after the action: {after:?}"
        );
    }
    assert!(
        !after.iter().any(|name| name == "Timeline position"),
        "an empty library paints no timeline index, so none is published: {after:?}"
    );
}
