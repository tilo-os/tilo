//! Shares a member gives: one record, or one item of it, shared with the
//! workspace ([`tilo_action_policy::share`]).
//!
//! A share is grants on the shared node, signed by the sharer's own device as
//! a membership operation and applied by every device's replay
//! ([`members_own`]): it gives only what the sharer holds, on one node, as
//! theirs; a window only from the device it runs on
//! ([`window_is_the_writers`]); and it ends only by its sharer or by one who
//! may revoke in the workspace.
use tilo_action_policy::share::{self, ShareAccess};
use tilo_action_policy::{
    Authority, Change, Context, Grant, GrantId, Subject, TreePath, WorkspacePolicy, roles,
    roles::governance,
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

/// Whether `device` (hex), sharing `node` as `share`, is the device the
/// window it names runs on: a window's tab id carries the salt of the device
/// that minted it, which hosts it ([`tilo_desktop_model::peer_salt_of`]).
/// Nobody shares another device's window, or says what it shows.
pub fn window_is_the_writers(device: &str, share: &str, node: &TreePath) -> Result<(), String> {
    use tilo_desktop_model::window_share::{window_of_node, window_of_share_id};
    let window = window_of_node(node).map(|(_, tab)| tab);
    let named = window_of_share_id(share);
    let tab = match (window, named) {
        (None, None) => return Ok(()),
        // The window's own node: under its share id, or as what the window
        // shows when that is its app's own record as a whole.
        (Some(tab), Some((named, _))) if named == tab => tab,
        // What a window shows: any one resource of the workspace.
        (None, Some((tab, true))) => tab,
        _ => return Err("a window is shared under its own share id".into()),
    };
    let salt = hex::decode(device)
        // ignored-failure: a device named by no hex key runs no window
        .ok()
        .map(|key| tilo_desktop_model::ids::device_salt_of_key(&key));
    if salt != Some(tilo_desktop_model::peer_salt_of(tab)) {
        return Err("only the device a window runs on can share it".into());
    }
    Ok(())
}

/// The grants `person`, writing from `device` (hex) on the workspace `doc`, gives
/// by sharing `node` with `access` as `share`, once each check a receiver
/// makes holds: they are neither banned nor restricted, the node is one
/// record or item of the workspace, a window is theirs to share, and they hold
/// all it gives.
pub fn share_grants(
    policy: &WorkspacePolicy,
    (person, device, authority, cx): (&str, &str, &Authority<'_>, &Context<'_>),
    share: &str,
    node: &TreePath,
    access: ShareAccess,
) -> Result<Vec<Grant>, String> {
    if policy.is_blocked(person) || policy.restricted_until(person, cx).is_some() {
        return Err("you can't share in this workspace right now".into());
    }
    let grants = share::share_grants(&policy.workspace, person, share, node, access)
        .map_err(|refusal| refusal.to_string())?;
    window_is_the_writers(device, share, node)?;
    share::check_held(authority, &grants, cx).map_err(|refusal| refusal.to_string())?;
    Ok(grants)
}

/// The share grants `person` may end of `share` in `policy`: those they gave,
/// or all of them when they may revoke in the workspace.
#[must_use]
pub fn ended_by(
    policy: &WorkspacePolicy,
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

/// The membership `change` leaves `policy` at when it is `person`'s by a
/// member's own rule, though the workspace's grants alone would not let them
/// make it: a share of what they hold ([`share_grants`]), the end of a share
/// they may end ([`ended_by`]), or a grant of a saved set they may assign
/// (`preset:<id>:<set>:<rule>`), as `presets` names its rules.
///
/// # Errors
/// The change is none of these, or breaks the rule it falls under.
pub(crate) fn members_own(
    policy: &WorkspacePolicy,
    presets: impl FnOnce() -> Vec<tilo_action_policy::Preset>,
    (person, device, authority, cx): (&str, &str, &Authority<'_>, &Context<'_>),
    change: &Change,
) -> Result<WorkspacePolicy, String> {
    let mut next = policy.clone();
    match change {
        Change::Grant(grant) => {
            if let Some(share) = share_of(&grant.id) {
                let access = [ShareAccess::View, ShareAccess::Edit]
                    .into_iter()
                    .find(|access| access.kinds() == grant.kinds)
                    .ok_or_else(|| {
                        format!("share grant {} gives no share's access", grant.id.as_str())
                    })?;
                let made = share_grants(
                    policy,
                    (person, device, authority, cx),
                    share,
                    &grant.node,
                    access,
                )?;
                if !made.contains(grant) {
                    return Err(format!(
                        "share grant {} is not the writer's share",
                        grant.id.as_str()
                    ));
                }
                // One share, one node.
                if policy
                    .grants
                    .iter()
                    .any(|held| share_of(&held.id) == Some(share) && held.node != grant.node)
                {
                    return Err(format!("share {share} names more than one node"));
                }
            } else if let Some((set, index)) = roles::preset_assigned(&grant.id) {
                assigned(&presets(), (set, index), (person, authority, cx), grant)?;
            } else {
                return Err(format!(
                    "grant {} is not the writer's to give",
                    grant.id.as_str()
                ));
            }
            next.grants.retain(|held| held.id != grant.id);
            next.grants.push(grant.clone());
        }
        Change::Revoke(id) => {
            let share = share_of(id)
                .ok_or_else(|| format!("grant {} is not the writer's to end", id.as_str()))?;
            if !ended_by(policy, (person, authority, cx), share).contains(id) {
                return Err(format!(
                    "share grant {} is not the writer's to end",
                    id.as_str()
                ));
            }
            next.grants.retain(|held| held.id != *id);
        }
        _ => return Err("not a change a member makes by their own rule".into()),
    }
    Ok(next)
}

/// Whether `grant`, rule `index` of the saved set `set`, is one `person` may
/// assign: the set is one of `presets`, they may assign it, the grant is that
/// rule on the workspace as theirs, and it gives no more than they hold.
fn assigned(
    presets: &[tilo_action_policy::Preset],
    (set, index): (&str, usize),
    (person, authority, cx): (&str, &Authority<'_>, &Context<'_>),
    grant: &Grant,
) -> Result<(), String> {
    let not_of_set = || format!("grant {} is not of the set it names", grant.id.as_str());
    let workspace = grant.node.workspace_id().ok_or_else(not_of_set)?;
    let preset = presets
        .iter()
        .find(|preset| preset.name.trim().to_lowercase() == set)
        .ok_or_else(not_of_set)?;
    if roles::assign_preset_verdict(authority, workspace, &preset.name, cx).answer
        != tilo_action_policy::Answer::Allow
    {
        return Err(format!("the set {set} is not the writer's to assign"));
    }
    let workspace_node = TreePath::workspace(workspace);
    let rule = preset.rules.get(index).ok_or_else(not_of_set)?;
    if grant.node != workspace_node.join(&rule.node)
        || grant.kinds != rule.kinds
        || grant.answer != rule.answer
        || grant.granted_by != Subject::Person(person.to_owned())
    {
        return Err(not_of_set());
    }
    WorkspacePolicy::check_within(authority, grant, cx).map_err(|refusal| refusal.to_string())
}
