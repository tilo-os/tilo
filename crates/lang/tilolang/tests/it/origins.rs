//! Types whose resources an outside service holds (`type Message (origin: imap.message)`):
//! the kind's fields with the kind's types, people's fields written by people, the service's by
//! the service alone, and outside effects handed to a capability the platform has.

use crate::common::{codes, compile_body};

const MESSAGE: &str = "type Message (origin: imap.message) {\n    subject: Text\n    unread: Bool\n    mailbox: Text\n    note: Text\n}\n";

#[test]
fn a_type_names_an_origins_kind_and_the_metadata_carries_it() {
    let compiled = compile_body(MESSAGE);
    let ty = compiled
        .meta
        .types
        .iter()
        .find(|t| t.name == "Message")
        .expect("the type");
    assert_eq!(ty.origin.as_deref(), Some("imap.message"));
    let (module, _) = tilolang::parser::parse_module(&format!("app t.x \"X\"\n{MESSAGE}"));
    let printed = tilolang::print::print_module(&module.expect("it parses"));
    assert!(
        printed.contains("type Message (origin: imap.message) {"),
        "{printed}"
    );
}

#[test]
fn a_kind_the_platform_lacks_or_a_field_of_another_type_is_refused() {
    assert_eq!(
        codes("type M (origin: imap.letter) {\n    subject: Text\n}"),
        ["E112"]
    );
    assert_eq!(
        codes("type M (origin: imap.message) {\n    unread: Text\n}"),
        ["E112"]
    );
}

#[test]
fn people_change_their_fields_and_only_the_service_writes_its_own() {
    let people = format!(
        "{MESSAGE}\naction read(m: Message, unread: Bool = not m.unread) {{\n    m.unread = unread\n    m.note = \"seen\"\n}}"
    );
    assert!(codes(&people).is_empty(), "{:?}", codes(&people));
    let service =
        format!("{MESSAGE}\naction retitle(m: Message) {{\n    m.subject = \"Mine now\"\n}}");
    assert_eq!(codes(&service), ["E113"]);
}

#[test]
fn an_outside_effect_goes_to_a_capability_the_platform_has() {
    let ok =
        "type D {\n    to: Text\n}\naction go(d: D) {\n    outside(\"smtp.send\", (d.to, d.to))\n}";
    assert!(codes(ok).is_empty(), "{:?}", codes(ok));
    let unknown = "type D {\n    to: Text\n}\naction go(d: D) {\n    outside(\"fax\", d.to)\n}";
    assert_eq!(codes(unknown), ["E022"]);
}

/// The session takes no outside effect: its actions are called as an app's
/// (`session.focus(window)`), so a string naming one of them is no capability.
#[test]
fn the_session_takes_no_outside_effect() {
    for capability in [
        "session.run",
        "session.act",
        "session.notify",
        "session.arrange",
    ] {
        let src = format!(
            "type D {{\n    to: Text\n}}\naction go(d: D) {{\n    outside(\"{capability}\", d.to)\n}}"
        );
        assert_eq!(codes(&src), ["E022"], "{capability}");
    }
}

/// An outside effect is queued on the action's resource: an action that takes none is refused
/// where it is written, not trapped at its first press.
#[test]
fn an_outside_effect_needs_a_resource_to_queue_on() {
    let none = "action go(key: Text) {\n    outside(\"post\", key)\n}";
    assert_eq!(codes(none), ["E047"]);
    let some =
        "type D {\n    to: Text\n}\naction go(d: D, key: Text) {\n    outside(\"post\", key)\n}";
    assert!(codes(some).is_empty(), "{:?}", codes(some));
}

#[test]
fn a_named_filter_tests_a_set_holding_a_value() {
    let src = "type T {\n    boxes: Set<Text>\n    search {\n        filter \"Inbox\" = boxes.contains(\"inbox\")\n    }\n}";
    let compiled = compile_body(src);
    let ty = compiled.meta.types.iter().find(|t| t.name == "T").unwrap();
    let inbox = ty
        .search
        .filters
        .iter()
        .find(|f| f.label == "Inbox")
        .unwrap();
    assert!(inbox.pred.is_some(), "{inbox:?}");
}

/// The capability an action's `outside(...)` calls name is what its metadata carries, so the
/// platform tags the action with it (`outside:youtube.search`) and a query naming a kind of an
/// outside service finds the app's own action that asks its origin.
#[test]
fn an_actions_outside_capability_is_its_metadatas() {
    let src = "type Video (origin: youtube.video) {\n    key: Text\n    title: Text\n}\n\
        action search(v: Video, query: Text) {\n    outside(\"youtube.search\", query)\n}";
    let compiled = compile_body(src);
    let action = compiled
        .meta
        .actions
        .iter()
        .find(|a| a.name == "search")
        .expect("the search action");
    assert_eq!(action.outside, vec!["youtube.search".to_owned()]);
    let plain = compile_body(
        "type N {\n    title: Text\n}\naction rename(n: N) {\n    n.title = \"Mine\"\n}",
    );
    let rename = plain
        .meta
        .actions
        .iter()
        .find(|a| a.name == "rename")
        .unwrap();
    assert!(rename.outside.is_empty(), "{rename:?}");
}
