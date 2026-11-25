//! An action's key and a control's key: the shortcut that runs a press, in the toolkit's one
//! spelling, and what the compiler refuses.

use crate::common::{codes, compile_body, validate};
use std::fmt::Write as _;

const NOTES: &str = r#"type Note (untitled: "Untitled") {
    title: Text
}

type Notebook {
    notes: Collection<Note>
}

action new_note(nb: Notebook) -> Note (icon: .plus) {
    nb.notes.insert(Note(title: ""))
}
"#;

/// A view with one `mode` state and a button per key it is given.
fn modes(keys: &[&str]) -> String {
    let mut buttons = String::new();
    for (at, key) in keys.iter().enumerate() {
        let _ = write!(
            buttons,
            "    ui.Button(\"Mode {at}\", key: \"{key}\") {{\n        on press: set(mode, {at})\n    }}\n"
        );
    }
    format!("view v() {{\n    state mode: Int = 0\n{buttons}}}\n")
}

/// The action `share` of the notes package, keyed by `key`.
fn share(key: &str) -> String {
    format!("{NOTES}\naction share(nb: Notebook) (key: \"{key}\") {{}}\n")
}

#[test]
fn an_actions_key_is_published_in_the_toolkits_one_spelling() {
    let meta = compile_body(&share("control+shift+Return")).meta;
    let share = meta
        .actions
        .iter()
        .find(|a| a.name == "share")
        .expect("the action");
    assert_eq!(
        share.key.as_deref(),
        Some("Ctrl+Shift+enter"),
        "whatever order and case it was written in"
    );
}

#[test]
fn a_key_the_desktop_keeps_is_refused() {
    // F1 names every shortcut the desktop answers, so no action can be bound to it.
    let got = codes(&share("F1"));
    assert!(
        got.iter().any(|c| c == "E090"),
        "expected E090, got {got:?}"
    );
}

#[test]
fn a_chord_that_is_no_shortcut_is_refused() {
    for wrong in ["Ctrl+", "Entr", "Ctrl+E+F"] {
        let got = codes(&share(wrong));
        assert!(
            got.iter().any(|c| c == "E090"),
            "expected E090 for {wrong:?}, got {got:?}"
        );
    }
}

/// What the person types where they answer an action's ask is published in order, a secret
/// marked so; it is no parameter of the action.
#[test]
fn an_actions_takes_are_published_in_order_with_its_secret() {
    let meta = compile_body(&format!(
        "{NOTES}\ndestructive action unlock(nb: Notebook) (takes: [\"User name\", (\"Password\", .secret)]) {{}}\n"
    ))
    .meta;
    let unlock = meta
        .actions
        .iter()
        .find(|a| a.name == "unlock")
        .expect("the action");
    let takes: Vec<(&str, bool)> = unlock
        .takes
        .iter()
        .map(|take| (take.label.as_str(), take.secret))
        .collect();
    assert_eq!(takes, [("User name", false), ("Password", true)]);
    assert_eq!(unlock.params.len(), 1, "what is typed is no parameter");
}

#[test]
fn a_take_that_is_no_words_is_refused() {
    assert!(
        codes(&format!(
            "{NOTES}\naction unlock(nb: Notebook) (takes: [nb]) {{}}\n"
        ))
        .contains(&"E090".to_owned())
    );
}

#[test]
fn a_controls_key_is_kept_in_the_toolkits_one_spelling() {
    let c = compile_body(&modes(&["control+1"]));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm);
    assert!(
        strings.contains("Ctrl+1"),
        "the key is published as one spelling, whatever it was written as"
    );
}

#[test]
fn a_controls_key_the_desktop_keeps_is_refused() {
    let got = codes(&modes(&["F1"]));
    assert!(
        got.iter().any(|c| c == "E090"),
        "expected E090, got {got:?}"
    );
}

#[test]
fn a_controls_chord_that_is_no_shortcut_is_refused() {
    for wrong in ["Ctrl+", "Entr", "Ctrl+E+F"] {
        let got = codes(&modes(&[wrong]));
        assert!(
            got.iter().any(|c| c == "E090"),
            "expected E090 for {wrong:?}, got {got:?}"
        );
    }
}

