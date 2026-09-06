//! Code's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! ## What is actually in the shared document
//!
//! Code's collaborative state has two layers: durable file records in
//! `root["files"]`, keyed by UUID, and each file's **buffer text**, stored as an
//! Automerge `Text` under `root["buffers"]` with the same key (see [`crate::doc`]). Because that text is
//! a `Text` object, every keystroke is a compact `EditText` action whose
//! dispatcher owns the native collaborative splice. Two peers' disjoint edits
//! rebase through bounded context; overlapping edits fail closed, and undo/redo
//! snapshots the whole document.
//!
//! File creation, name, path, parent, language, ordering and tombstones are
//! shared file-record fields. Tabs, caret/selection/scroll, folder expansion,
//! and dirty/save indicators are view-local presence or disk-projection state.
//!
//! Structural mutations are explicit validated actions. Their payloads contain
//! the UUID selected at authoring time (never a local arena id), so replay is a
//! pure function of payload and document. Updates carry an expected revision
//! for the field they change, so stale same-field requests are refused without
//! needlessly blocking a peer's independent rename, move, or language update.
//! Create is idempotent for duplicate/reordered replay.
//!
//! ## The split
//!
//! Document-mutating → an action: creating, renaming, moving, retagging, and
//! tombstoning durable files/folders (and creating a file's text body). Presentation →
//! never an action: which file is open, the tab order, the caret / selection /
//! scroll, the sidebar flap, the active theme, the bottom-panel tab. Those local
//! operations are still exported on file resources: opening a file is a view
//! operation and saving its collaborative buffer to disk is an effect. The
//! language server, linter, and embedded terminal remain app-owned services.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use qos_actions::{
    Action, ActionAddress, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec, RetryPolicy,
};

use crate::command_palette::Command;
use crate::doc::{CollaborativeFile, EditorDoc, FilePatch, ParentPatch};

/// Registry `type_id` for every Code action.
pub const CODE_EDITOR_ACTION_TYPE: &str = "code_editor.action";
pub const CODE_EDITOR_OPERATION_TYPE: &str = "code_editor.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeEditorOperation {
    Open {
        file_id: String,
    },
    /// Open a file and reproduce an exact editor location. A disk path is
    /// carried for project-search hits whose lazy tree node is not revealed
    /// yet; symbols use `node` directly. `show_search` restores the results
    /// panel for a result row but stays false for file/symbol navigation.
    RevealLocation {
        file_id: Option<String>,
        path: Option<String>,
        line: usize,
        col: usize,
        show_search: bool,
    },
    OpenFile {
        path: Option<String>,
    },
    OpenFolder {
        path: Option<String>,
    },
    Save {
        file_id: String,
        path: Option<String>,
    },
    Command {
        id: String,
        argument: Option<String>,
    },
}

/// # Errors
/// Returns an error when a command operation names no catalogue command.
pub fn operation_kind(operation: &CodeEditorOperation) -> Result<OperationKind, String> {
    let kind = match operation {
        CodeEditorOperation::Open { .. } | CodeEditorOperation::RevealLocation { .. } => {
            OperationKind::View
        }
        CodeEditorOperation::OpenFile { .. }
        | CodeEditorOperation::OpenFolder { .. }
        | CodeEditorOperation::Save { .. } => OperationKind::Effect,
        CodeEditorOperation::Command { id, .. } => {
            let command =
                Command::from_id(id).ok_or_else(|| format!("unknown code editor command: {id}"))?;
            match command {
                Command::OpenFolder
                | Command::OpenFile
                | Command::Save
                | Command::SaveAs
                | Command::SaveAll
                | Command::RevertFile => OperationKind::Effect,
                _ => OperationKind::View,
            }
        }
    };
    Ok(kind)
}

/// Build a deliberately-invalid action envelope for resource input or
/// serialization failures. Resource closures cannot return `Result`; this
/// keeps failures visible to the shared action host rather than panicking.
fn invalid_arguments(message: &str) -> EncodedAction {
    EncodedAction {
        type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: message.as_bytes().to_vec(),
    }
}

fn document_action_spec<A>(id: &str, label: &str, action: &A) -> ActionSpec
where
    A: Action + Serialize,
{
    match ActionSpec::immediate(id, label, CODE_EDITOR_ACTION_TYPE, action) {
        Ok(spec) => spec,
        Err(_) => ActionSpec::immediate_encoded(
            id,
            label,
            ExecutionLocus::ClientLocal,
            invalid_arguments("unable to encode code editor action"),
        ),
    }
}

fn file_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &CodeEditorOperation,
) -> ActionSpec {
    let encoded_operation = operation.clone();
    let spec = ActionSpec::typed_operation(
        id,
        label,
        "",
        kind,
        CODE_EDITOR_OPERATION_TYPE,
        Vec::new(),
        move |_| {
            EncodedAction::encode_operation(CODE_EDITOR_OPERATION_TYPE, &encoded_operation)
                .unwrap_or_else(|_| invalid_arguments("unable to encode code editor operation"))
        },
    );
    match operation {
        CodeEditorOperation::Open { .. } | CodeEditorOperation::RevealLocation { .. } => {
            spec.retry_policy(RetryPolicy::Idempotent)
        }
        CodeEditorOperation::OpenFile { .. } | CodeEditorOperation::OpenFolder { .. } => spec
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        CodeEditorOperation::Save { .. } => spec.destructive(),
        CodeEditorOperation::Command { .. } => spec,
    }
}

pub fn file_resource_id(file: impl std::fmt::Display) -> String {
    format!("code_editor.file:{file}")
}

pub fn folder_resource_id(folder: impl std::fmt::Display) -> String {
    format!("code_editor.folder:{folder}")
}

pub fn project_resource_id(project: impl std::fmt::Display) -> String {
    format!("code_editor.project:{project}")
}

pub fn open_file_address(file: impl std::fmt::Display) -> ActionAddress {
    ActionAddress::new(file_resource_id(file), "open")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRef {
    pub file_id: String,
    pub name: String,
    pub path: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultRef {
    pub file_id: Option<String>,
    pub path: String,
    pub disk_path: Option<String>,
    pub preview: String,
    pub line: usize,
    pub col: usize,
}

fn path_key(path: &str) -> String {
    path.as_bytes().iter().fold(
        String::with_capacity(path.len().saturating_mul(2)),
        |mut key, byte| {
            let _ = write!(key, "{byte:02x}");
            key
        },
    )
}

pub fn symbol_resource_id(symbol: &SymbolRef) -> String {
    format!(
        "code_editor.symbol:{}:{}:{}",
        symbol.file_id, symbol.line, symbol.col
    )
}

pub fn result_resource_id(result: &SearchResultRef) -> String {
    format!(
        "code_editor.search-result:{}:{}:{}",
        path_key(&result.path),
        result.line,
        result.col
    )
}

pub fn open_symbol_address(symbol: &SymbolRef) -> ActionAddress {
    ActionAddress::new(symbol_resource_id(symbol), "open")
}

pub fn reveal_result_address(result: &SearchResultRef) -> ActionAddress {
    ActionAddress::new(result_resource_id(result), "reveal")
}

/// Every collaborative operation Code exports on the shared document.
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CodeEditorAction {
    /// One character-addressed edit against an exact displayed base. The
    /// payload is proportional to the changed region, never the full file.
    EditText {
        file_id: String,
        expected_base_digest: String,
        start_chars: usize,
        delete_chars: usize,
        insert: String,
        /// Short immutable context makes a stale action safely rebaseable
        /// when a peer changed a disjoint range. It is deliberately bounded,
        /// so an action remains proportional to its changed region.
        #[serde(default)]
        before_context: String,
        #[serde(default)]
        deleted_text: String,
        #[serde(default)]
        after_context: String,
    },
    /// Create a durable file record and its Text body. `id` is a UUID minted at
    /// the command/resource boundary, not during replay.
    CreateFile {
        id: String,
        name: String,
        parent: Option<String>,
        language: String,
        order: i64,
        seed: String,
    },
    CreateFolder {
        id: String,
        name: String,
        parent: Option<String>,
        order: i64,
    },
    RenameFile {
        id: String,
        kind: EntryKind,
        name: String,
        expected_revision: i64,
    },
    MoveFile {
        id: String,
        kind: EntryKind,
        parent: Option<String>,
        order: i64,
        expected_revision: i64,
    },
    SetFileLanguage {
        id: String,
        language: String,
        expected_revision: i64,
    },
    DeleteFile {
        id: String,
        kind: EntryKind,
        expected_revision: i64,
    },
}

/// The durable tree entry type an action addresses. Serializing this enum
/// prevents malformed callers from publishing arbitrary history/resource kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Folder,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }
}

/// Durable, browseable history metadata for a Code mutation.
pub fn history_context(
    action: &CodeEditorAction,
    file_name: &str,
    project_id: &str,
) -> qos_actions::ActionContext {
    let mut context = qos_actions::ActionContext::for_action("org.qos.Code", "Code", action);
    let (id, kind, label) = match action {
        CodeEditorAction::EditText { file_id, .. } => (file_id.as_str(), "file", "Edit text"),
        CodeEditorAction::CreateFile { id, .. } => (id.as_str(), "file", "Create file"),
        CodeEditorAction::CreateFolder { id, .. } => (id.as_str(), "folder", "Create folder"),
        CodeEditorAction::RenameFile { id, kind, .. } => (id.as_str(), kind.as_str(), "Rename"),
        CodeEditorAction::MoveFile { id, kind, .. } => (id.as_str(), kind.as_str(), "Move"),
        CodeEditorAction::SetFileLanguage { id, .. } => (id.as_str(), "file", "Set file language"),
        CodeEditorAction::DeleteFile { id, kind, .. } => (id.as_str(), kind.as_str(), "Delete"),
    };
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
        id: if kind == "folder" {
            folder_resource_id(id)
        } else {
            file_resource_id(id)
        },
        kind: format!("code_editor.{kind}"),
        label: file_name.to_string(),
    });
    if let Some(extension) = file_name.rsplit_once('.').map(|(_, extension)| extension) {
        context
            .facets
            .push(qos_actions::ActionFacet::new("file-type", extension));
    }
    context.facets.push(qos_actions::ActionFacet::new(
        "project-resource",
        project_resource_id(project_id),
    ));
    context
}

