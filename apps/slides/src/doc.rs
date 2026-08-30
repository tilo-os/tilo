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

pub struct SlidesDoc;

impl SlidesDoc {
    // --- object lookup ------------------------------------------------------

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
        let slides = Self::slides_list_get(doc)?;
        match doc.get(&slides, idx) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
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
        let elements = match doc.get(&slide, "elements") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => return None,
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

    pub fn is_empty(doc: &AutoCommit) -> bool {
        match Self::slides_list_get(doc) {
            Some(list) => doc.length(&list) == 0,
            None => true,
        }
    }

    // --- writers ------------------------------------------------------------

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
        }
    }

    // --- targeted live edits ------------------------------------------------

    pub fn set_title(doc: &mut AutoCommit, title: &str) {
        let deck = Self::deck_ensure(doc);
        Self::write_text(doc, &deck, "title", title);
    }

    /// Splice into the deck title's text. `char_pos`/`del` are char counts —
    /// the live-edit path for the inline title rename (mirrors `splice_notes`).
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
        let slides = Self::slides_list_ensure(doc);
        let n = doc.length(&slides);
        let at = at.min(n);
        if let Ok(so) = doc.insert_object(&slides, at, ObjType::Map) {
            Self::write_slide(doc, &so, slide);
        }
    }

    pub fn remove_slide(doc: &mut AutoCommit, idx: usize) {
        if let Some(slides) = Self::slides_list_get(doc)
            && idx < doc.length(&slides)
        {
            let _ = doc.delete(&slides, idx);
        }
    }

    /// Move the slide at `from` to `to` (both pre-move indices semantics:
    /// the element is removed then re-inserted, deck snapshot rewritten).
    pub fn move_slide(doc: &mut AutoCommit, from: usize, to: usize) {
        // Easiest robust approach: read the whole deck, reorder in memory,
        // rewrite. Decks are small; ordering edits are rare.
        if let Some(mut deck) = Self::load(doc) {
            if from >= deck.slides.len() || to >= deck.slides.len() || from == to {
                return;
            }
            let s = deck.slides.remove(from);
            deck.slides.insert(to, s);
            Self::seed(doc, &deck);
        }
    }

    // --- reader -------------------------------------------------------------

    pub fn load(doc: &AutoCommit) -> Option<Deck> {
        let deck = Self::deck_get(doc)?;
        let title = text_field(doc, &deck, "title").unwrap_or_else(|| "Untitled deck".into());
        let palette = PaletteKind::from_str(
            &str_field(doc, &deck, "palette").unwrap_or_else(|| "indigo".into()),
        );
        let slides_list = match doc.get(&deck, "slides") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => return None,
        };
        let mut slides = Vec::new();
        for i in 0..doc.length(&slides_list) {
            let Ok(Some((Value::Object(ObjType::Map), so))) = doc.get(&slides_list, i) else {
                continue;
            };
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
        if slides.is_empty() {
            return None;
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
        let tid = match doc.get(&obj, "notes") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => panic!("notes text missing"),
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
        SlidesDoc::move_slide(&mut doc, 0, 2);
        let after = SlidesDoc::load(&doc).unwrap();
        assert_eq!(after.slides[2].id, first);
    }
}
