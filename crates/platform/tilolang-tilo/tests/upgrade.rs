//! Whether a new build takes over a running package in place: matched by declaration id, so a
//! rename keeps state and a changed type does not.

use tilolang::IdsLock;
use tilolang_runtime::meta::Meta;
use tilolang_tilo::upgrade::{Upgrade, compare};

const V1: &str = r#"app so.tilo.dev "Dev"

enum Mood { calm, busy }

type Note {
  title: Text merge text
  done: Bool
  mood: Mood
}

root Note

default view main for Note

view main(n: Note) {
  lens open: Bool = false
  page { input(bind: n.title, placeholder: "Title") }
}
"#;

/// Compile `first`, then `second` against the ids `first` locked: two builds of one package.
fn builds(first: &str, second: &str) -> (Meta, Meta) {
    let one = tilolang::compile_package(first, &IdsLock::new(), &[]).expect("v1 compiles");
    let two = tilolang::compile_package(second, &one.ids, &[]).expect("v2 compiles");
    (one.meta, two.meta)
}

fn reason(first: &str, second: &str) -> String {
    let (old, new) = builds(first, second);
    match compare(&old, &new) {
        Upgrade::Restart(reason) => reason,
        Upgrade::InPlace => panic!("expected a restart"),
    }
}

#[test]
fn a_change_of_code_only_takes_over_in_place() {
    let v2 = V1.replace("placeholder: \"Title\"", "placeholder: \"Name\"");
    let (old, new) = builds(V1, &v2);
    assert_eq!(compare(&old, &new), Upgrade::InPlace);
}

#[test]
fn added_and_removed_declarations_take_over_in_place() {
    let v2 = V1
        .replace("  done: Bool\n", "  pinned: Bool\n")
        .replace("enum Mood { calm, busy }", "enum Mood { calm, busy, away }");
    let (old, new) = builds(V1, &v2);
    assert_eq!(compare(&old, &new), Upgrade::InPlace);
}

#[test]
fn a_field_whose_type_changed_restarts_and_says_which() {
    let v2 = V1.replace("  done: Bool\n", "  done: Int\n");
    assert_eq!(
        reason(V1, &v2),
        "type Note: the field `done` changed from Bool to Int"
    );
}

#[test]
fn a_removed_case_restarts() {
    let v2 = V1.replace("enum Mood { calm, busy }", "enum Mood { calm }");
    assert_eq!(reason(V1, &v2), "enum Mood: the case `busy` was removed");
}

#[test]
fn view_state_whose_type_changed_restarts() {
    let v2 = V1.replace("lens open: Bool = false", "lens open: Int = 0");
    assert!(reason(V1, &v2).contains("`open` changed from lens Bool to lens Int"));
}

#[test]
fn a_new_name_restarts() {
    let v2 = V1.replace("\"Dev\"", "\"Develop\"");
    assert_eq!(
        reason(V1, &v2),
        "the app's name changed from \"Dev\" to \"Develop\""
    );
}
