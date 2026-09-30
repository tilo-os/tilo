//! The hex editor's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Byte edits are exported as canonical **local effect operations**. Their
//! offsets and literal bytes reach only this editor process; they are never
//! applied to Automerge, replicated, or recorded as a collaborative commit.
//! This preserves agent/palette driveability without leaking a local file.
//!
//! A byte buffer has no natural sub-items, so the resource tree is a single flat
//! `hex_editor.document` root that exports the byte-edit actions positioned at
//! the current cursor / selection.
//!
//! Presentation — the cursor, the selection, the scroll position, the
//! insert-vs-overwrite mode, the hex-vs-ASCII focus, go-to-offset navigation —
//! stays device-local. Loading and saving are local portal effects; direct paths
//! are accepted for an explicit local agent invocation but never written into a
//! document or action history.
//!
//! `HexEditorAction` remains decodable only to give stale callers a precise
//! refusal; its document `apply` path never accepts raw content.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResultSpec,
};

use crate::model::BufferEdit;

/// Registry `type_id` for every hex-editor action.
pub const HEX_EDITOR_ACTION_TYPE: &str = "hex_editor.action";
pub const HEX_EDITOR_OPERATION_TYPE: &str = "hex_editor.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HexOperation {
    Open {
        path: Option<String>,
    },
    Save {
        path: Option<String>,
    },
    Search,
    GotoOffset,
    Undo,
    Redo,
    /// A device-local byte mutation. The payload is routed only to the open
    /// editor process; it is never an Automerge action or collaboration event.
    Edit {
        edit: BufferEditWire,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferEditWire {
    Insert { offset: usize, bytes: Vec<u8> },
    Delete { offset: usize, len: usize },
    Overwrite { offset: usize, bytes: Vec<u8> },
}

impl From<&HexEditorAction> for BufferEditWire {
    fn from(action: &HexEditorAction) -> Self {
        match action {
            HexEditorAction::InsertBytes { offset, bytes } => Self::Insert {
                offset: *offset,
                bytes: bytes.clone(),
            },
            HexEditorAction::DeleteBytes { offset, len } => Self::Delete {
                offset: *offset,
                len: *len,
            },
            HexEditorAction::OverwriteBytes { offset, bytes } => Self::Overwrite {
                offset: *offset,
                bytes: bytes.clone(),
            },
        }
    }
}

impl From<BufferEditWire> for BufferEdit {
    fn from(edit: BufferEditWire) -> Self {
        match edit {
            BufferEditWire::Insert { offset, bytes } => Self::Insert { offset, bytes },
            BufferEditWire::Delete { offset, len } => Self::Delete { offset, len },
            BufferEditWire::Overwrite { offset, bytes } => Self::Overwrite { offset, bytes },
        }
    }
}

impl From<&BufferEdit> for BufferEditWire {
    fn from(edit: &BufferEdit) -> Self {
        match edit {
            BufferEdit::Insert { offset, bytes } => Self::Insert {
                offset: *offset,
                bytes: bytes.clone(),
            },
            BufferEdit::Delete { offset, len } => Self::Delete {
                offset: *offset,
                len: *len,
            },
            BufferEdit::Overwrite { offset, bytes } => Self::Overwrite {
                offset: *offset,
                bytes: bytes.clone(),
            },
        }
    }
}

/// # Errors
///
/// Returns the operation decoder's JSON error for an invalid local payload.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<HexOperation>, String> {
    if type_id != HEX_EDITOR_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Legacy byte intent shape, retained for local operation conversion only.
///
/// The three primitives — insert, delete, overwrite — are the whole of a hex
/// editor's document mutations; every keystroke that changes the bytes reduces
/// to one of them at a concrete offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HexEditorAction {
    /// Splice `bytes` in *before* `offset`, growing the buffer.
    InsertBytes { offset: usize, bytes: Vec<u8> },
    /// Remove `len` bytes starting at `offset`.
    DeleteBytes { offset: usize, len: usize },
    /// Replace the bytes already at `offset..offset+bytes.len()` in place; the
    /// buffer never grows (indices past the end are ignored).
    OverwriteBytes { offset: usize, bytes: Vec<u8> },
}

