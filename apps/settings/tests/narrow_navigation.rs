//! A narrow Settings window, driven the way a screen reader drives it.
//!
//! Below the shell's breakpoint Settings shows one screen at a time: it opens
//! on the selected pane with a Back arrow to the category list. Every step
//! here goes through the exported accessibility tree and its actions, as an
//! assistive technology does, never through a widget handle.

use settings::SettingsShell;
use ui::accesskit::{Action as AkAction, Role as AkRole};
use ui::framework::testing::TestApp;

/// A typical tiled desktop window, narrower than the shell's breakpoint.
const NARROW: (u32, u32) = (640, 480);

fn clickable(app: &mut TestApp<SettingsShell>, name: &str, role: AkRole) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label() == Some(name) && node.role() == role && node.supports_action(AkAction::Click)
    })
}

fn activate(app: &mut TestApp<SettingsShell>, name: &str) {
    let node = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("{name:?} is exported"));
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("{name:?} activates: {error}"));
    app.settle();
    app.harness().relayout();
    app.settle();
}

#[test]
fn choosing_the_open_category_from_the_narrow_list_shows_it() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    app.harness()
        .resize(NARROW)
        .expect("settings lays out narrow");
    app.settle();

    // Network is the category Settings opens on; narrow, its pane is shown.
    assert!(
        clickable(&mut app, "Back", AkRole::Button),
        "a narrow window opens on the pane, with a Back arrow"
    );
    assert!(
        !clickable(&mut app, "Network", AkRole::TreeItem),
        "the category list is one screen back"
    );

    activate(&mut app, "Back");
    assert!(
        clickable(&mut app, "Network", AkRole::TreeItem),
        "Back shows the category list"
    );

    activate(&mut app, "Network");
    assert!(
        clickable(&mut app, "Back", AkRole::Button)
            && !clickable(&mut app, "Network", AkRole::TreeItem),
        "choosing the selected category opens its pane instead of leaving the list on screen"
    );
}

/// The `[accessibility.atspi_acceptance]` strings the installed-image gate
/// reads from the manifest: `root`, then `required`.
fn manifest_scenario_names() -> Vec<String> {
    let manifest = include_str!("../tilo-app.toml");
    let section = manifest
        .split("[accessibility.atspi_acceptance]")
        .nth(1)
        .expect("the manifest declares an AT-SPI acceptance scenario");
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
    let mut names = value("root");
    names.extend(value("required"));
    names
}

/// The gate finds the window by the app's name and the list's entries,
/// after going Back once from the pane a narrow window opens on.
#[test]
fn the_manifest_scenario_names_are_on_the_narrow_list() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    app.harness()
        .resize(NARROW)
        .expect("settings lays out narrow");
    app.settle();
    activate(&mut app, "Back");
    let exposed: Vec<String> = app
        .harness()
        .a11y_nodes()
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    for name in manifest_scenario_names() {
        assert!(
            exposed.contains(&name),
            "{name:?} is not exposed: {exposed:?}"
        );
    }
}
