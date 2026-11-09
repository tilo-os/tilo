//! A type says once what one of its records is called while its title is empty
//! (`type Note (untitled: "Untitled")`): every surface that names one falls back to those
//! words, and the title field's input shows them as its placeholder.

use crate::common::{codes, compile_body, validate};

const NOTE: &str = r#"type Note (untitled: "Untitled") {
    title: Text
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
