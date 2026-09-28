//! Topology admission over the native `contents` wire shape. No UI dependency.
use automerge::AutoCommit;
use serde_json::Value;
use std::collections::BTreeMap;

fn json(doc: Option<&AutoCommit>) -> Result<Value, String> {
    doc.map(|doc| serde_json::to_value(automerge::AutoSerde::from(doc)).map_err(|e| e.to_string()))
        .transpose()
        .map(|v| v.unwrap_or(Value::Null))
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
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
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