impl Action for CodeEditorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::EditText {
                file_id,
                expected_base_digest,
                start_chars,
                delete_chars,
                insert,
                before_context,
                deleted_text,
                after_context,
            } => apply_text_edit(
                doc,
                file_id,
                expected_base_digest,
                *start_chars,
                *delete_chars,
                insert,
                before_context,
                deleted_text,
                after_context,
            ),
            Self::CreateFile {
                id,
                name,
                parent,
                language,
                order,
                seed,
            } => apply_create_file(doc, id, name, parent.as_deref(), language, *order, seed),
            Self::CreateFolder {
                id,
                name,
                parent,
                order,
            } => apply_create_folder(doc, id, name, parent.as_deref(), *order),
            Self::RenameFile {
                id,
                kind,
                name,
                expected_revision,
            } => apply_rename(doc, id, *kind, name, *expected_revision),
            Self::MoveFile {
                id,
                kind,
                parent,
                order,
                expected_revision,
            } => apply_move(
                doc,
                id,
                *kind,
                parent.as_deref(),
                *order,
                *expected_revision,
            ),
            Self::SetFileLanguage {
                id,
                language,
                expected_revision,
            } => apply_language(doc, id, language, *expected_revision),
            Self::DeleteFile {
                id,
                kind,
                expected_revision,
            } => apply_delete(doc, id, *kind, *expected_revision),
        }
    }

    fn locus(&self) -> ExecutionLocus {
        // Structural actions and a newly materialised buffer are authored
        // locally, then replayed by peers through the same document path.
        ExecutionLocus::ClientLocal
    }
}

pub fn text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Convert two complete text states into the one smallest character splice.
pub fn text_edit_action(file_id: String, base: &str, next: &str) -> CodeEditorAction {
    let mut prefix = 0usize;
    let mut before = base.chars();
    let mut after = next.chars();
    loop {
        match (before.next(), after.next()) {
            (Some(a), Some(b)) if a == b => prefix += 1,
            _ => break,
        }
    }
    let base_tail: Vec<_> = base.chars().skip(prefix).collect();
    let next_tail: Vec<_> = next.chars().skip(prefix).collect();
    let suffix = base_tail
        .iter()
        .rev()
        .zip(next_tail.iter().rev())
        .take_while(|(base_char, next_char)| base_char == next_char)
        .count();
    let deleted_len = base_tail.len().saturating_sub(suffix);
    let inserted_len = next_tail.len().saturating_sub(suffix);
    let deleted: String = base_tail.iter().take(deleted_len).collect();
    let base_chars: Vec<_> = base.chars().collect();
    let end = prefix + deleted.chars().count();
    let before_context: String = base_chars
        .iter()
        .skip(prefix.saturating_sub(32))
        .take(prefix.min(32))
        .collect();
    let after_context: String = base_chars.iter().skip(end).take(32).collect();
    CodeEditorAction::EditText {
        file_id,
        expected_base_digest: text_digest(base),
        start_chars: prefix,
        delete_chars: deleted.chars().count(),
        insert: next_tail.iter().take(inserted_len).collect(),
        before_context,
        deleted_text: deleted,
        after_context,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the validated arguments mirror the compact EditText wire payload field-for-field"
)]
fn apply_text_edit(
    doc: &mut Doc,
    file_id: &str,
    expected: &str,
    start: usize,
    delete: usize,
    insert: &str,
    before_context: &str,
    deleted_text: &str,
    after_context: &str,
) -> Result<(), ActionError> {
    let record = EditorDoc::file(doc, file_id)
        .ok_or_else(|| ActionError::Refused("file does not exist".to_string()))?;
    if record.deleted || record.kind != "file" || insert.len() > 8 * 1024 * 1024 {
        return Err(ActionError::Refused(
            "invalid text edit target or payload".to_string(),
        ));
    }
    let text = EditorDoc::read_buffer_key(doc, file_id)
        .ok_or_else(|| ActionError::Refused("file body is unavailable".to_string()))?;
    let len = text.chars().count();
    let (start, delete) = if text_digest(&text) == expected && start <= len && delete <= len - start
    {
        (start, delete)
    } else {
        rebase_text_edit(&text, before_context, deleted_text, after_context)
            .ok_or_else(|| ActionError::Refused("stale text edit".to_string()))?
    };
    let object = EditorDoc::buffer_text_key(doc, file_id)
        .ok_or_else(|| ActionError::Refused("file body is unavailable".to_string()))?;
    EditorDoc::splice(doc, &object, start, delete, insert);
    Ok(())
}

