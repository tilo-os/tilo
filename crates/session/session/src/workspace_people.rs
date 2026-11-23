//! The People tab of the workspace settings on every host: who shares the workspace, their roles,
//! and what the viewer may change of them. What the members are is the host's to say — the shell
//! reads its workspace's policy, a page asks the workspace's host — and the tab is built here, once,
//! from them.

use tilo_action_policy::{Grant, Lifetime, Role};
use tilo_desktop::workspace_settings::{self as domain, people::PeopleContent};
use tilo_desktop_surface::i18n::t;
use tilo_sheets::workspace_settings as sheet;

use crate::workspace_settings::Directory;

/// The People tab as `content` (the workspace's members, in the viewer's words) shows it.
#[must_use]
pub fn tab(content: PeopleContent) -> sheet::People {
    sheet::People {
        count: content.count,
        status: content.status,
        header: content.header,
        you_tag: content.you_tag,
        remove: content.remove,
        people: content
            .people
            .into_iter()
            .map(|entry| sheet::Person {
                key: entry.user,
                name: entry.name,
                you: entry.you,
                role: entry.role,
                role_key: Some(entry.role_key),
                additional_roles: entry.additional_roles,
                extra: entry.extra,
                offers: match entry.offers {
                    domain::people::Offers::Nothing => sheet::Offers::Nothing,
                    domain::people::Offers::Role => sheet::Offers::Role,
                    domain::people::Offers::RoleAndRemove => sheet::Offers::RoleAndRemove,
                },
                can_manage: entry.can_manage,
            })
            .collect(),
        roles: Role::ALL
            .into_iter()
            .filter(|role| Role::Editor.includes(*role))
            .map(|role| sheet::RoleChoice {
                key: role.key().to_owned(),
                label: domain::role_label(role),
                allows: domain::role_capability_preview(role),
            })
            .collect(),
        custom_roles: content
            .custom_roles
            .into_iter()
            .map(|(key, label)| sheet::RoleChoice {
                key,
                label,
                allows: String::new(),
            })
            .collect(),
        role_editor: sheet::RoleEditor {
            manage: content.manage_roles,
            title: t("workspace-settings-custom-roles"),
            create: t("workspace-settings-create-role"),
            name: t("workspace-settings-role-name"),
            rename: t("workspace-settings-rename-role"),
            save: t("workspace-settings-save-role"),
            remove_decision: sheet::Decision {
                title: t("workspace-settings-delete-role"),
                description: t("workspace-settings-delete-role-detail"),
                keep: t("workspace-settings-keep-role"),
                review: String::new(),
                confirm: t("workspace-settings-delete-role-confirm"),
            },
        },
        empty: content.empty,
        note: content.note,
        holders_header: content.holders_header,
        holders: content
            .holders
            .into_iter()
            .map(|holder| sheet::Holder {
                name: holder.name,
                can_read: holder.can_read,
            })
            .collect(),
        delegated_header: content.delegated_header,
        delegated: content
            .delegated
            .into_iter()
            .map(|delegation| sheet::Delegation {
                key: delegation.key,
                server: delegation.server,
                who: delegation.who,
                you: delegation.you,
                detail: delegation.detail,
                until: delegation.until,
                revocable: delegation.revocable,
            })
            .collect(),
        revoke: t("workspace-settings-delegated-revoke"),
        remove_decision: sheet::Decision {
            title: t("workspace-settings-remove-sheet-title"),
            description: t("workspace-settings-remove-sheet-description"),
            keep: t("workspace-settings-keep-access"),
            review: t("workspace-settings-review-activity"),
            confirm: t("workspace-settings-remove-person"),
        },
        revoke_decision: sheet::Decision {
            title: t("workspace-settings-delegated-revoke-sheet-title"),
            description: t("workspace-settings-delegated-revoke-sheet-description"),
            keep: t("workspace-settings-delegated-keep"),
            review: t("workspace-settings-review-activity"),
            confirm: t("workspace-settings-delegated-revoke"),
        },
    }
}

/// The work the workspace's members gave servers that a page read, in the
/// tab's terms: who gave which work to what, and until when.
#[must_use]
pub fn delegations(
    delegations: &[tilo_server_protocol::workspaces::WorkspaceDelegation],
) -> Vec<domain::people::MemberDelegation> {
    delegations
        .iter()
        .map(|delegation| {
            row(
                delegation.person.clone(),
                delegation.server.clone(),
                delegation.agent.clone(),
                &delegation.grants,
                delegation.expires_at_ns,
                false,
            )
        })
        .collect()
}

/// The work the members of the workspace this device holds gave servers, in
/// the tab's terms: who gave which work to what, until when, and which this
/// device signed, so may revoke.
#[must_use]
pub fn held_delegations(
    delegations: &[tilo_session_client::engine::Delegation],
) -> Vec<domain::people::MemberDelegation> {
    delegations
        .iter()
        .map(|delegation| {
            row(
                delegation.person.clone(),
                delegation.server.clone(),
                delegation.agent.clone(),
                &delegation.grants,
                delegation.expires_at_ns,
                delegation.revocable,
            )
        })
        .collect()
}

