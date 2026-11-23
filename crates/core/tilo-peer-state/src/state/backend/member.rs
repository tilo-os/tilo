//! One current-member algorithm over actual admitted workspace data.
use super::{PeerCore, Role, SystemTime};
use crate::state::DelegateInactive;
use crate::state::MembershipError;
use tilo_action_policy::MemberId;

/// The role `person` acts with in `workspace` under `policy`: none for a
/// stranger or a blocked member.
fn role_in(
    policy: &tilo_action_policy::WorkspacePolicy,
    workspace: &str,
    person: &str,
) -> Option<Role> {
    if policy.owner.as_deref() == Some(person) {
        return Some(Role::Owner);
    }
    let role = policy.members.get(person).copied()?;
    let node = tilo_action_policy::TreePath::workspace(workspace);
    let blocked = policy.grants.iter().any(|grant| {
        grant.grantee.person_id() == Some(person)
            && grant.node == node
            && grant.answer == tilo_action_policy::Answer::Deny
            && grant.kinds == tilo_action_policy::KindSet::ALL
    });
    (!blocked).then_some(role)
}
fn key_text(key: &[u8; 32]) -> String {
    hex::encode(key)
}
/// What its person having removed a device means to the question asked of it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Removal {
    /// The device is asked about now: removed, it holds no role.
    Refuses,
    /// A write of the device's history is asked about, one the cut of its
    /// removal keeps ([`crate::removals`]): it is decided as its person's,
    /// as it was when the workspace took it.
    IsHistory,
}
/// A current member's role, and the bound on it when its device is a
/// delegate.
fn current(
    peer: &PeerCore,
    source: &[u8; 32],
    now: SystemTime,
    removal: Removal,
) -> Result<(Role, Option<crate::state::ActingAs>), MembershipError> {
    let cert = match removal {
        Removal::Refuses => peer.certificate_typed(source, now),
        Removal::IsHistory => peer.certificate_held(source, now),
    }
    .map_err(MembershipError::from)?;
    let workspace = peer
        .workspace_doc
        .as_ref()
        .ok_or(MembershipError::RulesMissing)?;
    let policy = workspace.workspace_policy(&peer.workspace);
    let key = key_text(source);
    // Decoded once per document state, not on every lookup.
    let identity = workspace
        .identities()
        .get(key.as_str())
        .ok_or(MembershipError::Absent)?
        .as_ref()
        .map_err(|error| MembershipError::MalformedIdentity(error.clone()))?;
    if identity.device_cert != *cert {
        return Err(MembershipError::CertificateMismatch);
    }
    let mut revoked = false;
    for entry in &identity.revocations {
        if entry.user_public_key != cert.user_public_key {
            return Err(MembershipError::RevocationRoot);
        }
        // The historical temporary union only served is_revoked(source).
        // Verify EVERY row, in the same order, including duplicate targets and
        // rows after a matching target. No cloned revocation map is necessary.
        tilo_user_identity::verify_revocation_entry(entry).map_err(MembershipError::Revocation)?;
        revoked |= &entry.revoked_device_public_key == source;
    }
    if revoked && removal == Removal::Refuses {
        return Err(MembershipError::Revoked);
    }
    identity
        .delegate_acting(&peer.workspace, crate::state::now_ns(now))
        .map_err(MembershipError::Delegate)?;
    let person = key_text(&cert.user_public_key);
    let role =
        role_in(policy, &peer.workspace, person.as_str()).ok_or(MembershipError::NoMembership)?;
    // A delegate acts with its person's authority bounded by its own: an
    // agent by the agent preset. It does not govern the workspace, and never
    // rises above an Editor.
    let delegate = match identity.delegate.as_ref() {
        None => None,
        Some(row) => {
            // An active delegation names an agent: a retired bot's never verifies.
            let agent_id = row
                .delegation
                .delegate
                .agent_id()
                .ok_or(MembershipError::Delegate(DelegateInactive::Delegation))?;
            Some(crate::state::ActingAs {
                actor: tilo_action_policy::Actor::Member(MemberId::agent(agent_id.to_owned())),
                // Its person handed it the delegation its root-signed
                // certificate names.
                grants: tilo_action_policy::Acting::handed_agent(agent_id, person.as_str())
                    .handed_preset(&peer.workspace),
            })
        }
    };
    let role = if delegate.is_some() {
        tilo_action_policy::roles::agent_policy::role(role)
    } else {
        role
    };
    Ok((role, delegate))
}
/// Whether `cert`'s device is held to the foreign-device bound: a device
/// that is not the person's own, unless the replica was made on it or joined
/// through it (`origin`).
pub(in crate::state) fn foreign(cert: &tilo_user_identity::DeviceCert, origin: &[u8; 32]) -> bool {
    cert.capabilities.foreign && cert.device_public_key != *origin
}
/// The role `source`'s device acts with now, and the bound on it when it is
/// a delegate.
pub(in crate::state) fn role(
    peer: &PeerCore,
    source: &[u8; 32],
    now: SystemTime,
) -> Result<(Role, Option<crate::state::ActingAs>), MembershipError> {
    current(peer, source, now, Removal::Refuses)
}
/// [`role`] for a write of `source`'s history: the role its person holds,
/// whether or not they removed the device since. Asked only of a write the
/// cut of the removal keeps; it never says what the device may do now.
pub(in crate::state) fn role_in_history(
    peer: &PeerCore,
    source: &[u8; 32],
    now: SystemTime,
) -> Result<(Role, Option<crate::state::ActingAs>), MembershipError> {
    current(peer, source, now, Removal::IsHistory)
}
#[cfg(test)]
mod tests;
