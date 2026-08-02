//! The hex editor's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation the editor performs on its shared buffer is
//! a [`HexEditorAction`]: a small serializable value that deterministically
//! rewrites the `bytes` blob in the CRDT (see [`crate::doc`], where the whole
//! buffer lives as one Automerge `Bytes` scalar). Because the buffer already
//! lives in the shared document, an action replicates and replays for free. The
//! exact same [`EncodedAction`] is what the keyboard affordances (typing a hex
//! digit, an ASCII character, Backspace, Delete) dispatch and what the Ctrl+K
//! palette dispatches — one source of truth, one replay path.
//!
//! A byte buffer has no natural sub-items, so the resource tree is a single flat
//! `hex_editor.document` root that exports the byte-edit actions positioned at
//! the current cursor / selection.
//!
//! Presentation — the cursor, the selection, the scroll position, the
//! insert-vs-overwrite mode, the hex-vs-ASCII focus, go-to-offset navigation —
//! stays out of the document and never becomes an action. Loading and saving the
//! file bytes to disk is external I/O, not a collaborative edit, so it is not an
//! action either.
//!
//! ## Determinism contract
//!
//! Each variant carries the exact byte offset and the literal bytes, so
//! [`Action::apply`] is a pure function of the payload and the buffer it reads:
//! it never touches the RNG or the wall-clock, and out-of-range offsets are
//! clamped exactly as the model clamps them, so two replicas at the same heads
//! land on the byte-identical blob. A missing buffer is a **no-op** rather than
//! an error, so a bad replay can't wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::HexDoc;
use crate::model::BufferEdit;

/// Registry `type_id` for every hex-editor action.
pub const HEX_EDITOR_ACTION_TYPE: &str = "hex_editor.action";

/// Every collaborative operation the hex editor exports on its byte buffer.
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
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // No buffer seeded yet → nothing to edit. A no-op keeps replay from
        // wedging on an out-of-order envelope.
        let Some(mut buf) = HexDoc::read_bytes(doc) else {
            return Ok(());
        };
        match self {
            HexEditorAction::InsertBytes { offset, bytes } => {
                // Clamp exactly as `HexModel::insert_byte` does, so the action
                // reproduces the model's own result.
                let at = (*offset).min(buf.len());
                let tail = buf.split_off(at);
                buf.extend_from_slice(bytes);
                buf.extend_from_slice(&tail);
            }
            HexEditorAction::DeleteBytes { offset, len } => {
                if *offset < buf.len() {
                    let end = offset.saturating_add(*len).min(buf.len());
                    buf.drain(*offset..end);
                }
            }
            HexEditorAction::OverwriteBytes { offset, bytes } => {
                for (i, b) in bytes.iter().enumerate() {
                    let idx = offset.saturating_add(i);
                    if idx < buf.len() {
                        buf[idx] = *b;
                    }
                }
            }
        }
        HexDoc::write_bytes(doc, &buf);
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the hex editor's action decoder for palette execution + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<HexEditorAction>(HEX_EDITOR_ACTION_TYPE);
}

