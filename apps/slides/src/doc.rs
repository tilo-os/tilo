//! Automerge document model for a Slides deck.
//!
//! The deck lives as a live CRDT tree so two peers editing the same deck
//! converge, and so the app's undo/redo (snapshotting `State::doc`) works
//! for free:
//!
//! ```text
//! root
//! └── "deck" (Map)
//!     ├── "title"   (Text)
//!     ├── "palette" (Str)
//!     └── "slides"  (List)
//!         └── [i] (Map)
//!             ├── "id"         (Str)
//!             ├── "layout"     (Str)
//!             ├── "background" (Str)
//!             ├── "notes"      (Text)
//!             └── "elements"   (List)
//!                 └── [j] (Map)
//!                     ├── "id" / "kind" / "fill" / "stroke" / "align" (Str)
//!                     ├── "x" / "y" / "w" / "h" / "font_size" /
//!                     │   "stroke_width" / "radius"                 (F64)
//!                     ├── "z"                                       (Int)
//!                     ├── "bold" / "italic"                        (Bool)
//!                     └── "body"   (Map, qdoc rich text; text elements)
//! ```
//!
//! A text element's words are rich text: paragraphs with their marks, edited
//! by the SDK editor through [`SlidesAction::EditElementText`].
//!
//! [`SlidesAction::EditElementText`]: crate::actions::SlidesAction::EditElementText
//!
//! "Which slide / element am I editing" is deliberately NOT in the document
//! — it is per-peer view state held by the app widget, so two peers can be
//! on different slides while sharing one deck.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_f64_coerced, read_i64_coerced, read_str, read_str_or_text};

use crate::model::{Background, Deck, Element, ElementKind, Layout, PaletteKind, Slide, TextAlign};
use ui::widget::BlockContent;

/// Sparse order keys keep slide objects stable while their user-visible order
/// changes. A list move would tombstone/recreate the slide Map (and its Text
/// children); changing this scalar preserves those CRDT objects for concurrent
/// text/style edits. The large gap makes ordinary insert/reorder operations a
/// single absolute write.
const ORDER_GAP: i64 = 1_i64 << 32;

pub struct SlidesDoc;

impl SlidesDoc {
    // --- object lookup ------------------------------------------------------

