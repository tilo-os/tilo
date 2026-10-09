//! Code's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! ## What is actually in the shared document
//!
//! Code's collaborative state has two layers: durable file records in
//! `root["files"]`, keyed by UUID, and each file's **buffer text**, stored as an
//! Automerge `Text` under `root["buffers"]` with the same key (see `crate::doc`). Because that text is
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
//! scroll, the sidebar, the bottom-panel tab. Those local
//! operations are still exported on file resources: opening a file is a view
//! operation and saving its collaborative buffer to disk is an effect. The
//! language server, linter, and embedded terminal remain app-owned services.

use automerge::ReadDoc;
use automerge::transaction::Transactable;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ParamType, Resource,
};

use crate::doc::{CollaborativeFile, EditorDoc, FilePatch, ParentPatch};

/// Registry `type_id` for every Code action.
pub const CODE_EDITOR_ACTION_TYPE: &str = "code_editor.action";

/// Build a deliberately-invalid action envelope for resource input or
/// serialization failures. Resource closures cannot return `Result`; this
/// keeps failures visible to the shared action host rather than panicking.
pub(crate) fn invalid_arguments(message: &str) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
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

pub fn file_resource_id(file: impl std::fmt::Display) -> String {
    format!("code_editor.file:{file}")
}

pub fn folder_resource_id(folder: impl std::fmt::Display) -> String {
    format!("code_editor.folder:{folder}")
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
    hex::encode(path)
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
    ActionAddress::new(symbol_resource_id(symbol), "reveal")
}

pub fn reveal_result_address(result: &SearchResultRef) -> ActionAddress {
    ActionAddress::new(result_resource_id(result), "reveal")
}

/// One character-addressed edit against an exact displayed base. The payload
/// is proportional to the changed region, never the full file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextEdit {
    pub file_id: String,
    pub expected_base_digest: String,
    pub start_chars: usize,
    pub delete_chars: usize,
    pub insert: String,
    /// Short immutable context makes a stale action safely rebaseable
    /// when a peer changed a disjoint range. It is deliberately bounded,
    /// so an action remains proportional to its changed region.
    #[serde(default)]
    pub before_context: String,
    #[serde(default)]
    pub deleted_text: String,
    #[serde(default)]
    pub after_context: String,
}

/// One of the embedded terminal session's actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalSessionAction(
    pub <terminal::resource::Terminal as ui::tilo_resource::Resource>::Action,
);

impl PartialEq for TerminalSessionAction {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(&self.0).ok() == serde_json::to_value(&other.0).ok()
    }
}

impl CodeEditorAction {
    /// The project's action carrying the terminal session's `action`.
    pub fn terminal(
        action: <terminal::resource::Terminal as ui::tilo_resource::Resource>::Action,
    ) -> Self {
        Self::Terminal(TerminalSessionAction(action))
    }
}

/// Every collaborative operation Code exports on the shared document.
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CodeEditorAction {
    EditText(TextEdit),
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
    /// Replace a file's whole text with `text`, as the smallest splice
    /// against the text it has when the edit applies.
    ReplaceText {
        id: String,
        text: String,
    },
    /// A change to the embedded terminal's session, which the project keeps
    /// under its terminal panel.
    Terminal(TerminalSessionAction),
    /// Record a file or folder of the project's folder on disk, the first
    /// time the editor reads it. A file's text is read when it is opened
    /// ([`Self::EnsureBuffer`]). An entry that is already recorded, or whose
    /// path a peer recorded under another id, is left as it is.
    ImportEntry {
        id: String,
        kind: EntryKind,
        name: String,
        parent: Option<String>,
        language: String,
        order: i64,
    },
    /// Create a file buffer and seed it once. An existing buffer is left as it
    /// is, so a peer's unsaved text is not replaced by a disk read.
    EnsureBuffer {
        key: String,
        seed: String,
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

impl Action for CodeEditorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::EditText(edit) => edit.apply(doc),
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
            Self::Terminal(action) => {
                let panel = match doc.get(automerge::ROOT, "terminal_panel")? {
                    Some((automerge::Value::Object(automerge::ObjType::Map), panel)) => panel,
                    _ => {
                        doc.put_object(automerge::ROOT, "terminal_panel", automerge::ObjType::Map)?
                    }
                };
                terminal::resource::Terminal::apply_under(&action.0, doc, &panel)
            }
            Self::ReplaceText { id, text } => apply_replace_text(doc, id, text),
            Self::ImportEntry {
                id,
                kind,
                name,
                parent,
                language,
                order,
            } => apply_import(doc, id, *kind, name, parent.as_deref(), language, *order),
            Self::EnsureBuffer { key, seed } => {
                EditorDoc::ensure_buffer_key(doc, key, seed);
                Ok(())
            }
        }
    }

    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            Self::Terminal(action) => tilo_actions::Action::change_kind(&action.0),
            // What is on disk is content from outside: it syncs, but nobody
            // takes it back with an undo.
            Self::ImportEntry { .. } | Self::EnsureBuffer { .. } => {
                tilo_actions::ChangeKind::External
            }
            _ => tilo_actions::ChangeKind::Content,
        }
    }

    fn locus(&self) -> ExecutionLocus {
        // Structural actions and a newly materialised buffer are authored
        // locally, then replayed by peers through the same document path.
        ExecutionLocus::ClientLocal
    }
}

