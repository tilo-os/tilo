//! The People tab of the workspace settings on every host: who shares the workspace, their roles,
//! and what the viewer may change of them. What the members are is the host's to say — the shell
//! reads its workspace's policy, a page asks the workspace's host — and the tab is built here, once,
//! from them.

use tilo_action_policy::Role;
use tilo_desktop::workspace_settings::{self as domain, people::PeopleContent};
use tilo_desktop_surface::i18n::t;
use tilo_sheets::workspace_settings as sheet;

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
                extra: entry.extra,
                offers: match entry.offers {
                    domain::people::Offers::Nothing => sheet::Offers::Nothing,
                    domain::people::Offers::Role => sheet::Offers::Role,
                    domain::people::Offers::RoleAndRemove => sheet::Offers::RoleAndRemove,
                },
                can_manage: entry.can_manage,
            })
            .collect(),
        roles: [Role::Viewer, Role::Commenter, Role::Editor]
            .into_iter()
            .map(|role| sheet::RoleChoice {
                key: role.key().to_owned(),
                label: domain::role_label(role),
                allows: domain::role_capability_preview(role),
            })
            .collect(),
        empty: content.empty,
        note: content.note,
        remove_decision: sheet::Decision {
            title: t("workspace-settings-remove-sheet-title"),
            description: t("workspace-settings-remove-sheet-description"),
            keep: t("workspace-settings-keep-access"),
            review: t("workspace-settings-review-activity"),
            confirm: t("workspace-settings-remove-person"),
        },
    }
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
