//! A set of texts is words system search finds: the tags on a photo, the objects and pets it
//! shows. They ride on the entry the resource already publishes, so Photos needs no search of
//! its own for what its pictures hold.

use tilolang_runtime::Actor;
use tilolang_runtime::value::Value;
use tilolang_tilo::host::Host;

const SOURCE: &str = r#"
app test.tags "Tags"

/// A named thing with words kept as a set beside its name.
type Note (title: name) {
    name: Text
    tags: Set<Text>
}

root Note

default view note for Note

view note(n: Note) {
    text(n.name)
}

/// Names the note and gives it the tags it holds.
action make(n: Note) {
    n.name = "Harbour"
    n.tags.add("lighthouse")
    n.tags.add("gull")
}
"#;

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

#[test]
fn the_members_of_a_text_set_are_found_with_the_resource() {
    let host = Host::new(me(), || None).expect("a host");
    host.set_clock(|| "2026-01-02T03:04:05Z".to_owned());
    let package = host
        .load_builtin(SOURCE, "")
        .unwrap_or_else(|error| panic!("{error}"));
    let note = host.runtime().create_root("Note").expect("the root note");
    let make = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == "make")
        .expect("the make action")
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(&me(), (make.as_str(), vec![Value::resource(&note)]))
        .expect("the note is made")
    {
        tilolang_runtime::InvokeOutcome::Done(_) => {}
        other => panic!("make answered {other:?}"),
    }
    let entries = tilolang_tilo::search::entries(&host);
    let entry = entries
        .iter()
        .find(|entry| entry.title == "Harbour")
        .unwrap_or_else(|| panic!("the note's entry: {entries:?}"));
    let words: Vec<&str> = entry.fields.iter().map(|(word, _)| word.as_str()).collect();
    assert!(
        words.contains(&"lighthouse") && words.contains(&"gull"),
        "the set's members are words of the resource: {words:?}"
    );
}
