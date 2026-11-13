//! One current-member algorithm over actual admitted workspace data.
use super::{GovernedIdentity, PeerCore, Role, SystemTime};
use crate::state::MembershipError;
use std::collections::BTreeMap;
use tilo_action_policy::{Bound, Grant, Subject};
use tilo_workspace_join::delegation::Delegate;

trait Policy {
    fn owner(&self) -> Option<&str>;
    fn members(&self) -> &BTreeMap<String, Role>;
    fn grants(&self) -> &[Grant];
    fn identity(&self) -> &GovernedIdentity;
    /// The role `person` acts with in `workspace`: none for a stranger or a
    /// blocked member.
    fn role(&self, workspace: &str, person: &str) -> Option<Role> {
        if self.owner() == Some(person) {
            return Some(Role::Owner);
        }
        let role = self.members().get(person).copied()?;
        let subject = Subject::Person(person.to_owned());
        let node = tilo_action_policy::TreePath::workspace(workspace);
        let blocked = self.grants().iter().any(|grant| {
            grant.subject == subject
                && grant.node == node
                && grant.answer == tilo_action_policy::Answer::Deny
                && grant.kinds == tilo_action_policy::KindSet::ALL
        });
        (!blocked).then_some(role)
    }
}
fn key_text(key: &[u8; 32]) -> String {
    hex::encode(key)
}
/// A current member's role, and the bound on it when its device is a
/// delegate.
fn current<'peer, P: Policy>(
    peer: &'peer PeerCore,
    source: &[u8; 32],
    now: SystemTime,
    collect: impl FnOnce(&'peer crate::cached_document::CachedDocument) -> Result<P, MembershipError>,
) -> Result<(Role, Option<Bound>), MembershipError> {
    let cert = peer
        .certificate_typed(source, now)
        .map_err(MembershipError::from)?;
    let workspace = peer
        .workspace_doc
        .as_ref()
        .ok_or(MembershipError::GovernanceMissing)?;
    let policy = collect(workspace)?;
    let identity = policy.identity();
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
    if revoked {
        return Err(MembershipError::Revoked);
    }
    identity
        .delegate_acting(&peer.workspace, crate::state::now_ns(now))
        .map_err(MembershipError::Delegate)?;
    let person = key_text(&cert.user_public_key);
    let role = policy
        .role(&peer.workspace, person.as_str())
        .ok_or(MembershipError::NoMembership)?;
    // A delegate acts with its person's authority bounded by its own: an
    // agent by the agent preset, a bot by its derived grant in the workspace's
    // policy. Neither governs the workspace, and neither rises above an Editor.
    let delegate = identity
        .delegate
        .as_ref()
        .map(|row| match &row.delegation.delegate {
            Delegate::Agent { agent_id } => tilo_action_policy::roles::agent_turn(
                &peer.workspace,
                agent_id,
                person.as_str(),
                &[],
            ),
            Delegate::Bot { installation } => tilo_action_policy::bot::bound(installation),
        });
    let role = if delegate.is_some() {
        crate::state::delegates::agent_role(role)
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
struct LegacyPolicy<'a> {
    data: &'a tilo_action_policy::WorkspacePolicy,
    identity: &'a GovernedIdentity,
}
impl Policy for LegacyPolicy<'_> {
    fn owner(&self) -> Option<&str> {
        self.data.owner.as_deref()
    }
    fn members(&self) -> &BTreeMap<String, Role> {
        &self.data.members
    }
    fn grants(&self) -> &[Grant] {
        &self.data.grants
    }
    fn identity(&self) -> &GovernedIdentity {
        self.identity
    }
}
pub(in crate::state) fn legacy(
    peer: &PeerCore,
    source: &[u8; 32],
    now: SystemTime,
) -> Result<(Role, Option<Bound>), MembershipError> {
    current(peer, source, now, |workspace| {
        let data = workspace.workspace_policy(&peer.workspace);
        let key = key_text(source);
        // Decoded once per document state, not on every lookup.
        let identity = workspace
            .identities()
            .get(key.as_str())
            .ok_or(MembershipError::Absent)?
            .as_ref()
            .map_err(|error| MembershipError::MalformedIdentity(error.clone()))?;
        Ok(LegacyPolicy { data, identity })
    })
}
#[cfg(test)]
mod tests;