#[test]
fn two_controls_of_one_view_cannot_answer_the_same_key() {
    // `Control+1` is `Ctrl+1`: two spellings of one press.
    let got = codes(&modes(&["Ctrl+1", "control+1"]));
    assert!(
        got.iter().any(|c| c == "E090"),
        "expected E090, got {got:?}"
    );
}

#[test]
fn a_control_cannot_answer_a_key_an_action_of_the_window_does() {
    let got = codes(&format!(
        "{NOTES}\naction share(nb: Notebook) (key: \"Ctrl+1\") {{}}\n{}",
        modes(&["Ctrl+1"])
    ));
    assert!(
        got.iter().any(|c| c == "E090"),
        "expected E090, got {got:?}"
    );
}

#[test]
fn a_control_may_show_the_key_of_the_action_its_press_runs() {
    // The menu row for an action names the action's own key; its press runs that action, so one
    // press still has one meaning.
    let body = "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\ndefault view notebook for Notebook\naction share(nb: Notebook) (key: \"Ctrl+1\") {\n}\nview notebook(nb: Notebook) {\n    ui.Button(\"Share\", key: \"Ctrl+1\") {\n        on press: share(nb)\n    }\n}\n";
    let got = codes(body);
    assert!(
        got.is_empty(),
        "the control shows the action's key: {got:?}"
    );
}

#[test]
fn a_control_may_compute_its_key_where_the_view_runs() {
    let body = "view v() {\n    state on: Bool = true\n    ui.Button(\"One\", key: if on { .some(\"Ctrl+1\") } else { .none }) {\n        on press: set(on, not on)\n    }\n}\n";
    let got = codes(body);
    assert!(
        got.is_empty(),
        "the window matches what it comes to: {got:?}"
    );
}

#[test]
fn two_arms_of_one_branch_may_answer_the_same_key() {
    // Only one arm is drawn, so the key answers one control.
    let body = "view v() {\n    state mode: Text = \"a\"\n    if mode == \"a\" {\n        ui.Button(\"Back\", key: \"Ctrl+1\") {\n            on press: set(mode, \"a\")\n        }\n    } else {\n        ui.Button(\"On\", key: \"Ctrl+1\") {\n            on press: set(mode, \"b\")\n        }\n    }\n}\n";
    let got = codes(body);
    assert!(got.is_empty(), "one arm is drawn: {got:?}");
}

#[test]
fn a_control_after_a_branch_cannot_answer_the_branchs_key() {
    let body = "view v() {\n    state mode: Text = \"a\"\n    if mode == \"a\" {\n        ui.Button(\"Back\", key: \"Ctrl+1\") {\n            on press: set(mode, \"a\")\n        }\n    }\n    ui.Button(\"On\", key: \"Ctrl+1\") {\n        on press: set(mode, \"b\")\n    }\n}\n";
    let got = codes(body);
    assert!(
        got.iter().any(|c| c == "E090"),
        "the branch may be drawn beside the control: {got:?}"
    );
}

#[test]
fn two_views_may_each_answer_the_same_key() {
    let body = "view a() {\n    state mode: Int = 0\n    ui.Button(\"One\", key: \"Ctrl+1\") {\n        on press: set(mode, 1)\n    }\n}\nview b() {\n    state mode: Int = 0\n    ui.Button(\"One\", key: \"Ctrl+1\") {\n        on press: set(mode, 1)\n    }\n}\n";
    let got = codes(body);
    assert!(got.is_empty(), "each view answers its own key: {got:?}");
}

#[test]
fn a_navigation_places_key_names_the_place_and_is_no_shortcut() {
    // `key` of a navigation place is what names it, a word like `all`: no shortcut is read
    // from it, so it is never refused as one.
    let view = "view v() {\n    state place: Text = \"all\"\n    ui.NavList(selected: place) {\n        ui.NavItem(\"all\", \"All contacts\") {\n            on select: set(place, \"all\")\n        }\n        ui.NavItem(\"F1\", \"Starred\") {\n            on select: set(place, \"F1\")\n        }\n    }\n}\n";
    let got = codes(view);
    assert!(
        got.is_empty(),
        "a place's key is its name, not a key: {got:?}"
    );
}
