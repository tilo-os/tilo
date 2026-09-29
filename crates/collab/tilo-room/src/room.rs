//! Room-policy leaves shared by the shell and certified browser peer.
pub(crate) mod delivery_policy;
pub(crate) mod key_hex;
pub mod media;
pub(crate) mod policy;
pub(crate) mod private_policy;
pub(crate) mod read;
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
    BTreeMap<String, Vec<Vec<u8>>>,
    media::MediaGovernanceValue,
);
#[cfg(test)]
thread_local! { pub(crate) static POLICY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub fn governance_of(doc: Option<&AutoCommit>) -> RoomGovernance {
    let Some(doc) = doc else {
        return (
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            None,
            std::collections::BTreeMap::new(),
            media::MediaGovernanceValue::Absent,
        );
    };
    let mut owner = None;
    let mut caps = BTreeMap::new();
    let mut peers = BTreeMap::new();
    let mut identities = BTreeMap::new();
    // The legacy public output is unqualified owned data. The same borrowed
    // traversal serves bounded destinations that pay for each output copy.
    let result = read::visit_governance_entries(&read::LegacyRead(doc), |entry| {
        match entry {
            read::GovernanceEntry::Owner(value) => owner = Some(value.to_owned()),
            read::GovernanceEntry::Capability { key, bytes } => {
                caps.insert(key.to_owned(), bytes.to_vec());
            }
            read::GovernanceEntry::Peer { key, name } => {
                peers.insert(key.to_owned(), name.to_owned());
            }
            read::GovernanceEntry::Identity { key, bytes } => {
                identities.insert(key.to_owned(), bytes.to_vec());
            }
        }
        Ok::<(), std::convert::Infallible>(())
    });
    // LegacyRead has no resource refusals and the visitor is infallible.
    debug_assert!(result.is_ok());
    let invitation_receipts = invitation_receipts(doc).unwrap_or_default();
    (
        owner,
        caps,
        peers,
        identities,
        crate::coverage::governed_bytes(doc),
        invitation_receipts,
        media::governed_value(Some(doc)),
    )
}

pub(crate) fn invitation_receipts(
    doc: &AutoCommit,
) -> Result<BTreeMap<String, Vec<Vec<u8>>>, String> {
    let ledgers = doc
        .get_all(ROOT, "invitation_receipts_v1")
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(value, object)| match value {
            Value::Object(automerge::ObjType::Map) => Ok(object),
            _ => Err("governed invitation receipt ledger is not a map".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut receipts: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for object in ledgers {
        for key in doc.keys(&object) {
            let values = doc
                .get_all(&object, &key)
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(value, _)| match value {
                    Value::Scalar(value) => match value.as_ref() {
                        ScalarValue::Bytes(bytes) => Ok(bytes.clone()),
                        _ => Err("governed invitation receipt is not bytes".to_string()),
                    },
                    Value::Object(_) => Err("governed invitation receipt is not bytes".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            receipts.entry(key).or_default().extend(values);
        }
    }
    Ok(receipts)
}
pub fn extract_governed_fields(
    candidate: &AutoCommit,
) -> (Option<String>, Option<String>, Vec<CapOverride>) {
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};
    #[cfg(test)]
    POLICY_READS.with(|count| count.set(count.get().saturating_add(1)));
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
/// Changes with absent dependencies remain queued, as with `apply_changes`.
///
/// # Errors
///
/// Returns an error if a change is malformed or refused by isolated admission.
pub fn candidate(before: Option<&AutoCommit>, changes: &[Vec<u8>]) -> Result<AutoCommit, String> {
    // Each entry is one complete change chunk; their concatenation is ordinary
    // storage, admitted like any other untrusted document mutation.
    let storage = changes.concat();
    let mut candidate = before.cloned().unwrap_or_else(AutoCommit::new);
    candidate.reset_diff_cursor();
    if storage.is_empty() {
        return Ok(candidate);
    }
    tilo_automerge_isolate::apply(
        &mut candidate,
        &storage,
        tilo_automerge_isolate::Dependencies::RetainOrphans,
    )
    .map_err(|e| e.to_string())?;
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
    (governance_of(before) == governance_of(Some(after))
        && before.and_then(crate::durability::governed_bytes)
            == crate::durability::governed_bytes(after)
        && crate::document_import::governed_bytes(before)
            == crate::document_import::governed_bytes(Some(after)))
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
        let matches_person = match &cap.subject {
            Subject::Principal(Principal::Person(person)) => person == user,
            Subject::User(key) => hex::encode(key.as_bytes()) == user,
            _ => false,
        };
        if !matches_person
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
                });
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
