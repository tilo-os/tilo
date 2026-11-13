use super::{
    AutoCommit, INDEX, ObjId, ObjType, ObjectAt, ROOT, ReadDoc, RecordCheckout, RecordId,
    RecordKind, RecordSet, StringAt, Transactable, object_at, str_at, string_at,
};
use tilo_sort::CompactSort as _;

/// Select the workspace's record index. Malformed or conflicted metadata
/// fails closed; only an entirely absent index is created.
fn index_obj(doc: &mut AutoCommit) -> Option<ObjId> {
    match object_at(doc, &ROOT, INDEX, ObjType::Map) {
        ObjectAt::Missing => doc.put_object(&ROOT, INDEX, ObjType::Map).ok(),
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Invalid => None,
    }
}

fn index_ro(doc: &AutoCommit) -> Option<ObjId> {
    match object_at(doc, &ROOT, INDEX, ObjType::Map) {
        ObjectAt::Object(object) => Some(object),
        ObjectAt::Missing | ObjectAt::Invalid => None,
    }
}

/// Bring a record name into this workspace. Keyed by record id (not
/// appended to a list) so two machines referencing the same name
/// concurrently converge on one index entry. The document it resolves to
/// remains within this workspace's scope; this is not a grant to another
/// workspace.
pub fn reference(doc: &mut AutoCommit, id: &RecordId, kind: &RecordKind) -> bool {
    if RecordId::parse(id.as_str()).is_none() || !kind.is_valid() {
        return false;
    }
    let Some(idx) = index_obj(doc) else {
        return false;
    };
    match object_at(doc, &idx, id.as_str(), ObjType::Map) {
        ObjectAt::Object(object) => {
            // Refresh the type only if it actually differs, so
            // re-opening a record is not itself an edit.
            match string_at(doc, &object, "kind") {
                StringAt::String(existing) if existing == kind.as_str() => {}
                // Renamed: the index follows the kind's current name.
                StringAt::String(_) => {
                    let _ = doc.put(&object, "kind", kind.as_str());
                }
                StringAt::Missing | StringAt::Invalid => return false,
            }
            false
        }
        ObjectAt::Missing => {
            let Ok(entry) = doc.put_object(&idx, id.as_str(), ObjType::Map) else {
                return false;
            };
            let _ = doc.put(&entry, "kind", kind.as_str());
            true
        }
        ObjectAt::Invalid => false,
    }
}

/// Key, in a record's index entry, of what people call the record (a PDF's
/// file name, a document's heading): what finds it by name where its id is a
/// minted one that names nothing.
const NAME: &str = "name";

/// The longest name an entry is read with: the index is replicated input.
const MAX_NAME_CHARS: usize = 256;

/// Say what `id`, a record this workspace references, is called: `None`
/// when its app gives it no name of its own. Whether the entry changed;
/// a record the index does not reference is left alone.
pub fn set_name(doc: &mut AutoCommit, id: &RecordId, name: Option<&str>) -> bool {
    let Some(idx) = index_ro(doc) else {
        return false;
    };
    let ObjectAt::Object(entry) = object_at(doc, &idx, id.as_str(), ObjType::Map) else {
        return false;
    };
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    let held = match string_at(doc, &entry, NAME) {
        StringAt::String(held) => Some(held),
        StringAt::Missing => None,
        StringAt::Invalid => return false,
    };
    if held.as_deref() == name {
        return false;
    }
    match name {
        Some(name) => doc.put(&entry, NAME, name).is_ok(),
        None => doc.delete(&entry, NAME).is_ok(),
    }
}

