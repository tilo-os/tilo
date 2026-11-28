//! History says a field's value as the change found it and as it left it (`{limit.before}`,
//! `{limit.after}`), each to the readers who read the record then: one a change let in learns
//! nothing of what it held before, a secret field's value is said to no one, and a record kept on
//! one device has its history there alone.

use tilolang_runtime::{ResourceRef, Value};
use tilolang_tilo::host::Package;

use crate::fixtures::{Replica, Workspace, action_id, person};

const LOCKER: &str = r#"app test.locker "Locker"

type Locker {
  boxes: Collection<Box>
  scraps: Collection<Scrap>
}

/// A box only the people it names open.
type Box (readers: people) {
  name: Text
  people: Set<Actor>
  limit: Int = 3
  code: Text (secret) = ""
}

/// A note kept on the device it was written on.
type Scrap (readers: author here) {
  words: Text
}

root Locker

default view locker for Locker

view locker(l: Locker) {
  std.page(l) {
    for b in l.boxes {
      ui.Text(b.name)
    }
  }
}

action make(l: Locker, name: Text, owner: Actor) -> Box {
  let made = l.boxes.insert(Box(name: name))
  made.people.add(owner)
  return made
}

action set_limit(b: Box, to: Int) (
  label: "Set the limit",
  did: "set the limit from {limit.before} to {limit.after} in",
) {
  b.limit = to
}

action let_in(b: Box, guest: Actor, called: Text) (
  label: "Let someone in",
  did: "let someone in and named the box {name.after} in",
) {
  b.people.add(guest)
  b.name = called
}

action set_code(b: Box, code: Text (secret)) (label: "Set the code") {
  b.code = code
}

action jot(l: Locker, words: Text) -> Scrap {
  return l.scraps.insert(Scrap(words: words))
}

action rewrite(s: Scrap, text: Text) (label: "Rewrite", did: "rewrote {words.before} as {words.after} in") {
  s.words = text
}
"#;

const APP: &str = "test.locker";

/// Ada's device and Bo's, both members of Ada's workspace, with the locker on each.
fn devices() -> (Replica, Replica, Package) {
    let (ada, mut packages) = Replica::new("ada", Workspace::new(), &[(LOCKER, "")]);
    let package = packages.remove(0);
    let bo = Replica::new("bo", Workspace::new(), &[(LOCKER, &package.ids)]).0;
    for replica in [&ada, &bo] {
        crate::fixtures::stage_members(
            &replica.host,
            vec![
                tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
                tilolang_runtime::members::Member::person("bo", tilo_actions::policy::Role::Editor),
            ],
            None,
        );
    }
    ada.send_to(&bo);
    (ada, bo, package)
}

/// Ada runs `action`, and what she wrote reaches Bo's device.
fn act(
    ada: &Replica,
    bo: &Replica,
    package: &Package,
    action: &str,
    args: Vec<Value>,
) -> Option<ResourceRef> {
    let outcome = ada
        .host
        .runtime()
        .invoke_action(
            &person("ada"),
            (action_id(&package.meta, action).as_str(), args),
        )
        .unwrap();
    ada.commit();
    ada.send_to(bo);
    match outcome {
        tilolang_runtime::InvokeOutcome::Done(Value::Ref(made)) => Some((*made.target).clone()),
        _ => None,
    }
}

/// What `reader` reads on `replica` of each version of `at`, oldest first: the entry History
/// lists, and the sentence it says it with in English.
fn history(
    replica: &Replica,
    package: &Package,
    reader: &str,
    at: &ResourceRef,
) -> Vec<(serde_json::Value, String)> {
    let reader = person(reader);
    let source = tilolang::messages::source_catalogue(&package.meta);
    let words = tilolang_tilo::versions::Catalogue::new("en-US", &[source.as_str()]);
    let versions = replica
        .host
        .runtime()
        .versions(at)
        .expect("the store reads its versions");
    let mut said = serde_json::json!({
        "versions": versions
            .iter()
            .map(|version| {
                tilolang_tilo::versions::version_entry(&replica.host, APP, version, Some(&reader))
            })
            .collect::<Vec<_>>(),
    });
    tilolang_tilo::versions::say_versions(&mut said, &[], &words, "en-US");
    said["versions"]
        .as_array()
        .expect("the versions are listed")
        .iter()
        .map(|entry| {
            let sentence = entry["sentence"].as_str().unwrap_or_default().to_owned();
            (entry["changes"].clone(), sentence)
        })
        .collect()
}

