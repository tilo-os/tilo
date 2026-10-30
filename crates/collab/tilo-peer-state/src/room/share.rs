//! Shares a member gives: one record, or one item of it, shared with the
//! room ([`tilo_action_policy::share`]).
//!
//! A share is grants on the shared node, written by the sharer's own device
//! and checked by every receiver like any membership change
//! ([`super::membership`]): it gives only what the sharer holds, on one
//! node, as theirs; a window only from the device it runs on
//! ([`window_is_the_writers`]); and it ends only by its sharer or by one
//! who may revoke in the room.

use std::collections::BTreeMap;

use automerge::AutoCommit;
use tilo_action_policy::share::{self, ShareAccess};
use tilo_action_policy::{
    Authority, Context, Grant, GrantId, RoomPolicy, Subject, TreePath, roles::governance,
};

/// The start of every share grant's id.
const SHARE_PREFIX: &str = "share:";

/// The share `grant` is one of, when it is a share's: its id is
/// `share:<share>:<role>`.
#[must_use]
pub fn share_of(grant: &GrantId) -> Option<&str> {
    grant
        .as_str()
        .strip_prefix(SHARE_PREFIX)
        .and_then(|rest| rest.rsplit_once(':'))
        .map(|(share, _)| share)
}

/// Whether `device` (hex) may share `node` of the room `room` holds as the
/// share `share`, as far as windows go ([`tilo_desktop_model::window_share`]):
/// a window is shared only under its own share id, what a Tilo app's window
/// shows only under that window's, and either only by the device that
/// minted the window's tab, which hosts it
/// ([`tilo_desktop_model::window_share::device_salts`]). Nobody shares
/// another device's window, or says what it shows.
///
/// # Errors
/// Why the device may not share it.
pub fn window_is_the_writers(
    room: Option<&AutoCommit>,
    device: &str,
    share: &str,
    node: &TreePath,
) -> Result<(), String> {
    use tilo_desktop_model::window_share::{window_of_node, window_of_share_id};
    let window = window_of_node(node).map(|(_, tab)| tab);
    let named = window_of_share_id(share);
    let tab = match (window, named) {
        (None, None) => return Ok(()),
        // The window's own node: under its share id, or as what the window
        // shows when that is its app's own record as a whole.
        (Some(tab), Some((named, _))) if named == tab => tab,
        // What a window shows: any one resource of the room.
        (None, Some((tab, true))) => tab,
        _ => return Err("a window is shared under its own share id".into()),
    };
    let host = room
        .map(tilo_desktop_model::window_share::device_salts)
        .and_then(|owners| owners.get(&tilo_desktop_model::peer_salt_of(tab)).cloned());
    if host.as_deref() != Some(device) {
        return Err("only the device a window runs on can share it".into());
    }
    Ok(())
}

/// The grants `person`, writing from `device` (hex) on the room `doc`, gives
/// by sharing `node` with `access` as `share`, once each check a receiver
/// makes holds: they are neither banned nor restricted, the node is one
/// record or item of the room, a window is theirs to share, and they hold
/// all it gives.
///
/// # Errors
/// The first of those that does not hold.
pub fn share_grants(
    (doc, policy): (Option<&AutoCommit>, &RoomPolicy),
    (person, device, authority, cx): (&str, &str, &Authority<'_>, &Context<'_>),
    share: &str,
    node: &TreePath,
    access: ShareAccess,
) -> Result<Vec<Grant>, String> {
    if policy.is_blocked(person) || policy.restricted_until(person, cx).is_some() {
        return Err("you can't share in this room right now".into());
    }
    let grants = share::share_grants(&policy.room, person, share, node, access)
        .map_err(|refusal| refusal.to_string())?;
    window_is_the_writers(doc, device, share, node)?;
    share::check_held(authority, &grants, cx).map_err(|refusal| refusal.to_string())?;
    Ok(grants)
}

/// The share grants `person` may end of `share` in `policy`: those they gave,
/// or all of them when they may revoke in the room.
#[must_use]
pub fn ended_by(
    policy: &RoomPolicy,
    (person, authority, cx): (&str, &Authority<'_>, &Context<'_>),
    share: &str,
) -> Vec<GrantId> {
    let revokes = policy
        .check_action(authority, governance::REVOKE, cx, false)
        .is_ok();
    let sharer = Subject::Person(person.to_owned());
    policy
        .grants
        .iter()
        .filter(|grant| share::is_of_share(grant, share))
        .filter(|grant| revokes || grant.granted_by == sharer)
        .map(|grant| grant.id.clone())
        .collect()
}

/// The share grants a member's batch gives and ends, from `held` to
/// `wanted` on the room `at`, each checked as [`share_grants`] and
/// [`ended_by`] would make it for `person` writing from `device`.
///
/// # Errors
/// A share grant the writer could not have given or ended.
pub(super) fn changed(
    (at, held, wanted): (&AutoCommit, &RoomPolicy, &RoomPolicy),
    (person, device, authority, cx): (&str, &str, &Authority<'_>, &Context<'_>),
) -> Result<(Vec<Grant>, Vec<GrantId>), String> {
    let held_shares: BTreeMap<&GrantId, &Grant> = held
        .grants
        .iter()
        .filter(|grant| share_of(&grant.id).is_some())
        .map(|grant| (&grant.id, grant))
        .collect();
    let mut given = Vec::new();
    for grant in &wanted.grants {
        let Some(share) = share_of(&grant.id) else {
            continue;
        };
        if held_shares.get(&grant.id) == Some(&grant) {
            continue;
        }
        let made = [ShareAccess::View, ShareAccess::Edit]
            .into_iter()
            .find(|access| access.kinds() == grant.kinds)
            .ok_or_else(|| format!("share grant {} gives no share's access", grant.id.as_str()))
            .and_then(|access| {
                share_grants(
                    (Some(at), held),
                    (person, device, authority, cx),
                    share,
                    &grant.node,
                    access,
                )
            })?;
        if !made.contains(grant) {
            return Err(format!(
                "share grant {} is not the writer's share",
                grant.id.as_str()
            ));
        }
        // One share, one node.
        if wanted
            .grants
            .iter()
            .any(|other| share_of(&other.id) == Some(share) && other.node != grant.node)
        {
            return Err(format!("share {share} names more than one node"));
        }
        given.push(grant.clone());
    }
    let mut ended = Vec::new();
    for (id, grant) in held_shares {
        if wanted.grants.iter().any(|kept| kept.id == *id) {
            continue;
        }
        let share = share_of(id).unwrap_or_default();
        if !ended_by(held, (person, authority, cx), share).contains(id) {
            return Err(format!(
                "share grant {} is not the writer's to end",
                grant.id.as_str()
            ));
        }
        ended.push(id.clone());
    }
    Ok((given, ended))
}
