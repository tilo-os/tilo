//! Native room-policy leaves shared by the shell and certified browser peer.
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::Role;
use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};
use std::collections::BTreeMap;
pub type RoomGovernance = (
    Option<String>,
    BTreeMap<String, Vec<u8>>,
    BTreeMap<String, String>,
    BTreeMap<String, Vec<u8>>,
    Option<Vec<u8>>,
);
pub fn governance_of(doc: Option<&AutoCommit>) -> RoomGovernance {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    let Some(doc) = doc else {
        return (
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            None,
        );
    };
    let owner = match doc.get(&ROOT, "owner").ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    };
    let mut caps = std::collections::BTreeMap::new();
    if let Some((_, caps_id)) = doc.get(&ROOT, "capabilities").ok().flatten() {
        let keys: Vec<String> = doc.keys(&caps_id).collect();
        for key in keys {
            if let Some((Value::Scalar(s), _)) = doc.get(&caps_id, &key).ok().flatten()
                && let ScalarValue::Bytes(blob) = s.as_ref()
            {
                caps.insert(key, blob.clone());
            }
        }
    }
    // Peer names fold in beside the capability list, the same kind
    // of claim — left out, it'd be the one governance key a guest
    // could write, and an impostor's first target.
    let mut peers = std::collections::BTreeMap::new();
    if let Some((_, peers_id)) = doc.get(&ROOT, "peers").ok().flatten() {
        let keys: Vec<String> = doc.keys(&peers_id).collect();
        for key in keys {
            if let Some((Value::Scalar(s), _)) = doc.get(&peers_id, &key).ok().flatten()
                && let ScalarValue::Str(v) = s.as_ref()
            {
                peers.insert(key, v.to_string());
            }
        }
    }
    let mut identities = std::collections::BTreeMap::new();
    if let Some((_, object)) = doc.get(&ROOT, "peer_identities_v1").ok().flatten() {
        for key in doc.keys(&object) {
            if let Some((Value::Scalar(value), _)) = doc.get(&object, &key).ok().flatten()
                && let ScalarValue::Bytes(bytes) = value.as_ref()
            {
                identities.insert(key, bytes.clone());
            }
        }
    }
    (
        owner,
        caps,
        peers,
        identities,
        crate::coverage::governed_bytes(doc),
    )
}
pub fn extract_governed_fields(
    candidate: &AutoCommit,
) -> (Option<String>, Option<String>, Vec<CapOverride>) {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    let name = match candidate.get(&ROOT, "name").ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    };
    let owner = match candidate.get(&ROOT, "owner").ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    };
    let mut caps: Vec<CapOverride> = Vec::new();
    if let Some((_, caps_id)) = candidate.get(&ROOT, "capabilities").ok().flatten() {
        let keys: Vec<String> = candidate.keys(&caps_id).collect();
        for key in keys {
            if let Some((Value::Scalar(s), _)) = candidate.get(&caps_id, &key).ok().flatten()
                && let ScalarValue::Bytes(blob) = s.as_ref()
                && let Ok(cap) = serde_json::from_slice::<CapOverride>(blob)
            {
                caps.push(cap);
            }
        }
    }
    (name, owner, caps)
}
/// Parse the complete batch and apply to a disposable candidate, never the owner.
pub fn candidate(
    before: Option<&AutoCommit>,
    changes: &[Vec<u8>],
) -> Result<AutoCommit, automerge::AutomergeError> {
    let parsed = changes
        .iter()
        .map(|bytes| automerge::Change::try_from(bytes.as_slice()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut candidate = before.cloned().unwrap_or_else(AutoCommit::new);
    candidate.reset_diff_cursor();
    candidate.apply_changes(parsed)?;
    Ok(candidate)
}
pub fn declared_room_mismatches(candidate: &AutoCommit, room: uuid::Uuid) -> bool {
    let declared = match candidate.get(ROOT, "room").ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(value) => uuid::Uuid::parse_str(value).ok(),
            _ => None,
        },
        _ => None,
    };
    declared.is_some_and(|declared| declared != room)
}
pub fn governance_change_allowed(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    local: bool,
    pinned: Option<&str>,
    source: &str,
) -> bool {
    governance_of(before) == governance_of(Some(after))
        || !local && pinned.is_none_or(|pinned| pinned == source)
}
/// Exact native membership capability projection; a certificate alone has no role.
pub fn role_of(owner: Option<&str>, caps: &[CapOverride], user: &str, now: u64) -> Option<Role> {
    if owner == Some(user) {
        return Some(Role::Owner);
    }
    let mut best: Option<Verb> = None;
    let mut blocked = false;
    for cap in caps {
        if !matches!(&cap.subject,Subject::Principal(Principal::Person(person)) if person==user)
            || !cap.is_live_at(Some(now))
            || !matches!(cap.scope, Scope::Room(_) | Scope::Workspace)
        {
            continue;
        }
        match cap.effect {
            Effect::Grant => {
                best = Some(match best {
                    Some(prev) if prev.implies(cap.verb) => prev,
                    _ => cap.verb,
                })
            }
            Effect::Restrict if !cap.verb.implies(Verb::Read) => blocked = true,
            Effect::Restrict => {}
        }
    }
    if blocked {
        return Some(Role::Denied);
    }
    best.map(|v| match v {
        Verb::Admin => Role::Owner,
        Verb::Write => Role::Editor,
        Verb::Comment => Role::Commenter,
        Verb::Read | Verb::Invite => Role::Viewer,
    })
}
/// Effect receipts are immutable by value; omission cannot erase old evidence.
pub fn immutable_receipts(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    let Some(before) = before else {
        return true;
    };
    let after = receipt_values(after);
    receipt_values(before)
        .iter()
        .all(|(key, value)| after.get(key).is_none_or(|new| new == value))
}

pub fn receipt_values(doc: &AutoCommit) -> BTreeMap<String, serde_json::Value> {
    let Some((_, obj)) = doc.get(ROOT, "effect_receipts_v1").ok().flatten() else {
        return BTreeMap::new();
    };
    doc.keys(&obj)
        .filter_map(|key| {
            let (Value::Scalar(value), _) = doc.get(&obj, &key).ok().flatten()? else {
                return None;
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                return None;
            };
            Some((key, serde_json::from_slice(bytes).ok()?))
        })
        .collect()
}
