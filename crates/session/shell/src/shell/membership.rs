//! Workspace membership: the one implementation of changing a member's role
//! and removing a member.
//!
//! The `member.role.set` / `member.remove` shell actions and the Members tab
//! of workspace settings both call these functions, so a person clicking, a
//! script, an agent and a screen reader pass the same authority check and
//! produce the same policy-document writes. Two checks run, and neither
//! replaces the other: the shell action's gate (the room's policy at the
//! action's node, via `current_shell_action_authorized`) and the domain
//! function's own governance and owner rules in
//! [`tilo_desktop::workspace_settings`].

use crate::shell::chrome_state::ChromeState;
use crate::shell::search::action_api::ActionError;
use crate::shell::state::{CapRole, PolicyEdit, UserId, Workspace};

pub(crate) use tilo_action_broker::collaboration_admin::{REMOVE_ACTION, SET_ROLE_ACTION};

/// Why a membership edit did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MembershipError {
    /// There is no active workspace to edit.
    NoWorkspace,
    /// This machine does not govern the active workspace's room.
    NotGoverned,
    /// The local user may not invoke this membership action here.
    Unauthorized(&'static str),
    /// The participant is not a member of the active workspace.
    NotMember,
    /// The workspace's own rules refused the edit: the actor is not an
    /// administrator, the target is the recorded owner, or the actor
    /// tried to change their own membership.
    Refused,
}

impl std::fmt::Display for MembershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspace => f.write_str("there is no active workspace"),
            Self::NotGoverned => f.write_str("this device does not govern the active workspace"),
            Self::Unauthorized(action) => {
                write!(f, "current workspace authority does not permit {action}")
            }
            Self::NotMember => f.write_str("that participant is not a member of this workspace"),
            Self::Refused => f.write_str(
                "the workspace refused the change: only an administrator may change \
                 another member, and the owner's membership cannot be changed",
            ),
        }
    }
}

impl From<MembershipError> for ActionError {
    fn from(error: MembershipError) -> Self {
        Self::Refused(error.to_string())
    }
}

fn authorize(st: &ChromeState, action: &'static str) -> Result<(), MembershipError> {
    if crate::shell::search::providers::actions::current_shell_action_authorized(st, action) {
        Ok(())
    } else {
        Err(MembershipError::Unauthorized(action))
    }
}

/// `member`'s current role in the active workspace.
fn active_role_of(st: &ChromeState, member: &UserId) -> Result<CapRole, MembershipError> {
    let tree = st.tree();
    let workspace = tree
        .desktop
        .active_workspace()
        .ok_or(MembershipError::NoWorkspace)?;
    let role = workspace.role_of(member).ok_or(MembershipError::NotMember);
    drop(tree);
    role
}

/// Change `member`'s role in the active workspace. Setting the role a member
/// already holds is a successful no-op, so a retried invocation does not
/// rewrite their membership. Only an existing member's role can be set;
/// admitting someone new is an invitation.
///
/// # Errors
///
/// See [`MembershipError`].
pub(crate) fn set_member_role(
    st: &mut ChromeState,
    member: &UserId,
    role: CapRole,
) -> Result<(), MembershipError> {
    authorize(st, SET_ROLE_ACTION)?;
    if active_role_of(st, member)? == role {
        return Ok(());
    }
    edit_active_workspace_policy(st, |ws, actor| {
        tilo_desktop::workspace_settings::set_member_role(ws, actor, member, role)
    })
    .map(drop)
}

/// Remove `member` from the active workspace, revoking every grant
/// they hold there.
///
/// # Errors
///
/// See [`MembershipError`].
pub(crate) fn remove_member(st: &mut ChromeState, member: &UserId) -> Result<(), MembershipError> {
    authorize(st, REMOVE_ACTION)?;
    active_role_of(st, member)?;
    edit_active_workspace_policy(st, |ws, actor| {
        tilo_desktop::workspace_settings::remove_member(ws, actor, member)
    })
    .map(drop)
}

/// Apply a permission edit to the active workspace, then mirror it into the
/// replicated doc and flush. Without the mirror an edit would change only
/// the in-memory cache and the next boot would read the old answer back off
/// disk. `edit` runs with the local user as actor and does its own authority
/// check; an empty [`PolicyEdit`] is reported as [`MembershipError::Refused`].
///
/// # Errors
///
/// [`MembershipError::NoWorkspace`], [`MembershipError::NotGoverned`] or
/// [`MembershipError::Refused`].
pub(crate) fn edit_active_workspace_policy<F>(
    st: &mut ChromeState,
    edit: F,
) -> Result<PolicyEdit, MembershipError>
where
    F: FnOnce(&mut Workspace, &UserId) -> PolicyEdit,
{
    let actor = st.active_local_user();
    let mut tree = st.tree_mut();
    let (ws_id, room) = tree
        .desktop
        .active_workspace()
        .map(|workspace| (workspace.id, workspace.room))
        .ok_or(MembershipError::NoWorkspace)?;
    if !tree.governs_room(room) {
        return Err(MembershipError::NotGoverned);
    }
    let ws = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == ws_id)
        .ok_or(MembershipError::NoWorkspace)?;
    let changed = edit(ws, &actor);
    if changed.is_empty() {
        return Err(MembershipError::Refused);
    }
    tree.doc_write_policy(ws_id, &changed);
    tree.queue_save();
    drop(tree);
    st.bump();
    Ok(changed)
}
