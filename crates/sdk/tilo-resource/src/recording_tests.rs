use super::*;
use crate::history::commit_stamped_at_with_message;
use automerge::marks::ExpandMark;
use tilo_actions::{ActionId, EncodedAction};
use tilo_automerge::ApplyIncoming as _;

const ALICE: [u8; 2] = [0xa1, 0x1c];
const BOB: [u8; 2] = [0xb0, 0xb0];

fn as_actor(doc: &mut Doc, actor: [u8; 2]) {
    doc.set_actor(ActorId::from(actor.to_vec()));
}

/// Commit what `doc` holds at `seconds` with `message`.
fn commit(doc: &mut Doc, seconds: i64, message: Option<String>) -> ChangeHash {
    commit_stamped_at_with_message(doc, seconds, message).unwrap()
}

/// A resource edited before and during a window that starts at second 100:
/// before it, text that is deleted again and the record's identity; during
/// it, two people's concurrent edits of maps, lists, text and its marks,
/// counters and nested objects.
fn edited() -> Doc {
    let mut doc = Doc::new();
    as_actor(&mut doc, ALICE);
    let record = doc.put_object(ROOT, "tilo:record", ObjType::Map).unwrap();
    doc.put(&record, "id", "the-original-record").unwrap();
    let title = doc.put_object(ROOT, "title", ObjType::Text).unwrap();
    doc.splice_text(&title, 0, 0, "Groceries").unwrap();
    let items = doc.put_object(ROOT, "items", ObjType::List).unwrap();
    doc.insert(&items, 0, "bread").unwrap();
    doc.put(ROOT, "visits", ScalarValue::counter(1)).unwrap();
    commit(&mut doc, 10, None);
    let note = doc.put_object(ROOT, "note", ObjType::Text).unwrap();
    doc.splice_text(&note, 0, 0, "the door code is 4471")
        .unwrap();
    commit(&mut doc, 20, None);
    doc.delete(ROOT, "note").unwrap();
    commit(&mut doc, 30, None);

    // The window.
    doc.splice_text(&title, 9, 0, " for Sunday").unwrap();
    doc.mark(
        &title,
        Mark::new("strong".into(), true, 0, 9),
        ExpandMark::After,
    )
    .unwrap();
    commit(&mut doc, 100, None);
    let mut bob = doc.fork();
    as_actor(&mut bob, BOB);
    bob.insert(&items, 1, "milk").unwrap();
    let detail = bob.insert_object(&items, 2, ObjType::Map).unwrap();
    bob.put(&detail, "name", "eggs").unwrap();
    let tags = bob.put_object(&detail, "tags", ObjType::List).unwrap();
    bob.insert(&tags, 0, "fresh").unwrap();
    bob.increment(ROOT, "visits", 2).unwrap();
    commit(&mut bob, 110, None);
    doc.insert(&items, 0, "apples").unwrap();
    doc.delete(&items, 1).unwrap();
    doc.increment(ROOT, "visits", 1).unwrap();
    commit(&mut doc, 115, None);
    doc.merge_incoming(&mut bob).unwrap();
    doc.splice_text(&title, 0, 0, "My ").unwrap();
    doc.put(ROOT, "done", false).unwrap();
    commit(&mut doc, 120, None);
    doc
}

