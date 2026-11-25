//! Captures of the shell's questions, in both themes: the confirm for the scenario "a press that
//! needs a yes", standing at the point of the Notes window the Share was pressed at, and a
//! question nothing here started, its keyboard on declining it. The scenario is staged in
//! the shell's own package by the instance that sends its desktop to the window, the same staging
//! the shell's other tests read the scenario from.

#![cfg(target_os = "linux")]

use tilolang_runtime::{UiNode, Value};

use crate::shell_staged::StagedWindow;

/// The asked record the scenario leaves waiting, whose own `resolve` answers the confirm.
const ASKED: &[&str] = &["studio/invocations/share-1"];

/// The scenario staged: Notes' Share, pressed at (640, 212) in its window, waits for Ada.
const SCENARIO: &str = "a press that needs a yes";

/// The directory a capture is written to: beside the test's built binary, as the other shots
/// are, since a source tree read from a test may be read-only.
fn captures() -> std::path::PathBuf {
    std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("asking-at-press-shot")
}

/// `value`, out of the option it may be given in.
fn given(value: Option<&Value>) -> Option<&Value> {
    match value? {
        Value::Opt(inner) => inner.as_deref(),
        value => Some(value),
    }
}

/// Every text the nodes under `tree` say, as their props carry it.
fn texts(tree: &UiNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = vec![tree];
    while let Some(node) = at.pop() {
        out.extend(node.props.values().filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        }));
        at.extend(node.children.iter());
    }
    out
}

/// The shell's desktop as its scenario "a press that needs a yes" stages it, in both themes, saved
/// beside `out`.
#[test]
fn a_press_that_needs_a_yes_in_both_themes() {
    let mut dark = ui::Theme::dark();
    dark.reduce_motion = true;
    let mut shell = StagedWindow::staged(SCENARIO, ASKED, (1280, 800), dark);
    // The confirm is what the capture is of: it stands at the press, showing whom the call would
    // give the note to, in the tree the window is sent.
    let tree = shell.instance.root_tree().expect("the window's tree");
    let [confirm] = tree.of_kind("ui.Popover")[..] else {
        panic!("one confirm stands at the press: {tree:#?}");
    };
    assert_eq!(
        given(confirm.props.get("at")),
        Some(&Value::List(vec![Value::Float(640.0), Value::Float(212.0)])),
        "at the point of the window the press was in"
    );
    let shown = texts(confirm);
    for fact in ["Mia Clayton", "Can edit"] {
        assert!(
            shown.iter().any(|text| text.contains(fact)),
            "the confirm shows what the call would do ({fact}): {shown:?}"
        );
    }

    capture(&mut shell, "asking-at-press");
}

/// The shell's desktop as its scenario "a call that writes another record" stages it, in both
/// themes: a question no press here started, whose keyboard starts on declining it.
#[test]
fn a_question_nothing_here_started_in_both_themes() {
    let mut dark = ui::Theme::dark();
    dark.reduce_motion = true;
    let mut shell = StagedWindow::staged(
        "a call that writes another record",
        &["studio/invocations/mail-1", "studio/invocations/alarm-3"],
        (1280, 800),
        dark,
    );
    assert_eq!(
        shell.holds_keyboard().as_deref(),
        Some("Don't allow"),
        "nothing here started the call, so the keyboard starts on declining it"
    );
    capture(&mut shell, "asked-unpressed");
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
    let mut light = ui::Theme::light();
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