/// Rebase one edit only when its bounded surrounding context identifies one
/// untouched range. Ambiguous or overlapping same-position edits fail closed.
fn rebase_text_edit(
    text: &str,
    before: &str,
    deleted: &str,
    after: &str,
) -> Option<(usize, usize)> {
    let needle = format!("{before}{deleted}{after}");
    if needle.is_empty() {
        return None;
    }
    let mut matches = text.match_indices(&needle);
    let (byte, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let start_byte = byte + before.len();
    let start = text.get(..start_byte)?.chars().count();
    Some((start, deleted.chars().count()))
}

fn apply_create_file(
    doc: &mut Doc,
    id: &str,
    name: &str,
    parent: Option<&str>,
    language: &str,
    order: i64,
    seed: &str,
) -> Result<(), ActionError> {
    let path = canonical_path(doc, parent, name)?;
    validate_create(doc, id, name, &path, parent, "file", seed)?;
    let entry = CollaborativeFile {
        id: id.to_string(),
        kind: "file".to_string(),
        name: name.to_string(),
        path,
        parent: parent.map(str::to_owned),
        language: language.to_string(),
        order,
        deleted: false,
        name_revision: 0,
        structure_revision: 0,
        language_revision: 0,
        deletion_revision: 0,
    };
    // Duplicate delivery is intentionally an idempotent no-op: the existing
    // UUID owns the durable record and body, so it must never be overwritten.
    let _ = EditorDoc::create_file(doc, &entry, seed);
    Ok(())
}

fn apply_create_folder(
    doc: &mut Doc,
    id: &str,
    name: &str,
    parent: Option<&str>,
    order: i64,
) -> Result<(), ActionError> {
    let path = canonical_path(doc, parent, name)?;
    validate_create(doc, id, name, &path, parent, "folder", "")?;
    let folder = CollaborativeFile {
        id: id.to_string(),
        kind: "folder".to_string(),
        name: name.to_string(),
        path,
        parent: parent.map(str::to_owned),
        language: String::new(),
        order,
        deleted: false,
        name_revision: 0,
        structure_revision: 0,
        language_revision: 0,
        deletion_revision: 0,
    };
    let _ = EditorDoc::create_file_record(doc, &folder);
    Ok(())
}

fn current_entry(doc: &Doc, id: &str, kind: EntryKind) -> Result<CollaborativeFile, ActionError> {
    let current = EditorDoc::file(doc, id)
        .ok_or_else(|| ActionError::Refused("file does not exist".to_string()))?;
    if current.deleted || current.kind != kind.as_str() {
        return Err(ActionError::Refused(
            "stale or invalid file entry".to_string(),
        ));
    }
    Ok(current)
}

fn apply_rename(
    doc: &mut Doc,
    id: &str,
    kind: EntryKind,
    name: &str,
    expected: i64,
) -> Result<(), ActionError> {
    let current = current_entry(doc, id, kind)?;
    if current.name_revision != expected || name.trim().is_empty() {
        return Err(ActionError::Refused("stale or invalid rename".to_string()));
    }
    validate_name(name)?;
    validate_sibling_name(doc, id, current.parent.as_deref(), name)?;
    EditorDoc::update_file(
        doc,
        id,
        FilePatch {
            name: Some(name),
            revision_field: Some("name_revision"),
            ..FilePatch::default()
        },
    )
    .ok_or_else(|| ActionError::Refused("file changed during rename".to_string()))?;
    Ok(())
}

fn apply_move(
    doc: &mut Doc,
    id: &str,
    kind: EntryKind,
    parent: Option<&str>,
    order: i64,
    expected: i64,
) -> Result<(), ActionError> {
    let current = current_entry(doc, id, kind)?;
    if current.structure_revision != expected {
        return Err(ActionError::Refused("stale move".to_string()));
    }
    let path = canonical_path(doc, parent, &current.name)?;
    validate_move(doc, &current, parent, order, &path)?;
    EditorDoc::update_file(
        doc,
        id,
        FilePatch {
            parent: parent.map_or(ParentPatch::Root, ParentPatch::Parent),
            order: Some(order),
            revision_field: Some("structure_revision"),
            ..FilePatch::default()
        },
    )
    .ok_or_else(|| ActionError::Refused("file changed during move".to_string()))?;
    Ok(())
}

fn apply_language(
    doc: &mut Doc,
    id: &str,
    language: &str,
    expected: i64,
) -> Result<(), ActionError> {
    let current = current_entry(doc, id, EntryKind::File)?;
    if current.language_revision != expected || !valid_language(language) {
        return Err(ActionError::Refused(
            "stale or invalid language update".to_string(),
        ));
    }
    EditorDoc::update_file(
        doc,
        id,
        FilePatch {
            language: Some(language),
            revision_field: Some("language_revision"),
            ..FilePatch::default()
        },
    )
    .ok_or_else(|| ActionError::Refused("file changed during language update".to_string()))?;
    Ok(())
}

fn apply_delete(
    doc: &mut Doc,
    id: &str,
    kind: EntryKind,
    expected: i64,
) -> Result<(), ActionError> {
    let current = current_entry(doc, id, kind)?;
    if current.deletion_revision != expected {
        return Err(ActionError::Refused("stale delete".to_string()));
    }
    if kind == EntryKind::Folder
        && EditorDoc::files(doc)
            .iter()
            .any(|entry| !entry.deleted && entry.parent.as_deref() == Some(id))
    {
        return Err(ActionError::Refused(
            "folder must be empty before deletion".to_string(),
        ));
    }
    EditorDoc::update_file(
        doc,
        id,
        FilePatch {
            deleted: Some(true),
            revision_field: Some("deletion_revision"),
            ..FilePatch::default()
        },
    )
    .ok_or_else(|| ActionError::Refused("file changed during delete".to_string()))?;
    Ok(())
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

pub fn create_file_action(
    id: String,
    name: String,
    parent: Option<String>,
    order: i64,
    seed: String,
) -> CodeEditorAction {
    CodeEditorAction::CreateFile {
        id,
        language: crate::model::Language::from_path(&name).label().to_string(),
        name,
        parent,
        order,
        seed,
    }
}

fn validate_create(
    doc: &Doc,
    id: &str,
    name: &str,
    path: &str,
    parent: Option<&str>,
    kind: &str,
    seed: &str,
) -> Result<(), ActionError> {
    if uuid::Uuid::parse_str(id).is_err()
        || seed.len() > 8 * 1024 * 1024
        || !matches!(kind, "file" | "folder")
        || !valid_path(path)
        || path.rsplit('/').next() != Some(name)
    {
        return Err(ActionError::Refused(
            "invalid file identity or metadata".to_string(),
        ));
    }
    validate_name(name)?;
    if let Some(parent) = parent {
        let Some(parent_record) = EditorDoc::file(doc, parent) else {
            return Err(ActionError::Refused("parent does not exist".to_string()));
        };
        if parent_record.deleted || parent_record.kind != "folder" {
            return Err(ActionError::Refused(
                "parent is not an active folder".to_string(),
            ));
        }
    }
    if EditorDoc::files(doc)
        .into_iter()
        .any(|record| !record.deleted && record.path == path && record.id != id)
    {
        return Err(ActionError::Refused("path already exists".to_string()));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ActionError> {
    if name.trim().is_empty()
        || name.len() > 255
        || name.contains(['/', '\\'])
        || matches!(name, "." | "..")
    {
        return Err(ActionError::Refused("invalid resource name".to_string()));
    }
    Ok(())
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.split('/').any(|part| matches!(part, "" | "." | ".."))
}

fn valid_language(language: &str) -> bool {
    matches!(
        language,
        "Plain Text"
            | "Rust"
            | "Python"
            | "JavaScript"
            | "TypeScript"
            | "JSON"
            | "Markdown"
            | "TOML"
            | "HTML"
            | "CSS"
            | "C"
            | "C++"
            | "Go"
            | "Shell"
            | "YAML"
    )
}

fn validate_sibling_name(
    doc: &Doc,
    id: &str,
    parent: Option<&str>,
    name: &str,
) -> Result<(), ActionError> {
    if EditorDoc::files(doc).into_iter().any(|entry| {
        !entry.deleted
            && entry.id != id
            && entry.parent.as_deref() == parent
            && entry.name.eq_ignore_ascii_case(name)
    }) {
        return Err(ActionError::Refused(
            "a sibling already uses that name".to_string(),
        ));
    }
    Ok(())
}

fn validate_move(
    doc: &Doc,
    current: &CollaborativeFile,
    parent: Option<&str>,
    order: i64,
    canonical: &str,
) -> Result<(), ActionError> {
    if !valid_path(canonical) || order < 0 {
        return Err(ActionError::Refused("invalid move metadata".to_string()));
    }
    if let Some(parent) = parent {
        let Some(parent_record) = EditorDoc::file(doc, parent) else {
            return Err(ActionError::Refused("parent does not exist".to_string()));
        };
        if parent_record.deleted || parent_record.kind != "folder" {
            return Err(ActionError::Refused(
                "parent is not an active folder".to_string(),
            ));
        }
        let mut cursor = Some(parent_record);
        while let Some(ancestor) = cursor {
            if ancestor.id == current.id {
                return Err(ActionError::Refused(
                    "a folder cannot contain itself".to_string(),
                ));
            }
            cursor = ancestor
                .parent
                .as_deref()
                .and_then(|id| EditorDoc::file(doc, id));
        }
    }
    if EditorDoc::files(doc)
        .into_iter()
        .any(|entry| !entry.deleted && entry.id != current.id && entry.path == canonical)
    {
        return Err(ActionError::Refused("path already exists".to_string()));
    }
    Ok(())
}

fn canonical_path(doc: &Doc, parent: Option<&str>, name: &str) -> Result<String, ActionError> {
    match parent {
        None => Ok(name.to_string()),
        Some(parent) => EditorDoc::file(doc, parent)
            .filter(|record| !record.deleted && record.kind == "folder")
            .map(|record| format!("{}/{}", record.path, name))
            .ok_or_else(|| ActionError::Refused("parent is not an active folder".to_string())),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One durable project file as the resource tree presents it. `node` is only a
/// local projection hint; `id` is the UUID used by actions, history, search,
/// and peers. The list covers every materialised project file, not merely tabs.
#[derive(Debug, Clone)]
pub struct OpenFileRef {
    pub node: u64,
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub path: String,
    pub order: i64,
    /// Whether this buffer already has a filesystem destination. Scratch
    /// buffers require a picker and therefore cannot honestly export Save as
    /// an immediate effect.
    pub can_save: bool,
}

#[derive(Debug, Clone)]
pub struct FolderRef {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub path: String,
    pub order: i64,
}

/// The presentation context Code hands [`provide_resources`]: the project name,
/// the UUID captured for the next file action (so the palette's "New file"
/// targets the same resource as the on-screen command), and the file resources.
#[derive(Debug, Clone)]
pub struct CodeEditorResourceCtx {
    pub project_name: String,
    /// Document-persisted UUID: root resource identity must not collide across
    /// projects merely because they are both called “Project”.
    pub project_id: String,
    /// The UUID the next created file/buffer will use. Captured at authoring
    /// time so the Ctrl+K "New file" spec and the app's own `New File` command
    /// build the byte-identical action.
    pub next_file_id: String,
    pub next_file_name: String,
    pub next_folder_id: String,
    pub next_folder_name: String,
    pub open_files: Vec<OpenFileRef>,
    pub folders: Vec<FolderRef>,
    /// Declaration locations derived from the collaborative buffer text.
    pub symbols: Vec<SymbolRef>,
    /// The live project-wide result set shown in the bottom panel.
    pub search_results: Vec<SearchResultRef>,
}

/// The resource forest is an immediate UI snapshot, not a second complete
/// project index. Search and `ResourceProvider::resource` retain authority for
/// every omitted row. Keeping this small prevents a large project (or a noisy
/// search result set) from being flattened on every palette interaction.
pub const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;

fn project_resource(ctx: &CodeEditorResourceCtx) -> Resource {
    let label = if ctx.project_name.trim().is_empty() {
        "Project".to_string()
    } else {
        ctx.project_name.clone()
    };
    Resource::new(
        project_resource_id(&ctx.project_id),
        "code_editor.project",
        label,
    )
}

fn entry_rename_action(id: String, kind: EntryKind, revision: i64) -> ActionSpec {
    let (action_id, label, prompt) = match kind {
        EntryKind::File => ("rename", "Rename file", "new file name"),
        EntryKind::Folder => ("rename", "Rename folder", "new folder name"),
    };
    ActionSpec::typed(
        action_id,
        label,
        prompt,
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required("name", ParamType::Text, prompt)],
        move |args| match args.text("name").filter(|name| !name.trim().is_empty()) {
            Some(name) => encode(&CodeEditorAction::RenameFile {
                id: id.clone(),
                kind,
                name: name.to_string(),
                expected_revision: revision,
            })
            .unwrap_or_else(|_| invalid_arguments("valid rename is required")),
            None => invalid_arguments("a name is required"),
        },
    )
}

fn entry_move_action(
    id: String,
    kind: EntryKind,
    current_parent: Option<String>,
    order: i64,
    revision: i64,
) -> ActionSpec {
    let label = match kind {
        EntryKind::File => "Move file",
        EntryKind::Folder => "Move folder",
    };
    ActionSpec::typed(
        "move",
        label,
        "destination folder",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::optional(
            "parent",
            ParamType::Text,
            "destination folder resource UUID (use root for project root)",
        )],
        move |args| {
            let parent = match args.text("parent") {
                Some("root") => None,
                Some(parent) if !parent.is_empty() => Some(parent.to_string()),
                _ => current_parent.clone(),
            };
            encode(&CodeEditorAction::MoveFile {
                id: id.clone(),
                kind,
                parent,
                order,
                expected_revision: revision,
            })
            .unwrap_or_else(|_| invalid_arguments("valid move is required"))
        },
    )
}

fn save_action(file_id: String) -> ActionSpec {
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
                    file_id: file_id.clone(),
                    path: args
                        .text("path")
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned),
                },
            )
            .unwrap_or_else(|_| invalid_arguments("unable to encode save operation"))
        },
    )
    .destructive()
    .execution(ExecutionMode::Background { cancellable: true })
    .returns(ResultSpec::Job)
}

