//! Code's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! ## What is actually in the shared document
//!
//! Code's collaborative state is deliberately small. The one thing that lives in
//! the CRDT is each file's **buffer text**, stored as an Automerge `Text` under
//! `root["buffers"]` keyed by node id (see [`crate::doc`]). Because that text is
//! a `Text` object, every keystroke is a splice on the *native* collaborative
//! text path — two peers typing converge, and undo/redo snapshots the whole doc.
//! Those per-keystroke edits are **not** actions and must never be wrapped as
//! ones (the recipe's rule: a flowing text body edits through the text path, not
//! the action executor).
//!
//! The rest of the editor's world — the project file tree, which tabs are open,
//! the active tab, folder expansion, the detected language, the dirty flag — is
//! **in-memory only** ([`crate::model::Session`] / [`crate::model::Project`]); it
//! is not persisted to the document. So there is no file/tab/language model in
//! the CRDT to hang rename / delete / reorder / set-language actions off of, and
//! this module does **not** fabricate one.
//!
//! ## The one genuine structural mutation
//!
//! That leaves exactly one document-mutating operation that is *not* raw text:
//! **materialising a buffer** in `root["buffers"]`. Creating a new file, and a
//! peer (or a Ctrl+K "New file") bringing a buffer into existence, both come down
//! to "ensure a `Text` object exists for this node id, seeded with this content".
//! That is [`CodeEditorAction::CreateBuffer`] — the single collaborative,
//! replayable mutation Code exports. Its payload carries the node id and the seed,
//! so [`Action::apply`] is a pure function of `(payload, base document)`: no RNG,
//! no wall-clock, no app state. It is idempotent — a buffer that already exists is
//! left untouched — so a duplicate or out-of-order replay is a no-op, never a
//! wedge.
//!
//! ## The split
//!
//! Document-mutating → an action: creating a buffer (New file). Presentation →
//! never an action: which file is open, the tab order, the caret / selection /
//! scroll, the sidebar flap, the active theme, the bottom-panel tab. Those local
//! operations are still exported on file resources: opening a file is a view
//! operation and saving its collaborative buffer to disk is an effect. The
//! language server, linter, and embedded terminal remain app-owned services.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResourceProvider,
    ResultSpec, RetryPolicy,
};

use crate::doc::EditorDoc;

/// Registry `type_id` for every Code action.
pub const CODE_EDITOR_ACTION_TYPE: &str = "code_editor.action";
pub const CODE_EDITOR_OPERATION_TYPE: &str = "code_editor.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeEditorOperation {
    Open { node: u64 },
    OpenFile { path: Option<String> },
    OpenFolder { path: Option<String> },
    Save { node: u64, path: Option<String> },
}

fn file_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: CodeEditorOperation,
) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, CODE_EDITOR_OPERATION_TYPE, &operation)
        .expect("CodeEditorOperation is always serializable");
    match operation {
        CodeEditorOperation::Open { .. } => spec.retry_policy(RetryPolicy::Idempotent),
        CodeEditorOperation::OpenFile { .. } | CodeEditorOperation::OpenFolder { .. } => spec
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        CodeEditorOperation::Save { .. } => spec.destructive(),
    }
}

/// Every collaborative operation Code exports on the shared document.
///
/// There is exactly one, and by design: the only shared state that is not the
/// native collaborative text path is a buffer's *existence*. See the module
/// docs for why the file/tab/language model is not represented here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CodeEditorAction {
    /// Ensure a buffer `Text` object exists for file `node` in
    /// `root["buffers"]`, seeded with `seed` if it is freshly created. Keyed by
    /// node id and seeded from the payload, so replay reproduces the identical
    /// object on every replica; idempotent, so a buffer that already exists is
    /// left untouched.
    CreateBuffer { node: u64, seed: String },
}

/// Durable, browseable history metadata for a Code mutation.
pub fn history_context(action: &CodeEditorAction, file_name: &str) -> qos_actions::ActionContext {
    let mut context = qos_actions::ActionContext::for_action("org.qos.Code", "Code", action);
    let CodeEditorAction::CreateBuffer { node, .. } = action;
    context.action_label = "Create file".to_string();
    context.resource = Some(qos_actions::ActionResource {
        id: format!("code_editor.file:{node}"),
        kind: "code_editor.file".to_string(),
        label: file_name.to_string(),
    });
    if let Some(extension) = file_name.rsplit_once('.').map(|(_, extension)| extension) {
        context
            .facets
            .push(qos_actions::ActionFacet::new("file-type", extension));
    }
    context
}