fn the_window(doc: &Doc) -> Recording {
    record(
        doc,
        100_000,
        &doc.committed_document().get_heads(),
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn a_recording_holds_the_start_state_and_each_change_of_the_window() {
    let doc = edited();
    let recording = the_window(&doc);
    assert_eq!(recording.steps.len(), 4);
    assert_eq!(recording.step_count(), 4);
    assert_eq!((recording.from_ms, recording.to_ms), (100_000, 120_000));
    let source = doc.committed_document();
    let recorded = recording.doc().committed_document();
    let versions = recording.versions();
    for (step, original) in recording.steps.iter().enumerate() {
        let at = recording.heads_at(step.saturating_add(1)).unwrap();
        assert_eq!(at, vec![original.recorded]);
        let before: Vec<_> = source
            .get_changes_meta(&[])
            .into_iter()
            .map(|meta| meta.hash)
            .collect();
        let upto = before
            .iter()
            .position(|hash| *hash == original.original)
            .unwrap();
        let heads = frontier_of(source, &before[..=upto]);
        assert!(
            same_state(source, &heads, recorded, &at).unwrap(),
            "step {step} replays to the state its change made"
        );
        let meta = source.get_change_meta_by_hash(&original.original).unwrap();
        let version = &versions[step.saturating_add(1)];
        assert_eq!(
            version.actor,
            meta.actor.to_hex_string(),
            "the author stays"
        );
        assert_eq!(version.time_ms, meta.timestamp * 1000, "the time stays");
    }
    let start = recording.heads_at(0).unwrap();
    let before_window: Vec<_> = source
        .get_changes_meta(&[])
        .into_iter()
        .take(3)
        .map(|meta| meta.hash)
        .collect();
    let heads = frontier_of(source, &before_window);
    assert!(same_state(source, &heads, recorded, &start).unwrap());
    assert!(recording.heads_at(5).is_none());
}

/// The heads of the run `hashes` of `doc`'s causal order.
fn frontier_of(doc: &Automerge, hashes: &[ChangeHash]) -> Vec<ChangeHash> {
    let changes: Vec<Change> = hashes
        .iter()
        .map(|hash| {
            let meta = doc.get_change_meta_by_hash(hash).unwrap();
            Change {
                hash: *hash,
                deps: meta.deps.clone(),
                actor: meta.actor.clone().into_owned(),
                time_s: meta.timestamp,
                message: None,
            }
        })
        .collect();
    frontier(&changes)
}

#[test]
fn nothing_from_before_the_window_or_the_records_identity_is_in_it() {
    let mut doc = edited();
    let bytes = the_window(&doc).bytes();
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    // The full document holds the deleted note in its history.
    let full = doc.save();
    assert!(full.windows(4).any(|window| window == b"4471"));
    assert!(
        !contains(b"4471"),
        "deleted before the window, so not recorded"
    );
    assert!(!contains(b"the-original-record"));
    let loaded = Recording::load(&bytes, &Limits::default()).unwrap();
    assert!(loaded.doc().get(ROOT, "tilo:record").unwrap().is_none());
}

#[test]
fn another_member_loading_the_bytes_replays_the_same_states() {
    let doc = edited();
    let recording = the_window(&doc);
    let loaded = Recording::load(&recording.bytes(), &Limits::default()).unwrap();
    assert_eq!(loaded.step_count(), recording.step_count());
    assert_eq!((loaded.from_ms, loaded.to_ms), (100_000, 120_000));
    assert_eq!(loaded.versions(), recording.versions());
    for step in 0..=recording.step_count() {
        let mine = recording.heads_at(step).unwrap();
        let theirs = loaded.heads_at(step).unwrap();
        assert!(
            same_state(
                recording.doc().committed_document(),
                &mine,
                loaded.doc().committed_document(),
                &theirs
            )
            .unwrap()
        );
    }
}

#[test]
fn a_gesture_begun_before_the_window_keeps_its_action_in_the_recording() {
    let mut doc = Doc::new();
    as_actor(&mut doc, ALICE);
    let gesture = ActionId(uuid::Uuid::from_u128(7));
    let first = ActionContext::default()
        .in_gesture(Some(gesture))
        .with_app("so.tilo.notes", "Notes");
    let typed = |payload: &str| ActionContext {
        action: Some(EncodedAction {
            type_id: "notes.type".into(),
            payload: payload.as_bytes().to_vec(),
        }),
        ..first.clone()
    };
    doc.put(ROOT, "body", "a").unwrap();
    commit(&mut doc, 10, typed("\"a\"").change_message());
    doc.put(ROOT, "body", "ab").unwrap();
    let continued = typed("\"ab\"").continuation_of(&typed("\"a\"")).unwrap();
    commit(&mut doc, 200, Some(continued));
    let recording = record(
        &doc,
        100_000,
        &doc.committed_document().get_heads(),
        &Limits::default(),
    )
    .unwrap();
    let versions = recording.versions();
    let step = versions.last().unwrap();
    let original = committed_versions(&doc).last().unwrap().context.clone();
    assert!(
        original.is_some(),
        "the original reads its continuation whole"
    );
    assert_eq!(step.context, original);
    assert_eq!(
        step.context
            .as_ref()
            .and_then(|context| context.action.clone()),
        typed("\"ab\"").action
    );
    assert_eq!(
        versions
            .first()
            .unwrap()
            .context
            .as_ref()
            .map(|context| context.change),
        Some(ChangeKind::External),
        "the start state is nobody's step"
    );
}

#[test]
fn a_window_longer_than_the_limit_starts_later_and_keeps_the_end() {
    let mut doc = Doc::new();
    for second in 0..10 {
        doc.put(ROOT, "count", second).unwrap();
        commit(&mut doc, second, None);
    }
    let limits = Limits {
        steps: 3,
        ..Limits::default()
    };
    let recording = record(&doc, 0, &doc.committed_document().get_heads(), &limits).unwrap();
    assert_eq!(recording.step_count(), 3);
    assert_eq!((recording.from_ms, recording.to_ms), (7_000, 9_000));
    let start = recording.heads_at(0).unwrap();
    assert_eq!(
        recording
            .doc()
            .get_at(ROOT, "count", &start)
            .unwrap()
            .unwrap()
            .0,
        Value::from(6_i64)
    );
    let bytes = recording.bytes();
    assert_eq!(
        Recording::load(&bytes, &limits).unwrap().step_count(),
        3,
        "a recording within its limits loads"
    );
    let tighter = Limits {
        steps: 2,
        ..Limits::default()
    };
    assert_eq!(
        Recording::load(&bytes, &tighter).err(),
        Some(RecordingError::TooManySteps)
    );
    let tiny = Limits {
        bytes: 16,
        ..Limits::default()
    };
    assert!(matches!(
        record(&doc, 0, &doc.committed_document().get_heads(), &tiny),
        Err(RecordingError::TooLarge(_))
    ));
}

#[test]
fn an_end_outside_the_history_and_a_document_that_is_no_recording_are_refused() {
    let mut doc = edited();
    let mut other = Doc::new();
    other.put(ROOT, "elsewhere", true).unwrap();
    commit(&mut other, 5, None);
    let foreign = other.get_heads();
    assert_eq!(
        record(&doc, 0, &foreign, &Limits::default()).err(),
        Some(RecordingError::UnknownEnd)
    );
    assert!(matches!(
        Recording::load(&doc.save(), &Limits::default()),
        Err(RecordingError::Invalid(_))
    ));
}

#[test]
fn a_window_with_no_changes_is_the_state_alone() {
    let doc = edited();
    let recording = record(
        &doc,
        1_000_000,
        &doc.committed_document().get_heads(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(recording.step_count(), 0);
    assert_eq!(recording.to_ms, recording.from_ms);
    assert!(
        same_state(
            doc.committed_document(),
            &doc.committed_document().get_heads(),
            recording.doc().committed_document(),
            &recording.heads_at(0).unwrap()
        )
        .unwrap()
    );
}

/// A small deterministic generator, so the stress test needs no dependency.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap() % n.max(1)
    }
}

/// One random edit of `doc`'s text, list or map.
fn scribble(doc: &mut Doc, rng: &mut Lcg) {
    let (_, text) = doc.get(ROOT, "text").unwrap().unwrap();
    let (_, list) = doc.get(ROOT, "list").unwrap().unwrap();
    let words = ["a", "bc", "déf", "🙂", " ", "xyz"];
    match rng.below(7) {
        0 | 1 => {
            let at = rng.below(doc.length(&text).saturating_add(1));
            let word = words[rng.below(words.len())];
            doc.splice_text(&text, at, 0, word).unwrap();
        }
        2 => {
            let len = doc.length(&text);
            if len > 1 {
                let at = rng.below(len - 1);
                doc.splice_text(&text, at, 1, "").unwrap();
            }
        }
        3 => {
            let len = doc.length(&text);
            if len > 2 {
                let start = rng.below(len - 1);
                let end = start + 1 + rng.below(len - start - 1);
                let name = ["strong", "em"][rng.below(2)];
                let value = if rng.below(3) == 0 {
                    ScalarValue::Null
                } else {
                    ScalarValue::Boolean(true)
                };
                let expand = [ExpandMark::After, ExpandMark::None, ExpandMark::Both][rng.below(3)];
                doc.mark(&text, Mark::new(name.into(), value, start, end), expand)
                    .unwrap();
            }
        }
        4 => {
            let at = rng.below(doc.length(&list).saturating_add(1));
            if rng.below(2) == 0 {
                doc.insert(&list, at, i64::try_from(rng.below(100)).unwrap())
                    .unwrap();
            } else {
                let item = doc.insert_object(&list, at, ObjType::Map).unwrap();
                doc.put(&item, "n", i64::try_from(rng.below(100)).unwrap())
                    .unwrap();
            }
        }
        5 => {
            let len = doc.length(&list);
            if len > 0 {
                doc.delete(&list, rng.below(len)).unwrap();
            }
        }
        _ => {
            let key = ["k1", "k2", "k3"][rng.below(3)];
            if rng.below(3) == 0 {
                doc.delete(ROOT, key).unwrap();
            } else {
                doc.put(ROOT, key, i64::try_from(rng.below(10)).unwrap())
                    .unwrap();
            }
        }
    }
}

#[test]
fn every_step_of_many_concurrent_edits_replays_to_its_state() {
    for seed in 1..=12_u64 {
        let mut rng = Lcg(seed);
        let mut doc = Doc::new();
        as_actor(&mut doc, ALICE);
        doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        doc.put_object(ROOT, "list", ObjType::List).unwrap();
        commit(&mut doc, 1, None);
        let mut bob = doc.fork();
        as_actor(&mut bob, BOB);
        for second in 2..80_i64 {
            let writer = if rng.below(2) == 0 {
                &mut doc
            } else {
                &mut bob
            };
            for _ in 0..=rng.below(3) {
                scribble(writer, &mut rng);
            }
            commit_stamped_at_with_message(writer, second, None);
            if rng.below(5) == 0 {
                doc.merge_incoming(&mut bob).unwrap();
                bob.merge_incoming(&mut doc).unwrap();
            }
        }
        doc.merge_incoming(&mut bob).unwrap();
        let heads = doc.get_heads();
        let recording = record(&doc, 30_000, &heads, &Limits::default()).unwrap();
        let source = doc.committed_document();
        let order: Vec<ChangeHash> = source
            .get_changes_meta(&[])
            .into_iter()
            .map(|meta| meta.hash)
            .collect();
        for (step, original) in recording.steps.iter().enumerate() {
            let upto = order
                .iter()
                .position(|hash| *hash == original.original)
                .unwrap();
            let at = recording.heads_at(step + 1).unwrap();
            assert!(
                same_state(
                    source,
                    &frontier_of(source, &order[..=upto]),
                    recording.doc().committed_document(),
                    &at
                )
                .unwrap(),
                "seed {seed}, step {step}"
            );
        }
    }
}