/// Encode any [`HexEditorAction`] into the wire form the palette and the
/// keyboard affordances both dispatch. The single point that turns a buffer
/// edit into a replayable envelope.
pub fn encode(action: &HexEditorAction) -> EncodedAction {
    EncodedAction::encode(HEX_EDITOR_ACTION_TYPE, action)
        .expect("HexEditorAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the keyboard
// affordances (see `crate::view`) and the Ctrl+K resource tree below.
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

/// Map a model-journaled [`BufferEdit`] onto the action that reproduces it. This
/// is the bridge that makes a keystroke and a palette row build the identical
/// [`HexEditorAction`].
pub fn edit_to_action(edit: &BufferEdit) -> HexEditorAction {
    match edit {
        BufferEdit::Insert { offset, bytes } => insert_bytes_action(*offset, bytes.clone()),
        BufferEdit::Delete { offset, len } => delete_bytes_action(*offset, *len),
        BufferEdit::Overwrite { offset, bytes } => overwrite_bytes_action(*offset, bytes.clone()),
    }
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
}

/// Parse a one-byte hex value out of a palette prompt: accepts `ff`, `0xFF`, or
/// a bare `7`. Returns `None` for anything that isn't a single byte.
fn parse_hex_byte(s: &str) -> Option<u8> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u8::from_str_radix(t, 16).ok()
}

fn immediate_spec(id: &str, label: &str, action: &HexEditorAction) -> ActionSpec {
    ActionSpec::immediate(id, label, HEX_EDITOR_ACTION_TYPE, action)
        .expect("encode hex_editor action")
}

/// Build the resource tree the editor currently has open: the buffer as one
/// `hex_editor.document` root, exporting the byte-edit actions at the cursor /
/// selection. Every action is the same one the matching keystroke dispatches.
pub fn provide_resources(ctx: &HexResourceCtx) -> Vec<Resource> {
    let mut root = Resource::new("hex_editor.document", "hex_editor.document", ctx.label.clone());

    // Insert a byte at the cursor — a prompted hex value. The offset is the
    // clamped cursor so it is valid even for an append cursor / empty buffer.
    let at = ctx.cursor.min(ctx.len);
    root = root.with_action(ActionSpec::prompted(
        "insert-byte",
        "Insert byte",
        "Byte (hex)",
        "e.g. ff",
        ExecutionLocus::ClientLocal,
        move |arg: &str| {
            // A malformed value inserts nothing (an empty splice, a no-op),
            // rather than fabricating a byte.
            let bytes = parse_hex_byte(arg).map(|b| vec![b]).unwrap_or_default();
            encode(&insert_bytes_action(at, bytes))
        },
    ));

    // Overwrite the byte under the cursor — only when the cursor is on a byte.
    if ctx.cursor < ctx.len {
        let at = ctx.cursor;
        root = root.with_action(ActionSpec::prompted(
            "overwrite-byte",
            "Overwrite byte",
            "Byte (hex)",
            "e.g. ff",
            ExecutionLocus::ClientLocal,
            move |arg: &str| {
                let bytes = parse_hex_byte(arg).map(|b| vec![b]).unwrap_or_default();
                encode(&overwrite_bytes_action(at, bytes))
            },
        ));
    }

    // Delete — the active selection, else the single byte at the cursor.
    if let Some((lo, hi)) = ctx.selection {
        root = root.with_action(immediate_spec(
            "delete-selection",
            "Delete selection",
            &delete_bytes_action(lo, hi - lo + 1),
        ));
    } else if ctx.cursor < ctx.len {
        root = root.with_action(immediate_spec(
            "delete-byte",
            "Delete byte",
            &delete_bytes_action(ctx.cursor, 1),
        ));
    }

    vec![root]
}

impl ResourceProvider for crate::view::HexView {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        // Nothing published in the empty state — there is no open buffer.
        if !self.has_file() {
            return Vec::new();
        }
        provide_resources(&self.hex_resource_ctx())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::HexView;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            _ => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// A document seeded with a known buffer, via the same `write_model` the
    /// view uses to seed the shared doc.
    fn seeded(bytes: &[u8]) -> AutoCommit {
        let mut doc = AutoCommit::new();
        HexDoc::write_model(&mut doc, &crate::model::HexModel::new(bytes.to_vec()));
        doc
    }

    /// A view with a real buffer open, synced onto its own document so the
    /// resource hook reflects a live cursor.
    fn open_view(bytes: &[u8]) -> (HexView, AutoCommit) {
        let mut view = HexView::new(bytes.to_vec(), "buf.bin");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        view.sync(&mut doc, &root);
        (view, doc)
    }

    /// Ctrl+K on the editor yields one `hex_editor.document` root exporting the
    /// byte-edit actions at the cursor. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_document_resource() {
        let (mut view, doc) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(1, false);
        let resources = Widget::resources(&view, &doc);
        assert_eq!(resources.len(), 1, "one open buffer");
        let root = &resources[0];
        assert_eq!(root.kind, "hex_editor.document");
        let ids: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"insert-byte"), "{ids:?}");
        assert!(ids.contains(&"overwrite-byte"), "{ids:?}");
        assert!(ids.contains(&"delete-byte"), "{ids:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("Insert byte")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Delete byte")), "{labels:?}");
    }

    /// The empty state publishes nothing — there is no open buffer to act on.
    #[test]
    fn empty_state_publishes_no_resources() {
        let view = HexView::empty();
        let doc = AutoCommit::new();
        assert!(Widget::resources(&view, &doc).is_empty());
    }

    /// A selection swaps the single-byte delete for a whole-range one.
    #[test]
    fn selection_exports_a_range_delete() {
        let (mut view, doc) = open_view(&(0u8..=15).collect::<Vec<_>>());
        view.model_mut().move_to(2, false);
        view.model_mut().move_to(5, true); // select 2..=5
        let resources = Widget::resources(&view, &doc);
        let ids: Vec<&str> = resources[0].actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"delete-selection"), "{ids:?}");
        assert!(!ids.contains(&"delete-byte"), "a selection replaces the single-byte delete");
    }

    /// Dispatching an action mutates the buffer, records exactly one undo
    /// boundary through the host, and Ctrl+Z reverts precisely that edit.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        state.doc = seeded(&[0x00, 0x11, 0x22, 0x33]);
        let before_heads = state.doc.get_heads();

        let action = overwrite_bytes_action(1, vec![0xAB]);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads.clone(), None, None);

        assert_eq!(
            HexDoc::read_bytes(&state.doc).unwrap(),
            vec![0x00, 0xAB, 0x22, 0x33],
            "the action overwrote byte 1"
        );
        assert!(state.peek_last_undo().is_some(), "exactly one undo boundary recorded");

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            HexDoc::read_bytes(&state.doc).unwrap(),
            vec![0x00, 0x11, 0x22, 0x33],
            "undo reverted the overwrite and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a
    /// second independent replica (from the same base state) lands on the
    /// byte-identical document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = seeded(&[0xDE, 0xAD, 0xBE, 0xEF]);
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "precondition: same base state");

        // An insert in the middle — the growth path, not just an overwrite.
        let action = insert_bytes_action(2, vec![0xCA, 0xFE]);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "replay reproduced the identical change");
        assert_eq!(
            HexDoc::read_bytes(&a).unwrap(),
            vec![0xDE, 0xAD, 0xCA, 0xFE, 0xBE, 0xEF],
        );
        assert_eq!(HexDoc::read_bytes(&a).unwrap(), HexDoc::read_bytes(&b).unwrap());
    }

    /// A delete also converges byte-identically on a peer.
    #[test]
    fn delete_replays_byte_identical() {
        let mut a = seeded(&(0u8..=9).collect::<Vec<_>>());
        a.commit();
        let mut b = AutoCommit::load(&a.save()).unwrap();

        let action = delete_bytes_action(3, 4); // drop 3,4,5,6
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads());
        assert_eq!(HexDoc::read_bytes(&a).unwrap(), vec![0, 1, 2, 7, 8, 9]);
    }

    /// Single source of truth: the palette's delete spec and the keystroke's
    /// journaled edit encode byte-identically — one action, two triggers.
    #[test]
    fn palette_and_keystroke_encode_identically() {
        let (mut view, doc) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(2, false);

        // The palette's "Delete byte" spec.
        let resources = Widget::resources(&view, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "delete-byte")
            .expect("a delete-byte action");
        let from_spec = immediate_encode(spec);

        // The keystroke path: Delete at the cursor journals a `BufferEdit`,
        // which maps to the identical action.
        view.model_mut().delete_at_cursor();
        let edits = view.model_mut().take_edits();
        assert_eq!(edits.len(), 1, "one delete journaled");
        let from_keystroke = encode(&edit_to_action(&edits[0]));

        assert_eq!(from_spec.type_id, from_keystroke.type_id);
        assert_eq!(from_spec.payload, from_keystroke.payload, "one source of truth for delete");
    }

    /// The overwrite prompt builds the same action the hex-column keystroke
    /// does when both target the same byte.
    #[test]
    fn overwrite_prompt_matches_the_builder() {
        let (mut view, doc) = open_view(&[0x00, 0x11, 0x22, 0x33]);
        view.model_mut().move_to(1, false);
        let resources = Widget::resources(&view, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "overwrite-byte")
            .expect("an overwrite-byte action");
        let from_spec = prompted_encode(spec, "ab");
        let from_builder = encode(&overwrite_bytes_action(1, vec![0xAB]));
        assert_eq!(from_spec.payload, from_builder.payload, "overwrite is one source of truth");
    }
}