impl Action for HexEditorAction {
    fn apply(&self, _doc: &mut Doc) -> Result<(), ActionError> {
        Err(ActionError::Refused(
            "raw hex bytes are device-local; invoke the local resource operation".into(),
        ))
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the legacy decoder so stale envelopes fail with the action's
/// explicit local-only refusal rather than being misinterpreted.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<HexEditorAction>(HEX_EDITOR_ACTION_TYPE);
}

/// Encode a legacy raw-byte action for compatibility-refusal regression tests.
/// The resulting envelope is intentionally rejected by the collaboration
/// executor; live palette and agent clients use [`HexOperation`] instead.
///
/// # Panics
///
/// Never in practice: every [`HexEditorAction`] variant holds only `usize`
/// and `Vec<u8>` fields, which always serialize. `ActionSpec::prompted`'s
/// `build` closure must return an `EncodedAction` directly rather than a
/// `Result`, so there is no error path here to propagate to instead.
#[allow(
    clippy::expect_used,
    reason = "HexEditorAction always serializes (usize/Vec<u8> fields only); \
              the prompted-builder closure this feeds must return EncodedAction \
              directly, not a Result, so there is nowhere to propagate a failure"
)]
pub fn encode(action: &HexEditorAction) -> EncodedAction {
    EncodedAction::encode(HEX_EDITOR_ACTION_TYPE, action)
        .expect("HexEditorAction is always serializable")
}

// ===========================================================================
// Action builders for the local resource tree below.
// ===========================================================================

pub fn insert_bytes_action(offset: usize, bytes: Vec<u8>) -> HexEditorAction {
    HexEditorAction::InsertBytes { offset, bytes }
}

pub fn delete_bytes_action(offset: usize, len: usize) -> HexEditorAction {
    HexEditorAction::DeleteBytes { offset, len }
}

pub fn overwrite_bytes_action(offset: usize, bytes: Vec<u8>) -> HexEditorAction {
    HexEditorAction::OverwriteBytes { offset, bytes }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context the view hands [`provide_resources`]: the live
/// cursor, buffer length and selection. Everything here is view state the
/// document doesn't own — the palette needs it to target the byte-edit actions
/// at where the user is.
#[derive(Debug, Clone)]
pub struct HexResourceCtx {
    /// Label for the document resource (the file name, or a scratch label).
    pub label: String,
    pub cursor: usize,
    pub len: usize,
    /// The inclusive selection `(lo, hi)`, when one is active.
    pub selection: Option<(usize, usize)>,
    pub has_file: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Parse a one-byte hex value out of a palette prompt: accepts `ff`, `0xFF`, or
/// a bare `7`. Returns `None` for anything that isn't a single byte.
fn parse_hex_byte(s: &str) -> Option<u8> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u8::from_str_radix(t, 16).ok()
}

#[allow(
    clippy::expect_used,
    reason = "HexEditorAction always serializes (usize/Vec<u8> fields only); see encode()"
)]
fn immediate_spec(id: &str, label: &str, action: &HexEditorAction) -> ActionSpec {
    let spec = ActionSpec::operation(
        id,
        label,
        OperationKind::Effect,
        HEX_EDITOR_OPERATION_TYPE,
        &HexOperation::Edit {
            edit: action.into(),
        },
    )
    .expect("encode hex editor local operation");
    if id.contains("delete") {
        spec.destructive()
    } else {
        spec
    }
}

/// Open / save / undo / redo / search / goto — View and Effect ops that ride
/// [`crate::resource::HexEditor::local_operations`] through
/// `tilo_resource::publish`. Live cursor targeting stays on
/// [`live_buffer_extras`].
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "HexOperation always serializes; these builders feed ActionSpec closures"
)]
pub(crate) fn document_local_operations() -> Vec<ActionSpec> {
    // Offer the door without an empty-projection `unavailable` gate: invoke
    // time refuses save/search/goto when no buffer is open, and undo/redo when
    // the local journal is empty.
    root_palette_specs(&HexResourceCtx {
        label: "Untitled".into(),
        cursor: 0,
        len: 0,
        selection: None,
        has_file: true,
        can_undo: true,
        can_redo: true,
    })
}

