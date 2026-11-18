//! Reading a file's content versions out of the tree document.
//!
//! The device that writes a file's bytes keeps a version per content it wrote, in the
//! document of the chunk the file's node lives in: what the content a device keeps, frees
//! and asks for is bound to ([`crate::files`] types, `tilo_fs::namespace`). What a file's
//! change was and who made it is the tree's own History, not these.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::files::{FileDocumentId, FileVersionId, FileVersionV1};

/// The document's map of record histories.
pub const HISTORY_KEY: &str = "history";
/// One record's map of versions.
pub const VERSIONS_KEY: &str = "versions";

/// A record's versions, by their id.
#[must_use]
pub fn document_versions<D: ReadDoc>(
    doc: &D,
    document_id: FileDocumentId,
) -> BTreeMap<FileVersionId, FileVersionV1> {
    let mut versions = BTreeMap::new();
    for record in document_maps(doc, document_id) {
        for map in sub_maps(doc, &record, VERSIONS_KEY) {
            for key in doc.keys(&map) {
                let Some(bytes) = scalar_bytes(doc.get(&map, &key).ok().flatten()) else {
                    continue;
                };
                let Ok(version) =
                    collab_types::canonical::from_canonical_bytes::<FileVersionV1>(&bytes)
                else {
                    continue;
                };
                versions.insert(version.id, version);
            }
        }
    }
    versions
}

/// The versions `document_id` descends from having no child: its current states.
#[must_use]
pub fn computed_heads(
    versions: &BTreeMap<FileVersionId, FileVersionV1>,
) -> BTreeSet<FileVersionId> {
    let mut heads: BTreeSet<_> = versions.keys().copied().collect();
    for version in versions.values() {
        for parent in &version.parents {
            heads.remove(parent);
        }
    }
    heads
}

fn scalar_bytes(value: Option<(Value<'_>, ObjId)>) -> Option<Vec<u8>> {
    match value {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Every map holding record histories: one until two devices that each made
/// their own document meet, and one per device after, since a merge keeps both
/// maps under the key. A reader takes the maps together, or it would see the
/// versions of one device and none of the other's.
#[must_use]
pub fn roots<D: ReadDoc>(doc: &D) -> Vec<ObjId> {
    doc.get_all(automerge::ROOT, HISTORY_KEY)
        .map(|values| {
            values
                .into_iter()
                .filter_map(|(value, id)| match value {
                    Value::Object(ObjType::Map) => Some(id),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sub_maps<D: ReadDoc>(doc: &D, parent: &ObjId, key: &str) -> Vec<ObjId> {
    doc.get_all(parent, key)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(value, id)| match value {
            Value::Object(ObjType::Map) => Some(id),
            _ => None,
        })
        .collect()
}

/// Every map `document_id`'s history has: two devices that both wrote it each made
/// one.
fn document_maps<D: ReadDoc>(doc: &D, document_id: FileDocumentId) -> Vec<ObjId> {
    let mut maps = Vec::new();
    for root in roots(doc) {
        maps.extend(sub_maps(doc, &root, &document_id.0.to_string()));
    }
    maps
}
