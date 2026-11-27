//! What a member does by their own rule beside the workspace's grants: share a window only
//! from the device it runs on ([`window_is_the_writers`]), and assign a saved set of grants they
//! may assign ([`members_own`]).
use tilo_action_policy::{
    Authority, Change, Context, Grant, Grantee, TreePath, WorkspacePolicy, roles,
};

/// Whether `device` (hex), sharing `node`, is the device the window it names runs on: a
/// window's tab id carries the salt of the device that minted it, which hosts it
/// ([`tilo_desktop_model::peer_salt_of`]). Nobody shares another device's window; any other node
/// is no window's.
pub fn window_is_the_writers(device: &str, node: &TreePath) -> Result<(), String> {
    let Some((_, tab)) = tilo_desktop_model::window_share::window_of_node(node) else {
        return Ok(());
    };
    let salt = tilo_failure::ignored!(
        "a device named by no hex key runs no window",
        hex::decode(device)
            .ok()
            .map(|key| tilo_desktop_model::ids::device_salt_of_key(&key))
    );
    if salt != Some(tilo_desktop_model::peer_salt_of(tab)) {
        return Err("only the device a window runs on can share it".into());
    }
    Ok(())
}

/// The membership `change` leaves `policy` at when it is `person`'s by a
/// member's own rule, though the workspace's grants alone would not let them
/// make it: a grant of a saved set they may assign
/// (`preset:<id>:<set>:<rule>`), as `presets` names its rules.
///
/// # Errors
/// The change is none of these, or breaks the rule it falls under.
pub(crate) fn members_own(
    policy: &WorkspacePolicy,
    presets: impl FnOnce() -> Vec<tilo_action_policy::Preset>,
    (person, authority, cx): (&str, &Authority<'_>, &Context<'_>),
    change: &Change,
) -> Result<WorkspacePolicy, String> {
    let Change::Grant(grant) = change else {
        return Err("not a change a member makes by their own rule".into());
    };
    let Some((set, index)) = roles::preset_assigned(&grant.id) else {
        return Err(format!(
            "grant {} is not the writer's to give",
            grant.id.as_str()
        ));
    };
    assigned(&presets(), (set, index), (person, authority, cx), grant)?;
    let mut next = policy.clone();
    next.grants.retain(|kept| kept.id != grant.id);
    next.grants.push(grant.clone());
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
        || grant.granted_by != Grantee::person(person.to_owned())
    {
        return Err(not_of_set());
    }
    WorkspacePolicy::check_within(authority, grant, cx).map_err(|refusal| refusal.to_string())
}
