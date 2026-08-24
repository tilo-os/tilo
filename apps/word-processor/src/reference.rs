//! What a text selection in this document is *about*.
//!
//! The word processor's half of "select anything and say something about it":
//! given the current selection, mint the durable address of the thing being
//! talked about, so a message can carry it instead of describing it.
//!
//! ## The address agrees with what the app publishes
//!
//! Where the app already publishes a resource for a block — a heading becomes
//! `word-processor.section:<block>` in [`crate::actions::provide_resources`] —
//! a selection inside it mints *that same id*, not a parallel one. One
//! vocabulary: an address minted from a selection and an address surveyed
//! through the action door name the same thing, so a reader can go from the
//! reference to what can be done to it. `a_heading_selection_matches_the_id_the_app_publishes`
//! is the test that keeps the two from drifting apart.
//!
//! Ordinary body text has no published resource (there is nothing to *do* to a
//! paragraph that the document does not already offer), so it is addressed as
//! `word-processor.block:<block>`. The block id is the document's own — it
//! lives in the CRDT, so it means the same thing in a later session and on
//! another peer's machine, which is what a message outliving its frame needs.

use automerge::AutoCommit;

use qos_reference::{Referent, ResourceRef, Span};

use crate::widget::WordProcessorWidget;

/// Kind tag for a selection inside ordinary body text.
pub const BLOCK_KIND: &str = "word_processor.block";
/// Kind tag for a selection inside a heading — the same kind the app publishes
/// that heading under.
pub const SECTION_KIND: &str = "word_processor.section";

/// Longest label a chip shows before it is elided.
const LABEL_MAX: usize = 48;

fn short(text: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim();
    if t.chars().count() > LABEL_MAX {
        t.chars().take(LABEL_MAX - 1).collect::<String>() + "…"
    } else {
        t.to_string()
    }
}

/// The address of what a selection is about.
///
/// `selection` is the app's own per-block selected ranges,
/// `(block_id, char_start, char_end)`, exactly as
/// [`WordProcessor::selection_blocks`](crate::word_processor::WordProcessor::selection_blocks)
/// reports them — **character** offsets, which is the unit this app's own text
/// actions take, so the span is handed on in that unit rather than converted
/// into one the receiving action would misread.
///
/// A selection spanning several blocks is addressed by its **first** block,
/// with the span covering the selected run within it: that is where a reader
/// following the reference is taken. The tail is not lost information a
/// consumer needs — it can read on from the anchor — but the address is
/// deliberately one resource rather than a list, because "what is this about"
/// has one answer.
///
/// `None` when nothing is selected, so the gesture is simply not offered.
pub fn selection_reference(
    doc: &AutoCommit,
    selection: &[(String, usize, usize)],
) -> Option<ResourceRef> {
    let (block, start, end) = selection.first()?;
    if start == end && selection.len() == 1 {
        // A caret is not a selection; there is nothing being pointed at.
        return None;
    }

    let containers = qdoc::model::open(doc, &automerge::ROOT).ok()?;
    let snapshot = qdoc::model::materialize(doc, &containers);

    let view = snapshot.blocks.iter().find(|b| &b.id == block)?;
    let is_heading = view.kind == "heading";

    // The document's own title: its first heading. Used only for the label.
    let title = snapshot
        .blocks
        .iter()
        .find(|b| b.kind == "heading" && !b.text.trim().is_empty())
        .map(|b| short(&b.text));

    let label = if is_heading {
        short(&view.text)
    } else {
        // Ordinal among the flowing blocks (table cells are addressed as their
        // own resources, so they do not take paragraph numbers).
        let ordinal = snapshot
            .blocks
            .iter()
            .filter(|b| b.kind != "cell")
            .position(|b| &b.id == block)
            .map(|i| i + 1)
            .unwrap_or(1);
        // A middle dot rather than a connective word: the label is assembled
        // here, and a hard-coded "of" would be untranslatable English sitting
        // in the middle of a chip.
        match &title {
            Some(t) => format!("{t} · ¶{ordinal}"),
            None => format!("¶{ordinal}"),
        }
    };

    let (kind, id) = if is_heading {
        (SECTION_KIND, format!("word-processor.section:{block}"))
    } else {
        (BLOCK_KIND, format!("word-processor.block:{block}"))
    };

    Some(ResourceRef::ranged(
        kind,
        id,
        label,
        Span::chars(*start, *end),
    ))
}

