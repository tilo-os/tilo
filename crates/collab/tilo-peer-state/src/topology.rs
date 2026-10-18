//! Topology admission over the native `contents` wire shape. No UI dependency.
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc};
use serde_json::Value;
use std::collections::BTreeMap;

/// The root entries topology admission reads.
const TOPOLOGY_KEYS: [&str; 2] = ["contents", "contents_owners_v1"];

/// The document's topology entries as JSON, written the way
/// [`automerge::AutoSerde`] writes a document. Only these entries are read:
/// writing the whole room made each admission cost as much as the roster
/// and everything else the room holds.
fn json(doc: Option<&AutoCommit>) -> Result<Value, String> {
    let Some(doc) = doc else {
        return Ok(Value::Null);
    };
    let mut root = serde_json::Map::new();
    for key in TOPOLOGY_KEYS {
        if let Some((value, object)) = doc.get(ROOT, key).map_err(|e| e.to_string())? {
            root.insert(key.to_owned(), subtree(doc, &value, &object)?);
        }
    }
    Ok(Value::Object(root))
}
fn subtree(
    doc: &AutoCommit,
    value: &automerge::Value<'_>,
    object: &ObjId,
) -> Result<Value, String> {
    Ok(match value {
        automerge::Value::Object(ObjType::Map | ObjType::Table) => {
            let mut map = serde_json::Map::new();
            for key in doc.keys(object) {
                if let Some((value, child)) = doc.get(object, &key).map_err(|e| e.to_string())? {
                    map.insert(key, subtree(doc, &value, &child)?);
                }
            }
            Value::Object(map)
        }
        automerge::Value::Object(ObjType::Text) => {
            Value::String(doc.text(object).map_err(|e| e.to_string())?)
        }
        automerge::Value::Object(ObjType::List) => {
            let mut items = Vec::new();
            for index in 0..doc.length(object) {
                if let Some((value, child)) = doc.get(object, index).map_err(|e| e.to_string())? {
                    items.push(subtree(doc, &value, &child)?);
                }
            }
            Value::Array(items)
        }
        automerge::Value::Scalar(scalar) => {
            serde_json::to_value(scalar.as_ref()).map_err(|e| e.to_string())?
        }
    })
}
fn owners(value: &Value) -> Result<BTreeMap<u16, String>, String> {
    let Some(value) = value.get("contents_owners_v1") else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .ok_or("invalid topology ownership registry")?
        .iter()
        .map(|(salt, key)| {
            let salt = u16::from_str_radix(salt, 16).map_err(|_| "invalid topology owner salt")?;
            let key = key.as_str().ok_or("invalid topology owner key")?;
            if salt == 0 || key.len() != 64 || hex::decode(key).is_err() {
                return Err("invalid topology owner identity".into());
            }
            Ok((salt, key.to_owned()))
        })
        .collect()
}
fn entries(value: &Value) -> Result<BTreeMap<(u64, u8), Value>, String> {
    fn visit(
        value: &Value,
        level: u8,
        parent: &str,
        out: &mut BTreeMap<(u64, u8), Value>,
    ) -> Result<(), String> {
        let map = value.as_object().ok_or("invalid topology map")?;
        for (id, entry) in map {
            let raw = u64::from_str_radix(id, 16).map_err(|_| "invalid topology id")?;
            let object = entry.as_object().ok_or("invalid topology entry")?;
            let child = match level {
                0 => Some("groups"),
                1 => Some("tabs"),
                _ => None,
            };
            let mut own = object.clone();
            if let Some(key) = child {
                own.remove(key);
            }
            own.insert("$parent".into(), Value::String(parent.into()));
            if out.insert((raw, level), Value::Object(own)).is_some() {
                return Err("duplicate topology identity".into());
            }
            if let Some(nested) = child.and_then(|key| object.get(key)) {
                visit(nested, level.saturating_add(1), id, out)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    if let Some(contents) = value.get("contents") {
        visit(contents, 0, "", &mut out)?;
    }
    Ok(out)
}
/// Whether a topology entry is absent or carries nothing of its own (only
/// its place in the tree, `$parent`).
fn bare(entry: Option<&Value>) -> bool {
    entry.is_none_or(|entry| {
        entry
            .as_object()
            .is_some_and(|fields| fields.keys().all(|key| key == "$parent"))
    })
}
/// Registry writes require pinned governance authority. Registered owners alone
/// may change their entries; a legacy room with no registry remains authority-owned.
/// Record routes are stable identities: changing a live binding is never a rename.
///
/// # Errors
///
/// Returns an error if `source` may not make the registry change.
pub fn validate(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    source: &str,
    authority: &str,
) -> Result<(), String> {
    let before = json(before)?;
    let after = json(Some(after))?;
    let old_owners = owners(&before)?;
    let new_owners = owners(&after)?;
    if old_owners != new_owners {
        if source != authority {
            return Err("topology ownership registry requires pinned authority".into());
        }
        if old_owners
            .iter()
            .any(|(salt, key)| new_owners.get(salt) != Some(key))
        {
            return Err("topology owner namespace cannot be rebound".into());
        }
    }
    let old = entries(&before)?;
    let new = entries(&after)?;
    for id in old.keys().chain(new.keys()) {
        if old.get(id) == new.get(id) {
            continue;
        }
        let salt = (id.0 >> 48) as u16;
        // The Lounge's strip entry is nobody's: the bare container the
        // windows everyone puts in the Lounge hang under (each checked
        // against its own owner), so any member may create it and nobody
        // may write anything onto it.
        if salt == tilo_desktop_model::ids::SHARED_SALT
            && id.1 == 0
            && bare(old.get(id))
            && bare(new.get(id))
        {
            continue;
        }
        let owner = new_owners
            .get(&salt)
            .or_else(|| old_owners.get(&salt))
            .map_or(authority, String::as_str);
        // The pinned room governor republishes accepted multi-owner history in
        // its durable checkpoint, including to governance-first/reconnecting peers.
        let checkpoint = source == authority;
        if !checkpoint && source != owner {
            return Err("owned-content conflict: topology writer is not owner".into());
        }
        if id.1 == 2
            && let (Some(a), Some(b)) = (old.get(id), new.get(id))
            && a.get("record_id") != b.get("record_id")
        {
            return Err("canonical record route cannot be rebound".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    /// The topology entries read the same as in the whole document written
    /// by `AutoSerde`, and nothing else of the room is read.
    #[test]
    fn topology_reads_its_entries_as_the_whole_document_writes_them() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "peer_identities_v1", vec![7u8; 64]).unwrap();
        let owners = doc
            .put_object(ROOT, "contents_owners_v1", ObjType::Map)
            .unwrap();
        doc.put(&owners, "0001", "aa".repeat(32)).unwrap();
        let contents = doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
        let strip = doc
            .put_object(&contents, "0001000000000001", ObjType::Map)
            .unwrap();
        doc.put(&strip, "pinned", true).unwrap();
        doc.put(&strip, "order", 3_i64).unwrap();
        let title = doc.put_object(&strip, "title", ObjType::Text).unwrap();
        doc.splice_text(&title, 0, 0, "Notes").unwrap();
        let tags = doc.put_object(&strip, "tags", ObjType::List).unwrap();
        doc.insert(&tags, 0, "a").unwrap();
        doc.insert(&tags, 1, 2.5).unwrap();
        doc.commit();
        let whole = serde_json::to_value(automerge::AutoSerde::from(&doc)).unwrap();
        let read = json(Some(&doc)).unwrap();
        for key in TOPOLOGY_KEYS {
            assert_eq!(read.get(key), whole.get(key), "{key}");
        }
        assert!(read.get("peer_identities_v1").is_none());
        assert_eq!(json(None).unwrap(), Value::Null);
    }
    #[test]
    fn authority_checkpoint_can_introduce_other_registered_namespaces_after_governance() {
        let authority = "aa".repeat(32);
        let other = "bb".repeat(32);
        let mut before = AutoCommit::new();
        before.put(ROOT, "owner", "owner").unwrap();
        before.commit();
        let mut after = before.clone();
        let owners = after
            .put_object(ROOT, "contents_owners_v1", ObjType::Map)
            .unwrap();
        after.put(&owners, "0001", authority.as_str()).unwrap();
        after.put(&owners, "0002", other.as_str()).unwrap();
        let contents = after.put_object(ROOT, "contents", ObjType::Map).unwrap();
        for id in ["0001000000000001", "0002000000000001"] {
            let strip = after.put_object(&contents, id, ObjType::Map).unwrap();
            after.put(strip, "name", "Shared").unwrap();
        }
        after.commit();
        assert!(
            validate(Some(&before), &after, &authority, &authority).is_ok(),
            "a governed receiver must accept the pinned authority checkpoint covering all registered owners"
        );
        let mut bad = after.clone();
        let strip = bad.get(&contents, "0001000000000001").unwrap().unwrap().1;
        bad.put(strip, "name", "Stolen").unwrap();
        bad.commit();
        assert!(validate(Some(&after), &bad, &other, &authority).is_err());
    }
    /// Any member may open the Lounge's strip entry to put a window of its
    /// own under it; nobody may write onto the entry itself.
    #[test]
    fn a_member_may_open_the_lounge_entry_but_not_write_onto_it() {
        let authority = "aa".repeat(32);
        let member = "bb".repeat(32);
        let mut before = AutoCommit::new();
        let owners = before
            .put_object(ROOT, "contents_owners_v1", ObjType::Map)
            .unwrap();
        before.put(&owners, "0002", member.as_str()).unwrap();
        before.commit();
        let lounge = format!(
            "{:016x}",
            tilo_desktop_model::ids::shared_id("6f1c2c1e-6d3b-4c55-9d1e-2f6e5a1b9c01", "lounge")
        );
        let mut after = before.clone();
        let contents = after.put_object(ROOT, "contents", ObjType::Map).unwrap();
        let strip = after
            .put_object(&contents, lounge.as_str(), ObjType::Map)
            .unwrap();
        let groups = after.put_object(&strip, "groups", ObjType::Map).unwrap();
        let group = after
            .put_object(&groups, "0002000000000001", ObjType::Map)
            .unwrap();
        after.put(&group, "order", 3_u64).unwrap();
        after.commit();
        assert!(validate(Some(&before), &after, &member, &authority).is_ok());

        let mut renamed = after.clone();
        renamed.put(&strip, "name", "Mine").unwrap();
        renamed.commit();
        assert!(validate(Some(&after), &renamed, &member, &authority).is_err());
    }
    #[test]
    fn even_pinned_authority_cannot_rebind_a_live_record_route() {
        let authority = "aa".repeat(32);
        let mut before = AutoCommit::new();
        let contents = before.put_object(ROOT, "contents", ObjType::Map).unwrap();
        let strip = before
            .put_object(contents, "0001000000000001", ObjType::Map)
            .unwrap();
        let groups = before.put_object(strip, "groups", ObjType::Map).unwrap();
        let group = before
            .put_object(groups, "0001000000000002", ObjType::Map)
            .unwrap();
        let tabs = before.put_object(group, "tabs", ObjType::Map).unwrap();
        let tab = before
            .put_object(tabs, "0001000000000003", ObjType::Map)
            .unwrap();
        before
            .put(&tab, "record_id", "tab:0000000000000001")
            .unwrap();
        before.commit();
        let mut after = before.clone();
        after.put(tab, "record_id", "tab:0000000000000002").unwrap();
        after.commit();
        assert!(
            validate(Some(&before), &after, &authority, &authority)
                .unwrap_err()
                .contains("record route")
        );
    }
}
