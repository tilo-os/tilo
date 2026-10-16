//! Reading the project straight from its document: what the editor binds
//! its view to and what reads a file's text without the window's state.
//!
//! The [`Project`] record keeps each file's text as an Automerge `Text`
//! under `buffers`, keyed by the file's id:
//!
//! ```text
//! root
//! ├── "files"    the project's files and folders (a collection)
//! └── "buffers"  (Map, keyed by file id)
//!     └── <file id> (Text)   the file's contents, character-addressed
//! ```
//!
//! Storing the text as `Text` is what gives the editor collaborative
//! editing: every edit is a splice, so two people typing in one file
//! converge.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::tilo_resource::Resource;

use crate::project::Project;

/// Stateless namespace for editor-document helpers.
pub struct EditorDoc;

/// A file or folder of the project as the editor's tree takes it in: its
/// identity, place and language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollaborativeFile {
    pub id: String,
    /// `file` or `folder`.
    pub kind: String,
    pub name: String,
    /// The folders it is in and its name, joined by `/`.
    pub path: String,
    pub parent: Option<String>,
    pub language: String,
    /// Its place among the project's entries.
    pub order: i64,
    /// Whether it left the project since the editor last took it in.
    pub deleted: bool,
}

impl CollaborativeFile {
    /// Every file and folder of `project`, in order.
    #[must_use]
    pub fn all(project: &Project) -> Vec<Self> {
        project
            .files
            .iter()
            .enumerate()
            .map(|(order, (id, entry))| Self {
                id: id.to_string(),
                kind: entry.kind.as_str().to_owned(),
                name: entry.name.clone(),
                path: project.path_of(id).unwrap_or_else(|| entry.name.clone()),
                parent: entry.parent.as_ref().map(ToString::to_string),
                language: entry.language.clone(),
                order: i64::try_from(order).unwrap_or(i64::MAX),
                deleted: false,
            })
            .collect()
    }
}

impl EditorDoc {
    /// The file or folder `id` of the project in `doc`.
    pub fn file(doc: &AutoCommit, id: &str) -> Option<CollaborativeFile> {
        Self::files(doc).into_iter().find(|file| file.id == id)
    }

    /// Every file and folder of the project in `doc`, in order.
    pub fn files(doc: &AutoCommit) -> Vec<CollaborativeFile> {
        Project::read(doc)
            .map(|project| CollaborativeFile::all(&project))
            .unwrap_or_default()
    }

    /// The Text object of the file `key`.
    pub fn buffer_text_key(doc: &AutoCommit, key: &str) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), buffers))) = doc.get(&ROOT, "buffers") else {
            return None;
        };
        match doc.get(&buffers, key) {
            Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
            _ => None,
        }
    }

    /// A text numbered `node`, holding `seed` unless it already holds text:
    /// a document for the editor's view on its own.
    #[cfg(test)]
    pub fn ensure_buffer(doc: &mut AutoCommit, node: usize, seed: &str) -> ObjId {
        Self::ensure_buffer_key(doc, &node.to_string(), seed).unwrap_or(ROOT)
    }

    /// A text keyed `key`, holding `seed` unless it already holds text.
    #[cfg(test)]
    pub fn ensure_buffer_key(doc: &mut AutoCommit, key: &str, seed: &str) -> Option<ObjId> {
        use automerge::transaction::Transactable;
        if let Some(text) = Self::buffer_text_key(doc, key) {
            return Some(text);
        }
        let buffers = match doc.get(&ROOT, "buffers") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, "buffers", ObjType::Map).ok()?,
        };
        let text = doc.put_object(&buffers, key, ObjType::Text).ok()?;
        doc.splice_text(&text, 0, 0, seed).ok()?;
        Some(text)
    }

    /// The whole text of the file `key`.
    pub fn read_buffer_key(doc: &AutoCommit, key: &str) -> Option<String> {
        Self::buffer_text_key(doc, key).and_then(|t| doc.text(&t).ok())
    }

    /// Splice into a buffer's Text at a character index. `delete` characters
    /// are removed, then `insert` is inserted. Indices are in *characters*
    /// (Automerge Text is char-addressed), matching the editor's caret model.
    #[cfg(test)]
    pub fn splice(
        doc: &mut AutoCommit,
        text: &ObjId,
        char_pos: usize,
        delete: usize,
        insert: &str,
    ) {
        use automerge::transaction::Transactable;
        let _ = doc.splice_text(text, char_pos, delete as isize, insert);
    }
}