impl Referent for WordProcessorWidget {
    fn selection_reference(&self, doc: &AutoCommit) -> Option<ResourceRef> {
        selection_reference(doc, &self.inner.selection_blocks(doc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_reference::SpanUnit;

    /// A document seeded from markdown, plus its body block ids in order.
    fn seeded(markdown: &str) -> (ui::State, Vec<String>) {
        let mut state = ui::State::new();
        let mut ed = crate::EditorDoc::bind(&mut state.doc, &state.root);
        {
            use qdoc::FormatProvider;
            let mut dm = ed.edit(&mut state.doc);
            qdoc::MarkdownCodec
                .import(markdown.as_bytes(), &mut dm)
                .unwrap();
        }
        let ids = qdoc::model::blocks_in_order(&state.doc, &ed.containers);
        (state, ids)
    }

    const DOC: &str = "# Run sheet\n\nDoors at seven, music at eight.\n\nThe hall is booked until ten.\n";

    /// The headline property: a text selection produces an address that names
    /// the block, and a range within it — asserted on the id, never on how it
    /// is rendered.
    #[test]
    fn a_text_selection_resolves_to_that_block_and_range() {
        let (state, ids) = seeded(DOC);
        // ids[1] is the first paragraph ("Doors at seven, music at eight.").
        let selection = vec![(ids[1].clone(), 10, 15)];
        let r = selection_reference(&state.doc, &selection).expect("a selection is referenceable");

        assert_eq!(r.resource_id().as_str(), format!("word-processor.block:{}", ids[1]));
        assert_eq!(r.kind, BLOCK_KIND);
        assert_eq!(r.span, Some(Span::chars(10, 15)));
        assert!(r.is_partial());
    }

    /// The span is in the unit this app's own actions take, so a consumer can
    /// hand it straight to one.
    #[test]
    fn the_span_is_in_the_unit_this_apps_actions_use() {
        let (state, ids) = seeded(DOC);
        let r = selection_reference(&state.doc, &[(ids[1].clone(), 0, 5)]).unwrap();
        assert_eq!(r.span.unwrap().unit, SpanUnit::Chars);
    }

    /// Where the app publishes a resource for the block, the selection mints
    /// the *same* id — one vocabulary shared with the action door, not a
    /// parallel addressing scheme.
    #[test]
    fn a_heading_selection_matches_the_id_the_app_publishes() {
        let (state, ids) = seeded(DOC);
        let heading = ids[0].clone();

        let r = selection_reference(&state.doc, &[(heading.clone(), 0, 3)]).unwrap();

        // What the app publishes for that same heading, through the door.
        let ctx = crate::actions::WpResourceCtx {
            caret_block: heading.clone(),
            caret_offset: 0,
            target_blocks: vec![heading.clone()],
        };
        let published = crate::actions::provide_resources(&ctx, &state.doc);
        let ids_published: Vec<String> = published
            .iter()
            .flat_map(|root| root.children.iter())
            .map(|c| c.id.as_str().to_string())
            .collect();

        assert!(
            ids_published.contains(&r.resource_id().as_str().to_string()),
            "a selection minted `{}`, which the app never publishes: {ids_published:?}",
            r.resource_id().as_str()
        );
        assert_eq!(r.kind, SECTION_KIND);
    }

    /// A caret is not a selection — nothing is being pointed at, so the gesture
    /// is not offered rather than a reference to nothing being minted.
    #[test]
    fn a_bare_caret_is_not_referenceable() {
        let (state, ids) = seeded(DOC);
        assert!(selection_reference(&state.doc, &[(ids[1].clone(), 7, 7)]).is_none());
        assert!(selection_reference(&state.doc, &[]).is_none());
    }

    /// A selection crossing blocks is addressed by the block it starts in.
    #[test]
    fn a_multi_block_selection_is_addressed_by_its_first_block() {
        let (state, ids) = seeded(DOC);
        let selection = vec![(ids[1].clone(), 10, 31), (ids[2].clone(), 0, 4)];
        let r = selection_reference(&state.doc, &selection).unwrap();
        assert_eq!(r.resource_id().as_str(), format!("word-processor.block:{}", ids[1]));
        assert_eq!(r.span, Some(Span::chars(10, 31)));
    }

    /// The label is for people; it carries the document's own title and the
    /// paragraph's ordinal, and it is never what anything resolves on.
    #[test]
    fn the_label_reads_for_a_person_but_is_not_the_address() {
        let (state, ids) = seeded(DOC);
        let r = selection_reference(&state.doc, &[(ids[1].clone(), 0, 5)]).unwrap();
        assert!(r.label.contains("Run sheet"), "label was {:?}", r.label);
        assert!(r.label.contains('¶'), "label was {:?}", r.label);
        assert!(
            !r.label.contains(&ids[1]),
            "the label must not be the address"
        );
    }
}
