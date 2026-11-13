//! The membership a workspace kept before this layout, brought up into it.
//!
//! The 0.1 releases kept a workspace's membership operations under
//! [`EARLIER_KEY`] and projected the membership they gave into rows of the
//! document: each member's role (`members`), when each joined
//! (`members_joined_v1`) and the workspace's grants (`grants`), each a JSON
//! value by person or grant id. A person who came in through an invitation was
//! in those rows and in no operation: those releases read an admission from
//! its redemption receipt. This layout reads a workspace's membership from its
//! operations under [`super::MEMBERS_KEY`] alone, and neither the earlier
//! operations nor the rows, so read as it is, a workspace those releases kept
//! holds its owner and nobody else.
//!
//! The device that governs the workspace brings it up once
//! ([`crate::StatePeer::carry_earlier_membership`]): what the rows held is
//! written as operations of its own, the workspace owner's device's, one adding
//! each member at their role at the time they joined, oldest first, then one
//! with the grants the workspace held that the roles do not already give. The
//! rows are the governing device's own projection of the earlier operations,
//! kept in its own durable copy, which is what the workspace was. A replay of
//! the new log must give the same people at the same roles, or the bring-up is
//! refused and the workspace is left as it was.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::{Change, Grant, Role, WorkspacePolicy};
use tilo_user_identity::DeviceCert;

use super::op::{MembershipOpCommand, SignedMembershipOp};

/// The prefix of the document keys the earlier layout kept its operations
/// under.
pub const EARLIER_KEY: &str = "members_v2";
/// The rows the earlier layout projected the membership into.
const MEMBERS_ROWS: &str = "members";
const JOINED_ROWS: &str = "members_joined_v1";
const GRANTS_ROWS: &str = "grants";

/// Whether `key` is one of the earlier layout's operations.
fn earlier_op(key: &str) -> bool {
    key.strip_prefix(EARLIER_KEY)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `doc` holds an earlier membership log and none of this layout:
/// what [`membership`] brings up.
#[must_use]
pub fn holds_earlier_log(doc: &AutoCommit) -> bool {
    let mut earlier = false;
    for key in doc.keys(ROOT) {
        if super::op::keyed(&key).is_some() {
            return false;
        }
        earlier |= earlier_op(&key);
    }
    earlier
}

/// The document keys the earlier layout's membership is kept under, which
/// nothing reads once it is brought up: its operations and its rows.
#[must_use]
pub fn earlier_keys(doc: &AutoCommit) -> Vec<String> {
    doc.keys(ROOT)
        .filter(|key| {
            earlier_op(key) || [MEMBERS_ROWS, JOINED_ROWS, GRANTS_ROWS].contains(&key.as_str())
        })
        .collect()
}

/// A grant as the earlier layout wrote it, as this layout writes it: those
/// releases named a person, agent or bot a grant is for (or given by) on its
/// own (`{"person": ..}`), and this layout names each a member
/// (`{"member": {"person": ..}}`).
fn earlier_grant(mut grant: serde_json::Value) -> serde_json::Value {
    for field in ["subject", "granted_by"] {
        let Some(grantee) = grant.get_mut(field) else {
            continue;
        };
        let names_a_member = grantee.as_object().is_some_and(|named| {
            named.len() == 1
                && named
                    .keys()
                    .all(|kind| ["person", "agent", "bot"].contains(&kind.as_str()))
        });
        if names_a_member {
            *grantee = serde_json::json!({ "member": grantee.take() });
        }
    }
    grant
}

/// The JSON values of the row map at `key`, each read as `T` once `earlier`
/// gave it this layout's form, by name. A row that does not read is left out,
/// as the earlier layout's own reader left it out, and named in the log.
fn rows<T: serde::de::DeserializeOwned>(
    doc: &AutoCommit,
    key: &str,
    earlier: fn(serde_json::Value) -> serde_json::Value,
) -> BTreeMap<String, T> {
    let mut read = BTreeMap::new();
    let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(ROOT, key) else {
        return read;
    };
    for name in doc.keys(&map) {
        let value = match doc.get(&map, name.as_str()) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => serde_json::from_slice(bytes)
                    .and_then(|value| serde_json::from_value(earlier(value)))
                    .map_err(|error| error.to_string()),
                other => Err(format!("a {other:?}, not JSON bytes")),
            },
            other => Err(format!("{other:?}, not a value")),
        };
        match value {
            Ok(value) => {
                read.insert(name, value);
            }
            Err(error) => tracing::warn!(
                row = key,
                %name,
                %error,
                "a membership row an earlier release kept does not read and is left out"
            ),
        }
    }
    read
}

