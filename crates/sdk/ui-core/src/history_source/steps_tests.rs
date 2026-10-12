use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc};

use super::*;

/// No step has an inverse action: every one is undone by its patches.
#[allow(clippy::unnecessary_wraps, reason = "the shape every compensator has")]
fn plain(_: &Version) -> Result<Option<Inverse>, String> {
    Ok(None)
}

const GENESIS: &[u8] = b"genesis";
const ALICE: &[u8] = b"alice";
const BOB: &[u8] = b"bob";

fn scope(actor: &[u8]) -> Scope {
    Scope::Actor(ActorId::from(actor).to_hex_string())
}

fn marks(doc: &mut AutoCommit) -> ObjId {
    match doc.get(ROOT, "marks").unwrap() {
        Some((automerge::Value::Object(ObjType::Map), id)) => id,
        _ => doc.put_object(ROOT, "marks", ObjType::Map).unwrap(),
    }
}

/// A document whose `marks` map nobody's step created: undoing a first
/// mark then removes the mark, not the map a collaborator also writes in.
fn document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(GENESIS));
    doc.put_object(ROOT, "marks", ObjType::Map).unwrap();
    doc.commit();
    doc
}

/// `actor` adds mark `id` with `colour`, as one change.
fn add(doc: &mut AutoCommit, actor: &[u8], id: &str, colour: &str) {
    doc.set_actor(ActorId::from(actor));
    let marks = marks(doc);
    let mark = doc.put_object(&marks, id, ObjType::Map).unwrap();
    doc.put(&mark, "colour", colour).unwrap();
    doc.commit();
}

fn recolour(doc: &mut AutoCommit, actor: &[u8], id: &str, colour: &str) {
    doc.set_actor(ActorId::from(actor));
    let marks = marks(doc);
    let (_, mark) = doc.get(&marks, id).unwrap().unwrap();
    doc.put(&mark, "colour", colour).unwrap();
    doc.commit();
}

/// Run a verb as `actor`, committing what it wrote as its change.
fn verb<T>(
    doc: &mut AutoCommit,
    actor: &[u8],
    run: impl FnOnce(&mut AutoCommit) -> Result<T, String>,
) -> Result<T, String> {
    doc.set_actor(ActorId::from(actor));
    let result = run(doc);
    if result.is_ok() {
        doc.commit();
    } else {
        doc.rollback();
    }
    result
}

fn ids(doc: &AutoCommit) -> Vec<String> {
    match doc.get(ROOT, "marks").unwrap() {
        Some((_, marks)) => doc.keys(&marks).collect(),
        None => Vec::new(),
    }
}

fn colour(doc: &AutoCommit, id: &str) -> Option<String> {
    let (_, marks) = doc.get(ROOT, "marks").unwrap()?;
    let (_, mark) = doc.get(&marks, id).unwrap()?;
    let (value, _) = doc.get(&mark, "colour").unwrap()?;
    value.into_string().ok()
}

fn version_of(doc: &mut AutoCommit) -> String {
    short(&doc.get_heads()[0])
}

#[test]
fn undo_and_redo_take_back_only_the_scopes_own_steps_in_order() {
    let mut doc = document();
    add(&mut doc, ALICE, "a1", "red");
    add(&mut doc, ALICE, "a2", "green");
    add(&mut doc, BOB, "b1", "blue");

    let undone = verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 1, plain)).unwrap();
    assert_eq!(undone.len(), 1);
    assert_eq!(ids(&doc), ["a1", "b1"], "Alice's newest step, not Bob's");
    verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 1, plain)).unwrap();
    assert_eq!(ids(&doc), ["b1"]);
    let refused = verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 1, plain)).unwrap_err();
    assert!(
        refused.starts_with("You have nothing in effect to undo"),
        "{refused}"
    );

    verb(&mut doc, ALICE, |doc| redo(doc, &scope(ALICE), 1, plain)).unwrap();
    assert_eq!(ids(&doc), ["a1", "b1"], "the last undone comes back first");
    verb(&mut doc, ALICE, |doc| redo(doc, &scope(ALICE), 1, plain)).unwrap();
    assert_eq!(ids(&doc), ["a1", "a2", "b1"]);
    assert!(verb(&mut doc, ALICE, |doc| redo(doc, &scope(ALICE), 1, plain)).is_err());
    assert!(
        undone_is_empty(&doc),
        "every redo removed its undo's record"
    );

    // Undo after redo takes the redone work back again, in one step each.
    verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 2, plain)).unwrap();
    assert_eq!(ids(&doc), ["b1"]);
    verb(&mut doc, ALICE, |doc| redo(doc, &scope(ALICE), 5, plain)).unwrap();
    assert_eq!(ids(&doc), ["a1", "a2", "b1"]);

    verb(&mut doc, BOB, |doc| undo(doc, &scope(BOB), 1, plain)).unwrap();
    assert_eq!(ids(&doc), ["a1", "a2"]);
}