fn replace_text_action(file_id: String, base: String) -> ActionSpec {
    ActionSpec::typed(
        "replace-text",
        "Replace text",
        "complete document text",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "text",
            ParamType::Text,
            "complete document text",
        )],
        move |args| match args.text("text") {
            Some(next) if next.len() <= 8 * 1024 * 1024 => {
                encode(&text_edit_action(file_id.clone(), &base, next))
                    .unwrap_or_else(|_| invalid_arguments("valid replacement text is required"))
            }
            _ => invalid_arguments("replacement text is required and must be at most 8 MiB"),
        },
    )
}

fn file_resource_base(doc: &AutoCommit, f: &OpenFileRef) -> Resource {
    let label = if f.name.trim().is_empty() {
        "untitled".to_string()
    } else {
        f.name.clone()
    };
    let record = EditorDoc::file(doc, &f.id);
    let name_revision = record.as_ref().map_or(0, |file| file.name_revision);
    let structure_revision = record.as_ref().map_or(0, |file| file.structure_revision);
    let language_revision = record.as_ref().map_or(0, |file| file.language_revision);
    let deletion_revision = record.as_ref().map_or(0, |file| file.deletion_revision);
    let mut file = Resource::new(file_resource_id(&f.id), "code_editor.file", label).with_action(
        file_operation(
            "open",
            "Open file",
            OperationKind::View,
            &CodeEditorOperation::Open {
                file_id: f.id.clone(),
            },
        ),
    );
    if let Some(base) = EditorDoc::read_buffer_key(doc, &f.id) {
        file = file.with_action(replace_text_action(f.id.clone(), base));
    }
    if f.can_save {
        file = file.with_action(save_action(f.id.clone()));
    }
    file = file.with_action(entry_rename_action(
        f.id.clone(),
        EntryKind::File,
        name_revision,
    ));
    file = file.with_action(entry_move_action(
        f.id.clone(),
        EntryKind::File,
        record.as_ref().and_then(|file| file.parent.clone()),
        record.as_ref().map_or(0, |file| file.order),
        structure_revision,
    ));
    let language_id = f.id.clone();
    file = file.with_action(ActionSpec::typed(
        "set-language",
        "Set file language",
        "language name",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "language",
            ParamType::Text,
            "language name",
        )],
        move |args| match args
            .text("language")
            .filter(|value| !value.trim().is_empty())
        {
            Some(language) => encode(&CodeEditorAction::SetFileLanguage {
                id: language_id.clone(),
                language: language.to_string(),
                expected_revision: language_revision,
            })
            .unwrap_or_else(|_| invalid_arguments("valid language selection is required")),
            None => invalid_arguments("language is required"),
        },
    ));
    file = file.with_action(
        document_action_spec(
            "delete",
            "Delete file",
            &CodeEditorAction::DeleteFile {
                id: f.id.clone(),
                kind: EntryKind::File,
                expected_revision: deletion_revision,
            },
        )
        .destructive(),
    );
    file
}

fn symbol_resource(symbol: &SymbolRef) -> Resource {
    Resource::new(
        symbol_resource_id(symbol),
        "code_editor.symbol",
        symbol.name.clone(),
    )
    .with_action(file_operation(
        "open",
        "Open symbol",
        OperationKind::View,
        &CodeEditorOperation::RevealLocation {
            file_id: Some(symbol.file_id.clone()),
            path: None,
            line: symbol.line,
            col: symbol.col,
            show_search: false,
        },
    ))
}

fn result_resource(result: &SearchResultRef) -> Resource {
    Resource::new(
        result_resource_id(result),
        "code_editor.search-result",
        result.preview.clone(),
    )
    .with_action(file_operation(
        "reveal",
        "Reveal search result",
        OperationKind::View,
        &CodeEditorOperation::RevealLocation {
            file_id: result.file_id.clone(),
            path: result.disk_path.clone(),
            line: result.line,
            col: result.col,
            show_search: true,
        },
    ))
}