/// The membership `doc`'s earlier layout held for `workspace`: its owner as
/// this layout reads it, and the members, join times and grants its rows held.
/// `None` when the document holds no earlier log, or one this layout's log
/// already took over.
#[must_use]
pub fn membership(doc: &AutoCommit, workspace: &str) -> Option<WorkspacePolicy> {
    if !holds_earlier_log(doc) {
        return None;
    }
    let mut policy = super::replay::start(doc, workspace);
    let owner = policy.owner.clone();
    policy.members = rows::<Role>(doc, MEMBERS_ROWS, std::convert::identity);
    policy
        .members
        .retain(|person, _| owner.as_ref() != Some(person));
    policy.joined = rows::<u64>(doc, JOINED_ROWS, std::convert::identity);
    let grants: Vec<Grant> = rows::<Grant>(doc, GRANTS_ROWS, earlier_grant)
        .into_values()
        .collect();
    if !grants.is_empty() {
        policy.grants = grants;
    }
    super::replay::order(&mut policy);
    Some(policy)
}

/// This layout's operations that found `earlier` in `doc`, which holds none
/// yet, written by `device`, the workspace owner's, with `key`: one adding each
/// member at their role when they joined, oldest first, then one with every
/// grant the workspace held that the members' roles do not already give it.
/// How many it wrote.
///
/// # Errors
///
/// An operation cannot be signed or written.
pub fn found(
    doc: &mut AutoCommit,
    workspace: &str,
    earlier: &WorkspacePolicy,
    (device, key): (&DeviceCert, &dyn DeviceSigner),
    now_ns: u64,
) -> Result<usize, String> {
    let mut written = 0_usize;
    let mut parents = Vec::new();
    let mut write = |doc: &mut AutoCommit, at_ns: u64, changes: Vec<Change>| {
        let op = SignedMembershipOp::signed(
            device,
            key,
            std::mem::take(&mut parents),
            at_ns,
            MembershipOpCommand::Project { changes },
        )?;
        parents = vec![super::put_op(doc, &op)?];
        written = written.saturating_add(1);
        Ok::<_, String>(())
    };
    let mut people: Vec<(&String, &Role)> = earlier.members.iter().collect();
    people.sort_by_key(|(person, _)| (earlier.joined.get(*person).copied(), (*person).clone()));
    for (person, role) in people {
        let at_ns = earlier
            .joined
            .get(person)
            .map_or(now_ns, |ms| ms.saturating_mul(1_000_000));
        write(
            doc,
            at_ns,
            vec![Change::AddMember {
                person: person.clone(),
                role: *role,
            }],
        )?;
    }
    let held = super::replay(doc, workspace);
    let grants: Vec<Change> = earlier
        .grants
        .iter()
        .filter(|grant| !held.grants.contains(grant))
        .cloned()
        .map(Change::Grant)
        .collect();
    if !grants.is_empty() {
        write(doc, now_ns, grants)?;
    }
    Ok(written)
}

/// The people `policy` holds at their roles, for comparing two memberships.
#[must_use]
pub fn roles(policy: &WorkspacePolicy) -> BTreeMap<String, Option<Role>> {
    policy
        .members
        .keys()
        .map(|person| (person.clone(), policy.active_role(person)))
        .collect()
}
