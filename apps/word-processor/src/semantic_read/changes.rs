//! What a document edit answers: the blocks it changed, as
//! [`read-text`](super::markdown) renders them, so its caller sees the
//! result without reading the document again.
//!
//! ```text
//! 2 changed:
//! ## Standup {#b1}
//! Maya is out today {#b2}
//! 1 removed
//! ```
//!
//! A block longer than [`MAX_BLOCK_CHARS`] is cut there and ends in `…`: the
//! caller wrote it, so its start is enough to recognise it.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use automerge::{AutoCommit, PatchAction, Prop};

use super::{Segment, Shape};

/// The most changed blocks one answer lists; `read-text` reads the rest.
pub const MAX_LISTED: usize = 32;

/// The most characters of one block an answer shows.
pub const MAX_BLOCK_CHARS: usize = 160;

/// The blocks `changes`, an edit's patches over `doc`, touched.
pub fn summary(doc: &AutoCommit, changes: &[automerge::Patch]) -> String {
    let Ok(containers) = crate::model::open(doc) else {
        return "no document".to_owned();
    };
    let mut touched = BTreeSet::new();
    for patch in changes {
        let under_blocks = patch
            .path
            .iter()
            .find(|(object, _)| *object == containers.blocks)
            .map(|(_, prop)| prop.clone());
        let block = under_blocks.or_else(|| {
            (patch.obj == containers.blocks)
                .then(|| match &patch.action {
                    PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                        Some(Prop::Map(key.clone()))
                    }
                    _ => None,
                })
                .flatten()
        });
        if let Some(Prop::Map(id)) = block {
            touched.insert(id);
        }
    }
    if touched.is_empty() {
        return "no change".to_owned();
    }
    let blocks: Vec<Segment> = tilo_doc::model::materialize(doc, &containers)
        .blocks
        .into_iter()
        .filter(|block| touched.contains(&block.id))
        .map(|block| {
            let complete = block.text.chars().count() <= MAX_BLOCK_CHARS;
            Segment {
                shape: Shape::of_attrs(&block.attrs),
                marks: block
                    .marks
                    .into_iter()
                    .filter(|mark| mark.start < MAX_BLOCK_CHARS)
                    .map(|mark| tilo_doc::document::MarkSpan {
                        end: mark.end.min(MAX_BLOCK_CHARS),
                        ..mark
                    })
                    .collect(),
                block: block.id,
                kind: block.kind,
                text: if complete {
                    block.text
                } else {
                    block.text.chars().take(MAX_BLOCK_CHARS).collect()
                },
                complete,
            }
        })
        .collect();
    let removed = touched.len().saturating_sub(blocks.len());
    let mut out = String::new();
    if !blocks.is_empty() {
        let listed = blocks.len().min(MAX_LISTED);
        let _ = writeln!(out, "{} changed:", blocks.len());
        out.push_str(&super::markdown::render(
            blocks.get(..listed).unwrap_or_default(),
        ));
        if blocks.len() > listed {
            let _ = writeln!(
                out,
                "… {} more; read-text reads them",
                blocks.len() - listed
            );
        }
    }
    if removed > 0 {
        let _ = writeln!(out, "{removed} removed");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{MAX_BLOCK_CHARS, summary};
    use automerge::AutoCommit;
    use tilo_automerge::ApplyIncoming as _;

    #[test]
    fn a_bolded_block_answers_bold() {
        let mut source = crate::model::testing::empty().unwrap();
        let line = source.insert_block(None, "paragraph");
        source.splice_text(&line, 0, 0, "Deadline: October 15");
        let mut doc = AutoCommit::load(&source.save()).unwrap();
        let before = doc.get_heads();
        let mut edited = crate::model::reopen(doc.clone()).unwrap();
        edited.add_mark(&line, 0, 20, "strong", true);
        doc.merge_incoming(&mut AutoCommit::load(&edited.save()).unwrap())
            .unwrap();
        let after = doc.get_heads();
        let changes = doc.diff(&before, &after);
        assert_eq!(
            summary(&doc, &changes),
            format!("1 changed:\n**Deadline: October 15** {{#{line}}}\n")
        );
    }

    #[test]
    fn a_long_block_answers_with_its_start() {
        let mut source = crate::model::testing::empty().unwrap();
        let short = source.insert_block(None, "paragraph");
        source.splice_text(&short, 0, 0, "short");
        let mut doc = AutoCommit::load(&source.save()).unwrap();
        let before = doc.get_heads();
        let mut edited = crate::model::reopen(doc.clone()).unwrap();
        let long = edited.insert_block(Some(&short), "paragraph");
        edited.splice_text(&long, 0, 0, &"a".repeat(MAX_BLOCK_CHARS + 40));
        doc.merge_incoming(&mut AutoCommit::load(&edited.save()).unwrap())
            .unwrap();
        let after = doc.get_heads();
        let changes = doc.diff(&before, &after);
        assert_eq!(
            summary(&doc, &changes),
            format!("1 changed:\n{}… {{#{long}}}\n", "a".repeat(MAX_BLOCK_CHARS))
        );
    }
}
