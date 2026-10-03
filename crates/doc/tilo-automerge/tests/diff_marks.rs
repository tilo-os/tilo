//! A diff keeps each text object's formatting changes on that object, however
//! many objects it walks: reverting an action replays exactly these patches.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, PatchAction, ROOT, ReadDoc, ScalarValue};

#[test]
fn a_diff_keeps_the_marks_of_several_texts_apart() {
    let mut doc = AutoCommit::new();
    let texts: Vec<_> = ["Background", "Method", "Results"]
        .into_iter()
        .map(|word| {
            let text = doc.put_object(ROOT, word, ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, word).unwrap();
            text
        })
        .collect();
    let plain = doc.get_heads();
    for text in &texts {
        let end = doc.length(text);
        doc.mark(
            text,
            Mark::new("bold".into(), true, 0, end),
            ExpandMark::After,
        )
        .unwrap();
    }
    let bold = doc.get_heads();

    for (from, to, value) in [
        (&plain, &bold, ScalarValue::from(true)),
        (&bold, &plain, ScalarValue::Null),
    ] {
        let mut marked: Vec<_> = doc
            .diff(from, to)
            .into_iter()
            .filter_map(|patch| match patch.action {
                PatchAction::Mark { marks } => Some((patch.obj, marks)),
                _ => None,
            })
            .collect();
        marked.sort_by_key(|(obj, _)| texts.iter().position(|text| text == obj));
        let expected: Vec<_> = texts
            .iter()
            .map(|text| {
                let end = doc.length(text);
                (
                    text.clone(),
                    vec![Mark::new("bold".into(), value.clone(), 0, end)],
                )
            })
            .collect();
        assert_eq!(marked, expected, "{value:?}");
    }
}