#[allow(
    clippy::expect_used,
    reason = "HexOperation always serializes (serde-only payloads)"
)]
fn root_palette_specs(ctx: &HexResourceCtx) -> Vec<ActionSpec> {
    let mut specs = Vec::new();
    let open = ActionSpec::typed_operation(
        "open",
        "Open file",
        "path (omit to use the system file picker)",
        OperationKind::Effect,
        HEX_EDITOR_OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "file to open",
        )],
        |args: &ActionArgs| {
            EncodedAction::encode_operation(
                HEX_EDITOR_OPERATION_TYPE,
                &HexOperation::Open {
                    path: args
                        .text("path")
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned),
                },
            )
            .expect("HexOperation is always serializable")
        },
    )
    .destructive()
    .execution(ExecutionMode::Background { cancellable: true })
    .returns(ResultSpec::Job);
    specs.push(open);
    let save = ActionSpec::typed_operation(
        "save",
        "Save file",
        "path (omit to save in place or use the system file picker)",
        OperationKind::Effect,
        HEX_EDITOR_OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "save destination",
        )],
        |args: &ActionArgs| {
            EncodedAction::encode_operation(
                HEX_EDITOR_OPERATION_TYPE,
                &HexOperation::Save {
                    path: args
                        .text("path")
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned),
                },
            )
            .expect("HexOperation is always serializable")
        },
    )
    .destructive()
    .execution(ExecutionMode::Background { cancellable: true })
    .returns(ResultSpec::Job);
    specs.push(if ctx.has_file {
        save
    } else {
        save.unavailable("no_file", "Open a file first")
    });
    for (id, label, value, available) in [
        ("undo", "Undo local edit", HexOperation::Undo, ctx.can_undo),
        ("redo", "Redo local edit", HexOperation::Redo, ctx.can_redo),
    ] {
        let spec = ActionSpec::operation(
            id,
            label,
            OperationKind::Effect,
            HEX_EDITOR_OPERATION_TYPE,
            &value,
        )
        .expect("HexOperation serializes");
        specs.push(if available {
            spec
        } else {
            spec.unavailable("no_local_history", "No local edit available")
        });
    }
    for (id, label, value) in [
        ("search", "Search bytes", HexOperation::Search),
        ("goto-offset", "Go to offset", HexOperation::GotoOffset),
    ] {
        let mut spec = ActionSpec::operation(
            id,
            label,
            OperationKind::View,
            HEX_EDITOR_OPERATION_TYPE,
            &value,
        )
        .expect("HexOperation is always serializable")
        .with_contract(ActionContract::view());
        if !ctx.has_file {
            spec = spec.unavailable("no_file", "Open a file first");
        }
        specs.push(spec);
    }
    specs
}

/// Cursor / selection byte edits — live extras, not document state.
///
/// Hosted [`PublishesResources`](ui::resources::PublishesResources) returns
/// this slice alone so agents keep insert/overwrite/delete without a second
/// `hex_editor.document` root beside [`crate::resource::HexEditor`].
#[allow(clippy::expect_used, reason = "local edit operations always serialize")]
pub fn live_buffer_extras(ctx: &HexResourceCtx) -> Vec<Resource> {
    if !ctx.has_file {
        return Vec::new();
    }
    let specs = byte_edit_specs(ctx);
    if specs.is_empty() {
        return Vec::new();
    }
    let label = if let Some((lo, hi)) = ctx.selection {
        format!("Selection 0x{lo:X}..=0x{hi:X}")
    } else {
        format!("Cursor at 0x{:X}", ctx.cursor)
    };
    let mut cursor = Resource::new("hex_editor.cursor", "hex_editor.cursor", label);
    for spec in specs {
        cursor = cursor.with_action(spec);
    }
    vec![cursor]
}

#[allow(clippy::expect_used, reason = "local edit operations always serialize")]
fn byte_edit_specs(ctx: &HexResourceCtx) -> Vec<ActionSpec> {
    if !ctx.has_file {
        return Vec::new();
    }
    let mut specs = Vec::new();
    // Insert a byte at the cursor — a prompted hex value. The offset is the
    // clamped cursor so it is valid even for an append cursor / empty buffer.
    let at = ctx.cursor.min(ctx.len);
    specs.push(ActionSpec::prompted_operation(
        "insert-byte",
        "Insert byte",
        "Byte (hex)",
        "e.g. ff",
        OperationKind::Effect,
        HEX_EDITOR_OPERATION_TYPE,
        move |arg: &str| {
            // A malformed value inserts nothing (an empty splice, a no-op),
            // rather than fabricating a byte.
            let bytes = parse_hex_byte(arg).map(|b| vec![b]).unwrap_or_default();
            EncodedAction::encode_operation(
                HEX_EDITOR_OPERATION_TYPE,
                &HexOperation::Edit {
                    edit: (&insert_bytes_action(at, bytes)).into(),
                },
            )
            .expect("local edit serializes")
        },
    ));

    // Overwrite the byte under the cursor — only when the cursor is on a byte.
    if ctx.cursor < ctx.len {
        let at = ctx.cursor;
        specs.push(ActionSpec::prompted_operation(
            "overwrite-byte",
            "Overwrite byte",
            "Byte (hex)",
            "e.g. ff",
            OperationKind::Effect,
            HEX_EDITOR_OPERATION_TYPE,
            move |arg: &str| {
                let bytes = parse_hex_byte(arg).map(|b| vec![b]).unwrap_or_default();
                EncodedAction::encode_operation(
                    HEX_EDITOR_OPERATION_TYPE,
                    &HexOperation::Edit {
                        edit: (&overwrite_bytes_action(at, bytes)).into(),
                    },
                )
                .expect("local edit serializes")
            },
        ));
    }

    // Delete — the active selection, else the single byte at the cursor.
    if let Some((lo, hi)) = ctx.selection {
        specs.push(immediate_spec(
            "delete-selection",
            "Delete selection",
            &delete_bytes_action(lo, hi - lo + 1),
        ));
    } else if ctx.cursor < ctx.len {
        specs.push(immediate_spec(
            "delete-byte",
            "Delete byte",
            &delete_bytes_action(ctx.cursor, 1),
        ));
    }
    specs
}