#[test]
fn a_reader_of_both_versions_sees_both_values_and_one_let_in_only_what_it_became() {
    let (ada, bo, package) = devices();
    let locker = Value::resource(&ada.root(APP));
    let spare = act(
        &ada,
        &bo,
        &package,
        "make",
        vec![
            locker,
            Value::text("Spare keys"),
            Value::Actor(person("ada")),
        ],
    )
    .expect("a box is made");
    act(
        &ada,
        &bo,
        &package,
        "set_limit",
        vec![Value::resource(&spare), Value::Int(5)],
    );

    // Ada read the box all along: both values of the change, in the words the action declares.
    let hers = history(&ada, &package, "ada", &spare);
    let (limited, set) = hers.last().expect("the box has its changes");
    assert_eq!(
        *limited,
        serde_json::json!([{ "field": "limit", "before": 3, "after": 5 }])
    );
    assert!(set.starts_with("Set the limit from 3 to 5 in"), "{set}");

    act(
        &ada,
        &bo,
        &package,
        "let_in",
        vec![
            Value::resource(&spare),
            Value::Actor(person("bo")),
            Value::text("Bo's keys"),
        ],
    );

    // Bo, let in by that change, reads on his own device what it left, never what the box held
    // before he could read it.
    let his = history(&bo, &package, "bo", &spare);
    let (shared, let_in) = his.last().expect("Bo reads the change that let him in");
    let fields = shared.as_array().expect("the change lists its fields");
    assert!(
        fields.contains(&serde_json::json!({ "field": "name", "after": "Bo's keys" })),
        "{shared}"
    );
    assert!(
        fields.iter().all(|field| field.get("before").is_none()),
        "{shared}"
    );
    assert!(
        let_in.starts_with("Let someone in and named the box Bo's keys in"),
        "{let_in}"
    );
    assert!(!let_in.contains("Spare"), "{let_in}");
}

#[test]
fn a_secret_fields_value_is_said_to_no_one() {
    let (ada, bo, package) = devices();
    let locker = Value::resource(&ada.root(APP));
    let spare = act(
        &ada,
        &bo,
        &package,
        "make",
        vec![
            locker,
            Value::text("Spare keys"),
            Value::Actor(person("ada")),
        ],
    )
    .expect("a box is made");
    act(
        &ada,
        &bo,
        &package,
        "let_in",
        vec![
            Value::resource(&spare),
            Value::Actor(person("bo")),
            Value::text("Spare keys"),
        ],
    );
    act(
        &ada,
        &bo,
        &package,
        "set_code",
        vec![Value::resource(&spare), Value::text("4711")],
    );
    for (replica, reader) in [(&ada, "ada"), (&bo, "bo")] {
        let read = history(replica, &package, reader, &spare);
        let (changes, said) = read.last().expect("the box has its changes");
        assert_eq!(
            *changes,
            serde_json::json!([{ "field": "code", "secret": true }]),
            "{reader}"
        );
        assert!(said.starts_with("Changed Code of"), "{reader}: {said}");
        assert!(!said.contains("4711"), "{reader}: {said}");
    }
}

#[test]
fn a_record_kept_on_one_device_keeps_its_history_there_alone() {
    let (ada, mut packages) = Replica::new("ada", Workspace::new(), &[(LOCKER, "")]);
    let package = packages.remove(0);
    let laptop = Replica::new("ada", Workspace::new(), &[(LOCKER, &package.ids)]).0;
    let locker = Value::resource(&ada.root(APP));
    let scrap = act(
        &ada,
        &laptop,
        &package,
        "jot",
        vec![locker, Value::text("milk")],
    )
    .expect("a scrap is jotted");
    act(
        &ada,
        &laptop,
        &package,
        "rewrite",
        vec![Value::resource(&scrap), Value::text("oat milk")],
    );
    let here = history(&ada, &package, "ada", &scrap);
    let (_, said) = here.last().expect("the scrap has its changes here");
    assert!(said.starts_with("Rewrote milk as oat milk in"), "{said}");
    // Her other device was handed nothing of it: no version, so no value of one.
    let there = laptop.host.runtime().versions(&scrap);
    assert!(
        there.as_ref().map_or(true, Vec::is_empty),
        "the scrap's history stays on the device it was written on: {there:?}"
    );
}

