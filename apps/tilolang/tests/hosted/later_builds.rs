//! A later build of the package, as `tilolang dev` hands one to the window it runs when the
//! source is saved: the instance takes it over in its window while it runs.

use super::packages::{later, package_of};
use super::{NOTES, Session, press_new_note};

/// Notes with two notes made as a person makes them, the second one open.
fn two_notes() -> Session {
    let mut session = Session::notes();
    for title in ["Home", "Plan"] {
        press_new_note(&mut session);
        session.settle();
        session.window.type_text(title);
        session.settle();
    }
    session
}

/// `signature`'s parameter list with its last parameter — the tag a tag action takes — made
/// `Int`, whatever the source calls that parameter.
fn tag_takes_int(source: &str, signature: &str) -> String {
    let (head, rest) = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("{signature} is in the notes package"));
    let (params, tail) = rest.split_once(')').expect("the parameters end");
    let (before, after) = params.rsplit_once(": Text").expect("the tag is Text");
    format!("{head}{signature}{before}: Int{after}){tail}")
}

#[test]
fn a_later_build_takes_over_in_place_keeping_the_open_note_and_the_list() {
    let mut session = two_notes();
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(titles, ["Home", "Plan"]);
    assert_eq!(session.field_value("title"), Some(text("Plan")));
    assert!(
        session
            .shown()
            .iter()
            .any(|shown| shown == "Write something")
    );

    tilolang_app::updates::hand_over(later(package_of(NOTES), |source| {
        source.replace("\"Write something\"", "\"Write anything\"")
    }));
    session.settle();

    let shown = session.shown();
    assert!(
        shown.iter().any(|shown| shown == "Write anything"),
        "the new build draws the page: {shown:?}"
    );
    assert_eq!(
        session.field_value("title"),
        Some(text("Plan")),
        "the same note is still open"
    );
    let mut after = session.titles();
    after.sort();
    assert_eq!(after, titles, "and the notebook still lists every note");
}

#[test]
fn a_later_build_that_changes_a_field_type_starts_over_on_the_same_data() {
    let mut session = two_notes();
    assert_eq!(session.field_value("title"), Some(text("Plan")));

    tilolang_app::updates::hand_over(later(package_of(NOTES), |source| {
        let source = ["action tag(", "action untag(", "action file_note("]
            .iter()
            .fold(
                source.replace("tags: Set<Text>", "tags: Set<Int>"),
                |source, signature| tag_takes_int(&source, signature),
            );
        source
            .replace("ui.Chip(t)", "ui.Chip(\"{t}\")")
            .replace("tags.add(\"planning\")", "tags.add(0)")
    }));
    session.settle();

    let note = session
        .instance_host()
        .package(NOTES)
        .and_then(|package| package.meta.types.into_iter().find(|ty| ty.name == "Note"))
        .unwrap();
    assert!(
        note.fields.iter().any(|field| field.ty == "Set<Int>"),
        "the new build runs: {note:?}"
    );
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(titles, ["Home", "Plan"], "every note is still there");
    assert_eq!(
        session.field_value("title"),
        Some(text("Home")),
        "the window started over: it opens on the notebook's first note, not the one open before"
    );
}

fn text(text: &str) -> tilo_ui_host::Value {
    tilo_ui_host::Value::Text(text.to_owned())
}
