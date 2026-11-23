//! An action's key: the shortcut that runs it, in the toolkit's one spelling, and what the
//! compiler refuses.

use crate::common::{codes, compile_body};

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
