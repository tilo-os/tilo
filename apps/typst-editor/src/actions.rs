//! Canonical resource/operation surface for the Typst editor.
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) splits every
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
use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};

pub const OPERATION_TYPE: &str = "typst.operation";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypstOperation {
    Compile,
    Save { path: Option<String> },
    Open { path: Option<String> },
    ExportPdf { path: Option<String> },
    ZoomOut,
    ZoomIn,
    FitWidth,
    ToggleProblems,
    ShowEditor,
    ShowPreview,
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::expect_used,
    reason = "the variant is deliberately matched by value to select its exact action contract; serde-derived operation values are infallibly serializable"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: TypstOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, OPERATION_TYPE, &value)
        .expect("TypstOperation is always serializable");
    match value {
        TypstOperation::Compile => spec.retry_policy(RetryPolicy::Idempotent),
        TypstOperation::Save { .. }
        | TypstOperation::Open { .. }
        | TypstOperation::ExportPdf { .. } => spec
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        TypstOperation::ZoomOut | TypstOperation::ZoomIn => {
            spec.with_contract(ActionContract::view())
        }
        TypstOperation::ToggleProblems => spec.with_contract(ActionContract::view()),
        TypstOperation::FitWidth | TypstOperation::ShowEditor | TypstOperation::ShowPreview => spec
            .with_contract(ActionContract::view())
            .retry_policy(RetryPolicy::Idempotent),
    }
}

#[allow(
    clippy::expect_used,
    reason = "serde-derived operation values are infallibly serializable"
)]
fn path_operation(
    id: &'static str,
    label: &'static str,
    variant: fn(Option<String>) -> TypstOperation,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "path (omit to use the system file picker)",
        OperationKind::Effect,
        OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "filesystem path",
        )],
        move |args: &ActionArgs| {
            EncodedAction::encode_operation(
                OPERATION_TYPE,
                &variant(
                    args.text("path")
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned),
                ),
            )
            .expect("TypstOperation is always serializable")
        },
    )
    .destructive()
    .execution(ExecutionMode::Background { cancellable: true })
    .returns(ResultSpec::Job)
}

pub fn resources(_doc: &AutoCommit) -> Vec<Resource> {
    // Publish the same hierarchy a person sees: a workspace contains the
    // active document, which contains its rendered preview and export target.
    // This keeps global search and agents from seeing a flat pile of unrelated
    // commands, without eagerly indexing source text or materialising files.
    let preview = [
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
            "preview",
            "Show preview",
            OperationKind::View,
            TypstOperation::ShowPreview,
        ),
    ]
    .into_iter()
    .fold(
        Resource::new("typst.preview", "typst.preview", "Live preview"),
        |resource, (id, label, kind, op)| resource.with_action(operation(id, label, kind, op)),
    );
    let export = Resource::new("typst.export.pdf", "typst.export", "PDF export").with_action(
        path_operation("export-pdf", "Export PDF", |path| {
            TypstOperation::ExportPdf { path }
        }),
    );
    let mut document = Resource::new("typst.current", "typst.document", "Current document")
        .with_action(path_operation("save", "Save Typst file", |path| {
            TypstOperation::Save { path }
        }))
        .with_child(preview)
        .with_child(export);
    for (id, label, kind, op) in [
        (
            "compile",
            "Recompile",
            OperationKind::Effect,
            TypstOperation::Compile,
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
    ] {
        document = document.with_action(operation(id, label, kind, op));
    }
    vec![
        Resource::new("typst.workspace", "typst.workspace", "Typst workspace")
            .with_action(path_operation("open", "Open Typst file", |path| {
                TypstOperation::Open { path }
            }))
            .with_child(document),
    ]
}

/// Decode one operation payload advertised by this app.
///
/// # Errors
///
/// Returns an error when a payload for this app's operation type is malformed.
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
    use super::*;
    use crate::app::TypstApp;
    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
    use taffy::{NodeId, TaffyTree};
    use tilo_automerge::ApplyIncoming;
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

    #[test]
    fn file_effects_bind_direct_paths_and_keep_job_confirmation_contracts() {
        let resources = resources(&AutoCommit::new());
        for id in ["open", "save", "export-pdf"] {
            let spec = tilo_actions::flatten(&resources)
                .into_iter()
                .find(|flat| flat.action.id == id)
                .map(|flat| flat.action)
                .unwrap();
            assert_eq!(spec.params()[0].name, "path");
            assert!(matches!(
                spec.contract.execution,
                ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(spec.contract.result, ResultSpec::Job);
            assert!(!matches!(
                spec.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ));
            let args = serde_json::json!({"path": format!("/tmp/{id}")})
                .as_object()
                .unwrap()
                .clone();
            let encoded = spec.build_json(&args).unwrap();
            let decoded = decode(OPERATION_TYPE, &encoded.payload).unwrap().unwrap();
            let (TypstOperation::Open { path }
            | TypstOperation::Save { path }
            | TypstOperation::ExportPdf { path }) = decoded
            else {
                unreachable!();
            };
            assert_eq!(path.as_deref(), Some(format!("/tmp/{id}").as_str()));
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

        peer.merge_incoming(&mut doc).unwrap();
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
        assert_eq!(resources[0].kind, "typst.workspace");
        assert_eq!(
            resources[0].children.len(),
            1,
            "workspace owns the active document"
        );
        let document = &resources[0].children[0];
        assert_eq!(document.kind, "typst.document");
        assert!(
            document
                .children
                .iter()
                .any(|child| child.kind == "typst.preview")
        );
        assert!(
            document
                .children
                .iter()
                .any(|child| child.kind == "typst.export")
        );
        let actions = tilo_actions::flatten(&resources);
        assert!(actions.iter().any(|a| {
            a.action.id == "compile" && a.action.kind == tilo_actions::OperationKind::Effect
        }));
        assert!(actions.iter().any(|a| {
            a.action.id == "zoom-in" && a.action.kind == tilo_actions::OperationKind::View
        }));
        assert!(
            actions
                .iter()
                .all(|a| a.action.kind != tilo_actions::OperationKind::Document)
        );
        assert!(
            app.take_committed_action().is_none(),
            "effects/views do not create document undo boundaries"
        );
    }
}