/// One delegation in the tab's terms, its work said as the Permissions tab
/// says what a grant allows and its end as the tab says how long a grant
/// lasts: what a page and the session's own sheet say alike.
fn row(
    person: String,
    server: String,
    agent: Option<String>,
    grants: &[Grant],
    expires_at_ns: u64,
    revocable: bool,
) -> domain::people::MemberDelegation {
    domain::people::MemberDelegation {
        person,
        server,
        agent,
        work: domain::grant_set_sentence(grants, &Directory::default()),
        until: ui::format::lifetime(Lifetime::Until(expires_at_ns / 1_000_000)),
        revocable,
    }
}

/// The holders a workspace's keeper answered for, in the People tab's terms.
#[must_use]
pub fn holders(
    holders: &[tilo_server_protocol::workspaces::WorkspaceHolder],
) -> Vec<domain::people::MemberHolder> {
    holders
        .iter()
        .map(|holder| domain::people::MemberHolder {
            device: holder.device.clone(),
            name: holder.label.clone(),
            reads: holder.reads,
            cloud: holder.cloud,
        })
        .collect()
}

/// One tab's name in the strip, and the shorter one a narrow sheet uses.
#[must_use]
pub fn tab_label(tab: sheet::Tab) -> sheet::TabLabel {
    let (label, short) = match tab {
        sheet::Tab::General => (
            "workspace-settings-tab-general",
            "workspace-settings-tab-general",
        ),
        sheet::Tab::People => (
            "workspace-settings-tab-people",
            "workspace-settings-tab-people",
        ),
        sheet::Tab::Agents => (
            "workspace-settings-tab-agents",
            "workspace-settings-tab-agents",
        ),
        sheet::Tab::Permissions => (
            "workspace-settings-tab-privacy-security",
            "workspace-settings-tab-privacy",
        ),
        sheet::Tab::Apps => (
            "workspace-settings-tab-apps-files",
            "workspace-settings-tab-apps",
        ),
        sheet::Tab::Emoji => (
            "workspace-settings-tab-emoji",
            "workspace-settings-tab-emoji",
        ),
    };
    sheet::TabLabel {
        tab,
        label: t(label),
        short: t(short),
    }
}

/// What the person is in the workspace, as the sheet says under its name.
#[must_use]
pub fn role_line(role: Option<Role>) -> String {
    role.map_or_else(|| t("workspace-settings-role-guest"), domain::role_label)
}

/// The note the sheet shows when the workspace refused a change: the workspace's own reason.
#[must_use]
pub fn refusal(reason: &str) -> sheet::Notice {
    sheet::Notice {
        title: t("workspace-settings-refused"),
        detail: Some(reason.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_server_protocol::workspaces::{WorkspaceDelegation, WorkspaceHolder};

    /// A workspace's holders reach the People tab as what each is called and
    /// what it can read: a page and the session's own sheet say them alike.
    #[test]
    fn holders_keep_their_names_and_what_each_can_read() {
        let holder = |label: &str, reads: bool, cloud: bool| WorkspaceHolder {
            device: label.to_owned(),
            person: "person".to_owned(),
            label: label.to_owned(),
            reads,
            cloud,
        };
        let listed = holders(&[
            holder("Kitchen NAS", true, false),
            holder("tilo.sh", true, true),
            holder("Backup box", false, false),
        ]);
        assert_eq!(
            listed,
            [
                domain::people::MemberHolder {
                    device: "Kitchen NAS".to_owned(),
                    name: "Kitchen NAS".to_owned(),
                    reads: true,
                    cloud: false,
                },
                domain::people::MemberHolder {
                    device: "tilo.sh".to_owned(),
                    name: "tilo.sh".to_owned(),
                    reads: true,
                    cloud: true,
                },
                domain::people::MemberHolder {
                    device: "Backup box".to_owned(),
                    name: "Backup box".to_owned(),
                    reads: false,
                    cloud: false,
                },
            ]
        );
    }

    /// A page's delegation reads as the same words the session's own sheet
    /// gives it: what the work allows, and the day it ends.
    #[test]
    fn a_page_delegation_reads_in_the_sheets_words() {
        let member = tilo_action_policy::Grantee::person("member".to_owned());
        let grant = Grant::new(
            "delegated-work",
            member.clone(),
            tilo_action_policy::TreePath::workspace("w").app("so.tilo.mail"),
            tilo_action_policy::KindSet::READ_UNDOABLE,
            tilo_action_policy::Answer::Allow,
            member,
        );
        let read = delegations(&[WorkspaceDelegation {
            person: "member".to_owned(),
            server: "server".to_owned(),
            agent: None,
            grants: vec![grant.clone()],
            expires_at_ns: 1_800_000_000_000_000_000,
        }]);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].person, "member");
        assert_eq!(read[0].server, "server");
        assert_eq!(
            read[0].work,
            domain::grant_set_sentence(std::slice::from_ref(&grant), &Directory::default())
        );
        assert_eq!(
            read[0].until,
            ui::format::lifetime(Lifetime::Until(1_800_000_000_000))
        );
        assert!(
            !read[0].revocable,
            "a page cannot sign a revocation, so it offers none"
        );
        // The session's own delegation says the same words, and whether this
        // device may take it back.
        let held = held_delegations(&[tilo_session_client::engine::Delegation {
            key: "server/member".to_owned(),
            person: "member".to_owned(),
            server: "server".to_owned(),
            agent: None,
            grants: vec![grant],
            expires_at_ns: 1_800_000_000_000_000_000,
            revocable: true,
        }]);
        assert_eq!(held[0].work, read[0].work);
        assert_eq!(held[0].until, read[0].until);
        assert!(held[0].revocable);
    }
}