    // ROOT is Automerge's permanent, undeletable root Map, so put_object on
    // it cannot fail short of a corrupted document — there is no meaningful
    // ObjId to fall back to.
    #[allow(
        clippy::expect_used,
        reason = "put_object on ROOT cannot fail short of a corrupted document; see the comment above"
    )]
    fn deck_ensure(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "deck") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "deck", ObjType::Map)
                .expect("put deck"),
        }
    }

    fn deck_get(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "deck") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    // `deck` was just fetched-or-created above via `deck_ensure`, so
    // put_object on it cannot fail short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "put_object on the just-ensured deck Map cannot fail short of a corrupted document"
    )]
    fn slides_list_ensure(doc: &mut AutoCommit) -> ObjId {
        let deck = Self::deck_ensure(doc);
        match doc.get(&deck, "slides") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(&deck, "slides", ObjType::List)
                .expect("put slides"),
        }
    }

    fn slides_list_get(doc: &AutoCommit) -> Option<ObjId> {
        let deck = Self::deck_get(doc)?;
        match doc.get(&deck, "slides") {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
        }
    }

    fn slide_obj(doc: &AutoCommit, idx: usize) -> Option<ObjId> {
        Self::ordered_slides(doc)
            .get(idx)
            .map(|(_, _, object)| object.clone())
    }

    /// Logical deck order is rank then stable id. Existing documents without a
    /// rank migrate lazily and deterministically from their physical list
    /// order; ties from concurrent rank writes converge by immutable slide id.
    fn ordered_slides(doc: &AutoCommit) -> Vec<(i64, String, ObjId)> {
        let Some(slides) = Self::slides_list_get(doc) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        for physical in 0..doc.length(&slides) {
            if let Ok(Some((Value::Object(ObjType::Map), object))) = doc.get(&slides, physical) {
                let id = read_str(doc, &object, "id").unwrap_or_default();
                let rank = read_i64_coerced(doc, &object, "order")
                    .unwrap_or_else(|| (physical as i64).saturating_mul(ORDER_GAP));
                entries.push((rank, id, object));
            }
        }
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        entries
    }

    fn slide_physical_index(doc: &AutoCommit, target: &ObjId) -> Option<usize> {
        let slides = Self::slides_list_get(doc)?;
        (0..doc.length(&slides)).find(|physical| {
            matches!(doc.get(&slides, *physical), Ok(Some((Value::Object(ObjType::Map), object))) if object == *target)
        })
    }

    fn rank_for_slot(doc: &mut AutoCommit, slot: usize, omit: Option<&str>) -> i64 {
        let mut entries = Self::ordered_slides(doc)
            .into_iter()
            .filter(|(_, id, _)| Some(id.as_str()) != omit)
            .collect::<Vec<_>>();
        let slot = slot.min(entries.len());
        let previous = slot
            .checked_sub(1)
            .and_then(|i| entries.get(i).map(|e| e.0));
        let next = entries.get(slot).map(|e| e.0);
        match (previous, next) {
            (None, Some(next)) if next > i64::MIN + ORDER_GAP => next - ORDER_GAP,
            (Some(previous), None) if previous < i64::MAX - ORDER_GAP => previous + ORDER_GAP,
            (Some(previous), Some(next)) if next.saturating_sub(previous) > 1 => {
                previous + (next - previous) / 2
            }
            (None, None) => 0,
            _ => {
                // A very dense run is rare. Rebalance only the scalar rank
                // metadata, never slide/text objects, then recompute.
                for (index, (current_rank, _, object)) in entries.iter_mut().enumerate() {
                    let rank = (index as i64).saturating_mul(ORDER_GAP);
                    let _ = doc.put(object, "order", rank);
                    *current_rank = rank;
                }
                let previous = slot
                    .checked_sub(1)
                    .and_then(|i| entries.get(i).map(|e| e.0));
                let next = entries.get(slot).map(|e| e.0);
                match (previous, next) {
                    (None, Some(next)) => next - ORDER_GAP / 2,
                    (Some(previous), None) => previous + ORDER_GAP,
                    (Some(previous), Some(next)) => previous + (next - previous) / 2,
                    (None, None) => 0,
                }
            }
        }
    }

    /// The slides' order keys, in deck order: what an author computes the
    /// key of an inserted or moved slide from
    /// ([`rank_for_insert`], [`rank_for_move`]).
    pub fn slide_ranks(doc: &AutoCommit) -> Vec<i64> {
        Self::ordered_slides(doc)
            .into_iter()
            .map(|(rank, _, _)| rank)
            .collect()
    }

    /// Set a slide's already-authored absolute order key without replacing its
    /// CRDT Map or any nested Text objects. Returns false when the stable id
    /// has been deleted before this action arrived.
    pub fn set_slide_order(doc: &mut AutoCommit, id: &str, rank: i64) -> bool {
        let Some((_, _, object)) = Self::ordered_slides(doc)
            .into_iter()
            .find(|(_, candidate, _)| candidate == id)
        else {
            return false;
        };
        doc.put(&object, "order", rank).is_ok()
    }

    #[cfg(test)]
    pub(crate) fn slide_order(doc: &AutoCommit, id: &str) -> Option<i64> {
        Self::ordered_slides(doc)
            .into_iter()
            .find(|(_, candidate, _)| candidate == id)
            .map(|(rank, _, _)| rank)
    }

    fn elements_list(doc: &mut AutoCommit, slide_idx: usize) -> Option<ObjId> {
        let slide = Self::slide_obj(doc, slide_idx)?;
        Some(match doc.get(&slide, "elements") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc.put_object(&slide, "elements", ObjType::List).ok()?,
        })
    }

    fn element_obj(doc: &AutoCommit, slide_idx: usize, elem_id: &str) -> Option<ObjId> {
        let slide = Self::slide_obj(doc, slide_idx)?;
        let Ok(Some((Value::Object(ObjType::List), elements))) = doc.get(&slide, "elements") else {
            return None;
        };
        let n = doc.length(&elements);
        for j in 0..n {
            if let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&elements, j)
                && read_str(doc, &e, "id").as_deref() == Some(elem_id)
            {
                return Some(e);
            }
        }
        None
    }

    // --- low-level scalar readers ------------------------------------------

    /// True only for a genuinely uninitialized resource, not an authored empty deck.
    pub fn is_uninitialized(doc: &AutoCommit) -> bool {
        matches!(doc.get(ROOT, "deck"), Ok(None))
    }

    pub fn is_empty(doc: &AutoCommit) -> bool {
        match Self::slides_list_get(doc) {
            Some(list) => doc.length(&list) == 0,
            None => true,
        }
    }

    // --- writers ------------------------------------------------------------

    // `obj` is always a Map already live in the document (deck / slide /
    // element) when this is called, so put_object on it cannot fail short of
    // a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "put_object on a live document object cannot fail short of a corrupted document"
    )]
    fn write_text(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &str) {
        let tid = match doc.get(obj, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc.put_object(obj, key, ObjType::Text).expect("put text"),
        };
        ui::doc::splice_text_diffed(doc, &tid, value);
    }

    fn write_element(doc: &mut AutoCommit, obj: &ObjId, el: &Element) {
        let _ = doc.put(obj, "id", el.id.as_str());
        let _ = doc.put(obj, "kind", el.kind.as_str());
        let _ = doc.put(obj, "x", el.x);
        let _ = doc.put(obj, "y", el.y);
        let _ = doc.put(obj, "w", el.w);
        let _ = doc.put(obj, "h", el.h);
        let _ = doc.put(obj, "fill", el.fill.as_str());
        let _ = doc.put(obj, "stroke", el.stroke.as_str());
        let _ = doc.put(obj, "stroke_width", el.stroke_width);
        let _ = doc.put(obj, "font_size", el.font_size);
        let _ = doc.put(obj, "align", el.align.as_str());
        let _ = doc.put(obj, "bold", el.bold);
        let _ = doc.put(obj, "italic", el.italic);
        let _ = doc.put(obj, "radius", el.radius);
        let _ = doc.put(obj, "z", el.z);
        let _ = doc.put(obj, "src", el.src.as_str());
        if el.kind == ElementKind::Text {
            // The box's bold and italic travel as marks in the body.
            let _ = doc.put(obj, "bold", false);
            let _ = doc.put(obj, "italic", false);
            Self::write_body(doc, obj, &el.rich_blocks());
        }
    }

    /// Write `blocks` as the element's rich-text body, replacing any before.
    fn write_body(doc: &mut AutoCommit, element: &ObjId, blocks: &[BlockContent]) {
        let _ = doc.delete(element, "text");
        let Ok(body) = doc.put_object(element, "body", ObjType::Map) else {
            return;
        };
        let written = tilo_document::RichText::initialize_under(doc, &body).is_ok()
            && tilo_rich_text::fill_under(doc, &body, blocks).is_ok();
        if !written {
            tracing::warn!("text element body could not be written");
        }
    }

    /// The rich-text body of element `elem_id` on slide `slide_id`.
    pub fn element_body(doc: &AutoCommit, slide_id: &str, elem_id: &str) -> Option<ObjId> {
        let slide = Self::ordered_slides(doc)
            .into_iter()
            .find(|(_, id, _)| id == slide_id)
            .map(|(_, _, object)| object)?;
        let Ok(Some((Value::Object(ObjType::List), elements))) = doc.get(&slide, "elements") else {
            return None;
        };
        (0..doc.length(&elements)).find_map(|j| {
            let Ok(Some((Value::Object(ObjType::Map), element))) = doc.get(&elements, j) else {
                return None;
            };
            if read_str(doc, &element, "id").as_deref() != Some(elem_id) {
                return None;
            }
            match doc.get(&element, "body") {
                Ok(Some((Value::Object(ObjType::Map), body))) => Some(body),
                _ => None,
            }
        })
    }

    /// Set or clear mark `name` over every word of element `elem_id`'s body.
    /// Returns whether the element has a body to mark.
    pub fn mark_whole_body(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        name: &str,
        on: bool,
    ) -> bool {
        let Some((body, containers)) = Self::element_body_at(doc, slide_idx, elem_id) else {
            return false;
        };
        let blocks =
            tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body);
        let spans: Vec<tilo_document::action::Span> = blocks
            .iter()
            .filter(|block| !block.text.is_empty())
            .map(|block| tilo_document::action::Span {
                block: block.id.clone(),
                start: 0,
                end: block.text.chars().count(),
            })
            .collect();
        if spans.is_empty() {
            return true;
        }
        let action = tilo_document::RichTextAction::SetMark {
            region: tilo_document::action::RegionKind::Body,
            spans,
            name: name.to_string(),
            value: on.then_some(tilo_document::action::MarkVal::Bool(true)),
            also_clear: Vec::new(),
        };
        action.apply_under(doc, &body).is_ok()
    }

    // `obj` is the slide Map the caller just created/fetched, and `elements`
    // is that same Map's own list, so both put_object and insert_object below
    // cannot fail short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "put_object/insert_object on a live document object cannot fail short of a corrupted document"
    )]
    fn write_slide(doc: &mut AutoCommit, obj: &ObjId, slide: &Slide) {
        let _ = doc.put(obj, "id", slide.id.as_str());
        let _ = doc.put(obj, "layout", slide.layout.as_str());
        let _ = doc.put(obj, "background", slide.background.as_str());
        Self::write_text(doc, obj, "notes", &slide.notes);
        let elements = match doc.get(obj, "elements") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(obj, "elements", ObjType::List)
                .expect("put elements"),
        };
        // Fresh write: clear any existing then append in order.
        while doc.length(&elements) > 0 {
            let _ = doc.delete(&elements, 0);
        }
        for (j, el) in slide.elements.iter().enumerate() {
            let e = doc
                .insert_object(&elements, j, ObjType::Map)
                .expect("insert element");
            Self::write_element(doc, &e, el);
        }
    }

    /// Seed an (assumed empty) document with a whole deck.
    ///
    /// # Panics
    ///
    /// Never in practice: `slides` is the List just ensured above and `i`
    /// never exceeds its current length, so `insert_object` cannot fail
    /// short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "insert_object at a valid in-range index on the just-ensured slides List cannot fail short of a corrupted document"
    )]
    pub fn seed(doc: &mut AutoCommit, deck: &Deck) {
        let deck_obj = Self::deck_ensure(doc);
        Self::write_text(doc, &deck_obj, "title", &deck.title);
        let _ = doc.put(&deck_obj, "palette", deck.palette.as_str());
        let slides = Self::slides_list_ensure(doc);
        while doc.length(&slides) > 0 {
            let _ = doc.delete(&slides, 0);
        }
        for (i, s) in deck.slides.iter().enumerate() {
            let so = doc
                .insert_object(&slides, i, ObjType::Map)
                .expect("insert slide");
            Self::write_slide(doc, &so, s);
            let _ = doc.put(&so, "order", (i as i64).saturating_mul(ORDER_GAP));
        }
    }

    // --- targeted live edits ------------------------------------------------

    pub fn set_title(doc: &mut AutoCommit, title: &str) {
        let deck = Self::deck_ensure(doc);
        Self::write_text(doc, &deck, "title", title);
    }

    /// Splice into the deck title's text. `char_pos`/`del` are char counts —
    /// the live-edit path for the inline title rename (mirrors `splice_notes`).
    ///
    /// # Panics
    ///
    /// Never in practice: `deck` was just fetched-or-created above, so
    /// `put_object` on it cannot fail short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "put_object on the just-ensured deck Map cannot fail short of a corrupted document"
    )]
    pub fn splice_title(doc: &mut AutoCommit, char_pos: usize, del: usize, ins: &str) {
        let deck = Self::deck_ensure(doc);
        let tid = match doc.get(&deck, "title") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&deck, "title", ObjType::Text)
                .expect("put title"),
        };
        let _ = doc.splice_text(&tid, char_pos, del as isize, ins);
    }

    pub fn set_palette(doc: &mut AutoCommit, p: PaletteKind) {
        let deck = Self::deck_ensure(doc);
        let _ = doc.put(&deck, "palette", p.as_str());
    }

    pub fn set_slide_layout(doc: &mut AutoCommit, slide_idx: usize, layout: Layout) {
        if let Some(s) = Self::slide_obj(doc, slide_idx) {
            let _ = doc.put(&s, "layout", layout.as_str());
        }
    }

    pub fn set_slide_background(doc: &mut AutoCommit, slide_idx: usize, bg: Background) {
        if let Some(s) = Self::slide_obj(doc, slide_idx) {
            let _ = doc.put(&s, "background", bg.as_str());
        }
    }

    /// Splice into a slide's speaker notes. `char_pos`/`del` are char counts.
    ///
    /// # Panics
    ///
    /// Never in practice: `s` was just resolved to a live slide Map, so
    /// `put_object` on it cannot fail short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "put_object on a live slide Map cannot fail short of a corrupted document"
    )]
    pub fn splice_notes(
        doc: &mut AutoCommit,
        slide_idx: usize,
        char_pos: usize,
        del: usize,
        ins: &str,
    ) {
        if let Some(s) = Self::slide_obj(doc, slide_idx) {
            let tid = match doc.get(&s, "notes") {
                Ok(Some((Value::Object(ObjType::Text), id))) => id,
                _ => doc
                    .put_object(&s, "notes", ObjType::Text)
                    .expect("put notes"),
            };
            let _ = doc.splice_text(&tid, char_pos, del as isize, ins);
        }
    }

    /// The text of slide `slide_id`'s speaker notes, as its editor and its
    /// edits find it.
    pub fn notes_text(doc: &AutoCommit, slide_id: &str) -> Option<ObjId> {
        let slide = Self::slide_by_id(doc, slide_id)?;
        match doc.get(&slide, "notes") {
            Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
            _ => None,
        }
    }

    /// The text of slide `slide_id`'s speaker notes, created when the slide
    /// has none yet: where an edit of them applies.
    pub fn notes_text_for_edit(doc: &mut AutoCommit, slide_id: &str) -> Option<ObjId> {
        if let Some(notes) = Self::notes_text(doc, slide_id) {
            return Some(notes);
        }
        let slide = Self::slide_by_id(doc, slide_id)?;
        doc.put_object(&slide, "notes", ObjType::Text).ok()
    }

    fn slide_by_id(doc: &AutoCommit, slide_id: &str) -> Option<ObjId> {
        Self::ordered_slides(doc)
            .into_iter()
            .find(|(_, id, _)| id == slide_id)
            .map(|(_, _, object)| object)
    }

    pub fn set_element_pos(doc: &mut AutoCommit, slide_idx: usize, elem_id: &str, x: f64, y: f64) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let _ = doc.put(&e, "x", x);
            let _ = doc.put(&e, "y", y);
        }
    }

    pub fn set_element_rect(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    ) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let _ = doc.put(&e, "x", x);
            let _ = doc.put(&e, "y", y);
            let _ = doc.put(&e, "w", w);
            let _ = doc.put(&e, "h", h);
        }
    }

    pub fn set_element_str(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        key: &str,
        value: &str,
    ) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let _ = doc.put(&e, key, value);
        }
    }

    pub fn set_element_f64(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        key: &str,
        value: f64,
    ) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let _ = doc.put(&e, key, value);
        }
    }

    pub fn set_element_bool(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        key: &str,
        value: bool,
    ) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let _ = doc.put(&e, key, value);
        }
    }

    pub fn set_element_z(doc: &mut AutoCommit, slide_idx: usize, elem_id: &str, z: i64) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let _ = doc.put(&e, "z", z);
        }
    }

    /// Splice the words of text element `elem_id`, as a collaborator
    /// typing into it: `char_pos` counts characters of its plain text, a
    /// line per paragraph, and the splice lands in the paragraph there.
    pub fn splice_element_text(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        char_pos: usize,
        del: usize,
        ins: &str,
    ) {
        let Some((body, containers)) = Self::element_body_at(doc, slide_idx, elem_id) else {
            return;
        };
        let blocks =
            tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body);
        let mut at = char_pos;
        let Some((block, offset)) = blocks.iter().enumerate().find_map(|(index, block)| {
            let len = block.text.chars().count();
            if at <= len || index + 1 == blocks.len() {
                Some((block.id.clone(), at.min(len)))
            } else {
                at -= len + 1;
                None
            }
        }) else {
            return;
        };
        let action = tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block,
            at: offset,
            del,
            ins: ins.to_string(),
        };
        let _ = action.apply_under(doc, &body);
    }

    /// The body of element `elem_id` on slide `slide_idx`, and its containers.
    fn element_body_at(
        doc: &AutoCommit,
        slide_idx: usize,
        elem_id: &str,
    ) -> Option<(ObjId, tilo_doc::model::Containers)> {
        let element = Self::element_obj(doc, slide_idx, elem_id)?;
        let Ok(Some((Value::Object(ObjType::Map), body))) = doc.get(&element, "body") else {
            return None;
        };
        let containers = tilo_doc::model::open(doc, &body).ok()?;
        Some((body, containers))
    }

    pub fn add_element(doc: &mut AutoCommit, slide_idx: usize, el: &Element) {
        if let Some(elements) = Self::elements_list(doc, slide_idx) {
            let n = doc.length(&elements);
            if let Ok(e) = doc.insert_object(&elements, n, ObjType::Map) {
                Self::write_element(doc, &e, el);
            }
        }
    }

    pub fn remove_element(doc: &mut AutoCommit, slide_idx: usize, elem_id: &str) {
        if let Some(slide) = Self::slide_obj(doc, slide_idx)
            && let Ok(Some((Value::Object(ObjType::List), elements))) = doc.get(&slide, "elements")
        {
            let n = doc.length(&elements);
            for j in 0..n {
                if let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&elements, j)
                    && read_str(doc, &e, "id").as_deref() == Some(elem_id)
                {
                    let _ = doc.delete(&elements, j);
                    return;
                }
            }
        }
    }

    /// Insert a slide at `at` (clamped). Returns nothing; caller refreshes.
    pub fn insert_slide(doc: &mut AutoCommit, at: usize, slide: &Slide) {
        let rank = Self::rank_for_slot(doc, at, None);
        Self::insert_slide_with_order(doc, slide, rank);
    }

    /// Insert a slide with the rank already authored into its action payload.
    /// It appends physically so existing CRDT list entries keep their identity;
    /// user-visible order is exclusively the absolute rank plus stable id.
    pub fn insert_slide_with_order(doc: &mut AutoCommit, slide: &Slide, rank: i64) {
        let slides = Self::slides_list_ensure(doc);
        let n = doc.length(&slides);
        // Physical list placement is irrelevant once every entry has a stable
        // rank, so append without shifting unrelated list identities.
        if let Ok(so) = doc.insert_object(&slides, n, ObjType::Map) {
            Self::write_slide(doc, &so, slide);
            let _ = doc.put(&so, "order", rank);
        }
    }

    pub fn remove_slide(doc: &mut AutoCommit, idx: usize) {
        if let (Some(slides), Some(object)) =
            (Self::slides_list_get(doc), Self::slide_obj(doc, idx))
            && let Some(physical) = Self::slide_physical_index(doc, &object)
        {
            let _ = doc.delete(&slides, physical);
        }
    }

    /// Move the slide at `from` to `to` without replacing its CRDT Map or its
    /// nested Text objects. The logical order is independent rank metadata;
    /// this is therefore one scalar operation and concurrent text edits to the
    /// moved slide retain their original object targets.
    pub fn move_slide(doc: &mut AutoCommit, from: usize, to: usize) {
        let Some(id) = Self::ordered_slides(doc)
            .get(from)
            .map(|(_, id, _)| id.clone())
        else {
            return;
        };
        if let Some(rank) = rank_for_move(&Self::slide_ranks(doc), from, to) {
            let _ = Self::set_slide_order(doc, &id, rank);
        }
    }

    // --- reader -------------------------------------------------------------

    pub fn load(doc: &AutoCommit) -> Option<Deck> {
        let deck = Self::deck_get(doc)?;
        let title = read_str_or_text(doc, &deck, "title").unwrap_or_else(|| "Untitled deck".into());
        let palette = PaletteKind::from_str(
            &read_str(doc, &deck, "palette").unwrap_or_else(|| "indigo".into()),
        );
        let Ok(Some((Value::Object(ObjType::List), _slides_list))) = doc.get(&deck, "slides")
        else {
            return None;
        };
        let mut slides = Vec::new();
        for (_, _, so) in Self::ordered_slides(doc) {
            let id = read_str(doc, &so, "id").unwrap_or_else(crate::model::new_id);
            let layout = Layout::from_str(
                &read_str(doc, &so, "layout").unwrap_or_else(|| "title-content".into()),
            );
            let background = Background::from_str(
                &read_str(doc, &so, "background").unwrap_or_else(|| "surface".into()),
            );
            let notes = read_str_or_text(doc, &so, "notes").unwrap_or_default();
            let mut elements = Vec::new();
            if let Ok(Some((Value::Object(ObjType::List), el_list))) = doc.get(&so, "elements") {
                for j in 0..doc.length(&el_list) {
                    let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&el_list, j) else {
                        continue;
                    };
                    let body = match doc.get(&e, "body") {
                        Ok(Some((Value::Object(ObjType::Map), body))) => {
                            tilo_rich_text::blocks_under(doc, &body).ok()
                        }
                        _ => None,
                    };
                    let mut element = Element {
                        id: read_str(doc, &e, "id").unwrap_or_else(crate::model::new_id),
                        kind: ElementKind::from_str(
                            &read_str(doc, &e, "kind").unwrap_or_else(|| "text".into()),
                        ),
                        x: read_f64_coerced(doc, &e, "x").unwrap_or(0.0),
                        y: read_f64_coerced(doc, &e, "y").unwrap_or(0.0),
                        w: read_f64_coerced(doc, &e, "w").unwrap_or(200.0),
                        h: read_f64_coerced(doc, &e, "h").unwrap_or(80.0),
                        text: read_str_or_text(doc, &e, "text").unwrap_or_default(),
                        src: read_str(doc, &e, "src").unwrap_or_default(),
                        fill: read_str(doc, &e, "fill").unwrap_or_else(|| "text".into()),
                        stroke: read_str(doc, &e, "stroke").unwrap_or_else(|| "none".into()),
                        stroke_width: read_f64_coerced(doc, &e, "stroke_width").unwrap_or(0.0),
                        font_size: read_f64_coerced(doc, &e, "font_size").unwrap_or(32.0),
                        align: TextAlign::from_str(
                            &read_str(doc, &e, "align").unwrap_or_else(|| "left".into()),
                        ),
                        bold: read_bool(doc, &e, "bold").unwrap_or(false),
                        italic: read_bool(doc, &e, "italic").unwrap_or(false),
                        radius: read_f64_coerced(doc, &e, "radius").unwrap_or(0.0),
                        z: read_i64_coerced(doc, &e, "z").unwrap_or(0),
                        blocks: Vec::new(),
                    };
                    if let Some(blocks) = body {
                        element.set_blocks(blocks);
                    }
                    elements.push(element);
                }
            }
            slides.push(Slide {
                id,
                layout,
                background,
                notes,
                elements,
            });
        }
        Some(Deck {
            title,
            palette,
            slides,
        })
    }
}

