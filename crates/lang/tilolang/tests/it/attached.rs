//! What a record is to the resource it was made from: a type declares `(attached)` when its
//! records are about what they were made from (a comment on a task) rather than work made from
//! it (a task made from a message), which `meta(r).attached` reads and `std.linked` leaves out.

use crate::common::{codes, compile_body};

const COMMENT: &str = "type Comment (attached) {\n    text: RichText (body)\n}\n";

#[test]
fn a_type_declares_that_its_records_are_about_what_they_were_made_from() {
    let compiled = compile_body(&format!(
        "{COMMENT}type Note {{\n    title: Text (title)\n}}\n"
    ));
    let attached = |name: &str| {
        compiled
            .meta
            .types
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("the type {name}"))
            .attached
    };
    assert!(attached("Comment"), "a comment is about its task");
    assert!(!attached("Note"), "a note is work of its own");

    // The canonical text says it, and reads back as itself.
    let printed = tilolang::print_module(
        &tilolang::parser::parse_module(&format!("app t.x \"X\"\n{COMMENT}"))
            .0
            .expect("it parses"),
    );
    assert!(printed.contains("type Comment (attached) {"), "{printed}");
    let again =
        tilolang::print_module(&tilolang::parser::parse_module(&printed).0.expect("parses"));
    assert_eq!(printed, again);
}

#[test]
fn a_record_says_whether_it_is_about_what_it_was_made_from() {
    let said = format!(
        "{COMMENT}type Note {{\n    title: Text (title)\n}}\naction about(n: Note) -> Bool {{\n    meta(n).attached\n}}"
    );
    assert!(
        codes(&said).is_empty(),
        "`meta(r).attached` reads: {:?}",
        codes(&said)
    );
}
