//! Captures of the shell on two displays, in both themes: the display in use draws its own
//! strip's window and the whole bar, and the other display draws its own strip's window and the
//! compact bar, which carries no bell. The scenarios stage the display each part reads
//! (`device.screen`), so the captures show what a person sees on each screen, and the tree holds
//! each part to drawing one window.

#![cfg(target_os = "linux")]

use crate::fixtures::texts_walked as texts;
use crate::shell_staged::StagedWindow;

/// The display in use, whose strip holds one window and whose bar carries the bell.
const IN_USE: &str = "two displays, the one in use";
/// The other display, whose strip holds one window and whose bar is the compact one.
const OTHER: &str = "two displays, the other one";

/// The directory a capture is written to: the one the check names (`TILO_SHELL_DISPLAY_SHOTS`), or
/// else beside the test's built binary, as the other shots are, since a source tree read from a
/// test may be read-only.
fn captures() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("TILO_SHELL_DISPLAY_SHOTS") {
        return std::path::PathBuf::from(dir);
    }
    std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("shell-displays-shot")
}

/// The shell on two displays, each part drawn on its own display and saved in both themes: one
/// window per part, and the whole bar's bell only on the display in use.
#[test]
fn two_displays_in_both_themes() {
    for (scenario, name, whole_bar) in [(IN_USE, "in-use", true), (OTHER, "other", false)] {
        let mut dark = tilo_ui_base::Theme::dark();
        dark.reduce_motion = true;
        let mut shell = StagedWindow::staged(scenario, (1280, 800), dark);
        let tree = shell.instance.root_tree().expect("the window's tree");
        let windows = tree.of_kind("ui.WindowFrame");
        assert_eq!(
            windows.len(),
            1,
            "each display draws its own strip's window alone, from {scenario}: {tree:#?}"
        );
        let said = texts(&tree);
        let bar: Vec<&String> = said
            .iter()
            .filter(|text| {
                text.contains("Notifications") || text.contains("Tilo") || text.contains("Showing")
            })
            .collect();
        let bells = said
            .iter()
            .filter(|text| text.contains("Notifications"))
            .count();
        assert_eq!(
            bells,
            usize::from(whole_bar),
            "the display in use carries the whole bar, the other the compact one: {bar:?}"
        );
        assert!(
            said.iter().any(|text| text.contains("4:28 PM")),
            "every display's bar keeps the time: {bar:?}"
        );
        capture(&mut shell, name);
    }
}

/// `shell` drawn in the dark theme it was staged in and again in the light one, saved as
/// `{name}-dark.png` and `{name}-light.png` beside the build: a source tree read from a test may
/// be read-only.
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
        .relayout_themed((1280, 800), light)
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
