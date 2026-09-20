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
//!                     └── "text"                                   (Text)
//! ```
//!
//! "Which slide / element am I editing" is deliberately NOT in the document
//! — it is per-peer view state held by the app widget, so two peers can be
//! on different slides while sharing one deck.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{Background, Deck, Element, ElementKind, Layout, PaletteKind, Slide, TextAlign};

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
                let id = str_field(doc, &object, "id").unwrap_or_default();
                let rank = i64_field(doc, &object, "order")
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

    /// Produce the one absolute order key for a user-authored reorder.  This
    /// is deliberately a read-only calculation: an encoded action must carry
    /// this value to every replica rather than asking each receiver to derive
    /// a destination from its (possibly concurrently changed) local order.
    pub fn rank_for_move(doc: &AutoCommit, from: usize, to: usize) -> Option<i64> {
        let entries = Self::ordered_slides(doc);
        if from >= entries.len() || to >= entries.len() || from == to {
            return None;
        }
        let moved_id = entries.get(from)?.1.clone();
        let remaining = entries
            .into_iter()
            .filter(|(_, id, _)| id != &moved_id)
            .collect::<Vec<_>>();
        let slot = to.min(remaining.len());
        let previous = slot
            .checked_sub(1)
            .and_then(|index| remaining.get(index).map(|entry| entry.0));
        let next = remaining.get(slot).map(|entry| entry.0);
        Some(match (previous, next) {
            (None, Some(next)) => next.saturating_sub(ORDER_GAP),
            (Some(previous), None) => previous.saturating_add(ORDER_GAP),
            (Some(previous), Some(next)) if next.saturating_sub(previous) > 1 => {
                previous + (next - previous) / 2
            }
            // Dense order keys remain deterministic.  Equal ranks are sorted
            // by immutable slide id, so this is still a convergent absolute
            // operation; a later local insert can rebalance scalar metadata.
            (Some(previous), Some(_)) => previous,
            (None, None) => 0,
        })
    }

    /// Produce the absolute sparse key for an insertion at this authored
    /// logical slot. This does not mutate the document; callers put the rank
    /// into the durable action so a receiver never derives it from a changed
    /// local slide list.
    pub fn rank_for_insert(doc: &AutoCommit, at: usize) -> i64 {
        let entries = Self::ordered_slides(doc);
        let slot = at.min(entries.len());
        let previous = slot
            .checked_sub(1)
            .and_then(|index| entries.get(index).map(|entry| entry.0));
        let next = entries.get(slot).map(|entry| entry.0);
        match (previous, next) {
            (None, Some(next)) => next.saturating_sub(ORDER_GAP),
            (Some(previous), None) => previous.saturating_add(ORDER_GAP),
            (Some(previous), Some(next)) if next.saturating_sub(previous) > 1 => {
                previous + (next - previous) / 2
            }
            (Some(previous), Some(_)) => previous,
            (None, None) => 0,
        }
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
                && str_field(doc, &e, "id").as_deref() == Some(elem_id)
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
        Self::write_text(doc, obj, "text", &el.text);
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

    /// Resolve (or create) the CRDT text backing one slide's speaker notes.
    /// Shared editors bind to this object directly, preserving character-level
    /// concurrent edits instead of mirroring a whole string.
    pub fn notes_text_obj(doc: &mut AutoCommit, slide_idx: usize) -> Option<ObjId> {
        let slide = Self::slide_obj(doc, slide_idx)?;
        Some(match doc.get(&slide, "notes") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc.put_object(&slide, "notes", ObjType::Text).ok()?,
        })
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

    /// Splice into a text element's body. `char_pos`/`del` are char counts.
    ///
    /// # Panics
    ///
    /// Never in practice: `e` was just resolved to a live element Map, so
    /// `put_object` on it cannot fail short of a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "put_object on a live element Map cannot fail short of a corrupted document"
    )]
    pub fn splice_element_text(
        doc: &mut AutoCommit,
        slide_idx: usize,
        elem_id: &str,
        char_pos: usize,
        del: usize,
        ins: &str,
    ) {
        if let Some(e) = Self::element_obj(doc, slide_idx, elem_id) {
            let tid = match doc.get(&e, "text") {
                Ok(Some((Value::Object(ObjType::Text), id))) => id,
                _ => doc.put_object(&e, "text", ObjType::Text).expect("put text"),
            };
            let _ = doc.splice_text(&tid, char_pos, del as isize, ins);
        }
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
                    && str_field(doc, &e, "id").as_deref() == Some(elem_id)
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
        if let Some(rank) = Self::rank_for_move(doc, from, to) {
            let _ = Self::set_slide_order(doc, &id, rank);
        }
    }

    // --- reader -------------------------------------------------------------

    pub fn load(doc: &AutoCommit) -> Option<Deck> {
        let deck = Self::deck_get(doc)?;
        let title = text_field(doc, &deck, "title").unwrap_or_else(|| "Untitled deck".into());
        let palette = PaletteKind::from_str(
            &str_field(doc, &deck, "palette").unwrap_or_else(|| "indigo".into()),
        );
        let Ok(Some((Value::Object(ObjType::List), _slides_list))) = doc.get(&deck, "slides")
        else {
            return None;
        };
        let mut slides = Vec::new();
        for (_, _, so) in Self::ordered_slides(doc) {
            let id = str_field(doc, &so, "id").unwrap_or_else(crate::model::new_id);
            let layout = Layout::from_str(
                &str_field(doc, &so, "layout").unwrap_or_else(|| "title-content".into()),
            );
            let background = Background::from_str(
                &str_field(doc, &so, "background").unwrap_or_else(|| "surface".into()),
            );
            let notes = text_field(doc, &so, "notes").unwrap_or_default();
            let mut elements = Vec::new();
            if let Ok(Some((Value::Object(ObjType::List), el_list))) = doc.get(&so, "elements") {
                for j in 0..doc.length(&el_list) {
                    let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&el_list, j) else {
                        continue;
                    };
                    elements.push(Element {
                        id: str_field(doc, &e, "id").unwrap_or_else(crate::model::new_id),
                        kind: ElementKind::from_str(
                            &str_field(doc, &e, "kind").unwrap_or_else(|| "text".into()),
                        ),
                        x: f64_field(doc, &e, "x").unwrap_or(0.0),
                        y: f64_field(doc, &e, "y").unwrap_or(0.0),
                        w: f64_field(doc, &e, "w").unwrap_or(200.0),
                        h: f64_field(doc, &e, "h").unwrap_or(80.0),
                        text: text_field(doc, &e, "text").unwrap_or_default(),
                        src: str_field(doc, &e, "src").unwrap_or_default(),
                        fill: str_field(doc, &e, "fill").unwrap_or_else(|| "text".into()),
                        stroke: str_field(doc, &e, "stroke").unwrap_or_else(|| "none".into()),
                        stroke_width: f64_field(doc, &e, "stroke_width").unwrap_or(0.0),
                        font_size: f64_field(doc, &e, "font_size").unwrap_or(32.0),
                        align: TextAlign::from_str(
                            &str_field(doc, &e, "align").unwrap_or_else(|| "left".into()),
                        ),
                        bold: bool_field(doc, &e, "bold").unwrap_or(false),
                        italic: bool_field(doc, &e, "italic").unwrap_or(false),
                        radius: f64_field(doc, &e, "radius").unwrap_or(0.0),
                        z: i64_field(doc, &e, "z").unwrap_or(0),
                    });
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

fn str_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

fn text_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).ok(),
        Ok(Some((Value::Scalar(v), _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

fn f64_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<f64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_f64().or_else(|| v.to_i64().map(|i| i as f64)),
        _ => None,
    }
}

fn i64_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64().or_else(|| v.to_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn bool_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_bool(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ElementKind, seed_deck};

    #[test]
    fn write_text_append_one_char_is_bounded() {
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

    #[test]
    fn write_text_repeated_store_keeps_doc_bounded() {
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

    #[test]
    fn deck_round_trips_through_crdt() {
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

    #[test]
    fn live_pos_edit_persists() {
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

    #[test]
    fn add_and_remove_element() {
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

    #[test]
    fn insert_and_remove_slide() {
        let mut doc = AutoCommit::new();
        SlidesDoc::seed(&mut doc, &seed_deck());
        let n = SlidesDoc::load(&doc).unwrap().slides.len();
        SlidesDoc::insert_slide(&mut doc, 1, &Slide::new(Layout::Blank));
        assert_eq!(SlidesDoc::load(&doc).unwrap().slides.len(), n + 1);
        SlidesDoc::remove_slide(&mut doc, 1);
        assert_eq!(SlidesDoc::load(&doc).unwrap().slides.len(), n);
    }

    #[test]
    fn set_title_and_splice_title_persist() {
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

    #[test]
    fn move_slide_reorders() {
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

    #[test]
    fn reorder_and_concurrent_text_edit_preserve_the_same_slide_object() {
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
        a.merge(&mut b).expect("merge into a");
        b.merge(&mut a).expect("merge into b");

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
