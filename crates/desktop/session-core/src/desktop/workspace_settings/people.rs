//! The People tab's content: how many share the workspace, then one entry per person with their
//! role and what the viewer may offer them, in the viewer's words.

use super::{
    CapRole, Workspace, member_row_name, member_row_offers, members_authority_note, role_label,
};
use crate::desktop::i18n::{t, t_args};
use crate::desktop::people::PeopleNames;

/// One person of the list, in the viewer's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub user: String,
    pub name: String,
    pub you: bool,
    pub role: String,
    /// Their role, by the key a role change names it.
    pub role_key: String,
    pub additional_roles: Vec<String>,
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
    pub custom_roles: Vec<(String, String)>,
    pub manage_roles: bool,
    /// What an empty list says instead: its title and detail.
    pub empty: (String, String),
    pub note: Option<String>,
    /// The holders of the workspace, under their own heading.
    pub holders_header: String,
    pub holders: Vec<Holder>,
}

/// One holder of the workspace, in the viewer's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// What it goes by: its server's name, its site's name, or its device's
    /// label.
    pub name: String,
    /// What it can read of the workspace.
    pub can_read: String,
}

/// One holder of a workspace, as the viewer's device learns it from the
/// holder's own signed address: what it calls itself and what it can read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberHolder {
    /// Its device key, hex.
    pub device: String,
    pub name: String,
    /// Whether it holds the workspace's keys: it reads the workspace.
    pub reads: bool,
    /// Whether the machine under it is rented from a cloud provider.
    pub cloud: bool,
}

/// One member as the People tab reads them, from wherever the viewer's device learns of its
/// workspace's people: the shell's own policy, or a page's host answering for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub user: String,
    /// What the viewer calls them.
    pub name: String,
    pub role: CapRole,
    /// Whether the workspace is theirs: set when it was made, and no role it hands out.
    pub owns: bool,
    /// Whether the workspace's members made them an administrator.
    pub administrator: bool,
    /// How many permissions they hold beyond their role.
    pub grants: usize,
}

/// The members of `ws` in the tab's order, each named as `names` (the roster) names them.
#[must_use]
pub fn members(ws: &Workspace, names: &PeopleNames, viewer: &str) -> Vec<Member> {
    ws.policy
        .people()
        .into_iter()
        .map(|(user, role)| {
            let grants = ws.policy.extra_grants(&user);
            Member {
                name: member_row_name(
                    names,
                    ws,
                    viewer,
                    &tilo_action_policy::MemberId::person(user.clone()),
                ),
                owns: ws.is_owner(&user),
                administrator: ws.policy.is_administrator(&user),
                role,
                grants,
                user,
            }
        })
        .collect()
}

/// What the People tab shows of `ws` to `viewer` .
pub fn content(
    ws: &Workspace,
    names: &PeopleNames,
    viewer: &str,
    is_owner: bool,
    holders: &[MemberHolder],
) -> PeopleContent {
    let mut content = content_of(&members(ws, names, viewer), viewer, is_owner, holders);
    content.custom_roles.extend(
        ws.policy
            .custom_roles
            .iter()
            .map(|(id, name)| (id.clone(), name.clone())),
    );
    for person in &mut content.people {
        if let Some(roles) = ws.policy.assigned_roles.get(&person.user) {
            person.additional_roles.extend(roles.iter().cloned());
        }
    }
    content
}

