//! A resource's versions: the changes that made, changed or removed it, read from the history of
//! the documents it is in, each as the action that made it stamped it ([`Stamp`]).

use automerge::Automerge;
use serde::{Deserialize, Serialize};

use crate::admit::Stamp;

/// What one change did to a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionKind {
    Made,
    Changed,
    Removed,
}

/// One version of a resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// The action that made it; `None` for a change that names none.
    pub stamp: Option<Stamp>,
    pub kind: VersionKind,
    /// The fields it set; empty for one that made or removed the resource.
    pub fields: Vec<String>,
    /// It changed the resource's content.
    pub content: bool,
    /// The change, by its hash in hex.
    pub change: String,
    /// The counter of the change's first operation: its place in the order every holder agrees
    /// on.
    pub op: u64,
}

/// The versions of the resource `id` in the document `doc_id`, oldest first.
pub(crate) fn of(doc: &Automerge, doc_id: &str, id: &str) -> Vec<Version> {
    let mut versions = Vec::new();
    for change in doc.get_changes(&[]) {
        let Some((touched, _)) = crate::admit::touched(doc, doc_id, &change) else {
            continue;
        };
        let Some(touch) = touched.resources.get(id) else {
            continue;
        };
        let kind = if touch.made {
            VersionKind::Made
        } else if touch.removed {
            VersionKind::Removed
        } else {
            VersionKind::Changed
        };
        versions.push(Version {
            stamp: Stamp::of(&change),
            kind,
            fields: touch.fields.iter().cloned().collect(),
            content: touch.content,
            change: change.hash().to_string(),
            op: change.start_op().get(),
        });
    }
    versions
}
