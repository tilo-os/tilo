//! components.md, "A component is a described widget": the toolkit's components, read as the
//! package `ui`, checked against the descriptions the toolkit generates from each widget.

use crate::common::{compile_body, validate};

/// The diagnostics of a module body, as (code, message).
fn errors(body: &str) -> Vec<(String, String)> {
    let src = format!("app t.x \"X\"\n{body}\n");
    match crate::common::compile_with(&src, &[]) {
        Ok(_) => Vec::new(),
        Err(d) => d.into_iter().map(|d| (d.code, d.message)).collect(),
    }
}

/// The one error of `body`: its code and its message.
fn one_error(body: &str) -> (String, String) {
    let got = errors(body);
    assert_eq!(got.len(), 1, "one error for:\n{body}\ngot {got:#?}");
    got.into_iter().next().unwrap()
}

const NOTE: &str = r"type Note {
    title: Text (title)
    body: Text (body)
    pinned: Bool = false
    done: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

action open_note(n: Note) {
    n.pinned = n.pinned
}
";

fn with_note(view: &str) -> String {
    format!("{NOTE}\n{view}")
}

#[test]
fn a_component_call_compiles_to_its_node_with_props_slots_and_events() {
    let c = compile_body(&with_note(
        r#"view row_of(n: Note) {
    ui.ListRow(title: n.title, secondary: n.body, mark: if n.pinned { .pin }) {
        avatar: { ui.Avatar(name: n.title, glyph: .users) }
        trailing: { ui.Text("now", role: .caption) }
        on activate: open_note(n)
    }
}"#,
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    for want in [
        "ui.ListRow",
        "ui.Avatar",
        "ui.Text",
        "slot",
        "trailing",
        "activate",
        "caption",
    ] {
        assert!(strings.contains(want), "{want} reaches the host");
    }
}

#[test]
fn positional_props_follow_the_description() {
    let c = compile_body(&with_note(
        r#"view title_of(n: Note) {
    ui.Section("About") {
        ui.Text(n.title, role: .title)
        ui.PropertyList {
            ui.Property("Title", n.title)
        }
    }
}"#,
    ));
    validate(&c.wasm);
}

#[test]
fn a_prop_of_the_wrong_type_is_named_with_the_widgets_doc_text() {
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title, secondary: 3)\n}",
    ));
    assert_eq!(code, "E020");
    assert!(
        message.contains("`ui.ListRow`'s `secondary` is Option<Text>"),
        "{message}"
    );
    assert!(
        message.contains("the start of the item's content"),
        "the prop's doc text: {message}"
    );
    assert!(message.contains("found Int"), "{message}");
}

#[test]
fn a_missing_required_prop_is_named_with_its_doc_text() {
    let (code, message) = one_error("view v() {\n    ui.EmptyState(icon: .plus)\n}");
    assert_eq!(code, "E022");
    assert!(
        message.contains("`ui.EmptyState` needs `title`"),
        "{message}"
    );
    assert!(message.contains("one short sentence"), "{message}");
}

#[test]
fn an_unknown_argument_lists_the_components_arguments() {
    let (code, message) = one_error("view v() {\n    ui.Text(\"x\", colour: \"red\")\n}");
    assert_eq!(code, "E063");
    assert!(message.contains("has no argument `colour`"), "{message}");
    assert!(message.contains("value, role, numeric"), "{message}");
}

#[test]
fn an_event_given_as_an_argument_is_pointed_to_the_block() {
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title, activate: open_note(n))\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("on activate:"), "{message}");
}

#[test]
fn a_case_outside_the_design_systems_roles_is_refused() {
    let (code, message) = one_error("view v() {\n    ui.Text(\"x\", role: .shouty)\n}");
    assert_eq!(code, "E065");
    assert!(
        message.contains("TextRole has no case `.shouty`"),
        "{message}"
    );
    assert!(
        message.contains(".caption"),
        "the cases are listed: {message}"
    );
}

#[test]
fn an_unknown_component_slot_or_event_is_refused() {
    let (code, message) = one_error("view v() {\n    ui.Gauge(3)\n}");
    assert_eq!(code, "E060");
    assert!(
        message.contains("ListRow"),
        "the components are listed: {message}"
    );
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        footer: { ui.Text(\"x\") }\n    }\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("no slot `footer`"), "{message}");
    assert!(
        message.contains("`trailing`"),
        "the slots are listed: {message}"
    );
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        on swipe: open_note(n)\n    }\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("no event `swipe`"), "{message}");
}

#[test]
fn a_slot_that_accepts_one_component_refuses_others() {
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        avatar: { ui.Text(n.title) }\n    }\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("holds `ui.Avatar` only"), "{message}");
    let (code, _) =
        one_error("view v() {\n    ui.PropertyList {\n        ui.Text(\"not a fact\")\n    }\n}");
    assert_eq!(code, "E063");
}

#[test]
fn a_component_without_content_of_its_own_takes_no_block_content() {
    let (code, message) =
        one_error("view v() {\n    ui.Text(\"x\") {\n        ui.Text(\"y\")\n    }\n}");
    assert_eq!(code, "E063");
    assert!(message.contains("holds no content of its own"), "{message}");
}

#[test]
fn a_binding_binds_a_field_of_its_type() {
    let c = compile_body(&with_note(
        "view v(n: Note) {\n    ui.TextInput(value: n.title, placeholder: \"Title\")\n    ui.ListRow(title: n.title, checkbox: n.done)\n}",
    ));
    validate(&c.wasm);
    let (code, _) = one_error(&with_note(
        "view v(n: Note) {\n    ui.TextInput(value: n.done)\n}",
    ));
    assert_eq!(code, "E062");
    let (code, _) = one_error(&with_note(
        "view v(n: Note) {\n    ui.TextInput(value: \"fixed\")\n}",
    ));
    assert_eq!(code, "E062");
}

#[test]
fn a_row_view_may_be_the_toolkits_row_itself() {
    let c = compile_body(&with_note(
        "row view note_row for Note\n\nview note_row(n: Note) {\n    ui.ListRow(title: n.title, secondary: n.body)\n}",
    ));
    validate(&c.wasm);
}

#[test]
fn slots_and_events_print_back_as_written() {
    let src = "app t.x \"X\"\n\naction go() {\n}\n\nview v() {\n    ui.Card(title: \"Today\") {\n        ui.Text(\"Three things\")\n        on activate: go()\n    }\n    ui.ListRow(title: \"Row\") {\n        trailing: {\n            ui.Text(\"now\")\n        }\n    }\n}\n";
    let module = tilolang::parse(src).expect("parses");
    let printed = tilolang::print_module(&module);
    assert!(printed.contains("on activate: go()"), "{printed}");
    assert!(printed.contains("trailing: {"), "{printed}");
    let again = tilolang::parse(&printed).expect("the printed module parses");
    assert_eq!(
        tilolang::print_module(&again),
        printed,
        "printing is stable"
    );
}