pub fn text_digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
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
    CodeEditorAction::EditText(TextEdit {
        file_id,
        expected_base_digest: text_digest(base),
        start_chars: prefix,
        delete_chars: deleted.chars().count(),
        insert: next_tail.iter().take(inserted_len).collect(),
        before_context,
        deleted_text: deleted,
        after_context,
    })
}

impl TextEdit {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let Self {
            file_id,
            expected_base_digest: expected,
            start_chars: start,
            delete_chars: delete,
            insert,
            before_context,
            deleted_text,
            after_context,
        } = self;
        let (start, delete) = (*start, *delete);
        let record = EditorDoc::file(doc, file_id)
            .ok_or_else(|| ActionError::Refused("file does not exist".to_string()))?;
        if record.deleted
            || record.kind != "file"
            || insert.len() > crate::disk::MAX_TEXT_BYTES as usize
        {
            return Err(ActionError::Refused(
                "invalid text edit target or payload".to_string(),
            ));
        }
        let text = EditorDoc::read_buffer_key(doc, file_id)
            .ok_or_else(|| ActionError::Refused("file body is unavailable".to_string()))?;
        let len = text.chars().count();
        let (start, delete) =
            if text_digest(&text) == *expected && start <= len && delete <= len - start {
                (start, delete)
            } else {
                rebase_text_edit(&text, before_context, deleted_text, after_context)
                    .ok_or_else(|| ActionError::Refused("stale text edit".to_string()))?
            };
        let start_byte = byte_index(&text, start).ok_or_else(|| {
            ActionError::Refused("text edit position is outside the buffer".to_string())
        })?;
        let end_byte = byte_index(
            &text,
            start.checked_add(delete).ok_or_else(|| {
                ActionError::Refused("text edit range overflows the buffer".to_string())
            })?,
        )
        .ok_or_else(|| ActionError::Refused("text edit range is outside the buffer".to_string()))?;
        let final_bytes = text
            .len()
            .checked_sub(end_byte.saturating_sub(start_byte))
            .and_then(|remaining| remaining.checked_add(insert.len()))
            .ok_or_else(|| ActionError::Refused("text edit size overflows".to_string()))?;
        if final_bytes > crate::disk::MAX_TEXT_BYTES as usize {
            return Err(ActionError::Refused(format!(
                "text edit would exceed the {} byte buffer limit",
                crate::disk::MAX_TEXT_BYTES
            )));
        }
        let object = EditorDoc::buffer_text_key(doc, file_id)
            .ok_or_else(|| ActionError::Refused("file body is unavailable".to_string()))?;
        EditorDoc::splice(doc, &object, start, delete, insert);
        Ok(())
    }
}