fn undone_is_empty(doc: &AutoCommit) -> bool {
    undone(doc).is_empty()
}

#[test]
fn a_new_step_ends_redo_and_somebody_elses_later_edit_refuses_undo() {
    let mut doc = document();
    add(&mut doc, ALICE, "a1", "red");
    add(&mut doc, ALICE, "a2", "green");
    verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 1, plain)).unwrap();
    add(&mut doc, ALICE, "a3", "white");
    let refused = verb(&mut doc, ALICE, |doc| redo(doc, &scope(ALICE), 1, plain)).unwrap_err();
    assert!(refused.contains("redo"), "{refused}");

    recolour(&mut doc, ALICE, "a1", "pink");
    recolour(&mut doc, BOB, "a1", "black");
    add(&mut doc, ALICE, "a4", "grey");
    let refused = verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 2, plain)).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    assert_eq!(colour(&doc, "a1").as_deref(), Some("black"));
    assert!(
        ids(&doc).contains(&"a4".to_owned()),
        "a refusal writes nothing"
    );
}

#[test]
fn undoing_the_step_that_made_a_shared_container_is_refused_not_destructive() {
    let mut doc = AutoCommit::new();
    add(&mut doc, ALICE, "a1", "red");
    add(&mut doc, BOB, "b1", "blue");
    let refused = verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 1, plain)).unwrap_err();
    assert!(refused.contains("changed since by"), "{refused}");
    assert_eq!(ids(&doc), ["a1", "b1"], "Bob's mark stays");
}

#[test]
fn restore_checkpoint_and_diff_address_versions_by_name_or_change() {
    let mut doc = document();
    add(&mut doc, ALICE, "sky", "blue");
    add(&mut doc, ALICE, "sun", "yellow");
    verb(&mut doc, ALICE, |doc| checkpoint(doc, "before rework")).unwrap();
    let before = resolve(&doc, "before rework").unwrap();
    let first = version_of(&mut doc);
    assert_eq!(resolve(&doc, &first).unwrap(), doc.get_heads());

    recolour(&mut doc, ALICE, "sky", "mud");
    recolour(&mut doc, ALICE, "sun", "mud");
    add(&mut doc, ALICE, "smear", "mud");
    let now = doc.get_heads();
    let items = changed_items(&mut doc, &before, &now);
    let listed: Vec<(&str, &str)> = items
        .iter()
        .map(|item| (item.path.as_str(), item.change))
        .collect();
    assert_eq!(
        listed,
        [
            ("marks/smear", "added"),
            ("marks/sky", "changed"),
            ("marks/sun", "changed")
        ]
    );

    verb(&mut doc, ALICE, |doc| {
        restore(doc, &before, &["sky".to_owned()])
    })
    .unwrap();
    assert_eq!(colour(&doc, "sky").as_deref(), Some("blue"), "only the sky");
    assert_eq!(colour(&doc, "sun").as_deref(), Some("mud"));
    verb(&mut doc, ALICE, |doc| restore(doc, &before, &[])).unwrap();
    assert_eq!(ids(&doc), ["sky", "sun"]);
    assert_eq!(colour(&doc, "sun").as_deref(), Some("yellow"));
    assert_eq!(
        checkpoints(&doc).len(),
        1,
        "a restore keeps the checkpoints"
    );
    let again = verb(&mut doc, ALICE, |doc| restore(doc, &before, &[])).unwrap_err();
    assert!(again.contains("already matches"), "{again}");

    // A restore is a step of its own: undo takes it back.
    verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 1, plain)).unwrap();
    assert!(ids(&doc).contains(&"smear".to_owned()));

    let unknown = resolve(&doc, "nope").unwrap_err();
    assert!(unknown.contains("before rework"), "{unknown}");
    assert!(verb(&mut doc, ALICE, |doc| checkpoint(doc, "  ")).is_err());
}

