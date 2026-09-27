//! The plaintext body every CRDT-backed text tile in the shell is built on.
//!
//! [`NotesApp`](super::notes_app::NotesApp) and
//! [`EditorApp`](super::editor_app::EditorApp) render differently but share
//! this: one Automerge `Text` under a well-known key, a byte-offset caret
//! over a mirrored `String`, and a tile filling its parent.
//!
//! `text` mirrors the CRDT because `AutoCommit::text` allocates the whole
//! document — draw/hit-test read the mirror, only writes touch Automerge.
//! [`CrdtTextBody::sync`] re-establishes the invariant each frame.
//!
//! Every write goes through [`CrdtTextBody::splice`], addressing the exact
//! range that changed — rewriting the whole body per keystroke leaves every
//! element a tombstone, O(len) per edit and O(len²) over a session. Callers
//! with only a new whole string want [`ui::doc::splice_text_diffed`].

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::{PathSegment, PresenceKey};
use ui::text_selection::{next_char_boundary, prev_char_boundary};
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};

/// Keystrokes stop coalescing into a single undo entry after this long idle.
pub const COMMIT_DEBOUNCE: Duration = Duration::from_millis(800);

/// Read the text a [`CrdtTextBody`] bound to `key` holds under `root`,
/// without constructing the widget that owns it. A body sits at
/// `<root>/<key>` of **the tab's own document**, one per record — reach it
/// through [`crate::shell::tab_documents::tab_body`], which knows which
/// document a tab is.
pub fn read_body(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    let values = body_values(doc, root, key);
    (!values.is_empty()).then(|| project(doc, &values))
}

