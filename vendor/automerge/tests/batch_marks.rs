//! TILO(batched-marks): marks a batch writes with the text it makes are the
//! marks set on that text one after another once written: the same ops, so
//! the same document, which merges, reads and rolls back alike.

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{hydrate, ActorId, AutoCommit, ObjType, ReadDoc, TextEncoding, ROOT};

/// A small deterministic generator, so a failing case can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const EXPANDS: [ExpandMark; 4] = [
    ExpandMark::Before,
    ExpandMark::After,
    ExpandMark::Both,
    ExpandMark::None,
];

fn text_of(rng: &mut Rng) -> String {
    let letters = ["a", "b", "ö", "😀", " "];
    (0..1 + rng.below(12))
        .map(|_| letters[rng.below(letters.len())])
        .collect()
}

fn marks_of(rng: &mut Rng, len: usize) -> Vec<(Mark, ExpandMark)> {
    let names = ["bold", "link", "author"];
    (0..rng.below(7))
        .map(|i| {
            let a = rng.below(len + 1);
            let b = rng.below(len + 1);
            let mark = Mark::new(
                names[rng.below(names.len())].to_owned(),
                format!("v{i}"),
                a.min(b),
                a.max(b),
            );
            (mark, EXPANDS[rng.below(EXPANDS.len())])
        })
        .collect()
}

fn doc() -> AutoCommit {
    AutoCommit::new().with_actor(ActorId::from(b"writer".as_slice()))
}

fn marked(text: &str, marks: &[(Mark, ExpandMark)]) -> hydrate::Text {
    marks.iter().fold(
        hydrate::Text::new(TextEncoding::UnicodeCodePoint, text),
        |made, (mark, expand)| made.with_mark(mark.clone(), *expand),
    )
}

/// `marks` set on `text` one at a time, after the text is written.
fn mark_each(doc: &mut AutoCommit, text: &automerge::ObjId, marks: &[(Mark, ExpandMark)]) {
    for (mark, expand) in marks {
        doc.mark(text, mark.clone(), *expand).unwrap();
    }
}

#[test]
fn a_text_made_with_marks_is_the_text_marked_after() {
    for seed in 0..400 {
        let mut rng = Rng(seed);
        let first = text_of(&mut rng);
        let first_marks = marks_of(&mut rng, first.chars().count());
        let second = text_of(&mut rng);
        let second_marks = marks_of(&mut rng, second.chars().count());

        // A record holding two marked texts and a scalar after them.
        let mut batched = doc();
        let record = std::collections::HashMap::from([
            ("a", hydrate::Value::Text(marked(&first, &first_marks))),
            ("b", hydrate::Value::Text(marked(&second, &second_marks))),
            ("c", hydrate::Value::scalar(1)),
        ]);
        batched
            .batch_create_object(ROOT, "record", &hydrate::Value::Map(record.into()), false)
            .unwrap();

        let mut by_ops = doc();
        let record = by_ops.put_object(ROOT, "record", ObjType::Map).unwrap();
        let a = by_ops.put_object(&record, "a", ObjType::Text).unwrap();
        let b = by_ops.put_object(&record, "b", ObjType::Text).unwrap();
        by_ops.put(&record, "c", 1).unwrap();
        by_ops.splice_text(&a, 0, 0, &first).unwrap();
        mark_each(&mut by_ops, &a, &first_marks);
        by_ops.splice_text(&b, 0, 0, &second).unwrap();
        mark_each(&mut by_ops, &b, &second_marks);

        for doc in [&batched, &by_ops] {
            let record = doc.get(ROOT, "record").unwrap().unwrap().1;
            let a = doc.get(&record, "a").unwrap().unwrap().1;
            assert_eq!(doc.text(&a).unwrap(), first, "seed {seed}");
        }
        assert_eq!(
            batched.save(),
            by_ops.save(),
            "seed {seed}: {first:?} {first_marks:?} {second:?} {second_marks:?}"
        );
    }
}

