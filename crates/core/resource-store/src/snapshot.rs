//! What the index holds of one document, saved: what a start reads instead of the document.

use automerge::ChangeHash;
use serde::{Deserialize, Serialize};

use crate::index::{Change, Index};
use collab_types::store::Cell;

/// The layout of a snapshot: one of another is never read, and the document is loaded instead.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Saved {
    format: u32,
    readers: String,
    heads: Vec<[u8; 32]>,
    resources: Vec<SavedResource>,
}

#[derive(Serialize, Deserialize)]
struct SavedResource {
    kind: String,
    id: String,
    parent: Option<String>,
    order: Option<String>,
    fields: Vec<(String, Cell)>,
}

/// A snapshot read back: whose document it is, the heads it was taken at, and its resources.
pub struct Restored {
    pub readers: String,
    pub heads: Vec<ChangeHash>,
    pub changes: Vec<Change>,
}

/// `doc` as `index` holds it, saved; `None` for a document it has not seen.
pub fn save(index: &Index, doc: &str) -> Option<Vec<u8>> {
    let resources = index
        .held(doc)
        .filter_map(|(kind, id)| {
            let held = index.get(kind, id)?;
            Some(SavedResource {
                kind: kind.to_owned(),
                id: id.to_owned(),
                parent: held.parent.clone(),
                order: held.order.clone(),
                fields: held
                    .fields
                    .iter()
                    .map(|(name, cell)| (name.clone(), cell.clone()))
                    .collect(),
            })
        })
        .collect();
    let saved = Saved {
        format: FORMAT,
        readers: index.readers(doc)?.to_owned(),
        heads: index.heads(doc)?.iter().map(|head| head.0).collect(),
        resources,
    };
    postcard::to_allocvec(&saved).ok()
}

/// The snapshot `bytes` holds; `None` for bytes of another layout or none at all.
pub fn read(bytes: &[u8]) -> Option<Restored> {
    let saved: Saved = postcard::from_bytes(bytes).ok()?;
    (saved.format == FORMAT).then(|| Restored {
        readers: saved.readers,
        heads: saved.heads.into_iter().map(ChangeHash).collect(),
        changes: saved
            .resources
            .into_iter()
            .map(|resource| Change::Put {
                kind: resource.kind,
                id: resource.id,
                parent: resource.parent,
                order: resource.order,
                fields: resource.fields.into_iter().collect(),
            })
            .collect(),
    })
}