#[test]
fn history_follows_a_box_its_readers_moved_and_each_reader_reads_what_they_could() {
    let (ada, bo, package) = devices();
    let locker = Value::resource(&ada.root(APP));
    let spare = act(
        &ada,
        &bo,
        &package,
        "make",
        vec![
            locker,
            Value::text("Spare keys"),
            Value::Actor(person("ada")),
        ],
    )
    .expect("a box is made");
    act(
        &ada,
        &bo,
        &package,
        "set_limit",
        vec![Value::resource(&spare), Value::Int(5)],
    );
    // Letting Bo in changes who reads the box, which moves it to the document they read.
    act(
        &ada,
        &bo,
        &package,
        "let_in",
        vec![
            Value::resource(&spare),
            Value::Actor(person("bo")),
            Value::text("Bo's keys"),
        ],
    );

    // Ada read the box all along: its whole history, the move one step of it, with both values.
    let hers = history(&ada, &package, "ada", &spare);
    let said: Vec<&str> = hers.iter().map(|(_, sentence)| sentence.as_str()).collect();
    let [.., (limited, set), (shared, let_in)] = hers.as_slice() else {
        panic!("the box keeps its history across the move: {said:#?}");
    };
    assert!(set.starts_with("Set the limit from 3 to 5 in"), "{said:#?}");
    assert_eq!(
        *limited,
        serde_json::json!([{ "field": "limit", "before": 3, "after": 5 }])
    );
    assert!(
        let_in.starts_with("Let someone in and named the box Bo's keys in"),
        "{said:#?}"
    );
    let fields = shared.as_array().expect("the move lists its fields");
    assert!(
        fields.contains(
            &serde_json::json!({ "field": "name", "before": "Spare keys", "after": "Bo's keys" })
        ),
        "{shared}"
    );
    assert_eq!(
        said.len(),
        3,
        "made, its limit set and the move, which is no removal and second making: {said:#?}"
    );
    assert!(
        !said.iter().any(|sentence| sentence.starts_with("Deleted")),
        "{said:#?}"
    );

    // Bo reads it from the change that let him in, never what came before it.
    let his = history(&bo, &package, "bo", &spare);
    let heard: Vec<&str> = his.iter().map(|(_, sentence)| sentence.as_str()).collect();
    assert!(
        heard
            .first()
            .is_some_and(|first| first.starts_with("Let someone in")),
        "{heard:#?}"
    );
    assert!(
        !heard.iter().any(|sentence| sentence.contains("limit")),
        "{heard:#?}"
    );

    // Ada takes it back: the box returns to her alone, and its history to where it was kept.
    assert!(ada.undo_last(), "the move is taken back");
    ada.commit();
    ada.send_to(&bo);
    assert_eq!(ada.read(&spare, "name"), Value::text("Spare keys"));
    let hers = history(&ada, &package, "ada", &spare);
    let said: Vec<&str> = hers.iter().map(|(_, sentence)| sentence.as_str()).collect();
    assert!(
        said.iter()
            .any(|sentence| sentence.starts_with("Set the limit from 3 to 5 in"))
            && said
                .iter()
                .any(|sentence| sentence.contains("Let someone in")),
        "undone, the box keeps every step it took: {said:#?}"
    );
    assert_eq!(said.len(), 4, "and its undo is one step: {said:#?}");
    assert_eq!(
        bo.read(&bo.root(APP), "boxes"),
        Value::List(Vec::new()),
        "Bo reads the box no more once the change that let him in is taken back"
    );
}
