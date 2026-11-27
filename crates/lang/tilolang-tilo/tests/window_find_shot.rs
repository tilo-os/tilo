//! Captures of the window bar's find, in both themes: what it shows with matches, with none, its
//! options open, and opened to replace. The scenarios are staged in the shell's own package by the
//! instance that sends its desktop to the window, as the shell's other tests read them.

#![cfg(target_os = "linux")]

use crate::shell_staged::StagedWindow;

const SIZE: (u32, u32) = (1280, 800);

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
        .join("window-find-shot")
}

/// The shell staged as `scenario`, in the dark theme, at rest.
fn staged(scenario: &str) -> StagedWindow {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = true;
    StagedWindow::staged(scenario, SIZE, dark)
}

/// `shell` drawn in the dark theme it was staged in and again in the light one, saved as
/// `{name}-dark.png` and `{name}-light.png` beside the build.
fn capture(shell: &mut StagedWindow, name: &str) {
    let out = captures();
    std::fs::create_dir_all(&out).expect("the captures' directory");
    shell
        .harness
        .render_png(&out.join(format!("{name}-dark.png")));
    let mut light = tilo_ui_base::Theme::light();
    light.reduce_motion = true;
    shell
        .harness
        .relayout_themed(SIZE, light)
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

/// A find with matches: the field holds the query, the bar says the match it stands on of how
/// many the note's app found.
#[test]
fn a_find_with_matches_in_both_themes() {
    let mut shell = staged("a window found in");
    assert!(
        shell.harness.a11y_node_named("2 of 5").is_some(),
        "the bar shows the match of how many"
    );
    capture(&mut shell, "window-find-matches");
}

/// A find that found nothing: the bar says so where the count stands.
#[test]
fn a_find_that_found_nothing_in_both_themes() {
    let mut shell = staged("a window found nothing");
    assert!(
        shell.harness.a11y_node_named("No matches").is_some(),
        "the bar says nothing matched"
    );
    capture(&mut shell, "window-find-none");
}

/// The find's options open under its button, checked as the app runs its find, with the
/// replacement beside them where the content allows it.
#[test]
fn a_finds_options_open_in_both_themes() {
    let mut shell = staged("a window found in");
    // The options button, pressed where it stands: the window's composed tree is the app window's,
    // so its bar's controls are reached by what assistive technology reads of them.
    let id = shell
        .harness
        .a11y_node_named("Find options")
        .expect("the options button");
    let bounds = shell
        .harness
        .a11y_nodes()
        .into_iter()
        .find(|(node, _)| *node == id)
        .and_then(|(_, node)| node.bounds())
        .expect("the button stands on the window");
    let center = (bounds.x0.midpoint(bounds.x1), bounds.y0.midpoint(bounds.y1));
    shell.harness.click_at(center.0, center.1);
    shell.exchange();
    for item in ["Match case", "Whole word", "Regular expression"] {
        assert!(
            shell.harness.a11y_node_named(item).is_some(),
            "the menu offers {item}"
        );
    }
    capture(&mut shell, "window-find-options");
}

/// A find opened to replace: the replacement holds the keyboard beside the query, with Replace and
/// Replace all.
#[test]
fn a_find_opened_to_replace_in_both_themes() {
    let mut shell = staged("a window replacing");
    for part in ["Replace with", "Replace", "Replace all"] {
        assert!(
            shell.harness.a11y_node_named(part).is_some(),
            "the find offers {part}"
        );
    }
    capture(&mut shell, "window-find-replace");
}