fn folder_resource(ctx: &CodeEditorResourceCtx, doc: &AutoCommit, folder: &FolderRef) -> Resource {
    let record = EditorDoc::file(doc, &folder.id);
    let name_revision = record.as_ref().map_or(0, |entry| entry.name_revision);
    let structure_revision = record.as_ref().map_or(0, |entry| entry.structure_revision);
    let deletion_revision = record.as_ref().map_or(0, |entry| entry.deletion_revision);
    Resource::new(
        folder_resource_id(&folder.id),
        "code_editor.folder",
        folder.name.clone(),
    )
    .with_action(entry_rename_action(
        folder.id.clone(),
        EntryKind::Folder,
        name_revision,
    ))
    .with_action(entry_move_action(
        folder.id.clone(),
        EntryKind::Folder,
        folder.parent.clone(),
        folder.order,
        structure_revision,
    ))
    .with_action(document_action_spec(
        "new-file",
        "New file here",
        &create_file_action(
            ctx.next_file_id.clone(),
            ctx.next_file_name.clone(),
            Some(folder.id.clone()),
            i64::MAX / 2,
            String::new(),
        ),
    ))
    .with_action(document_action_spec(
        "new-folder",
        "New folder here",
        &CodeEditorAction::CreateFolder {
            id: ctx.next_folder_id.clone(),
            name: ctx.next_folder_name.clone(),
            parent: Some(folder.id.clone()),
            order: i64::MAX / 2,
        },
    ))
    .with_action(
        document_action_spec(
            "delete",
            "Delete folder",
            &CodeEditorAction::DeleteFile {
                id: folder.id.clone(),
                kind: EntryKind::Folder,
                expected_revision: deletion_revision,
            },
        )
        .destructive(),
    )
}

fn add_catalogue_actions(project: &mut Resource) {
    for (command, command_label, _) in Command::catalogue() {
        let operation = CodeEditorOperation::Command {
            id: command.id().to_string(),
            argument: None,
        };
        let Ok(kind) = operation_kind(&operation) else {
            continue;
        };
        let spec = if command == Command::GoToLine {
            ActionSpec::prompted_operation(
                format!("command-{}", command.id()),
                command_label,
                "Line number",
                "e.g. 42",
                kind,
                CODE_EDITOR_OPERATION_TYPE,
                move |argument| {
                    EncodedAction::encode_operation(
                        CODE_EDITOR_OPERATION_TYPE,
                        &CodeEditorOperation::Command {
                            id: command.id().to_string(),
                            argument: Some(argument.to_string()),
                        },
                    )
                    .unwrap_or_else(|_| invalid_arguments("unable to encode code editor command"))
                },
            )
        } else {
            file_operation(
                format!("command-{}", command.id()).as_str(),
                command_label,
                kind,
                &operation,
            )
        };
        project.actions.push(spec);
    }
}

fn add_project_actions(mut project: Resource, ctx: &CodeEditorResourceCtx) -> Resource {
    add_catalogue_actions(&mut project);
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
                    .unwrap_or_else(|_| invalid_arguments("unable to encode file operation"))
                },
            )
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        );
    }
    project
        .with_action(document_action_spec(
            "new-file",
            "New file",
            &create_file_action(
                ctx.next_file_id.clone(),
                ctx.next_file_name.clone(),
                None,
                i64::MAX / 2,
                String::new(),
            ),
        ))
        .with_action(document_action_spec(
            "new-folder",
            "New folder",
            &CodeEditorAction::CreateFolder {
                id: ctx.next_folder_id.clone(),
                name: ctx.next_folder_name.clone(),
                parent: None,
                order: i64::MAX / 2,
            },
        ))
}

fn attach_nested_resources(
    parent: &mut Resource,
    children: &mut HashMap<String, Vec<(i64, String)>>,
    resources: &mut HashMap<String, Resource>,
    visited: &mut HashSet<String>,
) {
    let mut rows = children.remove(parent.id.as_str()).unwrap_or_default();
    rows.sort();
    for (_, id) in rows {
        if !visited.insert(id.clone()) {
            continue;
        }
        if let Some(mut child) = resources.remove(&id) {
            attach_nested_resources(&mut child, children, resources, visited);
            parent.children.push(child);
        }
    }
}

fn root_folder_id(ctx: &CodeEditorResourceCtx) -> Option<&str> {
    ctx.folders
        .iter()
        .find(|folder| folder.parent.is_none())
        .map(|folder| folder.id.as_str())
}

/// Select a deterministic, ancestor-closed contextual tree. A folder or file
/// is included only when its real parent chain fits in the snapshot budget;
/// we never move a child to project root merely because its parent was too
/// expensive to publish. The lazy resolver below keeps every omitted address
/// reachable.
#[allow(
    clippy::too_many_lines,
    reason = "selection, ancestor validation, budget accounting, and projection construction must stay adjacent to make the no-false-reparenting invariant auditable"
)]
fn bounded_resource_snapshot(ctx: &CodeEditorResourceCtx) -> CodeEditorResourceCtx {
    let root_folder = root_folder_id(ctx);
    let folders = ctx
        .folders
        .iter()
        .map(|folder| (folder.id.as_str(), folder))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::new();
    for folder in &ctx.folders {
        if folder.parent.is_some() {
            candidates.push((
                folder.path.clone(),
                true,
                folder.order,
                folder.id.clone(),
                None,
            ));
        }
    }
    for file in &ctx.open_files {
        candidates.push((
            file.path.clone(),
            false,
            file.order,
            file.id.clone(),
            Some(file),
        ));
    }
    candidates.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
    });

    let mut selected_folders = HashSet::new();
    let mut selected_files = HashSet::new();
    for (_, is_folder, _, id, file) in candidates {
        let mut chain = Vec::new();
        let mut parent = if is_folder {
            folders
                .get(id.as_str())
                .and_then(|folder| folder.parent.as_deref())
        } else {
            file.and_then(|file| file.parent.as_deref())
        };
        let mut valid = true;
        let mut seen = HashSet::new();
        while let Some(parent_id) = parent {
            if Some(parent_id) == root_folder {
                break;
            }
            if !seen.insert(parent_id) {
                valid = false;
                break;
            }
            let Some(folder) = folders.get(parent_id) else {
                valid = false;
                break;
            };
            chain.push(folder.id.clone());
            parent = folder.parent.as_deref();
        }
        if !valid {
            continue;
        }
        if is_folder {
            chain.push(id.clone());
        }
        chain.reverse();
        let needed = chain
            .iter()
            .filter(|folder| !selected_folders.contains(folder.as_str()))
            .count()
            .saturating_add(usize::from(
                !is_folder && !selected_files.contains(id.as_str()),
            ));
        if selected_folders
            .len()
            .saturating_add(selected_files.len())
            .saturating_add(needed)
            > EAGER_RESOURCE_SNAPSHOT_LIMIT
        {
            continue;
        }
        selected_folders.extend(chain);
        if !is_folder {
            selected_files.insert(id);
        }
    }

    let used = selected_folders.len().saturating_add(selected_files.len());
    let remaining = EAGER_RESOURCE_SNAPSHOT_LIMIT.saturating_sub(used);
    CodeEditorResourceCtx {
        project_name: ctx.project_name.clone(),
        project_id: ctx.project_id.clone(),
        next_file_id: ctx.next_file_id.clone(),
        next_file_name: ctx.next_file_name.clone(),
        next_folder_id: ctx.next_folder_id.clone(),
        next_folder_name: ctx.next_folder_name.clone(),
        open_files: ctx
            .open_files
            .iter()
            .filter(|file| selected_files.contains(file.id.as_str()))
            .cloned()
            .collect(),
        folders: ctx
            .folders
            .iter()
            .filter(|folder| selected_folders.contains(folder.id.as_str()))
            .cloned()
            .collect(),
        symbols: Vec::new(),
        search_results: ctx.search_results.iter().take(remaining).cloned().collect(),
    }
}