/// What the index says `id` is called, if it names it.
pub fn name_of(doc: &AutoCommit, id: &RecordId) -> Option<String> {
    let idx = index_ro(doc)?;
    let ObjectAt::Object(entry) = object_at(doc, &idx, id.as_str(), ObjType::Map) else {
        return None;
    };
    let name: String = str_at(doc, &entry, NAME)?
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Drop a record's reference from this workspace. The record itself is
/// untouched — another workspace may hold an independently scoped replica.
pub fn unreference(doc: &mut AutoCommit, id: &RecordId) -> bool {
    let Some(idx) = index_ro(doc) else {
        return false;
    };
    match object_at(doc, &idx, id.as_str(), ObjType::Map) {
        ObjectAt::Object(_) => doc.delete(&idx, id.as_str()).is_ok(),
        ObjectAt::Missing | ObjectAt::Invalid => false,
    }
}

/// Whether this workspace references `id`.
pub fn holds(doc: &AutoCommit, id: &RecordId) -> bool {
    index_ro(doc).is_some_and(|idx| {
        let ObjectAt::Object(entry) = object_at(doc, &idx, id.as_str(), ObjType::Map) else {
            return false;
        };
        str_at(doc, &entry, "kind")
            .map(RecordKind::new)
            .is_some_and(|kind| kind.is_valid())
    })
}

/// Every well-formed record this workspace references.  This is retained
/// for callers that already have their own work bound; hosts that learn a
/// collaborator-supplied index should use [`discoverable_references`]
/// instead.
pub fn references(doc: &AutoCommit) -> Vec<RecordCheckout> {
    let Some(idx) = index_ro(doc) else {
        return Vec::new();
    };
    let mut out: Vec<RecordCheckout> = doc
        .keys(&idx)
        .filter_map(|k| {
            let id = RecordId::parse(&k)?;
            let ObjectAt::Object(object) = object_at(doc, &idx, k.as_str(), ObjType::Map) else {
                return None;
            };
            let kind = RecordKind::new(str_at(doc, &object, "kind")?);
            let checkout = RecordCheckout::new(id, kind);
            checkout.is_valid().then_some(checkout)
        })
        .collect();
    out.compact_sort();
    out
}

/// A deterministic, resource-bounded view of collaborator-supplied record
/// names.  It reads at most a small fixed prefix of the CRDT map and
/// returns at most [`super::MAX_DISCOVERED_RECORDS`] valid entries.
/// It does **not** open records, create persistence, or join record
/// routes; the caller must make that explicit after choosing a record.
pub fn discoverable_references(doc: &AutoCommit) -> Vec<RecordCheckout> {
    let Some(idx) = index_ro(doc) else {
        return Vec::new();
    };
    let mut out: Vec<RecordCheckout> = doc
        .keys(&idx)
        .take(super::MAX_INDEX_KEYS_SCANNED)
        .filter_map(|key| {
            let id = RecordId::parse(&key)?;
            let ObjectAt::Object(entry) = object_at(doc, &idx, key.as_str(), ObjType::Map) else {
                return None;
            };
            let kind = RecordKind::new(str_at(doc, &entry, "kind")?);
            let checkout = RecordCheckout::new(id, kind);
            checkout.is_valid().then_some(checkout)
        })
        .collect();
    out.compact_sort();
    out.truncate(super::MAX_DISCOVERED_RECORDS);
    out
}

/// Records this workspace holds that reference `target` under `relation` —
/// how an app finds "the annotations for this document" without a
/// second index.
pub fn referencing(
    workspace: &AutoCommit,
    docs: &RecordSet,
    target: &RecordId,
    relation: &str,
) -> Vec<RecordCheckout> {
    references(workspace)
        .into_iter()
        .filter(|c| {
            docs.peek(&c.id).is_some_and(|d| {
                super::references(d)
                    .iter()
                    .any(|r| &r.target == target && r.relation == relation)
            })
        })
        .collect()
}

/// Copy, the explicit verb: mints a new record id rather than reusing
/// the origin's. The new document is created empty by the caller's
/// next [`RecordSet::checkout`]; this returns the identity to check out.
pub fn duplicate(doc: &mut AutoCommit, origin: &RecordCheckout) -> RecordCheckout {
    let copy = RecordCheckout::new(RecordId::minted(), origin.kind.clone());
    reference(doc, &copy.id, &copy.kind);
    copy
}