/// Every `Text` stored under `<root>/<key>`, in Automerge's
/// deterministic conflict order (identical on every replica).
///
/// The body is created on first sync, so two replicas that open the same
/// empty record before syncing each create their own `Text`: a key
/// conflict where a plain `get` shows one winner and the other replica's
/// typing vanishes from view. Reads therefore project every value.
fn body_values(doc: &AutoCommit, root: &ObjId, key: &str) -> Vec<ObjId> {
    doc.get_all(root, key)
        .map(|values| {
            values
                .into_iter()
                .filter_map(|(value, id)| {
                    matches!(value, Value::Object(ObjType::Text)).then_some(id)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The characters a body shows: its `Text`, or every conflicting value
/// joined in conflict order. A value identical to an earlier one (two
/// replicas that each wrote the same seed and nothing else) is shown once;
/// dropping an exact duplicate loses no characters.
fn project(doc: &AutoCommit, values: &[ObjId]) -> String {
    let mut seen: Vec<String> = Vec::with_capacity(values.len());
    for id in values {
        let text = doc.text(id).unwrap_or_default();
        if !seen.contains(&text) {
            seen.push(text);
        }
    }
    seen.concat()
}

/// A single Automerge `Text` object plus the mirror and caret arithmetic
/// around it.
pub struct CrdtTextBody {
    /// Automerge key under the tile's root, distinct per app so two text
    /// tiles don't write over each other.
    key: &'static str,
    /// The record whose document this body lives in — part of the
    /// presence key, or two tabs of one app publish the same caret address.
    record: Option<String>,
    /// Mirror of the CRDT text, byte-indexed, kept in step by
    /// [`Self::sync`] and [`Self::splice`].
    pub text: String,
    /// When the last edit landed. Drives the undo-coalescing debounce.
    last_edit: Option<Instant>,
}

impl CrdtTextBody {
    /// An empty body bound to `key`.
    pub fn new(key: &'static str) -> Self {
        Self {
            key,
            record: None,
            text: String::new(),
            last_edit: None,
        }
    }

    /// A body pre-seeded with `text`. The seed is written to the CRDT
    /// lazily on the first [`Self::sync`], so repeated boots against an
    /// existing document don't keep appending it.
    pub fn with_seed(key: &'static str, text: impl Into<String>) -> Self {
        Self {
            key,
            record: None,
            text: text.into(),
            last_edit: None,
        }
    }

    /// Called by the hosting tile through
    /// [`ui::widget::Widget::set_record_scope`].
    pub fn set_record(&mut self, record: &ui::record::RecordId) {
        self.record = Some(record.as_str().to_string());
    }

    /// Presence key for this body: record first, field second, because a
    /// caret is a position in a *document* — a key naming only the field
    /// would address every tab of the app at once. An unhosted body keeps
    /// the bare field path.
    pub fn presence_key(&self) -> PresenceKey {
        match &self.record {
            Some(record) => PresenceKey::Widget {
                path: vec![
                    PathSegment::Field(record.clone()),
                    PathSegment::Field(self.key.into()),
                ],
            },
            None => PresenceKey::Widget {
                path: vec![PathSegment::Field(self.key.into())],
            },
        }
    }

    /// Resolve (or create) the `Text` object backing this body. `None`
    /// only on a corrupt document; callers treat it as "skip this edit".
    ///
    /// A body left with several values (see [`body_values`]) is merged in
    /// place into one `Text` holding exactly the projection the mirror
    /// shows, so the caller's offsets stay valid.
    fn resolve(&self, state: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
        let values = body_values(state, root, self.key);
        if let [id] = values.as_slice() {
            return Some(id.clone());
        }
        let projected = project(state, &values);
        let id = state.put_object(root, self.key, ObjType::Text).ok()?;
        if !projected.is_empty() {
            state.splice_text(&id, 0, 0, &projected).ok()?;
        }
        Some(id)
    }

    /// `Widget::sync` body: reads the CRDT into the mirror, seeding it from
    /// the constructor's text only if no body exists yet.
    pub fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let values = body_values(state, root, self.key);
        if let [id] = values.as_slice() {
            if let Ok(t) = state.text(id) {
                self.text = t;
            }
        } else if !values.is_empty() {
            self.text = project(state, &values);
        } else if let Ok(id) = state.put_object(root, self.key, ObjType::Text)
            && !self.text.is_empty()
        {
            let _ = state.splice_text(&id, 0, 0, &self.text);
        }
    }

    /// Replace byte `range` with `s` in both mirror and CRDT, splicing only
    /// that span; returns the offset just past the inserted text.
    pub fn splice(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        range: Range<usize>,
        s: &str,
    ) -> usize {
        let Some(body) = self.resolve(state, root) else {
            return range.start;
        };
        // Automerge `Text` is char-addressed; the mirror is byte-addressed.
        // A `range` that doesn't land on char boundaries would be an
        // upstream bug (Automerge itself would then splice at the wrong
        // char), so treat it the same as "resolve failed": skip the edit
        // rather than panic on the byte slice.
        let Some(before) = self.text.get(..range.start) else {
            return range.start;
        };
        let Some(changed) = self.text.get(range.clone()) else {
            return range.start;
        };
        let char_start = before.chars().count();
        let char_count = changed.chars().count();
        self.text.replace_range(range.clone(), s);
        // A single edit's char count is bounded by the buffer length,
        // nowhere near isize::MAX; `unwrap_or` only matters for a
        // document large enough that no editor could scroll it anyway.
        let char_count_isize = isize::try_from(char_count).unwrap_or(isize::MAX);
        let _ = state.splice_text(&body, char_start, char_count_isize, s);
        self.last_edit = Some(Instant::now());
        range.start.saturating_add(s.len())
    }

    /// Nearest char boundary at or below `pos`, clamped to the buffer.
    pub fn clamp(&self, pos: usize) -> usize {
        let mut p = pos.min(self.text.len());
        while p > 0 && !self.text.is_char_boundary(p) {
            p = p.saturating_sub(1);
        }
        p
    }

    /// Char boundary before `pos`, saturating at the start of the buffer.
    pub fn prev_boundary(&self, pos: usize) -> usize {
        prev_char_boundary(&self.text, pos)
    }

    /// Char boundary after `pos`, saturating at the end of the buffer.
    pub fn next_boundary(&self, pos: usize) -> usize {
        next_char_boundary(&self.text, pos)
    }

    /// Char index (the unit `PresenceCtx::carets` speaks) to a byte offset
    /// into the mirror; walks past the end without panicking since peer
    /// state can be a little ahead of ours.
    pub fn char_index_to_byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Whether the typing burst has gone quiet long enough to close an undo entry.
    pub fn needs_commit(&self) -> bool {
        self.last_edit
            .is_some_and(|t| t.elapsed() >= COMMIT_DEBOUNCE)
    }

    /// `Widget::on_commit` delegate — the burst is now closed.
    pub fn on_commit(&mut self) {
        self.last_edit = None;
    }
}

/// `Widget::build_layout` body for a tile that fills its parent.
///
/// `widget` must be the `self` pointer of the caller — that is what makes
/// the `node_map` entry point back at the right widget during dispatch.
/// Does not panic: `TaffyTree::new_leaf` can only err on an exhausted
/// node capacity, which a single-leaf insert into a live UI tree cannot
/// reach, but this degrades to a dummy `NodeId` and no `node_map` entry
/// rather than assert it.
pub fn full_bleed_leaf<S: std::hash::BuildHasher>(
    taffy: &mut TaffyTree<()>,
    node_map: &mut HashMap<NodeId, *mut dyn Widget, S>,
    widget: *mut dyn Widget,
) -> NodeId {
    let style = Style {
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::percent(1.0),
        },
        ..Style::default()
    };
    let Ok(node) = taffy.new_leaf(style) else {
        // Genuinely unreachable for a single leaf in a live tree, but
        // `TaffyTree::new_leaf` returns `Result`, and this crate never
        // unwraps a Result it can instead handle — a zero rect with no
        // node_map entry degrades to "this tile doesn't lay out this
        // frame" rather than tearing down the whole compositor.
        return NodeId::new(0);
    };
    node_map.insert(node, widget);
    node
}

/// `Widget::apply_layout` body matching [`full_bleed_leaf`]: taffy's
/// computed box translated into the parent's coordinate space. Does not
/// panic: `node` always comes from this same frame's `full_bleed_leaf`
/// call into the same `taffy` tree so a layout for it always exists, but
/// a missing layout degrades to a zero-size `Rect` at `origin` rather
/// than assert it.
pub fn full_bleed_rect(taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> Rect {
    let Ok(l) = taffy.layout(node) else {
        return Rect::new(origin.x, origin.y, origin.x, origin.y);
    };
    Rect::new(
        origin.x + f64::from(l.location.x),
        origin.y + f64::from(l.location.y),
        origin.x + f64::from(l.location.x) + f64::from(l.size.width),
        origin.y + f64::from(l.location.y) + f64::from(l.size.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;

    const KEY: &str = "test_body";

    fn bind() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let tile = doc
            .put_object(&automerge::ROOT, "tile", ObjType::Map)
            .expect("tile map");
        (doc, tile)
    }

    #[test]
    fn crdt_text_seeds_and_splices_only_the_changed_char_span() {
        fn sync_seeds_the_crdt_then_reads_it_back() {
            let (mut doc, tile) = bind();
            let mut a = CrdtTextBody::with_seed(KEY, "hello");
            a.sync(&mut doc, &tile);

            let (val, id) = doc.get(&tile, KEY).unwrap().expect("body exists");
            assert!(matches!(val, Value::Object(ObjType::Text)));
            assert_eq!(doc.text(&id).unwrap(), "hello");

            // A second body bound to the same doc reads it through rather than
            // re-seeding — otherwise every boot would append the welcome text.
            let mut b = CrdtTextBody::with_seed(KEY, "a different seed");
            b.sync(&mut doc, &tile);
            assert_eq!(b.text, "hello");
            assert_eq!(doc.text(&id).unwrap(), "hello");
        }

        fn splice_inserts_deletes_and_replaces_in_both_places() {
            let (mut doc, tile) = bind();
            let mut b = CrdtTextBody::new(KEY);
            b.sync(&mut doc, &tile);

            let caret = b.splice(&mut doc, &tile, 0..0, "abcd");
            assert_eq!(caret, 4);
            assert_eq!(b.text, "abcd");

            let caret = b.splice(&mut doc, &tile, 1..3, ""); // delete
            assert_eq!(caret, 1);
            assert_eq!(b.text, "ad");

            let caret = b.splice(&mut doc, &tile, 0..1, "XY"); // replace
            assert_eq!(caret, 2);
            assert_eq!(b.text, "XYd");

            let (_, id) = doc.get(&tile, KEY).unwrap().expect("body exists");
            assert_eq!(doc.text(&id).unwrap(), "XYd");
        }

        fn splice_only_touches_the_changed_span() {
            // Appending one char must cost one inserted element, not a rewrite
            // of the whole text.
            let (mut doc, tile) = bind();
            let mut b = CrdtTextBody::with_seed(KEY, "x".repeat(64));
            b.sync(&mut doc, &tile);
            let (_, id) = doc.get(&tile, KEY).unwrap().expect("body exists");
            let before = doc.length(&id);

            b.splice(&mut doc, &tile, 64..64, "y");

            assert_eq!(
                doc.length(&id),
                before + 1,
                "a one-char append must add exactly one element",
            );
            assert!(doc.text(&id).unwrap().ends_with("xy"));
        }

        fn multibyte_splices_address_chars_not_bytes() {
            let (mut doc, tile) = bind();
            let mut b = CrdtTextBody::with_seed(KEY, "aβc");
            b.sync(&mut doc, &tile);
            // Delete the two-byte β, which sits at bytes 1..3 / char 1.
            b.splice(&mut doc, &tile, 1..3, "");
            assert_eq!(b.text, "ac");
            let (_, id) = doc.get(&tile, KEY).unwrap().expect("body exists");
            assert_eq!(
                doc.text(&id).unwrap(),
                "ac",
                "byte offsets must be converted to char offsets before splicing",
            );
        }

        sync_seeds_the_crdt_then_reads_it_back();
        splice_inserts_deletes_and_replaces_in_both_places();
        splice_only_touches_the_changed_span();
        multibyte_splices_address_chars_not_bytes();
    }

    /// Two replicas open the same empty record and each type first: each
    /// sync created its own body `Text`, and a plain read showed only one.
    #[test]
    fn concurrent_first_edits_on_two_replicas_both_survive() {
        for _ in 0..16 {
            let (mut base, tile) = bind();
            base.commit();
            let mut left_doc = base.fork();
            let mut right_doc = base.fork();
            let mut left = CrdtTextBody::new(KEY);
            let mut right = CrdtTextBody::new(KEY);
            left.sync(&mut left_doc, &tile);
            right.sync(&mut right_doc, &tile);
            left.splice(&mut left_doc, &tile, 0..0, "left");
            right.splice(&mut right_doc, &tile, 0..0, "right");
            left_doc.merge_incoming(&mut right_doc).unwrap();
            left.sync(&mut left_doc, &tile);
            assert!(
                left.text.contains("left") && left.text.contains("right"),
                "both first edits survive; got {:?}",
                left.text
            );
            let shown = read_body(&left_doc, &tile, KEY).unwrap();
            assert_eq!(shown, left.text);
            // The next edit upgrades to one Text holding exactly what was shown.
            let end = left.text.len();
            left.splice(&mut left_doc, &tile, end..end, "!");
            assert_eq!(
                read_body(&left_doc, &tile, KEY).unwrap(),
                format!("{shown}!")
            );
            assert_eq!(left_doc.get_all(&tile, KEY).unwrap().len(), 1);
        }
    }

    /// Two replicas that each seeded the same welcome text show it once.
    #[test]
    fn concurrent_identical_seeds_are_shown_once() {
        let (mut base, tile) = bind();
        base.commit();
        let mut left_doc = base.fork();
        let mut right_doc = base.fork();
        CrdtTextBody::with_seed(KEY, "hello").sync(&mut left_doc, &tile);
        CrdtTextBody::with_seed(KEY, "hello").sync(&mut right_doc, &tile);
        left_doc.merge_incoming(&mut right_doc).unwrap();
        assert_eq!(read_body(&left_doc, &tile, KEY).unwrap(), "hello");
    }

    #[test]
    fn crdt_text_char_boundaries_commit_debounce_and_presence_key() {
        fn boundaries_step_whole_chars() {
            let b = CrdtTextBody::with_seed(KEY, "aβc");
            // β occupies bytes 1..3.
            assert_eq!(b.next_boundary(1), 3);
            assert_eq!(b.prev_boundary(3), 1);
            // Ends are saturating, not wrapping.
            assert_eq!(b.prev_boundary(0), 0);
            assert_eq!(b.next_boundary(4), 4);
            // Clamp snaps into the middle of a char, and past the end.
            assert_eq!(b.clamp(2), 1);
            assert_eq!(b.clamp(99), 4);
        }

        fn char_index_to_byte_handles_multibyte_and_overrun() {
            let b = CrdtTextBody::with_seed(KEY, "aβc");
            assert_eq!(b.char_index_to_byte(0), 0);
            assert_eq!(b.char_index_to_byte(1), 1);
            assert_eq!(b.char_index_to_byte(2), 3);
            // A peer whose caret ran past our copy must not panic us.
            assert_eq!(b.char_index_to_byte(99), 4);
        }

        fn commit_debounce_opens_on_edit_and_closes_on_commit() {
            let (mut doc, tile) = bind();
            let mut b = CrdtTextBody::new(KEY);
            b.sync(&mut doc, &tile);
            assert!(!b.needs_commit(), "an untouched body has nothing to commit");
            b.splice(&mut doc, &tile, 0..0, "hi");
            // Still inside the debounce window, so the burst stays open.
            assert!(!b.needs_commit());
            b.last_edit = Some(Instant::now().checked_sub(COMMIT_DEBOUNCE).unwrap());
            assert!(b.needs_commit(), "a quiet burst must close");
            b.on_commit();
            assert!(!b.needs_commit());
        }

        fn presence_key_names_the_body_field() {
            let b = CrdtTextBody::new(KEY);
            assert_eq!(
                b.presence_key(),
                PresenceKey::Widget {
                    path: vec![PathSegment::Field(KEY.into())],
                }
            );
        }

        boundaries_step_whole_chars();
        char_index_to_byte_handles_multibyte_and_overrun();
        commit_debounce_opens_on_edit_and_closes_on_commit();
        presence_key_names_the_body_field();
    }
}