fn nest_project_resources(
    project: &mut Resource,
    ctx: &CodeEditorResourceCtx,
    root_folder: Option<&str>,
) {
    let folder_ids: HashSet<_> = ctx
        .folders
        .iter()
        .map(|folder| folder_resource_id(&folder.id))
        .collect();
    let file_ids: HashSet<_> = ctx
        .open_files
        .iter()
        .map(|file| file_resource_id(&file.id))
        .collect();
    let mut resources = HashMap::new();
    let mut ancillary = Vec::new();
    for child in std::mem::take(&mut project.children) {
        if folder_ids.contains(child.id.as_str()) || file_ids.contains(child.id.as_str()) {
            resources.insert(child.id.0.clone(), child);
        } else {
            ancillary.push(child);
        }
    }
    let mut children: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    for file in &ctx.open_files {
        let parent = match file.parent.as_deref() {
            None => None,
            Some(parent) if Some(parent) == root_folder => None,
            Some(parent) => Some(folder_resource_id(parent)),
        };
        children
            .entry(parent.unwrap_or_else(|| project_resource_id(&ctx.project_id)))
            .or_default()
            .push((file.order, file_resource_id(&file.id)));
    }
    for folder in &ctx.folders {
        let parent = if Some(folder.parent.as_deref().unwrap_or_default()) == root_folder {
            project_resource_id(&ctx.project_id)
        } else {
            folder_resource_id(folder.parent.as_deref().unwrap_or_default())
        };
        children
            .entry(parent)
            .or_default()
            .push((folder.order, folder_resource_id(&folder.id)));
    }
    let mut visited = HashSet::new();
    attach_nested_resources(project, &mut children, &mut resources, &mut visited);
    // A bounded snapshot never lies about hierarchy. Malformed/orphaned rows
    // stay out of this contextual tree and remain available only through their
    // exact lazy address, rather than being falsely reparented to project root.
    project.children.extend(ancillary);
}

/// Build the resource tree Code currently has open: the project root (always
/// present, exporting "New file") and one `code_editor.file:<uuid>` per file
/// buffer. The project's "New file" spec builds the same [`CreateFile`] action
/// the on-screen command dispatches — one source of truth, one replay path.
///
/// File resources export structured mutations (rename/delete) as well as their
/// native collaborative text path. They are addressable by a stable UUID for
/// OS-wide enumeration, history and agent navigation.
///
/// [`CreateFile`]: CodeEditorAction::CreateFile
///
pub fn provide_resources(ctx: &CodeEditorResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut project = add_project_actions(project_resource(ctx), ctx);
    let root_folder = root_folder_id(ctx).map(str::to_owned);
    let snapshot = bounded_resource_snapshot(ctx);

    for file in &snapshot.open_files {
        project = project.with_child(file_resource_base(doc, file));
    }
    for folder in &snapshot.folders {
        project = project.with_child(folder_resource(&snapshot, doc, folder));
    }
    for result in &snapshot.search_results {
        project = project.with_child(result_resource(result));
    }

    nest_project_resources(&mut project, &snapshot, root_folder.as_deref());
    vec![project]
}

fn lazy_folder_resource(
    ctx: &CodeEditorResourceCtx,
    doc: &AutoCommit,
    folder: &FolderRef,
) -> Resource {
    let mut resource = folder_resource(ctx, doc, folder);
    let mut children = ctx
        .folders
        .iter()
        .filter(|child| child.parent.as_deref() == Some(folder.id.as_str()))
        .map(|child| (child.path.clone(), child.order, child.id.clone(), true))
        .chain(
            ctx.open_files
                .iter()
                .filter(|child| child.parent.as_deref() == Some(folder.id.as_str()))
                .map(|child| (child.path.clone(), child.order, child.id.clone(), false)),
        )
        .collect::<Vec<_>>();
    children.sort();
    for (_, _, id, is_folder) in children.into_iter().take(EAGER_RESOURCE_SNAPSHOT_LIMIT) {
        if is_folder {
            if let Some(child) = ctx.folders.iter().find(|child| child.id == id) {
                resource = resource.with_child(folder_resource(ctx, doc, child));
            }
        } else if let Some(child) = ctx.open_files.iter().find(|child| child.id == id) {
            resource = resource.with_child(file_resource_base(doc, child));
        }
    }
    resource
}

pub fn resolve_resource(
    ctx: &CodeEditorResourceCtx,
    doc: &AutoCommit,
    id: &str,
) -> Option<Resource> {
    if id == project_resource_id(&ctx.project_id) {
        return Some(add_project_actions(project_resource(ctx), ctx));
    }
    if let Some(file_id) = id.strip_prefix("code_editor.file:") {
        return ctx
            .open_files
            .iter()
            .find(|file| file.id == file_id)
            .map(|file| file_resource_base(doc, file));
    }
    if let Some(folder_id) = id.strip_prefix("code_editor.folder:") {
        return ctx
            .folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .map(|folder| lazy_folder_resource(ctx, doc, folder));
    }
    if let Some(symbol) = ctx
        .symbols
        .iter()
        .find(|symbol| symbol_resource_id(symbol) == id)
    {
        return ctx
            .open_files
            .iter()
            .any(|file| file.id == symbol.file_id)
            .then(|| symbol_resource(symbol));
    }
    ctx.search_results
        .iter()
        .find(|result| result_resource_id(result) == id)
        .map(result_resource)
}