/// Every change of `doc`, as the step index reads it.
fn every_version(doc: &AutoCommit) -> Vec<Version> {
    let document = doc.committed_document();
    document
        .get_changes_meta(&[])
        .iter()
        .map(|meta| tilo_resource::history::version_of(document, meta))
        .collect()
}

#[test]
fn a_change_is_classified_from_its_own_operations_as_its_diff_reads_it() {
    let mut doc = document();
    add(&mut doc, ALICE, "a1", "red");
    add(&mut doc, ALICE, "a2", "green");
    add(&mut doc, BOB, "b1", "blue");
    verb(&mut doc, ALICE, |doc| undo(doc, &scope(ALICE), 2, plain)).unwrap();
    verb(&mut doc, ALICE, |doc| redo(doc, &scope(ALICE), 1, plain)).unwrap();
    verb(&mut doc, ALICE, |doc| checkpoint(doc, "kept")).unwrap();
    recolour(&mut doc, BOB, "b1", "black");
    let before = resolve(&doc, "kept").unwrap();
    verb(&mut doc, ALICE, |doc| restore(doc, &before, &[])).unwrap();
    // Bookkeeping beneath a root key of its own, a counter on the root,
    // changes that take back what they wrote, and an outside compensation
    // still pending.
    doc.set_actor(ActorId::from(BOB));
    let ledger = doc.put_object(ROOT, "tilo:ledger", ObjType::Map).unwrap();
    doc.put(&ledger, "entry", "kept").unwrap();
    doc.commit();
    doc.put(ROOT, "count", ScalarValue::counter(0)).unwrap();
    doc.commit();
    doc.increment(ROOT, "count", 2).unwrap();
    doc.commit();
    // A key written and taken back in one change, and a value written
    // twice in one.
    doc.put(ROOT, "scratch", 1_i64).unwrap();
    doc.delete(ROOT, "scratch").unwrap();
    doc.commit();
    doc.put(ROOT, "value", "first").unwrap();
    doc.put(ROOT, "value", "second").unwrap();
    doc.commit();
    let target = doc.get_heads()[0];
    doc.put(ROOT, format!("{UNDONE_KEY}{target}"), 1_i64)
        .unwrap();
    doc.commit();
    let document = doc.committed_document();
    let versions = every_version(&doc);
    assert!(versions.len() > 10, "{} changes", versions.len());
    let mut seen = HashSet::new();
    for version in &versions {
        let read = format!("{:?}", kind_in(document, version));
        assert_eq!(
            read,
            format!("{:?}", kind_by_diff(document, version)),
            "change {}",
            version.hash
        );
        seen.insert(read.split('(').next().unwrap_or_default().to_owned());
    }
    for kind in ["Undo", "Redo", "Edit", "Bookkeeping"] {
        assert!(seen.contains(kind), "no {kind} among {seen:?}");
    }
}

/// How long classifying the newest change of a document holding `count`
/// marks takes, at best of several reads.
fn classify_newest(count: usize) -> std::time::Duration {
    let mut doc = document();
    doc.set_actor(ActorId::from(ALICE));
    let map = marks(&mut doc);
    for index in 0..count {
        let mark = doc
            .put_object(&map, format!("m{index}"), ObjType::Map)
            .unwrap();
        doc.put(&mark, "colour", "red").unwrap();
    }
    doc.commit();
    recolour(&mut doc, ALICE, "m0", "blue");
    let newest = every_version(&doc).pop().unwrap();
    let document = doc.committed_document();
    (0..5)
        .map(|_| {
            let start = std::time::Instant::now();
            assert!(matches!(kind_in(document, &newest), Kind::Edit));
            start.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn classifying_a_change_costs_the_same_however_large_the_document() {
    let small = classify_newest(50);
    let large = classify_newest(20_000);
    assert!(
        large <= small * 4 + std::time::Duration::from_micros(200),
        "classifying one recolour took {large:?} beside 20,000 marks and {small:?} beside 50"
    );
}