/// Convert a character-addressed Automerge position to a byte boundary without
/// slicing through UTF-8. The document stores text in character coordinates,
/// while the resource budget is necessarily measured in bytes.
fn byte_index(text: &str, character: usize) -> Option<usize> {
    if character == text.chars().count() {
        Some(text.len())
    } else {
        text.char_indices().nth(character).map(|(byte, _)| byte)
    }
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

fn apply_replace_text(doc: &mut Doc, id: &str, text: &str) -> Result<(), ActionError> {
    if text.len() > crate::disk::MAX_TEXT_BYTES as usize {
        return Err(ActionError::Refused(format!(
            "replacement text must be at most {} bytes",
            crate::disk::MAX_TEXT_BYTES
        )));
    }
    current_entry(doc, id, EntryKind::File)?;
    let base = EditorDoc::read_buffer_key(doc, id)
        .ok_or_else(|| ActionError::Refused("file has no text yet".to_string()))?;
    text_edit_action(id.to_string(), &base, text).apply(doc)
}

fn apply_import(
    doc: &mut Doc,
    id: &str,
    kind: EntryKind,
    name: &str,
    parent: Option<&str>,
    language: &str,
    order: i64,
) -> Result<(), ActionError> {
    if EditorDoc::file(doc, id).is_some() {
        return Ok(());
    }
    let Ok(path) = canonical_path(doc, parent, name) else {
        // The folder it is in was removed from the project.
        return Ok(());
    };
    let taken = EditorDoc::files(doc)
        .into_iter()
        .any(|record| !record.deleted && record.path == path);
    if taken {
        return Ok(());
    }
    let seed = "";
    validate_create(doc, id, name, &path, parent, kind.as_str(), seed)?;
    let entry = CollaborativeFile {
        id: id.to_string(),
        kind: kind.as_str().to_string(),
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
    EditorDoc::create_file_record(doc, &entry);
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
        || seed.len() > crate::disk::MAX_TEXT_BYTES as usize
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

/// How many live rows of one kind (files, symbols, search hits) the published
/// tree carries; every omitted row still resolves by id. Keeping this small
/// prevents a large project (or a noisy search result set) from being
/// flattened on every palette interaction.
pub const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;

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

fn replace_text_action(file_id: String) -> ActionSpec {
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
            Some(text) => encode(&CodeEditorAction::ReplaceText {
                id: file_id.clone(),
                text: text.to_string(),
            })
            .unwrap_or_else(|_| invalid_arguments("valid replacement text is required")),
            None => invalid_arguments("replacement text is required"),
        },
    )
}

/// Document mutations for one collaborative file — rename / move / language /
/// delete / replace-text. Opening and saving it are the editor's effects,
/// which its live rows add under the same id.
pub(crate) fn publish_file_document(file: &CollaborativeFile) -> Resource {
    let label = if file.name.trim().is_empty() {
        "untitled".to_string()
    } else {
        file.name.clone()
    };
    let mut resource = Resource::new(file_resource_id(&file.id), "code_editor.file", label)
        .with_action(replace_text_action(file.id.clone()))
        .with_action(crate::text_operations::edit_action(file.id.clone()));
    resource = resource.with_action(entry_rename_action(
        file.id.clone(),
        EntryKind::File,
        file.name_revision,
    ));
    resource = resource.with_action(entry_move_action(
        file.id.clone(),
        EntryKind::File,
        file.parent.clone(),
        file.order,
        file.structure_revision,
    ));
    let language_id = file.id.clone();
    let language_revision = file.language_revision;
    resource = resource.with_action(ActionSpec::typed(
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
    resource = resource.with_action(
        document_action_spec(
            "delete",
            "Delete file",
            &CodeEditorAction::DeleteFile {
                id: file.id.clone(),
                kind: EntryKind::File,
                expected_revision: file.deletion_revision,
            },
        )
        .destructive(),
    );
    resource
}

/// Document mutations for one collaborative folder.
pub(crate) fn publish_folder_document(
    folder: &CollaborativeFile,
    files: &[CollaborativeFile],
    next_file: Option<(String, String)>,
    next_folder: Option<(String, String)>,
) -> Resource {
    let has_children = files
        .iter()
        .any(|entry| !entry.deleted && entry.parent.as_deref() == Some(folder.id.as_str()));
    let delete = document_action_spec(
        "delete",
        "Delete folder",
        &CodeEditorAction::DeleteFile {
            id: folder.id.clone(),
            kind: EntryKind::Folder,
            expected_revision: folder.deletion_revision,
        },
    )
    .destructive();
    let delete = if has_children {
        delete.unavailable("folder_not_empty", "Empty the folder before deleting it")
    } else {
        delete
    };
    let (file_id, file_name) =
        next_file.unwrap_or_else(|| (uuid::Uuid::new_v4().to_string(), "untitled".into()));
    let (folder_id, folder_name) =
        next_folder.unwrap_or_else(|| (uuid::Uuid::new_v4().to_string(), "untitled".into()));
    Resource::new(
        folder_resource_id(&folder.id),
        "code_editor.folder",
        folder.name.clone(),
    )
    .with_action(entry_rename_action(
        folder.id.clone(),
        EntryKind::Folder,
        folder.name_revision,
    ))
    .with_action(entry_move_action(
        folder.id.clone(),
        EntryKind::Folder,
        folder.parent.clone(),
        folder.order,
        folder.structure_revision,
    ))
    .with_action(document_action_spec(
        "new-file",
        "New file here",
        &create_file_action(
            file_id,
            file_name,
            Some(folder.id.clone()),
            i64::MAX / 2,
            String::new(),
        ),
    ))
    .with_action(document_action_spec(
        "new-folder",
        "New folder here",
        &CodeEditorAction::CreateFolder {
            id: folder_id,
            name: folder_name,
            parent: Some(folder.id.clone()),
            order: i64::MAX / 2,
        },
    ))
    .with_action(delete)
}

/// File and folder Document children for [`tilo_resource::publish`].
pub(crate) fn publish_document_children(files: &[CollaborativeFile]) -> Vec<Resource> {
    let mut children = Vec::new();
    for file in files.iter().filter(|file| !file.deleted) {
        match file.kind.as_str() {
            "file" => {
                children.push(publish_file_document(file));
            }
            "folder" => children.push(publish_folder_document(file, files, None, None)),
            _ => {}
        }
    }
    children
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use tilo_actions::LocalTransport;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    #[test]
    fn importing_a_disk_entry_is_outside_content_and_never_overrides_a_record() {
        let mut doc = AutoCommit::new();
        let import = |id: &str, name: &str| CodeEditorAction::ImportEntry {
            id: id.into(),
            kind: EntryKind::File,
            name: name.into(),
            parent: None,
            language: "Rust".into(),
            order: 0,
        };
        let first = import(&test_file_id(1), "main.rs");
        assert_eq!(first.change_kind(), tilo_actions::ChangeKind::External);
        first.apply(&mut doc).unwrap();
        let record = EditorDoc::file(&doc, &test_file_id(1)).expect("recorded");
        assert_eq!(record.path, "main.rs");
        assert!(
            EditorDoc::read_buffer_key(&doc, &test_file_id(1)).is_none(),
            "a file's text is read when it is opened"
        );
        // The same entry again, or another id at a path a peer recorded, is
        // left as it is.
        first.apply(&mut doc).unwrap();
        import(&test_file_id(2), "main.rs").apply(&mut doc).unwrap();
        assert!(EditorDoc::file(&doc, &test_file_id(2)).is_none());
    }

    #[test]
    fn replace_text_splices_against_the_text_it_meets() {
        let mut doc = AutoCommit::new();
        test_file_action(5, "fn main() {}\n")
            .apply(&mut doc)
            .unwrap();
        let replace = CodeEditorAction::ReplaceText {
            id: test_file_id(5),
            text: "fn main() { run() }\n".into(),
        };
        // A peer's edit landed first: the replacement still applies whole.
        text_edit_action(test_file_id(5), "fn main() {}\n", "// peer\nfn main() {}\n")
            .apply(&mut doc)
            .unwrap();
        replace.apply(&mut doc).unwrap();
        assert_eq!(
            EditorDoc::read_buffer_key(&doc, &test_file_id(5)).as_deref(),
            Some("fn main() { run() }\n")
        );
    }

    #[test]
    fn ensure_buffer_keeps_text_that_is_already_there() {
        let mut doc = AutoCommit::new();
        CodeEditorAction::EnsureBuffer {
            key: "file-1".into(),
            seed: "hello".into(),
        }
        .apply(&mut doc)
        .unwrap();
        CodeEditorAction::EnsureBuffer {
            key: "file-1".into(),
            seed: "replaced".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            EditorDoc::read_buffer_key(&doc, "file-1").as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn the_terminal_session_lives_in_one_panel_of_the_project() {
        use terminal::resource::Terminal;
        let mut doc = AutoCommit::new();
        assert!(EditorDoc::terminal_panel(&doc).is_none());
        let target = terminal::target::SessionTarget::default_environment().encode();
        CodeEditorAction::terminal(Terminal::RECORD_TARGET.with(target).build(0).unwrap())
            .apply(&mut doc)
            .unwrap();
        let panel = EditorDoc::terminal_panel(&doc).expect("terminal map");
        CodeEditorAction::terminal(
            Terminal::CLAIM_PANE
                .with("0b5c6a3e-2d4f-4c4b-9a7e-6f1d2c3b4a59".to_owned())
                .build(0)
                .unwrap(),
        )
        .apply(&mut doc)
        .unwrap();
        assert_eq!(EditorDoc::terminal_panel(&doc), Some(panel.clone()));
        let session = Terminal::read_under(&doc, &panel).unwrap();
        assert_eq!(
            session.session_target(),
            Some(terminal::target::SessionTarget::default_environment())
        );
        assert_eq!(session.pane_id(), "0b5c6a3e-2d4f-4c4b-9a7e-6f1d2c3b4a59");
        assert!(
            doc.get(automerge::ROOT, "pane").unwrap().is_none(),
            "nothing of the session is written beside the project's files"
        );
    }

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does (decode from the wire, apply through a local transport).
    fn dispatch(doc: &mut AutoCommit, action: &CodeEditorAction) {
        let encoded = encode(action).unwrap();
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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

    #[test]
    fn create_and_text_edit_actions_write_seeds_once_and_carry_minimal_payloads() {
        // Each phase builds its own document.
        create_file_action_writes_the_seeded_text();
        create_file_action_is_idempotent();
        edit_text_is_unicode_safe_and_carries_only_changed_region();
        large_buffer_edit_payload_is_bounded_to_the_changed_region();
    }

    /// Creating a buffer through the action materialises its seeded text in the
    /// document.
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
    fn create_file_action_is_idempotent() {
        let mut doc = AutoCommit::new();
        dispatch(&mut doc, &test_file_action(1, "first"));
        dispatch(&mut doc, &test_file_action(1, "second"));
        assert_eq!(
            EditorDoc::read_buffer_key(&doc, &test_file_id(1)).as_deref(),
            Some("first")
        );
    }

    fn edit_text_is_unicode_safe_and_carries_only_changed_region() {
        let action = text_edit_action("file".into(), "αβγδ", "αβXδ");
        let CodeEditorAction::EditText(TextEdit {
            start_chars,
            delete_chars,
            insert,
            deleted_text,
            before_context,
            after_context,
            ..
        }) = action
        else {
            unreachable!("text_edit_action always returns EditText");
        };
        assert_eq!((start_chars, delete_chars), (2, 1));
        assert_eq!(insert, "X");
        assert_eq!(deleted_text, "γ");
        assert_eq!(before_context, "αβ");
        assert_eq!(after_context, "δ");
    }

    fn large_buffer_edit_payload_is_bounded_to_the_changed_region() {
        let base = format!("{}needle{}", "a".repeat(512 * 1024), "z".repeat(512 * 1024));
        let next = base.replacen("needle", "N", 1);
        let CodeEditorAction::EditText(TextEdit {
            insert,
            deleted_text,
            before_context,
            after_context,
            ..
        }) = text_edit_action("file".into(), &base, &next)
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
    fn stale_deleted_or_oversized_edits_are_refused() {
        // Each phase builds its own document.
        edit_refuses_to_grow_a_collaborative_buffer_past_the_text_limit();
        text_edit_refuses_stale_or_deleted_target();
        a_stale_same_field_action_is_refused();
    }

    fn edit_refuses_to_grow_a_collaborative_buffer_past_the_text_limit() {
        let id = test_file_id(700);
        let mut doc = AutoCommit::new();
        let seed = "a".repeat(crate::disk::MAX_TEXT_BYTES as usize);
        dispatch(
            &mut doc,
            &create_file_action(id.clone(), "limit.rs".into(), None, 0, seed.clone()),
        );
        let action = text_edit_action(id.clone(), &seed, &(seed.clone() + "x"));
        assert!(action.apply(&mut doc).is_err());
        assert_eq!(
            EditorDoc::read_buffer_key(&doc, &id).as_deref(),
            Some(seed.as_str())
        );
    }

    #[test]
    fn concurrent_actions_converge_on_cross_replay() {
        // Each phase forks its own replicas from a fresh base.
        disjoint_and_same_position_text_edits_converge_on_replay();
        concurrent_disjoint_actions_replay_on_two_replicas_without_manual_retry();
        concurrent_rename_and_move_derive_the_same_path_on_cross_replay();
    }

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
        let left_env =
            tilo_actions::apply_local(&mut first, &left, encode(&left).unwrap()).unwrap();
        let right_env =
            tilo_actions::apply_local(&mut second, &right, encode(&right).unwrap()).unwrap();
        tilo_actions::replay(&mut first, &right_env, &registry()).unwrap();
        tilo_actions::replay(&mut second, &left_env, &registry()).unwrap();
        assert_eq!(
            EditorDoc::read_buffer_key(&first, &id),
            EditorDoc::read_buffer_key(&second, &id)
        );

        let mut one = AutoCommit::load(&bytes).unwrap();
        let mut two = AutoCommit::load(&bytes).unwrap();
        let a = text_edit_action(id.clone(), "abcdefgh", "abcdXefgh");
        let b = text_edit_action(id.clone(), "abcdefgh", "abcdYefgh");
        let a_env = tilo_actions::apply_local(&mut one, &a, encode(&a).unwrap()).unwrap();
        let b_env = tilo_actions::apply_local(&mut two, &b, encode(&b).unwrap()).unwrap();
        tilo_actions::replay(&mut one, &b_env, &registry()).unwrap();
        tilo_actions::replay(&mut two, &a_env, &registry()).unwrap();
        assert_eq!(
            EditorDoc::read_buffer_key(&one, &id),
            EditorDoc::read_buffer_key(&two, &id)
        );
    }

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

    const MAIN_TEXT: &str = "fn main() {}\n";
    const MAIN_EDITED: &str = "fn main() { run() }\n";
    /// `test_file_id(22)` and `(23)` as resource addresses.
    const MAIN_FILE: &str = "code_editor.file:00000000-0000-4000-8000-000000000016";
    const LIB_FILE: &str = "code_editor.file:00000000-0000-4000-8000-000000000017";

    /// A project folder holding `main.rs` and `src/lib.rs`, built through the
    /// same create actions the tree's commands dispatch.
    fn project_seed() -> AutoCommit {
        let mut doc = AutoCommit::new();
        for action in [
            CodeEditorAction::CreateFolder {
                id: test_file_id(20),
                name: "demo".into(),
                parent: None,
                order: 0,
            },
            CodeEditorAction::CreateFolder {
                id: test_file_id(21),
                name: "src".into(),
                parent: Some(test_file_id(20)),
                order: 0,
            },
            create_file_action(
                test_file_id(22),
                "main.rs".into(),
                Some(test_file_id(20)),
                1,
                MAIN_TEXT.into(),
            ),
            create_file_action(
                test_file_id(23),
                "lib.rs".into(),
                Some(test_file_id(21)),
                0,
                "pub fn lib() {}\n".into(),
            ),
        ] {
            dispatch(&mut doc, &action);
        }
        doc.commit();
        doc
    }

    /// The project as the framework publishes it from the document.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let state = <crate::resource::CodeEditor as ui::tilo_resource::Resource>::read(doc)
            .expect("project reads");
        vec![ui::tilo_resource::publish::<crate::resource::CodeEditor>(
            crate::resource::PROJECT_KIND,
            "demo",
            &state,
        )]
    }

    fn record(doc: &AutoCommit, id: u64) -> CollaborativeFile {
        EditorDoc::file(doc, &test_file_id(id)).expect("seeded tree entry")
    }

    fn encoded(action: &CodeEditorAction) -> EncodedAction {
        encode(action).expect("a code editor action encodes")
    }

    fn rename_main(doc: &AutoCommit) -> EncodedAction {
        encoded(&CodeEditorAction::RenameFile {
            id: test_file_id(22),
            kind: EntryKind::File,
            name: "app.rs".into(),
            expected_revision: record(doc, 22).name_revision,
        })
    }

    fn lib_language(doc: &AutoCommit) -> EncodedAction {
        encoded(&CodeEditorAction::SetFileLanguage {
            id: test_file_id(23),
            language: "Python".into(),
            expected_revision: record(doc, 23).language_revision,
        })
    }

    fn edit_main() -> EncodedAction {
        encoded(&text_edit_action(test_file_id(22), MAIN_TEXT, MAIN_EDITED))
    }

    ui::action_contract_tests! {
        seed: project_seed(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: ["New file", "New folder", "Rename file", "Delete file"],
        replay: [
            create_file => |_| encoded(&test_file_action(2, MAIN_TEXT)),
            edit_text => |_| edit_main(),
            rename_file => |doc| rename_main(doc),
        ],
        undo: [
            create_file => |_| encoded(&test_file_action(9, "hello")),
            set_language => |doc| lib_language(doc),
            move_to_project_root => |doc| encoded(&CodeEditorAction::MoveFile {
                id: test_file_id(23),
                kind: EntryKind::File,
                parent: Some(test_file_id(20)),
                order: 5,
                expected_revision: record(doc, 23).structure_revision,
            }),
        ],
        encodes: [
            delete_file => (MAIN_FILE, "delete", |doc, _| {
                encoded(&CodeEditorAction::DeleteFile {
                    id: test_file_id(22),
                    kind: EntryKind::File,
                    expected_revision: record(doc, 22).deletion_revision,
                })
            }),
            rename_file => (MAIN_FILE, "rename", arg: "app.rs", |doc, _| rename_main(doc)),
            set_language => (LIB_FILE, "set-language", arg: "Python", |doc, _| lib_language(doc)),
            replace_text => (MAIN_FILE, "replace-text", arg: MAIN_EDITED, |_, _| {
                encoded(&CodeEditorAction::ReplaceText {
                    id: test_file_id(22),
                    text: MAIN_EDITED.into(),
                })
            }),
        ],
    }

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
            tilo_actions::apply_local(&mut first, &rename, encode(&rename).unwrap()).unwrap();
        let language_envelope =
            tilo_actions::apply_local(&mut second, &language, encode(&language).unwrap()).unwrap();
        tilo_actions::replay(&mut first, &language_envelope, &registry()).unwrap();
        tilo_actions::replay(&mut second, &rename_envelope, &registry()).unwrap();
        let first_record = EditorDoc::file(&first, &test_file_id(88)).unwrap();
        let second_record = EditorDoc::file(&second, &test_file_id(88)).unwrap();
        assert_eq!(first_record.name, "renamed.rs");
        assert_eq!(first_record.language, "Rust");
        assert_eq!(first_record, second_record);
    }

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
            tilo_actions::apply_local(&mut first, &rename, encode(&rename).unwrap()).unwrap();
        let move_envelope =
            tilo_actions::apply_local(&mut second, &move_file, encode(&move_file).unwrap())
                .unwrap();
        tilo_actions::replay(&mut first, &move_envelope, &registry()).unwrap();
        tilo_actions::replay(&mut second, &rename_envelope, &registry()).unwrap();
        let first_record = EditorDoc::file(&first, &file).unwrap();
        let second_record = EditorDoc::file(&second, &file).unwrap();
        assert_eq!(first_record.path, "dest/renamed.rs");
        assert_eq!(first_record, second_record);
    }
}