// ===========================================================================
// Field readers
// ===========================================================================

/// The one absolute order key for moving the slide at `from` to `to`, among
/// slides whose keys in deck order are `ranks`. Computed once by the author:
/// the action carries this value to every replica rather than asking each
/// receiver to derive a destination from its (possibly concurrently changed)
/// local order.
pub fn rank_for_move(ranks: &[i64], from: usize, to: usize) -> Option<i64> {
    if from >= ranks.len() || to >= ranks.len() || from == to {
        return None;
    }
    let remaining: Vec<i64> = ranks
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != from)
        .map(|(_, rank)| *rank)
        .collect();
    Some(rank_between(&remaining, to))
}

/// The absolute key of a slide inserted at slot `at` among slides whose keys
/// in deck order are `ranks`.
pub fn rank_for_insert(ranks: &[i64], at: usize) -> i64 {
    rank_between(ranks, at)
}

fn rank_between(ranks: &[i64], slot: usize) -> i64 {
    let slot = slot.min(ranks.len());
    let previous = slot
        .checked_sub(1)
        .and_then(|index| ranks.get(index).copied());
    let next = ranks.get(slot).copied();
    match (previous, next) {
        (None, Some(next)) => next.saturating_sub(ORDER_GAP),
        (Some(previous), None) => previous.saturating_add(ORDER_GAP),
        (Some(previous), Some(next)) if next.saturating_sub(previous) > 1 => {
            previous + (next - previous) / 2
        }
        // Dense order keys remain deterministic. Equal ranks are sorted by
        // immutable slide id, so this is still a convergent absolute
        // operation.
        (Some(previous), Some(_)) => previous,
        (None, None) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ElementKind, seed_deck};
    use tilo_automerge::ApplyIncoming;

    /// One slide `slide-1` with a two-line bold title `title`, an italic
    /// body `body` and a shape `box`.
    fn text_deck() -> AutoCommit {
        let mut slide = Slide::new(Layout::TitleContent);
        slide.id = "slide-1".into();
        for (id, kind, text, bold, italic) in [
            (
                "title",
                ElementKind::Text,
                "Quarterly review\nFirst line",
                true,
                false,
            ),
            ("body", ElementKind::Text, "plain words", false, true),
            ("box", ElementKind::Rect, "", false, false),
        ] {
            let mut element = if kind == ElementKind::Text {
                Element::text(text, 10.0, 10.0, 400.0, 80.0, 32.0)
            } else {
                Element::shape(kind, 10.0, 10.0, 400.0, 80.0)
            };
            element.id = id.into();
            element.bold = bold;
            element.italic = italic;
            slide.elements.push(element);
        }
        let mut doc = AutoCommit::new();
        SlidesDoc::seed(
            &mut doc,
            &Deck {
                title: "Old deck".into(),
                palette: PaletteKind::Indigo,
                slides: vec![slide],
            },
        );
        doc.commit();
        doc
    }

    #[test]
    fn a_text_box_is_edited_by_rich_text_actions() {
        use tilo_actions::Action as _;
        let mut doc = text_deck();
        let body = SlidesDoc::element_body(&doc, "slide-1", "body").unwrap();
        let state = tilo_document::RichText::read_under(&doc, &body).unwrap();
        let block = state.body().id_at(0).unwrap().to_owned();
        crate::actions::SlidesAction::EditElementText {
            slide_id: "slide-1".into(),
            element_id: "body".into(),
            edit: tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: "Some ".into(),
            },
        }
        .apply(&mut doc)
        .unwrap();
        let deck = SlidesDoc::load(&doc).unwrap();
        assert_eq!(deck.slides[0].elements[1].text, "Some plain words");
        // An element that is not there is refused, and nothing is written.
        let heads = doc.get_heads();
        assert!(
            crate::actions::SlidesAction::EditElementText {
                slide_id: "slide-1".into(),
                element_id: "gone".into(),
                edit: tilo_document::RichTextAction::AppendParagraph { text: "x".into() },
            }
            .apply(&mut doc)
            .is_err()
        );
        assert_eq!(doc.get_heads(), heads);
    }

    #[test]
    fn set_slide_text_rewrites_a_slides_title_and_body_by_number_or_id() {
        use tilo_actions::Action as _;
        let mut doc = text_deck();
        let set = |slide: &str, title: Option<&str>, body: Option<&str>| {
            crate::actions::SlidesAction::SetSlideText {
                slide: slide.into(),
                title: title.map(Into::into),
                body: body.map(Into::into),
            }
        };
        set("1", Some("Launch *plan*"), Some("Goals\n# Timeline\n"))
            .apply(&mut doc)
            .unwrap();
        let deck = SlidesDoc::load(&doc).unwrap();
        let texts: Vec<&str> = deck.slides[0]
            .elements
            .iter()
            .take(2)
            .map(|element| element.text.as_str())
            .collect();
        assert_eq!(texts, ["Launch *plan*", "Goals\n# Timeline"]);

        set("slide-1", None, Some("Only the body"))
            .apply(&mut doc)
            .unwrap();
        let deck = SlidesDoc::load(&doc).unwrap();
        assert_eq!(deck.slides[0].elements[0].text, "Launch *plan*");
        assert_eq!(deck.slides[0].elements[1].text, "Only the body");

        // A slide that is not there, or empty text, writes nothing.
        let heads = doc.get_heads();
        assert!(set("9", Some("x"), None).apply(&mut doc).is_err());
        assert!(set("1", Some("x"), Some(" ")).apply(&mut doc).is_err());
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(
            SlidesDoc::load(&doc).unwrap().slides[0].elements[0].text,
            "Launch *plan*"
        );
    }

    /// Deck round-trips, bounded text writes, positions and titles persist.
    #[test]
    fn deck_doc_round_trips_and_writes_bounded_text() {
        // Case `write_text_append_one_char_is_bounded`.
        {
            // `write_text` is the production text-write path for every deck string
            // (title / notes / element text). Appending one char must splice only
            // the new char, not rewrite the whole value.
            let mut doc = AutoCommit::new();
            let obj = doc.put_object(&ROOT, "scratch", ObjType::Map).unwrap();
            let long = "t".repeat(300);
            SlidesDoc::write_text(&mut doc, &obj, "notes", &long);
            let before = doc.save().len();
            let mut longer = long.clone();
            longer.push('!');
            SlidesDoc::write_text(&mut doc, &obj, "notes", &longer);
            let grew = doc.save().len() - before;
            let Ok(Some((Value::Object(ObjType::Text), tid))) = doc.get(&obj, "notes") else {
                panic!("notes text missing")
            };
            assert_eq!(doc.text(&tid).unwrap(), longer, "value round-trips");
            assert!(grew < 40, "one-char text edit grew doc by {grew} bytes");
        }

        // Case `write_text_repeated_store_keeps_doc_bounded`.
        {
            let mut doc = AutoCommit::new();
            let obj = doc.put_object(&ROOT, "scratch", ObjType::Map).unwrap();
            let long = "s".repeat(300);
            SlidesDoc::write_text(&mut doc, &obj, "notes", &long);
            let a = doc.save().len();
            for _ in 0..50 {
                SlidesDoc::write_text(&mut doc, &obj, "notes", &long);
            }
            let b = doc.save().len();
            assert_eq!(
                a, b,
                "re-storing the same text grew the document {a} -> {b}"
            );
        }

        // Case `deck_round_trips_through_crdt`.
        {
            let mut doc = AutoCommit::new();
            let deck = seed_deck();
            SlidesDoc::seed(&mut doc, &deck);
            let loaded = SlidesDoc::load(&doc).expect("deck loads");
            assert_eq!(loaded.slides.len(), deck.slides.len());
            assert_eq!(loaded.title, deck.title);
            assert_eq!(loaded.palette, deck.palette);
            // First slide first element survives with its kind + geometry.
            let a = &deck.slides[0].elements[0];
            let b = &loaded.slides[0].elements[0];
            assert_eq!(a.kind, b.kind);
            assert!((a.x - b.x).abs() < 0.001);
        }

        // Case `live_pos_edit_persists`.
        {
            let mut doc = AutoCommit::new();
            let deck = seed_deck();
            SlidesDoc::seed(&mut doc, &deck);
            let eid = deck.slides[1].elements[0].id.clone();
            SlidesDoc::set_element_pos(&mut doc, 1, &eid, 11.0, 22.0);
            let loaded = SlidesDoc::load(&doc).unwrap();
            let el = loaded.slides[1].element(&eid).unwrap();
            assert!((el.x - 11.0).abs() < 0.001);
            assert!((el.y - 22.0).abs() < 0.001);
        }

        // Case `set_title_and_splice_title_persist`.
        {
            let mut doc = AutoCommit::new();
            SlidesDoc::seed(&mut doc, &seed_deck());
            // Full replace via set_title.
            SlidesDoc::set_title(&mut doc, "Quarterly review");
            assert_eq!(SlidesDoc::load(&doc).unwrap().title, "Quarterly review");
            // Char-level splice (the inline-rename path): insert " 2026" at the end.
            let at = "Quarterly review".chars().count();
            SlidesDoc::splice_title(&mut doc, at, 0, " 2026");
            assert_eq!(
                SlidesDoc::load(&doc).unwrap().title,
                "Quarterly review 2026"
            );
            // Delete the first char.
            SlidesDoc::splice_title(&mut doc, 0, 1, "");
            assert_eq!(SlidesDoc::load(&doc).unwrap().title, "uarterly review 2026");
        }
    }

    /// Element/slide insert-remove, moves and reorder with concurrent text edits.
    #[test]
    fn deck_structure_edits_persist_and_survive_concurrency() {
        // Case `add_and_remove_element`.
        {
            let mut doc = AutoCommit::new();
            SlidesDoc::seed(&mut doc, &seed_deck());
            let before = SlidesDoc::load(&doc).unwrap().slides[0].elements.len();
            let el = Element::shape(ElementKind::Ellipse, 10.0, 10.0, 50.0, 50.0);
            let id = el.id.clone();
            SlidesDoc::add_element(&mut doc, 0, &el);
            assert_eq!(
                SlidesDoc::load(&doc).unwrap().slides[0].elements.len(),
                before + 1
            );
            SlidesDoc::remove_element(&mut doc, 0, &id);
            assert_eq!(
                SlidesDoc::load(&doc).unwrap().slides[0].elements.len(),
                before
            );
        }

        // Case `insert_and_remove_slide`.
        {
            let mut doc = AutoCommit::new();
            SlidesDoc::seed(&mut doc, &seed_deck());
            let n = SlidesDoc::load(&doc).unwrap().slides.len();
            SlidesDoc::insert_slide(&mut doc, 1, &Slide::new(Layout::Blank));
            assert_eq!(SlidesDoc::load(&doc).unwrap().slides.len(), n + 1);
            SlidesDoc::remove_slide(&mut doc, 1);
            assert_eq!(SlidesDoc::load(&doc).unwrap().slides.len(), n);
        }

        // Case `move_slide_reorders`.
        {
            let mut doc = AutoCommit::new();
            SlidesDoc::seed(&mut doc, &seed_deck());
            let deck = SlidesDoc::load(&doc).unwrap();
            let first = deck.slides[0].id.clone();
            let unaffected_object = SlidesDoc::slide_obj(&doc, 1).expect("second slide object");
            SlidesDoc::move_slide(&mut doc, 0, 2);
            let after = SlidesDoc::load(&doc).unwrap();
            assert_eq!(after.slides[2].id, first);
            assert_eq!(
                SlidesDoc::slide_obj(&doc, 0),
                Some(unaffected_object),
                "moving one slide leaves every unrelated CRDT slide object live"
            );
        }

        // Case `reorder_and_concurrent_text_edit_preserve_the_same_slide_object`.
        {
            let mut a = AutoCommit::new();
            SlidesDoc::seed(&mut a, &seed_deck());
            a.commit();
            let bytes = a.save();
            let mut b = AutoCommit::load(&bytes).expect("replica loads");

            let before = SlidesDoc::load(&a).expect("seeded deck");
            let moved_slide = before.slides[1].id.clone();
            let element = before.slides[1].elements[0].id.clone();
            let original = before.slides[1].elements[0].text.clone();

            SlidesDoc::move_slide(&mut a, 1, 3);
            SlidesDoc::splice_element_text(
                &mut b,
                1,
                &element,
                original.chars().count(),
                0,
                " + collaboration",
            );
            a.merge_incoming(&mut b).expect("merge into a");
            b.merge_incoming(&mut a).expect("merge into b");

            let left = SlidesDoc::load(&a).expect("merged deck");
            let right = SlidesDoc::load(&b).expect("merged deck");
            assert_eq!(left, right, "both merge directions converge");
            let index = left
                .slides
                .iter()
                .position(|slide| slide.id == moved_slide)
                .expect("moved slide remains live");
            assert_eq!(index, 3, "rank metadata preserved the reorder");
            assert_eq!(
                left.slides[index]
                    .element(&element)
                    .expect("same element")
                    .text,
                original + " + collaboration",
                "the concurrent text splice survived the reorder"
            );
        }
    }
}
