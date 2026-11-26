//! Captures of the quick settings' refused states in both themes: a search for networks that
//! failed with the press that looks again, a network whose password it would not take with its
//! field open under its row, a saved network's menu, and a paired accessory's own page.

#![cfg(target_os = "linux")]

use crate::shell_staged::StagedWindow;

/// The scenario staged in every capture here.
const SCENARIO: &str = "the quick settings, refused";

/// The directory a capture is written to: beside the test's built binary, as the other shots are,
/// since a source tree read from a test may be read-only.
fn captures() -> std::path::PathBuf {
    std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("quick-states-shot")
}

/// The shell's desktop as the scenario stages it, in the dark theme and then the light one, saved
/// as `{name}-dark.png` and `{name}-light.png`.
fn capture(shell: &mut StagedWindow, name: &str, (width, height): (u32, u32)) {
    let out = captures();
    std::fs::create_dir_all(&out).expect("the captures' directory");
    shell
        .harness
        .render_png(&out.join(format!("{name}-dark.png")));
    let mut light = tilo_ui_base::Theme::light();
    light.reduce_motion = true;
    shell
        .harness
        .relayout_themed((width, height), light)
        .expect("the light frame");
    shell.exchange();
    shell
        .harness
        .render_png(&out.join(format!("{name}-light.png")));
    for theme in ["dark", "light"] {
        let path = out.join(format!("{name}-{theme}.png"));
        assert!(
            std::fs::metadata(&path).is_ok_and(|held| held.len() > 0),
            "the {theme} capture is written: {}",
            path.display()
        );
    }
}

/// The panel over a device that refused, with the field open under the network whose password it
/// would not take.
#[test]
fn the_refused_panel_in_both_themes() {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = true;
    let mut shell = StagedWindow::staged(SCENARIO, (1280, 800), dark);
    shell.press("Studio");
    capture(&mut shell, "quick-refused", (1280, 800));
}

/// The menu at a saved network's row, open over the panel: the commands it offers stand over the
/// rows under it rather than in place of them.
#[test]
fn a_saved_networks_menu_in_both_themes() {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = true;
    let mut shell = StagedWindow::staged(SCENARIO, (1280, 800), dark);
    shell.press("This network");
    capture(&mut shell, "quick-menu", (1280, 800));
}

/// One accessory's page: what the device knows of it, the name it is called by, the devices it can
/// move to, and the ways to disconnect or forget it.
#[test]
fn an_accessorys_page_in_both_themes() {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = true;
    // Tall enough that the accessory's row stands in the panel and the page fits under it.
    let mut shell = StagedWindow::staged(SCENARIO, (1280, 1600), dark);
    shell.press("Sony WH-1000XM5");
    capture(&mut shell, "quick-accessory", (1280, 1600));
}
