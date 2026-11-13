//! The People tab's content: how many share the workspace, then one entry per person with their
//! role and what the viewer may offer them, in the viewer's words.

use super::{
    CapRole, UserId, Workspace, member_row_name, member_row_offers, members_authority_note,
    ordered_roster, role_label,
};
use crate::collaborators::PeopleNames;
use crate::i18n::{t, t_args};

/// One person of the list, in the viewer's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub user: UserId,
    pub name: String,
    pub you: bool,
    pub role: String,
    /// Their extra permissions, only when they have any.
    pub extra: Option<String>,
    pub offers: Offers,
    /// Whether the viewer may change their role and remove them.
    pub can_manage: bool,
}

/// What a person's row offers the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offers {
    /// Nothing: the owner's role is a fact of the row, not a choice.
    Nothing,
    /// Their role, but not Remove: no one removes themselves here.
    Role,
    RoleAndRemove,
}

/// Everything the People tab shows, resolved to the person's words. The page is rebuilt only when
/// this changes.
#[derive(Clone, Debug, PartialEq)]
pub struct PeopleContent {
    pub count: String,
    /// What the tab says under the count: what removing someone does.
    pub status: String,
    pub header: String,
    pub you_tag: String,
    pub remove: String,
    pub people: Vec<Person>,
    /// What an empty list says instead: its title and detail.
    pub empty: (String, String),
    pub note: Option<String>,
}

/// What the People tab shows of `ws` to `viewer` .
pub fn content(
    ws: &Workspace,
    names: &PeopleNames,
    viewer: &UserId,
    is_owner: bool,
) -> PeopleContent {
    let roster = ordered_roster(ws);
    let people = roster
        .iter()
        .map(|(user, role)| {
            let owns = ws.is_owner(user);
            let subject = tilo_action_policy::Grantee::person(user.clone());
            let grants = ws
                .policy
                .grants
                .iter()
                .filter(|grant| grant.subject == subject)
                .count();
            let (offers_role, offers_remove) = member_row_offers(owns, user, viewer);
            Person {
                user: user.clone(),
                name: member_row_name(names, ws, viewer, user),
                you: user == viewer,
                // An owner by role who does not own the workspace reads as Admin.
                role: if *role == CapRole::Owner && !owns {
                    t("workspace-settings-role-admin")
                } else {
                    role_label(*role)
                },
                extra: (grants > 0).then(|| {
                    t_args(
                        "workspace-settings-member-extra",
                        &[("count", grants.into())],
                    )
                }),
                offers: match (offers_role, offers_remove) {
                    (true, true) => Offers::RoleAndRemove,
                    (true, false) => Offers::Role,
                    (false, _) => Offers::Nothing,
                },
                can_manage: is_owner && !owns && user != viewer,
            }
        })
        .collect();
    PeopleContent {
        count: t_args(
            "workspace-settings-members-count",
            &[("count", roster.len().into())],
        ),
        status: t("workspace-settings-members-help"),
        header: t("workspace-settings-people-header"),
        you_tag: t("workspace-settings-you"),
        remove: t("workspace-settings-remove"),
        people,
        empty: (
            t("workspace-settings-members-empty-title"),
            t("workspace-settings-members-empty-detail"),
        ),
        note: members_authority_note(is_owner),
    }
}