impl Action for CodeEditorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CodeEditorAction::CreateBuffer { node, seed } => {
                EditorDoc::ensure_buffer(doc, *node as usize, seed);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Materialising a buffer is a local content op; peers replay it.
        ExecutionLocus::ClientLocal
    }
}

/// Register Code's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<CodeEditorAction>(CODE_EDITOR_ACTION_TYPE);
}

/// Encode any [`CodeEditorAction`] into the wire form the palette and the
/// on-screen affordances both dispatch — the single point that turns a Code
/// action into a replayable envelope.
///
/// # Errors
/// Returns an error if `action` can't be serialized (it always can — every
/// `CodeEditorAction` variant holds only plain, serializable data — so this
/// is here for the caller to propagate rather than something expected to
/// trigger).
pub fn encode(action: &CodeEditorAction) -> Result<EncodedAction, ActionError> {
    EncodedAction::encode(CODE_EDITOR_ACTION_TYPE, action)
}

/// Build a "create buffer" action for `node`, seeded with `seed`. The single
/// source of truth shared by the on-screen "New file" affordance (see
/// [`crate::app`]) and the Ctrl+K resource tree below.
pub fn create_buffer_action(node: u64, seed: &str) -> CodeEditorAction {
    CodeEditorAction::CreateBuffer {
        node,
        seed: seed.to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One open buffer, as the resource tree presents it: its node id (the buffer
/// key) and display name. Derived from the in-memory session — nothing here is
/// fabricated, it mirrors the tabs the user has open.
#[derive(Debug, Clone)]
pub struct OpenFileRef {
    pub node: u64,
    pub name: String,
    /// Whether this buffer already has a filesystem destination. Scratch
    /// buffers require a picker and therefore cannot honestly export Save as
    /// an immediate effect.
    pub can_save: bool,
}

/// The presentation context Code hands [`provide_resources`]: the project name,
/// the id the next created buffer will take (so the palette's "New file" targets
/// the same buffer the on-screen command does), and the open buffers.
#[derive(Debug, Clone)]
pub struct CodeEditorResourceCtx {
    pub project_name: String,
    /// The node id the next created file/buffer will use. Captured at authoring
    /// time so the Ctrl+K "New file" spec and the app's own `New File` command
    /// build the byte-identical action.
    pub next_id: u64,
    pub open_files: Vec<OpenFileRef>,
}

/// Build the resource tree Code currently has open: the project root (always
/// present, exporting "New file") and one `code_editor.file:<node>` per open
/// buffer. The project's "New file" spec builds the same [`CreateBuffer`] action
/// the on-screen command dispatches — one source of truth, one replay path.
///
/// The file children carry no doc-mutating actions of their own: an open file's
/// only collaborative mutation is editing its text, which travels the native
/// text path, not the action executor. They are published so the open buffers
/// are addressable by their stable `code_editor.file` kind (OS-wide resource
/// enumeration), and go-to-file navigation stays in the app's own quick-open.
///
/// [`CreateBuffer`]: CodeEditorAction::CreateBuffer
///
/// # Panics
/// Panics if the "New file" action fails to encode. `CreateBuffer` holds
/// only a `u64` and a `String`, which always serialize, so this is not a
/// data-dependent failure; the [`ResourceProvider`] trait this feeds gives
/// no `Result` to propagate one through instead.
pub fn provide_resources(ctx: &CodeEditorResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let label = if ctx.project_name.trim().is_empty() {
        "Project".to_string()
    } else {
        ctx.project_name.clone()
    };
    let mut project = Resource::new("code_editor.project", "code_editor.project", label);
    for (id, label, folder) in [
        ("open-file", "Open file", false),
        ("open-folder", "Open project folder", true),
    ] {
        project = project.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "path (omit to use the system file picker)",
                OperationKind::Effect,
                CODE_EDITOR_OPERATION_TYPE,
                vec![ActionParam::optional(
                    "path",
                    ParamType::Text,
                    "filesystem path",
                )],
                move |args: &ActionArgs| {
                    EncodedAction::encode_operation(
                        CODE_EDITOR_OPERATION_TYPE,
                        &if folder {
                            CodeEditorOperation::OpenFolder {
                                path: args
                                    .text("path")
                                    .filter(|p| !p.is_empty())
                                    .map(str::to_owned),
                            }
                        } else {
                            CodeEditorOperation::OpenFile {
                                path: args
                                    .text("path")
                                    .filter(|p| !p.is_empty())
                                    .map(str::to_owned),
                            }
                        },
                    )
                    .expect("CodeEditorOperation is always serializable")
                },
            )
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        );
    }

    // Project-level: create a new (untitled) buffer. Immediate, not prompted —
    // the file's *name* lives only in the in-memory model, so a pure-document
    // action has nowhere to carry it; the app names the freshly-adopted buffer.
    #[allow(
        clippy::expect_used,
        reason = "CreateBuffer holds only a u64 and a String, which always encode; ResourceProvider::resources returns Vec<Resource> with no Result to propagate a failure through instead"
    )]
    let new_file = ActionSpec::immediate(
        "new-file",
        "New file",
        CODE_EDITOR_ACTION_TYPE,
        &create_buffer_action(ctx.next_id, ""),
    )
    .expect("encode new-file action");
    project = project.with_action(new_file);

    for f in &ctx.open_files {
        let label = if f.name.trim().is_empty() {
            "untitled".to_string()
        } else {
            f.name.clone()
        };
        let mut file = Resource::new(
            format!("code_editor.file:{}", f.node),
            "code_editor.file",
            label,
        )
        .with_action(file_operation(
            "open",
            "Open file",
            OperationKind::View,
            CodeEditorOperation::Open { node: f.node },
        ));
        let node = f.node;
        file = file.with_action(
            ActionSpec::typed_operation(
                "save",
                "Save file",
                "path (omit to save in place)",
                OperationKind::Effect,
                CODE_EDITOR_OPERATION_TYPE,
                vec![ActionParam::optional(
                    "path",
                    ParamType::Text,
                    "save destination",
                )],
                move |args: &ActionArgs| {
                    EncodedAction::encode_operation(
                        CODE_EDITOR_OPERATION_TYPE,
                        &CodeEditorOperation::Save {
                            node,
                            path: args
                                .text("path")
                                .filter(|p| !p.is_empty())
                                .map(str::to_owned),
                        },
                    )
                    .expect("CodeEditorOperation is always serializable")
                },
            )
            .destructive()
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        );
        project = project.with_child(file);
    }

    vec![project]
}