#[test]
fn edits_and_marks_made_alongside_merge_alike() {
    for seed in 0..200 {
        let mut rng = Rng(1000 + seed);
        let text = text_of(&mut rng);
        let len = text.chars().count();
        let marks = marks_of(&mut rng, len);

        let mut batched = doc();
        batched
            .batch_create_object(ROOT, "t", &hydrate::Value::Text(marked(&text, &marks)), false)
            .unwrap();
        batched.commit();
        let mut by_ops = doc();
        let t = by_ops.put_object(ROOT, "t", ObjType::Text).unwrap();
        by_ops.splice_text(&t, 0, 0, &text).unwrap();
        mark_each(&mut by_ops, &t, &marks);
        by_ops.commit();

        // Someone else types at every mark's edges and marks a stretch,
        // while the writer goes on typing and marking.
        let mut peer = batched
            .fork()
            .with_actor(ActorId::from(b"peer".as_slice()));
        let t = peer.get(ROOT, "t").unwrap().unwrap().1;
        let mut edges: Vec<usize> = marks
            .iter()
            .flat_map(|(mark, _)| [mark.start, mark.end])
            .collect();
        edges.sort_unstable();
        edges.dedup();
        for at in edges.iter().rev() {
            peer.splice_text(&t, *at, 0, "X").unwrap();
        }
        let a = rng.below(len + 1);
        let b = rng.below(len + 1);
        peer.mark(
            &t,
            Mark::new("bold".into(), "peer", a.min(b), a.max(b)),
            EXPANDS[rng.below(4)],
        )
        .unwrap();
        peer.commit();
        let theirs = peer.get_changes(&batched.get_heads());

        let mut read = Vec::new();
        for doc in [&mut batched, &mut by_ops] {
            let t = doc.get(ROOT, "t").unwrap().unwrap().1;
            doc.splice_text(&t, len / 2, 0, "Y").unwrap();
            doc.mark(&t, Mark::new("link".into(), "mine", 0, 1), ExpandMark::Both)
                .unwrap();
            doc.commit();
            doc.apply_changes(theirs.clone()).unwrap();
            read.push((doc.text(&t).unwrap(), doc.marks(&t).unwrap(), doc.save()));
        }
        assert_eq!(read[0], read[1], "seed {seed}: {text:?} {marks:?}");
    }
}

#[test]
fn a_rolled_back_batch_leaves_the_document_as_it_was() {
    for seed in 0..100 {
        let mut rng = Rng(5000 + seed);
        let mut doc = doc();
        let before = doc.put_object(ROOT, "before", ObjType::Text).unwrap();
        doc.splice_text(&before, 0, 0, "kept").unwrap();
        doc.commit();
        let saved = doc.save();

        let text = text_of(&mut rng);
        let marks = marks_of(&mut rng, text.chars().count());
        doc.batch_create_object(ROOT, "t", &hydrate::Value::Text(marked(&text, &marks)), false)
            .unwrap();
        doc.rollback();
        assert_eq!(doc.save(), saved, "seed {seed}");

        // And the next writes give what they give on a copy that never saw it.
        let mut fresh = AutoCommit::load(&saved)
            .unwrap()
            .with_actor(ActorId::from(b"writer".as_slice()));
        for doc in [&mut doc, &mut fresh] {
            let t = doc.put_object(ROOT, "t", ObjType::Text).unwrap();
            doc.splice_text(&t, 0, 0, &text).unwrap();
            mark_each(doc, &t, &marks);
        }
        assert_eq!(doc.save(), fresh.save(), "seed {seed}");
    }
}

#[test]
fn a_mark_must_lie_within_the_text_it_is_made_with() {
    let mut doc = doc();
    let made = hydrate::Text::new(TextEncoding::UnicodeCodePoint, "ab").with_mark(
        Mark::new("bold".into(), true, 1, 3),
        ExpandMark::After,
    );
    assert!(doc
        .batch_create_object(ROOT, "t", &hydrate::Value::Text(made), false)
        .is_err());
}
