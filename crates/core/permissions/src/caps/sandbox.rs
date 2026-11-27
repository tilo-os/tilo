//! A workspace's sandbox: what apps' code may reach outside their documents
//! (filesystem paths, file records, devices and network hosts granted through
//! portals), kept as the list of [`CapOverride`]s the workspace holds. Never
//! what a person grants an agent; that is the workspace's policy.

use tilo_action_policy::{PolicyEdit, Refusal, WorkspacePolicy, roles};
use tilo_member_types::MemberId;

use super::{CapOverride, Effect};

/// The live capabilities naming app `app_id` at `now`, Unix seconds.
#[must_use]
pub fn of_app_at<'a>(sandbox: &'a [CapOverride], app_id: &str, now: u64) -> Vec<&'a CapOverride> {
    let live_at = Some(now.saturating_mul(1000));
    sandbox
        .iter()
        .filter(|c| c.names_app(app_id) && c.effect == Effect::Grant && c.is_live_at(live_at))
        .collect()
}

/// Record `cap` in `sandbox`, minting an id if the caller didn't supply one.
/// A re-grant of the same (grantee, app, scope, effect, access) merges,
/// keeping the longer lifetime. Its id.
pub fn grant(sandbox: &mut Vec<CapOverride>, mut cap: CapOverride) -> String {
    if let Some(held) = sandbox.iter_mut().find(|c| {
        c.grantee == cap.grantee
            && c.app == cap.app
            && c.scope == cap.scope
            && c.effect == cap.effect
            && c.access == cap.access
    }) {
        held.lifetime = held.lifetime.max(cap.lifetime);
        if !cap.id.is_empty() {
            held.id = cap.id;
        }
        if cap.granted_by.is_some() {
            held.granted_by = cap.granted_by;
        }
        return held.id.clone();
    }
    if cap.id.is_empty() {
        // Random rather than drawn from a window id space: ids key replicated
        // maps, so they must not collide across devices.
        cap.id = format!("cap-{}", uuid::Uuid::new_v4().simple());
    }
    let id = cap.id.clone();
    sandbox.push(cap);
    id
}

/// Drop the capability with this id. Whether anything was removed.
pub fn revoke(sandbox: &mut Vec<CapOverride>, id: &str) -> bool {
    let before = sandbox.len();
    sandbox.retain(|c| c.id != id);
    sandbox.len() != before
}

/// The first instant after `now` (Unix seconds) at which one of the
/// capabilities stops being live. `None` while none expires later.
#[must_use]
pub fn next_expiry_after(sandbox: &[CapOverride], now: u64) -> Option<u64> {
    sandbox
        .iter()
        .filter_map(|cap| tilo_action_policy::until_secs(cap.lifetime))
        .filter(|deadline| *deadline > now)
        .min()
}

/// Record `cap` as `actor`, who must be allowed to grant in the workspace
/// `policy` decides at `now`.
///
/// # Errors
/// The actor may not grant.
pub fn grant_as_at(
    policy: &WorkspacePolicy,
    sandbox: &mut Vec<CapOverride>,
    actor: &str,
    cap: CapOverride,
    now: u64,
) -> Result<PolicyEdit, Refusal> {
    policy.require_at(actor, roles::governance::GRANT, now)?;
    let id = grant(sandbox, cap.granted_by(MemberId::person(actor.to_owned())));
    Ok(PolicyEdit {
        sandbox_granted: vec![id],
        ..PolicyEdit::default()
    })
}

/// Drop the capability `id` as `actor`, who must be allowed to revoke in the
/// workspace `policy` decides at `now`.
///
/// # Errors
/// The actor may not revoke.
pub fn revoke_as_at(
    policy: &WorkspacePolicy,
    sandbox: &mut Vec<CapOverride>,
    actor: &str,
    id: &str,
    now: u64,
) -> Result<PolicyEdit, Refusal> {
    policy.require_at(actor, roles::governance::REVOKE, now)?;
    Ok(PolicyEdit {
        sandbox_revoked: revoke(sandbox, id)
            .then(|| id.to_owned())
            .into_iter()
            .collect(),
        ..PolicyEdit::default()
    })
}
