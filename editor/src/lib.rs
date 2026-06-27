//! `editor` — binds a `qdoc` canonical rich-text document into a host automerge
//! doc (e.g. `ui::State.doc`) so the document reuses the UI toolkit's
//! undo / incremental-sync / presence machinery without a message layer.
//!
//! The interactive GUI widget (draw/layout/keyboard over winit) is intentionally
//! deferred — it is app polish, not part of CRDT-native file-format support. This
//! crate currently provides the binding and proves the end-to-end integration
//! (undo, two-peer convergence, timeline, codec round-trip) against the real
//! `ui::State` via tests.

pub mod word_processor;
pub use word_processor::WordProcessor;

use automerge::{AutoCommit, ObjId};

/// A document binding: the resolved container ids + the per-replica id
/// generator. Holds no copy of the doc — it operates over a borrowed host
/// `AutoCommit` (the same one `ui::State` owns, snapshots for undo, and syncs).
pub struct EditorDoc {
    pub containers: qdoc::Containers,
    pub ids: qdoc::IdGen,
}

impl EditorDoc {
    /// Bind to a host doc at `root`, initializing the canonical schema if it is
    /// not already present.
    pub fn bind(doc: &mut AutoCommit, root: &ObjId) -> Self {
        let containers =
            qdoc::model::open(doc, root).unwrap_or_else(|_| qdoc::model::init(doc, root));
        let ids = qdoc::IdGen::new(doc.get_actor().to_hex_string());
        Self { containers, ids }
    }

    /// A mutable editing view over the host doc.
    pub fn edit<'a>(&'a mut self, doc: &'a mut AutoCommit) -> qdoc::DocMut<'a> {
        qdoc::DocMut { doc, c: &self.containers, ids: &mut self.ids }
    }

    pub fn blocks(&self, doc: &AutoCommit) -> Vec<String> {
        qdoc::model::blocks_in_order(doc, &self.containers)
    }

    pub fn text(&self, doc: &AutoCommit, id: &str) -> Option<String> {
        qdoc::model::block_text(doc, &self.containers, id)
    }

    pub fn snapshot(&self, doc: &AutoCommit) -> qdoc::DocSnapshot {
        qdoc::model::materialize(doc, &self.containers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qdoc::{FormatProvider, MarkdownCodec};

    fn first_text(snap: &qdoc::DocSnapshot) -> Option<String> {
        snap.blocks.first().map(|b| b.text.clone())
    }

    /// A qdoc document living in `ui::State.doc` reuses ui's full-snapshot
    /// undo/redo — proving the binding doesn't need its own undo layer.
    #[test]
    fn binding_reuses_ui_state_undo() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        s.commit_action(None, None); // checkpoint after schema init

        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Title\n\nHello\n", &mut dm).unwrap();
        }
        s.commit_action(None, None);
        assert_eq!(first_text(&ed.snapshot(&s.doc)), Some("Title".to_string()));

        s.undo(None, None);
        assert_eq!(ed.snapshot(&s.doc).blocks.len(), 0, "undo reverts to the empty schema");

        s.redo(None, None);
        assert_eq!(first_text(&ed.snapshot(&s.doc)), Some("Title".to_string()));
    }

    /// Two `ui::State` replicas editing concurrently converge through ui's
    /// incremental sync (`save_incremental`/`load_incremental`).
    #[test]
    fn two_peers_converge_through_ui_state() {
        let mut sa = ui::State::new();
        let mut ed_a = EditorDoc::bind(&mut sa.doc, &sa.root);
        sa.commit_action(None, None);

        // Fork B from A's serialized state, with a distinct actor id.
        let bytes = sa.save();
        let mut sb = ui::State::load(&bytes).unwrap();
        sb.doc.set_actor(automerge::ActorId::random());
        let mut ed_b = EditorDoc::bind(&mut sb.doc, &sb.root);

        {
            let mut dm = ed_a.edit(&mut sa.doc);
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "A-edit");
        }
        {
            let mut dm = ed_b.edit(&mut sb.doc);
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "B-edit");
        }

        let da = sa.save_incremental();
        let db = sb.save_incremental();
        sa.load_incremental(&db).unwrap();
        sb.load_incremental(&da).unwrap();

        let snap_a = ed_a.snapshot(&sa.doc);
        let snap_b = ed_b.snapshot(&sb.doc);
        assert_eq!(snap_a, snap_b, "peers converge to identical documents");
        assert_eq!(snap_a.blocks.len(), 2, "both concurrent edits are present");
    }

    /// The version timeline + time-travel work over a bound document.
    #[test]
    fn timeline_over_bound_doc() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        let p = {
            let mut dm = ed.edit(&mut s.doc);
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "hello");
            id
        };
        {
            let mut dm = ed.edit(&mut s.doc);
            dm.splice_text(&p, 5, 0, " world");
        }

        let vers = qdoc::history::versions(&mut s.doc);
        assert!(vers.len() >= 2, "multiple authored changes");
        let last = vers.len() - 1;
        assert_eq!(
            qdoc::history::materialize_at(&mut s.doc, last).unwrap().blocks[0].text,
            "hello world"
        );
        let saw_hello = (0..vers.len()).any(|i| {
            qdoc::history::materialize_at(&mut s.doc, i)
                .ok()
                .and_then(|sn| sn.blocks.first().map(|b| b.text == "hello"))
                .unwrap_or(false)
        });
        assert!(saw_hello, "timeline should expose the intermediate 'hello' state");
    }
}