// Inherent resource publication (was ResourceProvider)
impl crate::view::HexView {
    pub fn publish_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let ctx = self.hex_resource_ctx();
        let state = crate::resource::HexState {
            label: ctx.label.clone(),
        };
        let mut root = ui::tilo_resource::publish::<crate::resource::HexEditor>(
            "hex_editor.document",
            &ctx.label,
            &state,
        );
        root.children.extend(live_buffer_extras(&ctx));
        vec![root]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::HexView;
    use tilo_actions::LocalTransport;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// A view with a real local buffer plus the framework document required by
    /// the resource-provider trait. The bytes remain outside that document.
    fn open_view(bytes: &[u8]) -> (HexView, AutoCommit) {
        let mut view = HexView::new(bytes.to_vec(), "buf.bin");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        view.sync(&mut doc, &root);
        (view, doc)
    }

    /// A buffer open with the cursor on its second byte, so every byte-edit
    /// operation is published.
    fn view_on_second_byte() -> HexView {
        let (mut view, _) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(1, false);
        view
    }

    // Byte edits are device-local effects. The published door also carries
    // apply/group for the resource; the sweep checks the surface protocol.
    ui::action_contract_tests! {
        seed: open_view(&[0x00, 0x11, 0x22, 0x33]).1,
        registry: ActionRegistry::new(),
        resources: |doc| view_on_second_byte().publish_resources(doc),
        palette: ["Insert byte", "Overwrite byte", "Delete byte"],
    }

