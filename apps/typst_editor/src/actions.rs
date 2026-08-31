//! Resource + action-model status for the Typst editor.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an `Action` (a serializable value that deterministically changes the
//! shared Automerge document, so undo and multi-peer replay come for free),
//! while presentation / view state stays plain app code. Applying that split to
//! the Typst editor leaves **nothing** on the action side:
//!
//! - The only collaborative state in the document is the source text: a single
//!   Automerge `Text` at `root["source"]`. It is edited through the shared
//!   [`ui::CodeEditor`](ui::widget::code_editor::CodeEditor) on the toolkit's
//!   CRDT-native text path — caret, selection, undo, IME and multi-peer
//!   convergence all live there — so text edits are deliberately **not** wrapped
//!   as actions. The recipe is explicit that the native text path already
//!   replicates and undoes for free; re-encoding a keystroke as an action would
//!   duplicate what the `Text` object already gives.
//!
//! - There is no structural CRDT to migrate. The document library is the user's
//!   real `.typ` files on disk, loaded one at a time into `root["source"]`;
//!   switching documents is a text *diff* of that same object, not a structural
//!   mutation. Page size, fonts and the compile target are Typst source
//!   directives *inside* that text (`#set page(...)` and friends), not separate
//!   document fields. No assets or images are stored in the document.
//!
//! - Compile, diagnostics, PDF export, Save/Open and zoom are either external
//!   (the Typst compiler and the filesystem) or pure presentation. None of them
//!   mutate the shared document, so none is an action.
//!
//! Consequently the Typst editor exports **no** actions and publishes **no**
//! Ctrl+K resource tree: [`Widget::resources`](ui::widget::Widget::resources)
//! stays empty and
//! [`take_committed_action`](ui::widget::Widget::take_committed_action) stays
//! `None` (both toolkit defaults). Rather than fabricate a document/settings
//! model just to have actions, this module records that finding and proves it on
//! the production path — see the tests below.

#[cfg(test)]
mod tests {
    use crate::app::TypstApp;
    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
    use taffy::{NodeId, TaffyTree};
    use ui::widget::Widget;
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey, SmolStr};

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    /// A single character keypress through the real event path (the same trick
    /// the app's and the toolkit's own widget tests use for winit's `pub(crate)`
    /// `KeyEventExtra`).
    fn char_event(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// Pump one event through the production `Widget::handle_event`, returning
    /// whether it was consumed.
    fn send(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent, mods: Modifiers) -> bool {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            doc,
            &ROOT,
            &mut handled,
            &mut focus,
            NodeId::new(0),
            &taffy,
        );
        handled
    }

    /// A demo app with its first `sync` run so `root["source"]` is seeded and the
    /// editor is bound — the same state the live app reaches on its first frame.
    fn synced() -> (TypstApp, AutoCommit) {
        let mut app = TypstApp::with_demo_documents();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn source_id(doc: &AutoCommit) -> ObjId {
        match doc.get(&ROOT, "source").unwrap() {
            Some((Value::Object(ObjType::Text), id)) => id,
            other => panic!("root[\"source\"] must be an Automerge Text, got {other:?}"),
        }
    }

    /// The document holds exactly one thing: the source text. There is no
    /// structural CRDT (no map of documents, no settings object, no assets), so
    /// there is nothing for an action to mutate beyond the native `Text`.
    #[test]
    fn source_text_is_the_only_crdt_state() {
        let (_app, doc) = synced();
        let keys: Vec<String> = doc.keys(&ROOT).collect();
        assert_eq!(
            keys,
            vec!["source".to_string()],
            "the document holds only the source text, nothing structural: {keys:?}"
        );
        match doc.get(&ROOT, "source").unwrap() {
            Some((Value::Object(ObjType::Text), _)) => {}
            other => panic!("root[\"source\"] must be an Automerge Text, got {other:?}"),
        }
    }

    /// The collaboration guarantee the native text path already provides: a
    /// keystroke driven through the production `Widget::handle_event` splices
    /// into `root["source"]`, and an independent replica that merges the change
    /// converges byte-identically (same head hashes, not merely the same text) —
    /// exactly what an action executor would give, so no action is needed.
    #[test]
    fn typing_replicates_byte_identically_on_a_peer() {
        let (mut app, mut doc) = synced();
        let src = source_id(&doc);
        doc.commit();
        let base = doc.save();
        let before = doc.text(&src).unwrap();

        let mut peer = AutoCommit::load(&base).unwrap();
        assert_eq!(
            doc.get_heads(),
            peer.get_heads(),
            "precondition: replicas share a base"
        );

        // No modifiers → the keystroke falls through to the focused editor, which
        // splices it into the bound source object on the CRDT-native text path.
        assert!(
            send(&mut app, &mut doc, &char_event("Z"), Modifiers::default()),
            "keystroke consumed by the editor"
        );
        doc.commit();
        let after = doc.text(&src).unwrap();
        assert_eq!(
            after.chars().count(),
            before.chars().count() + 1,
            "the keystroke inserted exactly one character into the shared text"
        );

        peer.merge(&mut doc).unwrap();
        assert_eq!(
            doc.get_heads(),
            peer.get_heads(),
            "byte-identical convergence after merge"
        );
        assert_eq!(
            peer.text(&src).unwrap(),
            after,
            "the peer sees the same source text"
        );
    }

    /// Switching documents (Ctrl+]) is a bulk edit of the *same* `root["source"]`
    /// Text — a native text diff — not a structural mutation. The source content
    /// changes, but the document still holds only the one `source` key and still
    /// the same `Text` object, so there is no structural operation to encode.
    #[test]
    fn switching_documents_is_a_native_text_diff() {
        let (mut app, mut doc) = synced();
        let src_before = source_id(&doc);
        let text_before = doc.text(&src_before).unwrap();

        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(
            send(&mut app, &mut doc, &char_event("]"), ctrl),
            "Ctrl+] consumed"
        );

        let keys: Vec<String> = doc.keys(&ROOT).collect();
        assert_eq!(
            keys,
            vec!["source".to_string()],
            "still only the source key: {keys:?}"
        );
        let src_after = source_id(&doc);
        assert_eq!(
            src_before, src_after,
            "same Text object, spliced in place — not replaced"
        );
        assert_ne!(
            doc.text(&src_after).unwrap(),
            text_before,
            "the switch swapped the source content"
        );
    }

    /// The honest steady state: the Typst editor fabricates no action or resource
    /// surface. `Widget::resources` is empty (nothing to publish to Ctrl+K) and
    /// no action host records an undo boundary, because no action is ever
    /// dispatched.
    #[test]
    fn exports_no_actions_and_no_resource_tree() {
        let (mut app, doc) = synced();
        assert!(
            Widget::resources(&app, &doc).is_empty(),
            "the editor publishes no Ctrl+K resource tree — its only shared state is native text"
        );
        assert!(
            app.take_committed_action().is_none(),
            "no action is dispatched, so no action-host undo boundary is recorded"
        );
    }
}
