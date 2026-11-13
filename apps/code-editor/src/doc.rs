//! Reading the project straight from its document: what the editor binds
//! its view to and what reads a file's text without the window's state.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::tilo_resource::Resource;

use crate::project::Project;

pub struct EditorDoc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollaborativeFile {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub parent: Option<String>,
    pub language: String,
    pub order: i64,
    pub deleted: bool,
}

impl CollaborativeFile {
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
    pub fn file(doc: &AutoCommit, id: &str) -> Option<CollaborativeFile> {
        Self::files(doc).into_iter().find(|file| file.id == id)
    }

    pub fn files(doc: &AutoCommit) -> Vec<CollaborativeFile> {
        Project::read(doc)
            .map(|project| CollaborativeFile::all(&project))
            .unwrap_or_default()
    }

    pub fn buffer_text_key(doc: &AutoCommit, key: &str) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), buffers))) = doc.get(&ROOT, "buffers") else {
            return None;
        };
        match doc.get(&buffers, key) {
            Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn ensure_buffer(doc: &mut AutoCommit, node: usize, seed: &str) -> ObjId {
        Self::ensure_buffer_key(doc, &node.to_string(), seed).unwrap_or(ROOT)
    }

    #[cfg(test)]
    pub fn ensure_buffer_key(doc: &mut AutoCommit, key: &str, seed: &str) -> Option<ObjId> {
        if let Some(text) = Self::buffer_text_key(doc, key) {
            return Some(text);
        }
        let buffers = ui::tilo_resource::Slot::Key(&ROOT, "buffers")
            .object(doc, ObjType::Map)
            .ok()?;
        ui::tilo_resource::write_key(doc, &buffers, key, &seed.to_owned()).ok()?;
        Self::buffer_text_key(doc, key)
    }

    pub fn read_buffer_key(doc: &AutoCommit, key: &str) -> Option<String> {
        Self::buffer_text_key(doc, key).and_then(|t| doc.text(&t).ok())
    }

    #[cfg(test)]
    pub fn splice(
        doc: &mut AutoCommit,
        text: &ObjId,
        char_pos: usize,
        delete: usize,
        insert: &str,
    ) {
        let _ = ui::tilo_resource::TextEdit::Splice {
            at: char_pos,
            delete,
            insert: insert.to_owned(),
        }
        .apply_under(doc, text);
    }
}