    /// The document resource's action set follows the buffer state: open/save
    /// jobs, the byte edits at the cursor, the empty state, and a selection.
    #[test]
    fn document_resource_actions_follow_buffer_state() {
        // Open and save publish direct paths as confirmed jobs.
        let state = crate::resource::HexState {
            label: "x.bin".into(),
        };
        let root = ui::tilo_resource::publish::<crate::resource::HexEditor>(
            "hex_editor.document",
            "x.bin",
            &state,
        );
        for id in ["open", "save"] {
            let spec = root.actions.iter().find(|a| a.id == id).unwrap();
            assert_eq!(spec.params()[0].name, "path", "case {id}");
            assert!(
                matches!(
                    spec.contract.execution,
                    ExecutionMode::Background { cancellable: true }
                ),
                "case {id}"
            );
            assert_eq!(spec.contract.result, ResultSpec::Job, "case {id}");
            assert!(
                !matches!(
                    spec.contract.confirmation,
                    tilo_actions::ConfirmationPolicy::None
                ),
                "case {id}"
            );
            let args = serde_json::json!({"path": "/tmp/x.bin"})
                .as_object()
                .unwrap()
                .clone();
            let encoded = spec.build_json(&args).unwrap();
            assert!(
                matches!(
                    decode_operation(HEX_EDITOR_OPERATION_TYPE, &encoded.payload)
                        .unwrap()
                        .unwrap(),
                    HexOperation::Open { path: Some(_) } | HexOperation::Save { path: Some(_) }
                ),
                "case {id}"
            );
        }

        // Ctrl+K yields the document door plus the cursor slice. Byte edits
        // stay on the cursor, not on the document root.
        let (mut view, doc) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(1, false);
        let resources = ui::resources::resources(&view, &doc);
        assert_eq!(resources.len(), 1, "one open buffer");
        let root = &resources[0];
        assert_eq!(root.kind, "hex_editor.document");
        let cursor = root
            .children
            .iter()
            .find(|child| child.kind == "hex_editor.cursor")
            .expect("cursor slice");
        let ids: Vec<&str> = cursor.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"insert-byte"), "{ids:?}");
        assert!(ids.contains(&"overwrite-byte"), "{ids:?}");
        assert!(ids.contains(&"delete-byte"), "{ids:?}");

        // The empty document still publishes open and save. Invoke time refuses
        // save when no buffer is open; the door itself stays available.
        let view = HexView::empty();
        let doc = AutoCommit::new();
        let resources = ui::resources::resources(&view, &doc);
        assert_eq!(resources.len(), 1);
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|action| action.id == "open")
        );
        assert!(
            resources[0]
                .actions
                .iter()
                .find(|action| action.id == "save")
                .is_some_and(|action| action.contract.availability.is_available())
        );

        // A selection swaps the single-byte delete for a whole-range one.
        let (mut view, doc) = open_view(&(0u8..=15).collect::<Vec<_>>());
        view.model_mut().move_to(2, false);
        view.model_mut().move_to(5, true); // select 2..=5
        let resources = ui::resources::resources(&view, &doc);
        let cursor = resources[0]
            .children
            .iter()
            .find(|child| child.kind == "hex_editor.cursor")
            .expect("selection slice");
        let ids: Vec<&str> = cursor.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"delete-selection"), "{ids:?}");
        assert!(
            !ids.contains(&"delete-byte"),
            "a selection replaces the single-byte delete"
        );
    }

    /// Raw-byte actions are device-local: the collaboration executor refuses
    /// them and none of them (overwrite, insert, delete) replays to a peer.
    #[test]
    fn raw_byte_actions_are_local_only() {
        // Legacy raw-byte envelopes are rejected by the collaboration executor.
        let mut state = ui::State::new();
        state.doc = AutoCommit::new();
        let action = overwrite_bytes_action(1, vec![0xAB]);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        assert!(
            tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).is_err()
        );

        // A raw-byte insert in the middle — the growth path, not just an
        // overwrite — cannot be replayed to another device.
        let mut a = AutoCommit::new();
        let action = insert_bytes_action(2, vec![0xCA, 0xFE]);
        let encoded = encode(&action);
        assert!(tilo_actions::apply_local(&mut a, &action, encoded).is_err());

        // Deletes are likewise local-only operations.
        let mut a = AutoCommit::new();
        let action = delete_bytes_action(3, 4); // drop 3,4,5,6
        let encoded = encode(&action);
        assert!(tilo_actions::apply_local(&mut a, &action, encoded).is_err());
    }

    /// Palette specs encode the same local edit as the keyboard and builder
    /// paths: delete-byte vs the keystroke, overwrite-byte vs the builder.
    #[test]
    fn palette_specs_encode_the_same_edit_as_keystrokes_and_builders() {
        // Palette effects and keyboard edits use the same local edit shape.
        let (mut view, doc) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(2, false);

        // The palette's "Delete byte" spec.
        let resources = ui::resources::resources(&view, &doc);
        let spec = resources[0]
            .children
            .iter()
            .find(|child| child.kind == "hex_editor.cursor")
            .expect("cursor slice")
            .actions
            .iter()
            .find(|a| a.id == "delete-byte")
            .expect("a delete-byte action");
        let from_spec = ui::testing::action_contract::spec_encoding(spec, None);

        // The keystroke path records a compact local redo edit.
        view.model_mut().delete_at_cursor();
        let edits = view.model_mut().take_local_history();
        assert_eq!(edits.len(), 1, "one delete journaled");
        let expected = HexOperation::Edit {
            edit: (&edits[0].redo).into(),
        };
        assert!(matches!(
            decode_operation(HEX_EDITOR_OPERATION_TYPE, &from_spec.payload).unwrap(),
            Some(operation) if operation == expected
        ));

        // The overwrite prompt builds the same action the hex-column keystroke
        // does when both target the same byte.
        let (mut view, doc) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(1, false);
        let resources = ui::resources::resources(&view, &doc);
        let spec = resources[0]
            .children
            .iter()
            .find(|child| child.kind == "hex_editor.cursor")
            .expect("cursor slice")
            .actions
            .iter()
            .find(|a| a.id == "overwrite-byte")
            .expect("an overwrite-byte action");
        let from_spec = ui::testing::action_contract::spec_encoding(spec, Some("ab"));
        let expected = HexOperation::Edit {
            edit: (&overwrite_bytes_action(1, vec![0xAB])).into(),
        };
        assert!(matches!(
            decode_operation(HEX_EDITOR_OPERATION_TYPE, &from_spec.payload).unwrap(),
            Some(operation) if operation == expected
        ));
    }
}