impl ResourceProvider for crate::app::CodeApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.code_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    #[test]
    fn file_io_has_typed_paths_and_background_job_contracts() {
        let ctx = CodeEditorResourceCtx {
            project_name: "demo".into(),
            next_id: 2,
            open_files: vec![OpenFileRef {
                node: 1,
                name: "x.rs".into(),
                can_save: false,
            }],
        };
        let resources = provide_resources(&ctx, &AutoCommit::new());
        for spec in resources[0]
            .actions
            .iter()
            .filter(|a| a.id.starts_with("open-"))
            .chain(
                resources[0].children[0]
                    .actions
                    .iter()
                    .filter(|a| a.id == "save"),
            )
        {
            assert_eq!(spec.params()[0].name, "path");
            assert!(matches!(
                spec.contract.execution,
                ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(spec.contract.result, ResultSpec::Job);
            if spec.id == "save" {
                assert!(!matches!(
                    spec.contract.confirmation,
                    qos_actions::ConfirmationPolicy::None
                ));
            }
        }
    }

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does (decode from the wire, apply through a local transport).
    fn dispatch(doc: &mut AutoCommit, action: &CodeEditorAction) {
        let encoded = encode(action).unwrap();
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    /// Creating a buffer through the action materialises its seeded text in the
    /// document.
    #[test]
    fn create_buffer_action_writes_the_seeded_text() {
        let mut doc = AutoCommit::new();
        assert!(EditorDoc::buffer_text(&doc, 7).is_none(), "no buffer yet");
        dispatch(&mut doc, &create_buffer_action(7, "fn main() {}\n"));
        assert_eq!(
            EditorDoc::read_buffer(&doc, 7).as_deref(),
            Some("fn main() {}\n")
        );
    }

    /// The action is idempotent: a second create with a different seed does not
    /// overwrite the existing buffer (so a duplicate/out-of-order replay is a
    /// no-op, not a wedge).
    #[test]
    fn create_buffer_action_is_idempotent() {
        let mut doc = AutoCommit::new();
        dispatch(&mut doc, &create_buffer_action(1, "first"));
        dispatch(&mut doc, &create_buffer_action(1, "second"));
        assert_eq!(EditorDoc::read_buffer(&doc, 1).as_deref(), Some("first"));
    }

    /// Ctrl+K on Code yields the project root exporting "New file" and a
    /// `code_editor.file:<node>` child per open buffer. Drives the real
    /// resource-builder path and the real palette.
    #[test]
    fn ctrl_k_publishes_the_project_and_file_tree() {
        let ctx = CodeEditorResourceCtx {
            project_name: "demo".into(),
            next_id: 42,
            open_files: vec![
                OpenFileRef {
                    node: 3,
                    name: "main.rs".into(),
                    can_save: true,
                },
                OpenFileRef {
                    node: 5,
                    name: "lib.rs".into(),
                    can_save: true,
                },
            ],
        };
        let doc = AutoCommit::new();
        let resources = provide_resources(&ctx, &doc);
        assert_eq!(resources.len(), 1, "one project root");
        let project = &resources[0];
        assert_eq!(project.kind, "code_editor.project");
        assert_eq!(project.label, "demo");
        let p_actions: Vec<&str> = project.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(p_actions.contains(&"new-file"), "{p_actions:?}");

        assert_eq!(project.children.len(), 2, "one resource per open buffer");
        assert_eq!(project.children[0].kind, "code_editor.file");
        assert_eq!(project.children[0].id.as_str(), "code_editor.file:3");
        assert_eq!(project.children[0].label, "main.rs");
        let file_actions = &project.children[0].actions;
        assert_eq!(
            file_actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec!["open", "save"]
        );
        assert_eq!(file_actions[0].kind, OperationKind::View);
        assert_eq!(file_actions[1].kind, OperationKind::Effect);
        assert_eq!(
            immediate_encode(&file_actions[0]).type_id,
            CODE_EDITOR_OPERATION_TYPE
        );
        assert_eq!(file_actions[1].params()[0].name, "path");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("New file")), "{labels:?}");
    }

    /// The full undo covenant on the action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        let before_heads = state.doc.get_heads();
        assert!(EditorDoc::buffer_text(&state.doc, 9).is_none());

        // Create a buffer via the action + host undo recording.
        let encoded = encode(&create_buffer_action(9, "hello")).unwrap();
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            EditorDoc::read_buffer(&state.doc, 9).as_deref(),
            Some("hello"),
            "the action created the buffer"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            EditorDoc::buffer_text(&state.doc, 9).is_none(),
            "undo reverted the created buffer and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = AutoCommit::new();
        // A pre-existing buffer, so the base state is non-trivial.
        EditorDoc::ensure_buffer(&mut a, 1, "base\n");
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = create_buffer_action(2, "fn main() {}\n");
        let encoded = encode(&action).unwrap();
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            EditorDoc::read_buffer(&b, 2).as_deref(),
            Some("fn main() {}\n")
        );
    }

    /// Single source of truth: the palette's "New file" spec encodes
    /// byte-identically to the builder the on-screen command calls, for the same
    /// target node id.
    #[test]
    fn palette_and_builder_encode_identically() {
        let ctx = CodeEditorResourceCtx {
            project_name: "demo".into(),
            next_id: 12,
            open_files: Vec::new(),
        };
        let doc = AutoCommit::new();
        let resources = provide_resources(&ctx, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-file")
            .unwrap();

        let from_spec = immediate_encode(spec);
        let from_builder = encode(&create_buffer_action(12, "")).unwrap();
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "new-file is one source of truth"
        );
    }
}