/// What the People tab shows of `members`, in their order, to `viewer` .
#[must_use]
pub fn content_of(
    members: &[Member],
    viewer: &str,
    is_owner: bool,
    holders: &[MemberHolder],
) -> PeopleContent {
    // An administrator governs every member who is not one; only the owner governs them too.
    let administers = is_owner
        || members
            .iter()
            .any(|member| member.user == *viewer && member.administrator);
    let people = members
        .iter()
        .map(|member| {
            let (user, owns) = (&member.user, member.owns);
            let (offers_role, offers_remove) = member_row_offers(owns, user, viewer);
            Person {
                user: user.clone(),
                name: member.name.clone(),
                you: user == viewer,
                // An owner by role who does not own the workspace reads as Admin, and an Editor
                // the workspace made an administrator as one: everyone sees who governs here.
                role: if member.role == CapRole::Owner && !owns {
                    t("workspace-settings-role-admin")
                } else if !owns && member.administrator {
                    super::administrator_label()
                } else {
                    role_label(member.role)
                },
                role_key: member.role.key().to_owned(),
                additional_roles: Vec::new(),
                extra: (member.grants > 0).then(|| {
                    t_args(
                        "workspace-settings-member-extra",
                        &[("count", member.grants.into())],
                    )
                }),
                offers: match (offers_role, offers_remove) {
                    (true, true) => Offers::RoleAndRemove,
                    (true, false) => Offers::Role,
                    (false, _) => Offers::Nothing,
                },
                can_manage: !owns
                    && user != viewer
                    && (is_owner || administers && !member.administrator),
            }
        })
        .collect();
    PeopleContent {
        count: t_args(
            "workspace-settings-members-count",
            &[("count", members.len().into())],
        ),
        status: t("workspace-settings-members-help"),
        header: t("workspace-settings-people-header"),
        you_tag: t("workspace-settings-you"),
        remove: t("workspace-settings-remove"),
        people,
        custom_roles: Vec::new(),
        manage_roles: administers,
        empty: (
            t("workspace-settings-members-empty-title"),
            t("workspace-settings-members-empty-detail"),
        ),
        note: members_authority_note(administers),
        holders_header: t("workspace-settings-kept-header"),
        holders: holders
            .iter()
            .map(|holder| Holder {
                name: if holder.name.trim().is_empty() {
                    t("workspace-settings-kept-unnamed")
                } else {
                    holder.name.clone()
                },
                can_read: match (holder.reads, holder.cloud) {
                    (false, _) => t("workspace-settings-kept-stores"),
                    (true, true) => t("workspace-settings-kept-reads-cloud"),
                    (true, false) => t("workspace-settings-kept-reads"),
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(user: &str, role: CapRole, owns: bool, administrator: bool) -> Member {
        Member {
            user: user.to_owned(),
            name: user.to_owned(),
            role,
            owns,
            administrator,
            grants: 0,
        }
    }

    fn manages(content: &PeopleContent) -> Vec<(&str, bool)> {
        content
            .people
            .iter()
            .map(|person| (person.user.as_str(), person.can_manage))
            .collect()
    }

    #[test]
    fn each_holder_says_what_it_can_read_of_the_workspace() {
        let members = [member("owner", CapRole::Owner, true, false)];
        let holders = [
            MemberHolder {
                device: "nas".to_owned(),
                name: "Kitchen NAS".to_owned(),
                reads: true,
                cloud: false,
            },
            MemberHolder {
                device: "cloud".to_owned(),
                name: "tilo.sh".to_owned(),
                reads: true,
                cloud: true,
            },
            MemberHolder {
                device: "backup".to_owned(),
                name: "Backup box".to_owned(),
                reads: false,
                cloud: false,
            },
        ];
        let content = content_of(&members, &"owner".to_owned(), true, &holders);
        assert_eq!(content.holders_header, "Where it is kept");
        let said: Vec<(&str, &str)> = content
            .holders
            .iter()
            .map(|holder| (holder.name.as_str(), holder.can_read.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                ("Kitchen NAS", "Reads the workspace"),
                (
                    "tilo.sh",
                    "Reads the workspace, and so can the cloud provider"
                ),
                ("Backup box", "Keeps an encrypted copy and reads none of it"),
            ]
        );
    }

    #[test]
    fn who_may_change_whom_follows_the_workspace_rules() {
        let members = [
            member("owner", CapRole::Owner, true, false),
            member("admin", CapRole::Editor, false, true),
            member("eve", CapRole::Editor, false, false),
        ];
        // The owner governs everyone but themselves.
        let owner = content_of(&members, &"owner".to_owned(), true, &[]);
        assert_eq!(
            manages(&owner),
            [("owner", false), ("admin", true), ("eve", true)]
        );
        assert_eq!(owner.note, None);
        // An administrator governs every member who is not one, never the owner.
        let admin = content_of(&members, &"admin".to_owned(), false, &[]);
        assert_eq!(
            manages(&admin),
            [("owner", false), ("admin", false), ("eve", true)]
        );
        assert_eq!(admin.note, None);
        assert_eq!(
            admin.people[1].role,
            crate::desktop::workspace_settings::administrator_label()
        );
        // Anyone else governs nobody, and is told who does.
        let eve = content_of(&members, &"eve".to_owned(), false, &[]);
        assert!(manages(&eve).iter().all(|(_, may)| !may));
        assert!(eve.note.is_some());
        assert_eq!(eve.people[2].role_key, "editor");
    }
}
