//! A type says once which field names its records (`type Draft (title: subject)`), and once
//! what one is called while that field is empty (`(untitled: "New message")`): every surface
//! that names one reads that field and falls back to those words, and the field's input shows
//! them as its placeholder.

use crate::common::{codes, compile_body, compile_with, validate};

const NOTE: &str = r#"type Note (untitled: "Untitled") {
    title: Text
    body: Text
}
"#;

const DRAFT: &str = r#"type Draft (title: subject, untitled: "New message") {
    from: Text
    to: Text
    subject: Text
    body: Text
}
"#;

/// The type `Note` as the package describes it.
fn note(compiled: &tilolang::Compiled) -> tilolang::meta::TypeMeta {
    compiled
        .meta
        .types
        .iter()
        .find(|t| t.name == "Note")
        .expect("the type")
        .clone()
}

/// The type `Draft` as the package describes it.
fn draft(compiled: &tilolang::Compiled) -> tilolang::meta::TypeMeta {
    compiled
        .meta
        .types
        .iter()
        .find(|t| t.name == "Draft")
        .expect("the type")
        .clone()
}

#[test]
fn the_type_says_which_field_names_its_records() {
    let compiled = compile_body(DRAFT);
    assert_eq!(draft(&compiled).title.as_deref(), Some("subject"));
    assert_eq!(draft(&compiled).untitled.as_deref(), Some("New message"));
    let (module, _) = tilolang::parser::parse_module(&format!("app t.x \"X\"\n{DRAFT}"));
    let printed = tilolang::print::print_module(&module.expect("it parses"));
    assert!(
        printed.contains(r#"type Draft (title: subject, untitled: "New message") {"#),
        "{printed}"
    );
}

#[test]
fn a_type_that_says_nothing_is_named_by_its_first_text_field() {
    let compiled = compile_body(NOTE);
    assert_eq!(note(&compiled).title, None);
    assert_eq!(
        note(&compiled).fields.first().map(|f| f.name.as_str()),
        Some("title")
    );
}

#[test]
fn a_type_must_name_a_text_field_it_has() {
    assert_eq!(
        codes("type Note (title: titel) {\n    title: Text\n}"),
        ["E140"]
    );
    assert_eq!(
        codes("type Note (title: done) {\n    title: Text\n    done: Bool\n}"),
        ["E140"]
    );
}

#[test]
fn a_register_of_text_is_never_what_a_record_is_called() {
    assert_eq!(
        codes("type Note (title: key) {\n    key: Text merge register\n    title: Text\n}"),
        ["E140"]
    );
}

#[test]
fn a_title_no_field_can_fill_is_erroring_once() {
    assert_eq!(
        codes("type Note (title: done, untitled: \"Note\") {\n    title: Text\n    done: Bool\n}"),
        ["E140"]
    );
}

#[test]
fn a_mistyped_title_names_the_field_it_meant() {
    let err = compile_with(
        "app t.x \"X\"\ntype Note (title: titel) {\n    title: Text\n}\n",
        &[],
    )
    .expect_err("no such field");
    let message = err.first().map(|d| d.message.as_str()).unwrap_or_default();
    assert!(message.contains("did you mean `title`?"), "{message}");
}

#[test]
fn the_words_reach_the_metadata_and_the_source_prints_them() {
    let compiled = compile_body(NOTE);
    assert_eq!(note(&compiled).untitled.as_deref(), Some("Untitled"));
    let (module, _) = tilolang::parser::parse_module(&format!("app t.x \"X\"\n{NOTE}"));
    let printed = tilolang::print::print_module(&module.expect("it parses"));
    assert!(
        printed.contains("type Note (untitled: \"Untitled\") {"),
        "{printed}"
    );
}

#[test]
fn the_catalogue_translates_the_words_with_the_rest() {
    let compiled = compile_body(NOTE);
    let catalogue = tilolang::messages::source_catalogue(&compiled.meta);
    assert!(catalogue.contains(".untitled = Untitled"), "{catalogue}");
}

#[test]
fn the_title_fields_input_placeholder_is_those_words() {
    let compiled = compile_body(&format!(
        "{NOTE}\nview note_editor(n: Note) {{\n    ui.TextInput(bind: n.title, title: true)\n}}\n"
    ));
    validate(&compiled.wasm);
    let strings = String::from_utf8_lossy(&compiled.wasm).into_owned();
    assert!(
        strings.contains(".untitled"),
        "the placeholder is the type's untitled word"
    );
}

#[test]
fn a_control_that_edits_the_title_takes_the_placeholder_without_drawing_it_large() {
    let compiled = compile_body(&format!(
        "{NOTE}\nview note_row(n: Note) {{\n    ui.TextInput(bind: n.title)\n}}\n"
    ));
    validate(&compiled.wasm);
    assert!(
        String::from_utf8_lossy(&compiled.wasm).contains(".untitled"),
        "the field prompts with what the type says"
    );
}

#[test]
fn the_words_are_required_and_need_a_title_field_to_fall_back_for() {
    assert_eq!(
        codes("type Note (untitled: \"  \") {\n    title: Text\n}"),
        ["E139"]
    );
    assert_eq!(
        codes("type Note (untitled: \"Untitled\") {\n    done: Bool\n}"),
        ["E139"]
    );
}

#[test]
fn an_unknown_option_is_refused() {
    assert_eq!(
        codes("type Note (untitel: \"Untitled\") {\n    title: Text\n}"),
        ["E003"]
    );
}