impl ResourceProvider for crate::app::CodeApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.code_resource_ctx(), doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        resolve_resource(&self.code_resource_ctx(), doc, id)
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
            project_id: "00000000-0000-4000-8000-000000000001".into(),
            next_file_id: "new-file".into(),
            next_file_name: "untitled.txt".into(),
            next_folder_id: "new-folder".into(),
            next_folder_name: "untitled-folder".into(),
            open_files: vec![OpenFileRef {
                node: 1,
                id: "file-1".into(),
                name: "x.rs".into(),
                parent: None,
                path: "x.rs".into(),
                order: 0,
                can_save: true,
            }],
            folders: Vec::new(),
            symbols: Vec::new(),
            search_results: Vec::new(),
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

    fn test_file_action(id: u64, seed: &str) -> CodeEditorAction {
        create_file_action(
            test_file_id(id),
            format!("file-{id}.rs"),
            None,
            id as i64,
            seed.to_string(),
        )
    }

    fn test_file_id(id: u64) -> String {
        format!("00000000-0000-4000-8000-{id:012x}")
    }

    /// Creating a buffer through the action materialises its seeded text in the
    /// document.
    #[test]
    fn create_file_action_writes_the_seeded_text() {
        let mut doc = AutoCommit::new();
        assert!(
            EditorDoc::buffer_text_key(&doc, &test_file_id(7)).is_none(),
            "no buffer yet"
        );
        dispatch(&mut doc, &test_file_action(7, "fn main() {}\n"));
        assert_eq!(
            EditorDoc::read_buffer_key(&doc, &test_file_id(7)).as_deref(),
            Some("fn main() {}\n")
        );
    }

    /// The action is idempotent: a second create with a different seed does not
    /// overwrite the existing buffer (so a duplicate/out-of-order replay is a
    /// no-op, not a wedge).
    #[test]
    fn create_file_action_is_idempotent() {
        let mut doc = AutoCommit::new();
        dispatch(&mut doc, &test_file_action(1, "first"));
        dispatch(&mut doc, &test_file_action(1, "second"));
        assert_eq!(
            EditorDoc::read_buffer_key(&doc, &test_file_id(1)).as_deref(),
            Some("first")
        );
    }

    #[test]
    fn edit_text_is_unicode_safe_and_carries_only_changed_region() {
        let action = text_edit_action("file".into(), "αβγδ", "αβXδ");
        let CodeEditorAction::EditText {
            start_chars,
            delete_chars,
            insert,
            deleted_text,
            before_context,
            after_context,
            ..
        } = action
        else {
            unreachable!("text_edit_action always returns EditText");
        };
        assert_eq!((start_chars, delete_chars), (2, 1));
        assert_eq!(insert, "X");
        assert_eq!(deleted_text, "γ");
        assert_eq!(before_context, "αβ");
        assert_eq!(after_context, "δ");
    }

    #[test]
    fn large_buffer_edit_payload_is_bounded_to_the_changed_region() {
        let base = format!("{}needle{}", "a".repeat(512 * 1024), "z".repeat(512 * 1024));
        let next = base.replacen("needle", "N", 1);
        let CodeEditorAction::EditText {
            insert,
            deleted_text,
            before_context,
            after_context,
            ..
        } = text_edit_action("file".into(), &base, &next)
        else {
            unreachable!("text_edit_action always returns EditText");
        };
        assert_eq!(
            (insert, deleted_text),
            ("N".to_string(), "needle".to_string())
        );
        assert!(before_context.chars().count() <= 32);
        assert!(after_context.chars().count() <= 32);
    }

    #[test]
    fn disjoint_and_same_position_text_edits_converge_on_replay() {
        let id = test_file_id(71);
        let mut base = AutoCommit::new();
        dispatch(
            &mut base,
            &create_file_action(id.clone(), "f.rs".into(), None, 0, "abcdefgh".into()),
        );
        let bytes = base.save();
        let mut first = AutoCommit::load(&bytes).unwrap();
        let mut second = AutoCommit::load(&bytes).unwrap();
        let left = text_edit_action(id.clone(), "abcdefgh", "aXbcdefgh");
        let right = text_edit_action(id.clone(), "abcdefgh", "abcdefgYh");
        let left_env = qos_actions::apply_local(&mut first, &left, encode(&left).unwrap()).unwrap();
        let right_env =
            qos_actions::apply_local(&mut second, &right, encode(&right).unwrap()).unwrap();
        qos_actions::replay(&mut first, &right_env, &registry()).unwrap();
        qos_actions::replay(&mut second, &left_env, &registry()).unwrap();
        assert_eq!(
            EditorDoc::read_buffer_key(&first, &id),
            EditorDoc::read_buffer_key(&second, &id)
        );

        let mut one = AutoCommit::load(&bytes).unwrap();
        let mut two = AutoCommit::load(&bytes).unwrap();
        let a = text_edit_action(id.clone(), "abcdefgh", "abcdXefgh");
        let b = text_edit_action(id.clone(), "abcdefgh", "abcdYefgh");
        let a_env = qos_actions::apply_local(&mut one, &a, encode(&a).unwrap()).unwrap();
        let b_env = qos_actions::apply_local(&mut two, &b, encode(&b).unwrap()).unwrap();
        qos_actions::replay(&mut one, &b_env, &registry()).unwrap();
        qos_actions::replay(&mut two, &a_env, &registry()).unwrap();
        assert_eq!(
            EditorDoc::read_buffer_key(&one, &id),
            EditorDoc::read_buffer_key(&two, &id)
        );
    }

    #[test]
    fn text_edit_refuses_stale_or_deleted_target() {
        let id = test_file_id(72);
        let mut doc = AutoCommit::new();
        dispatch(
            &mut doc,
            &create_file_action(id.clone(), "f.rs".into(), None, 0, "old".into()),
        );
        let stale = text_edit_action(id.clone(), "old", "new");
        let replacement = text_edit_action(id.clone(), "old", "other");
        dispatch(&mut doc, &replacement);
        assert!(stale.apply(&mut doc).is_err());
        let delete = CodeEditorAction::DeleteFile {
            id: id.clone(),
            kind: EntryKind::File,
            expected_revision: 0,
        };
        dispatch(&mut doc, &delete);
        assert!(
            text_edit_action(id, "other", "again")
                .apply(&mut doc)
                .is_err()
        );
    }

    /// Ctrl+K on Code yields the project root exporting "New file" and a
    /// `code_editor.file:<node>` child per open buffer. Drives the real
    /// resource-builder path and the real palette.
    #[test]
    fn ctrl_k_publishes_the_project_and_file_tree() {
        let ctx = CodeEditorResourceCtx {
            project_name: "demo".into(),
            project_id: "00000000-0000-4000-8000-000000000002".into(),
            next_file_id: "new-file".into(),
            next_file_name: "untitled.txt".into(),
            next_folder_id: "new-folder".into(),
            next_folder_name: "untitled-folder".into(),
            open_files: vec![
                OpenFileRef {
                    node: 3,
                    id: "file-3".into(),
                    name: "main.rs".into(),
                    parent: None,
                    path: "main.rs".into(),
                    order: 0,
                    can_save: true,
                },
                OpenFileRef {
                    node: 5,
                    id: "file-5".into(),
                    name: "lib.rs".into(),
                    parent: None,
                    path: "lib.rs".into(),
                    order: 1,
                    can_save: true,
                },
            ],
            folders: Vec::new(),
            symbols: Vec::new(),
            search_results: Vec::new(),
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
        assert_eq!(project.children[0].id.as_str(), "code_editor.file:file-3");
        assert_eq!(project.children[0].label, "main.rs");
        let file_actions = &project.children[0].actions;
        assert_eq!(
            file_actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec!["open", "save", "rename", "move", "set-language", "delete"]
        );
        assert_eq!(file_actions[0].kind, OperationKind::View);
        assert_eq!(file_actions[1].kind, OperationKind::Effect);
        assert_eq!(
            immediate_encode(&file_actions[0]).type_id,
            CODE_EDITOR_OPERATION_TYPE
        );
        assert_eq!(file_actions[1].params()[0].name, "path");

        // The one palette surfaces the tree's actions as rows; a query reaches
        // actions beyond the empty-query display cap as well.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        palette.set_query("New file");
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
        assert!(EditorDoc::buffer_text_key(&state.doc, &test_file_id(9)).is_none());

        // Create a buffer via the action + host undo recording.
        let encoded = encode(&test_file_action(9, "hello")).unwrap();
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            EditorDoc::read_buffer_key(&state.doc, &test_file_id(9)).as_deref(),
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
            EditorDoc::buffer_text_key(&state.doc, &test_file_id(9)).is_none(),
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
        EditorDoc::ensure_buffer_key(&mut a, "base-file", "base\n");
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = test_file_action(2, "fn main() {}\n");
        let encoded = encode(&action).unwrap();
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            EditorDoc::read_buffer_key(&b, &test_file_id(2)).as_deref(),
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
            project_id: "00000000-0000-4000-8000-000000000003".into(),
            next_file_id: "new-file".into(),
            next_file_name: "untitled.txt".into(),
            next_folder_id: "new-folder".into(),
            next_folder_name: "untitled-folder".into(),
            open_files: Vec::new(),
            folders: Vec::new(),
            symbols: Vec::new(),
            search_results: Vec::new(),
        };
        let doc = AutoCommit::new();
        let resources = provide_resources(&ctx, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-file")
            .unwrap();

        let from_spec = immediate_encode(spec);
        let from_builder = encode(&create_file_action(
            "new-file".into(),
            "untitled.txt".into(),
            None,
            i64::MAX / 2,
            String::new(),
        ))
        .unwrap();
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "new-file is one source of truth"
        );
    }

    #[test]
    fn concurrent_disjoint_actions_replay_on_two_replicas_without_manual_retry() {
        let create = test_file_action(88, "fn main() {}\n");
        let mut base = AutoCommit::new();
        dispatch(&mut base, &create);
        let bytes = base.save();
        let mut first = AutoCommit::load(&bytes).unwrap();
        let mut second = AutoCommit::load(&bytes).unwrap();
        let rename = CodeEditorAction::RenameFile {
            id: test_file_id(88),
            kind: EntryKind::File,
            name: "renamed.rs".into(),
            expected_revision: 0,
        };
        let language = CodeEditorAction::SetFileLanguage {
            id: test_file_id(88),
            language: "Rust".into(),
            expected_revision: 0,
        };
        let rename_envelope =
            qos_actions::apply_local(&mut first, &rename, encode(&rename).unwrap()).unwrap();
        let language_envelope =
            qos_actions::apply_local(&mut second, &language, encode(&language).unwrap()).unwrap();
        qos_actions::replay(&mut first, &language_envelope, &registry()).unwrap();
        qos_actions::replay(&mut second, &rename_envelope, &registry()).unwrap();
        let first_record = EditorDoc::file(&first, &test_file_id(88)).unwrap();
        let second_record = EditorDoc::file(&second, &test_file_id(88)).unwrap();
        assert_eq!(first_record.name, "renamed.rs");
        assert_eq!(first_record.language, "Rust");
        assert_eq!(first_record, second_record);
    }

    #[test]
    fn a_stale_same_field_action_is_refused() {
        let mut doc = AutoCommit::new();
        dispatch(&mut doc, &test_file_action(89, ""));
        let rename = CodeEditorAction::RenameFile {
            id: test_file_id(89),
            kind: EntryKind::File,
            name: "first.rs".into(),
            expected_revision: 0,
        };
        dispatch(&mut doc, &rename);
        let stale = CodeEditorAction::RenameFile {
            id: test_file_id(89),
            kind: EntryKind::File,
            name: "stale.rs".into(),
            expected_revision: 0,
        };
        assert!(stale.apply(&mut doc).is_err());
    }

    #[test]
    fn concurrent_rename_and_move_derive_the_same_path_on_cross_replay() {
        let file = test_file_id(91);
        let folder = test_file_id(92);
        let mut base = AutoCommit::new();
        dispatch(
            &mut base,
            &CodeEditorAction::CreateFolder {
                id: folder.clone(),
                name: "dest".into(),
                parent: None,
                order: 0,
            },
        );
        dispatch(
            &mut base,
            &create_file_action(file.clone(), "main.rs".into(), None, 1, String::new()),
        );
        let bytes = base.save();
        let mut first = AutoCommit::load(&bytes).unwrap();
        let mut second = AutoCommit::load(&bytes).unwrap();
        let rename = CodeEditorAction::RenameFile {
            id: file.clone(),
            kind: EntryKind::File,
            name: "renamed.rs".into(),
            expected_revision: 0,
        };
        let move_file = CodeEditorAction::MoveFile {
            id: file.clone(),
            kind: EntryKind::File,
            parent: Some(folder),
            order: 0,
            expected_revision: 0,
        };
        let rename_envelope =
            qos_actions::apply_local(&mut first, &rename, encode(&rename).unwrap()).unwrap();
        let move_envelope =
            qos_actions::apply_local(&mut second, &move_file, encode(&move_file).unwrap()).unwrap();
        qos_actions::replay(&mut first, &move_envelope, &registry()).unwrap();
        qos_actions::replay(&mut second, &rename_envelope, &registry()).unwrap();
        let first_record = EditorDoc::file(&first, &file).unwrap();
        let second_record = EditorDoc::file(&second, &file).unwrap();
        assert_eq!(first_record.path, "dest/renamed.rs");
        assert_eq!(first_record, second_record);
    }

    #[test]
    fn folder_resources_are_nested_and_never_leak_local_paths() {
        let root = test_file_id(70);
        let src = test_file_id(71);
        let main = test_file_id(72);
        let ctx = CodeEditorResourceCtx {
            project_name: "demo".into(),
            project_id: "00000000-0000-4000-8000-000000000004".into(),
            next_file_id: test_file_id(73),
            next_file_name: "untitled.txt".into(),
            next_folder_id: test_file_id(74),
            next_folder_name: "untitled-folder".into(),
            open_files: vec![OpenFileRef {
                node: 99,
                id: main.clone(),
                name: "main.rs".into(),
                parent: Some(src.clone()),
                path: "demo/src/main.rs".into(),
                order: 3,
                can_save: false,
            }],
            folders: vec![
                FolderRef {
                    id: root.clone(),
                    name: "demo".into(),
                    parent: None,
                    path: "demo".into(),
                    order: 0,
                },
                FolderRef {
                    id: src.clone(),
                    name: "src".into(),
                    parent: Some(root),
                    path: "demo/src".into(),
                    order: 1,
                },
            ],
            symbols: Vec::new(),
            search_results: Vec::new(),
        };
        let mut doc = AutoCommit::new();
        for record in [
            CollaborativeFile {
                id: src.clone(),
                kind: "folder".into(),
                name: "src".into(),
                path: "demo/src".into(),
                parent: None,
                language: String::new(),
                order: 1,
                deleted: false,
                name_revision: 0,
                structure_revision: 0,
                language_revision: 0,
                deletion_revision: 0,
            },
            CollaborativeFile {
                id: main.clone(),
                kind: "file".into(),
                name: "main.rs".into(),
                path: "demo/src/main.rs".into(),
                parent: Some(src.clone()),
                language: "Rust".into(),
                order: 3,
                deleted: false,
                name_revision: 0,
                structure_revision: 0,
                language_revision: 0,
                deletion_revision: 0,
            },
        ] {
            EditorDoc::create_file_record(&mut doc, &record);
        }
        let project = provide_resources(&ctx, &doc).pop().unwrap();
        let src_resource = &project.children[0];
        assert_eq!(src_resource.id.as_str(), folder_resource_id(&src));
        assert_eq!(
            src_resource.children[0].id.as_str(),
            file_resource_id(&main)
        );
        assert!(
            src_resource
                .actions
                .iter()
                .any(|action| action.id == "new-file")
        );
        assert!(
            src_resource
                .actions
                .iter()
                .any(|action| action.id == "move")
        );
        for flat in qos_actions::flatten(&[project]) {
            assert!(!flat.resource_id.as_str().contains("/tmp/"));
            assert!(!flat.resource_id.as_str().contains("disk:"));
        }
    }

    #[test]
    fn bounded_snapshot_preserves_real_ancestry_and_lazy_folder_children() {
        fn count(resource: &Resource) -> usize {
            1usize.saturating_add(resource.children.iter().map(count).sum::<usize>())
        }

        let root = "root".to_string();
        let mut folders = vec![FolderRef {
            id: root.clone(),
            name: "workspace".into(),
            parent: None,
            path: "workspace".into(),
            order: 0,
        }];
        let mut parent = root;
        for index in 0..(EAGER_RESOURCE_SNAPSHOT_LIMIT + 8) {
            let id = format!("folder-{index}");
            folders.push(FolderRef {
                id: id.clone(),
                name: format!("f{index}"),
                parent: Some(parent.clone()),
                path: format!(
                    "workspace/{}",
                    (0..=index)
                        .map(|n| format!("f{n}"))
                        .collect::<Vec<_>>()
                        .join("/")
                ),
                order: index as i64,
            });
            parent = id;
        }
        let mut open_files = Vec::new();
        for index in 0..(EAGER_RESOURCE_SNAPSHOT_LIMIT + 4) {
            open_files.push(OpenFileRef {
                node: index as u64,
                id: format!("deep-file-{index}"),
                name: format!("child-{index}.rs"),
                parent: Some(parent.clone()),
                path: format!("workspace/deep/child-{index}.rs"),
                order: index as i64,
                can_save: false,
            });
        }
        let ctx = CodeEditorResourceCtx {
            project_name: "workspace".into(),
            project_id: "project".into(),
            next_file_id: "new-file".into(),
            next_file_name: "untitled.txt".into(),
            next_folder_id: "new-folder".into(),
            next_folder_name: "untitled-folder".into(),
            open_files,
            folders,
            symbols: Vec::new(),
            search_results: Vec::new(),
        };
        let doc = AutoCommit::new();
        let project = provide_resources(&ctx, &doc)
            .pop()
            .expect("project resource");

        assert!(
            count(&project) <= EAGER_RESOURCE_SNAPSHOT_LIMIT.saturating_add(1),
            "project plus its bounded contextual descendants"
        );
        assert_eq!(
            project.children.len(),
            1,
            "only the real first folder is at root"
        );
        assert_eq!(
            project.children[0].id.as_str(),
            "code_editor.folder:folder-0"
        );
        assert!(
            !project
                .children
                .iter()
                .any(|child| child.id.as_str().starts_with("code_editor.file:")),
            "a deep file with omitted ancestors must not be falsely reparented"
        );

        let deep_folder = folder_resource_id(&parent);
        let lazy = resolve_resource(&ctx, &doc, &deep_folder)
            .expect("deep folder remains lazy-addressable");
        assert_eq!(lazy.children.len(), EAGER_RESOURCE_SNAPSHOT_LIMIT);
        assert!(
            lazy.children
                .iter()
                .all(|child| child.id.as_str().starts_with("code_editor.file:"))
        );
        assert!(lazy.children.iter().all(|child| {
            ctx.open_files.iter().any(|file| {
                child.id.as_str() == file_resource_id(&file.id)
                    && file.parent.as_deref() == Some(parent.as_str())
            })
        }));
    }
}
