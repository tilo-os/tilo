//! Canonical resource/operation surface for the Typst editor.
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
//! Source typing remains on the CRDT-native text path. Compile/filesystem work
//! is exported as an effect and local navigation/zoom as view operations.

use automerge::AutoCommit;
use qos_actions::{ActionContract, ActionSpec, OperationKind, Resource, RetryPolicy};
use serde::{Deserialize, Serialize};

pub const OPERATION_TYPE: &str = "typst.operation";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypstOperation {
    Compile,
    Save,
    Open,
    ExportPdf,
    ZoomOut,
    ZoomIn,
    FitWidth,
    ToggleProblems,
    ShowEditor,
    ShowPreview,
}

fn operation(id: &str, label: &str, kind: OperationKind, value: TypstOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, OPERATION_TYPE, &value)
        .expect("TypstOperation is always serializable");
    match value {
        TypstOperation::Compile => spec.retry_policy(RetryPolicy::Idempotent),
        TypstOperation::Save | TypstOperation::Open => spec.destructive(),
        TypstOperation::ExportPdf => spec.destructive(),
        TypstOperation::ZoomOut | TypstOperation::ZoomIn => {
            spec.with_contract(ActionContract::view())
        }
        TypstOperation::ToggleProblems => spec.with_contract(ActionContract::view()),
        TypstOperation::FitWidth | TypstOperation::ShowEditor | TypstOperation::ShowPreview => spec
            .with_contract(ActionContract::view())
            .retry_policy(RetryPolicy::Idempotent),
    }
}

pub fn resources(_doc: &AutoCommit) -> Vec<Resource> {
    let mut document = Resource::new("typst.current", "typst.document", "Current document");
    for (id, label, kind, op) in [
        (
            "compile",
            "Recompile",
            OperationKind::Effect,
            TypstOperation::Compile,
        ),
        (
            "save",
            "Save Typst file",
            OperationKind::Effect,
            TypstOperation::Save,
        ),
        (
            "open",
            "Open Typst file",
            OperationKind::Effect,
            TypstOperation::Open,
        ),
        (
            "export-pdf",
            "Export PDF",
            OperationKind::Effect,
            TypstOperation::ExportPdf,
        ),
        (
            "zoom-in",
            "Zoom in",
            OperationKind::View,
            TypstOperation::ZoomIn,
        ),
        (
            "zoom-out",
            "Zoom out",
            OperationKind::View,
            TypstOperation::ZoomOut,
        ),
        (
            "fit-width",
            "Fit preview width",
            OperationKind::View,
            TypstOperation::FitWidth,
        ),
        (
            "problems",
            "Toggle problems",
            OperationKind::View,
            TypstOperation::ToggleProblems,
        ),
        (
            "editor",
            "Show editor",
            OperationKind::View,
            TypstOperation::ShowEditor,
        ),
        (
            "preview",
            "Show preview",
            OperationKind::View,
            TypstOperation::ShowPreview,
        ),
    ] {
        document = document.with_action(operation(id, label, kind, op));
    }
    vec![document]
}

pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<TypstOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

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

    /// The source remains native text while effects/views are discoverable.
    #[test]
    fn exports_effect_and_view_operations_without_fake_document_actions() {
        let (mut app, doc) = synced();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1);
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|a| { a.id == "compile" && a.kind == qos_actions::OperationKind::Effect })
        );
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|a| { a.id == "zoom-in" && a.kind == qos_actions::OperationKind::View })
        );
        assert!(
            resources[0]
                .actions
                .iter()
                .all(|a| a.kind != qos_actions::OperationKind::Document)
        );
        assert!(
            app.take_committed_action().is_none(),
            "effects/views do not create document undo boundaries"
        );
    }
}
