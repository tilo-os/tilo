//! Workspace settings: what the sheet shows of the workspace in front, and
//! what the person's choices there change.
//!
//! The sheet shows and asks; this carries out. Every change to the workspace
//! goes through the workspace's own policy functions — the ones the desk's
//! member commands use — so a change the person may not make is refused
//! there, whatever the sheet showed, and the tree is only written when
//! something changed.

use std::cell::{Cell, RefCell};

use collab_types::caps::Expiry;
use tilo_action_policy::Role;
use tilo_desktop::authority::WorkspaceAuthority as _;
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::collaborators::PeopleNames;
use tilo_desktop::desktop_actions::DESK_RESOURCE;
use tilo_desktop::workspace_settings as domain;
use tilo_desktop_model::{DesktopTree, UserId, Workspace, role_from_id, role_id};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_sheets::Seat as _;
use tilo_sheets::workspace_settings as sheet;

use crate::keys::{Invocation, Invocations};
use crate::lists::AppLists;
use crate::workspaces::{DeviceChoice, Personal};

/// Open the sheet on the workspace in front: its panes as the workspace
/// stands, and its name field ready for a new name.
pub(crate) fn open(
    seat: Option<&sheet::WorkspaceSettingsSeat>,
    handle: &DesktopHandle,
    person: &UserId,
    lists: &AppLists,
    fed: &Cell<Option<u64>>,
    personal: Option<&RefCell<Personal>>,
) {
    let Some(seat) = seat else {
        return;
    };
    show(seat, handle, person, lists, fed, personal);
    seat.reset();
    seat.presentation().open(false);
}

/// Keep the sheet's panes current while it is up: the workspace changes
/// while the sheet is open (a role set, a grant given), and so does what
/// other people do in it.
pub(crate) fn refresh(
    seat: Option<&sheet::WorkspaceSettingsSeat>,
    handle: &DesktopHandle,
    person: &UserId,
    lists: &AppLists,
    fed: &Cell<Option<u64>>,
    personal: Option<&RefCell<Personal>>,
) {
    let Some(seat) = seat else {
        return;
    };
    if !seat.presentation().is_open() || fed.get() == Some(handle.revision()) {
        return;
    }
    show(seat, handle, person, lists, fed, personal);
}

/// Show the sheet's panes as `tree` stands for `person`, keeping the tab it
/// shows.
fn show(
    seat: &sheet::WorkspaceSettingsSeat,
    handle: &DesktopHandle,
    person: &UserId,
    lists: &AppLists,
    fed: &Cell<Option<u64>>,
    personal: Option<&RefCell<Personal>>,
) {
    let shown = settings(
        &handle.read(Clone::clone),
        person,
        lists,
        seat.settings().tab,
        personal,
    );
    seat.show(shown);
    fed.set(Some(handle.revision()));
}

/// Carry out what the person asked for in the sheet.
pub(crate) fn follow(
    seat: Option<&sheet::WorkspaceSettingsSeat>,
    handle: &DesktopHandle,
    person: &UserId,
    lists: &AppLists,
    fed: &Cell<Option<u64>>,
    intents: &RefCell<Vec<ChromeIntent>>,
    invocations: &Invocations,
    personal: Option<&RefCell<Personal>>,
) {
    let Some(seat) = seat else {
        return;
    };
    for effect in seat.take_asked() {
        match effect {
            sheet::WorkspaceSettingsEffect::ShowTab { tab } => {
                // The sheet shows the tab the person picked, and keeps it
                // for the next opening.
                if let Some(tab) = sheet::Tab::from_key(&tab) {
                    let mut shown = seat.settings();
                    shown.tab = tab;
                    seat.show(shown);
                }
            }
            sheet::WorkspaceSettingsEffect::Close => {}
            sheet::WorkspaceSettingsEffect::ToggleDevice { device } => {
                if toggle_device(handle, personal, &device) {
                    // The row says the choice at once: a workspace kept off
                    // this device leaves the desk under the sheet.
                    show(seat, handle, person, lists, fed, personal);
                }
            }
            sheet::WorkspaceSettingsEffect::Rename { name } => {
                intents
                    .borrow_mut()
                    .push(ChromeIntent::RenameWorkspace { name });
            }
            sheet::WorkspaceSettingsEffect::Move { step } => move_workspace(handle, step),
            sheet::WorkspaceSettingsEffect::SetRole { member, role } => {
                let Some(role) = role_from_id(&role) else {
                    tracing::debug!(%role, "no such role: nothing changed");
                    continue;
                };
                edit(handle, person, |ws, actor| {
                    domain::set_member_role(ws, actor, &member, role)
                });
            }
            sheet::WorkspaceSettingsEffect::RemoveMember { member } => {
                edit(handle, person, |ws, actor| {
                    domain::remove_member(ws, actor, &member)
                });
            }
            sheet::WorkspaceSettingsEffect::Grant {
                subject,
                permission,
                expiry,
            } => edit(handle, person, |ws, actor| {
                domain::draft_for(ws, &subject, &permission, &expiry).map_or_else(
                    || {
                        tracing::debug!(%subject, %permission, "no such grant: nothing changed");
                        domain::PolicyEdit::default()
                    },
                    |draft| domain::apply_grant_draft(ws, actor, &draft),
                )
            }),
            sheet::WorkspaceSettingsEffect::Revoke { grant } => {
                edit(handle, person, |ws, actor| {
                    domain::apply_revoke(ws, actor, &grant)
                });
            }
            sheet::WorkspaceSettingsEffect::ReviewActivity => {
                intents.borrow_mut().push(ChromeIntent::ShowActivity(None));
            }
            sheet::WorkspaceSettingsEffect::Invite => {
                intents.borrow_mut().push(ChromeIntent::Invite);
            }
            sheet::WorkspaceSettingsEffect::Ownership => {
                // The button is named by what it does; search opens on that
                // action, which asks before it transfers anything.
                let query = handle
                    .read(|tree| {
                        tree.active_workspace()
                            .and_then(|ws| domain::ownership_button(ws, person))
                            .map(|(_, action)| action)
                    })
                    .unwrap_or_else(|| t("workspace-settings-ownership-transfer"));
                open_search(invocations, query);
            }
            sheet::WorkspaceSettingsEffect::SavedRoles => {
                open_search(invocations, t("workspace-settings-query-saved-roles"));
            }
            other => tracing::debug!(
                ?other,
                "workspace settings asks what the session cannot do yet"
            ),
        }
    }
}

/// Open the search on `query`, where the row the person needs is.
fn open_search(invocations: &Invocations, query: String) {
    invocations
        .borrow_mut()
        .push(Invocation::new(DESK_RESOURCE, "open-search").with("query", query));
}

/// Apply `edit` to the workspace in front as `person`, and keep the tree
/// when it changed something: the policy functions refuse a change the
/// person may not make, and a refusal changes nothing.
fn edit(
    handle: &DesktopHandle,
    person: &UserId,
    edit: impl FnOnce(&mut Workspace, &UserId) -> domain::PolicyEdit,
) {
    let mut tree = handle.read(Clone::clone);
    let Some(workspace) = tree.active_workspace_mut() else {
        return;
    };
    if edit(workspace, person).is_empty() {
        return;
    }
    handle.replace_tree(tree);
}

/// Put the workspace in front one place earlier or later among this
/// device's workspaces.
fn move_workspace(handle: &DesktopHandle, step: i32) {
    let mut tree = handle.read(Clone::clone);
    let Some(at) = tree
        .workspaces
        .iter()
        .position(|workspace| workspace.id == tree.active)
    else {
        return;
    };
    let last = tree.workspaces.len().saturating_sub(1);
    let to = if step < 0 {
        at.saturating_sub(usize::try_from(step.unsigned_abs()).unwrap_or(usize::MAX))
    } else {
        at.saturating_add(usize::try_from(step).unwrap_or(usize::MAX))
            .min(last)
    };
    if to != at {
        tree.workspaces.swap(at, to);
        handle.replace_tree(tree);
    }
}

/// Everything the sheet shows of `tree` for `person`.
fn settings(
    tree: &DesktopTree,
    person: &UserId,
    lists: &AppLists,
    tab: sheet::Tab,
    personal: Option<&RefCell<Personal>>,
) -> sheet::Settings {
    let workspace = tree.active_workspace();
    let names = PeopleNames::of(tree, person, own_name().as_deref());
    let apps = Directory { lists };
    sheet::Settings {
        title: t("workspace-settings-title"),
        name: workspace.map_or_else(
            || t("workspace-settings-workspace-fallback"),
            |ws| ws.name.clone(),
        ),
        role: workspace
            .and_then(|ws| ws.role_of(person))
            .map_or_else(|| t("workspace-settings-role-guest"), domain::role_label),
        close: t("workspace-settings-close"),
        tabs: tabs(),
        tab,
        general: general(tree, personal),
        people: workspace.map_or_else(sheet::People::default, |ws| people(ws, &names, person)),
        permissions: workspace.map_or_else(sheet::Permissions::default, |ws| {
            permissions(ws, person, &apps)
        }),
        ..sheet::Settings::default()
    }
}

/// The General tab: which of the person's devices show the workspace, and
/// its place among theirs. A workspace that is not the person's own has no
/// devices to show it on: it is theirs only while they are in it.
fn general(tree: &DesktopTree, personal: Option<&RefCell<Personal>>) -> sheet::General {
    let devices = match (tree.active_workspace(), personal) {
        (Some(workspace), Some(personal)) => personal.borrow().device_choices(workspace.room()),
        _ => Vec::new(),
    };
    let at = tree.active_workspace().and_then(|workspace| {
        tree.workspaces
            .iter()
            .position(|one| one.id == workspace.id)
    });
    let last = tree.workspaces.len().saturating_sub(1);
    sheet::General {
        devices: sheet::Group {
            title: t("workspace-settings-devices-title"),
            description: t("workspace-settings-devices-local"),
        },
        device_rows: devices.into_iter().map(device_row).collect(),
        move_labels: [
            t("workspace-settings-move-earlier"),
            t("workspace-settings-move-later"),
        ],
        move_enabled: [at.is_some_and(|at| at > 0), at.is_some_and(|at| at < last)],
        ..sheet::General::default()
    }
}

/// One of the person's devices as the General tab lists it: named as its
/// own label names it, the one this session runs on saying so.
fn device_row(choice: DeviceChoice) -> sheet::Device {
    sheet::Device {
        key: hex::encode(choice.device),
        label: if choice.here {
            t_args(
                "workspace-settings-device-here",
                &[("device", choice.label.into())],
            )
        } else {
            choice.label
        },
        toggle: t(if choice.shown {
            "workspace-settings-device-shown"
        } else {
            "workspace-settings-device-hidden"
        }),
        shown: choice.shown,
    }
}

/// Show the workspace in front on one of the person's devices, or keep it
/// off there: the choice is written to the person's own list, which their
/// devices keep in step, and this device honours it at once.
fn toggle_device(
    handle: &DesktopHandle,
    personal: Option<&RefCell<Personal>>,
    device: &str,
) -> bool {
    let Some(personal) = personal else {
        return false;
    };
    let Some(device) = device_key(device) else {
        tracing::debug!(%device, "not a device key: nothing changed");
        return false;
    };
    let Some(room) = handle.read(|tree| tree.active_workspace().map(Workspace::room)) else {
        return false;
    };
    let mut personal = personal.borrow_mut();
    let Some(shown) = personal.shown_on(room, &device) else {
        return false;
    };
    personal.set_shown(room, device, !shown)
}

/// The device `key` names, as the sheet hands it back.
fn device_key(key: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(key).ok()?;
    <[u8; 32]>::try_from(bytes).ok()
}

/// The sheet's tabs, each by the name the strip shows and the shorter one a
/// narrow sheet uses.
fn tabs() -> Vec<sheet::TabLabel> {
    let short = [
        "workspace-settings-tab-general",
        "workspace-settings-tab-people",
        "workspace-settings-tab-agents",
        "workspace-settings-tab-privacy",
        "workspace-settings-tab-apps",
        "workspace-settings-tab-emoji",
    ];
    sheet::Tab::ALL
        .into_iter()
        .zip(short)
        .map(|(tab, short)| sheet::TabLabel {
            tab,
            label: t(label_key(tab)),
            short: t(short),
        })
        .collect()
}

/// What a tab is called in the strip.
fn label_key(tab: sheet::Tab) -> &'static str {
    match tab {
        sheet::Tab::General => "workspace-settings-tab-general",
        sheet::Tab::People => "workspace-settings-tab-people",
        sheet::Tab::Agents => "workspace-settings-tab-agents",
        sheet::Tab::Permissions => "workspace-settings-tab-privacy-security",
        sheet::Tab::Apps => "workspace-settings-tab-apps-files",
        sheet::Tab::Emoji => "workspace-settings-tab-emoji",
    }
}

/// What the person here is called on their own row.
fn own_name() -> Option<String> {
    ui::local_identity::load().map(|identity| identity.display_name)
}

/// The People tab: who shares the workspace, their roles, and where its
/// ownership stands.
fn people(ws: &Workspace, names: &PeopleNames, person: &UserId) -> sheet::People {
    let content = domain::overlay::people::content(ws, names, person, ws.is_owner(person));
    sheet::People {
        count: content.count,
        status: content.status,
        ownership: content.ownership,
        header: content.header,
        you_tag: content.you_tag,
        remove: content.remove,
        people: content
            .people
            .iter()
            .map(|entry| sheet::Person {
                key: entry.user.clone(),
                name: entry.name.clone(),
                you: entry.you,
                role: entry.role.clone(),
                role_key: ws.role_of(&entry.user).map(|role| role_id(role).to_owned()),
                extra: entry.extra.clone(),
                offers: match entry.offers {
                    domain::overlay::people::Offers::Nothing => sheet::Offers::Nothing,
                    domain::overlay::people::Offers::Role => sheet::Offers::Role,
                    domain::overlay::people::Offers::RoleAndRemove => sheet::Offers::RoleAndRemove,
                },
                can_manage: entry.can_manage,
            })
            .collect(),
        roles: [Role::Viewer, Role::Commenter, Role::Editor]
            .into_iter()
            .map(|role| sheet::RoleChoice {
                key: role_id(role).to_owned(),
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

/// The Privacy & Security tab: the permissions the workspace gives, who
/// holds each, and the composer giving one more.
fn permissions(
    ws: &Workspace,
    person: &UserId,
    apps: &dyn domain::AppDirectory,
) -> sheet::Permissions {
    let listed = domain::listed_permissions(ws, apps, person);
    let manage = ws.may_administer(person);
    let mut sections: Vec<sheet::Holders> = Vec::new();
    for entry in &listed {
        let permission = sheet::Permission {
            key: entry.id.clone(),
            holder: entry.holder.clone(),
            detail: match &entry.granted_by {
                Some(by) => t_args(
                    "workspace-settings-permission-detail",
                    &[
                        ("allows", entry.allows.as_str().into()),
                        ("grantor", by.as_str().into()),
                    ],
                ),
                None => entry.allows.clone(),
            },
            expiry: entry.expiry.clone(),
        };
        match sections
            .iter_mut()
            .find(|section| section.title == entry.section)
        {
            Some(section) => section.permissions.push(permission),
            None => sections.push(sheet::Holders {
                title: entry.section.clone(),
                permissions: vec![permission],
            }),
        }
    }
    sheet::Permissions {
        intro: sheet::Intro {
            title: t("workspace-settings-permissions-intro-title"),
            help: t("workspace-settings-permissions-intro-detail"),
            authority: if manage {
                t_args(
                    "workspace-settings-active-grants",
                    &[("count", listed.len().into())],
                )
            } else {
                t("workspace-settings-view-only")
            },
        },
        saved_roles: t_args(
            "workspace-settings-saved-roles",
            &[("count", ws.presets.len().into())],
        ),
        manage,
        composer: sheet::Composer {
            header: t("workspace-settings-grant-header"),
            who: t("workspace-settings-who"),
            who_agent: t("workspace-settings-who-agent"),
            subjects: domain::grant_subjects(ws)
                .iter()
                .filter_map(|subject| {
                    domain::grant_subject_key(subject).map(|key| sheet::Choice {
                        key,
                        label: subject.display(ws, person, apps),
                    })
                })
                .collect(),
            nobody: t("workspace-settings-nobody-to-grant"),
            what: t("workspace-settings-what-it-allows"),
            permissions: domain::grant_choices()
                .into_iter()
                .map(|(key, label)| sheet::Choice {
                    key: key.to_owned(),
                    label,
                })
                .collect(),
            how_long: t("workspace-settings-expiry-caption"),
            expiries: domain::expiry_choices()
                .into_iter()
                .map(|(key, label)| sheet::Choice {
                    key: key.to_owned(),
                    label,
                })
                .collect(),
            default_expiry: domain::expiry_key(Expiry::EndOfSession).to_owned(),
            grant: t("workspace-settings-grant"),
        },
        sections,
        revoke: t("workspace-settings-revoke"),
        empty: (
            t("workspace-settings-permissions-empty-title"),
            t("workspace-settings-permissions-empty-detail"),
        ),
        footnote: (!manage).then(|| t("workspace-settings-owner-only")),
        revoke_decision: sheet::Decision {
            title: t("workspace-settings-revoke-sheet-title"),
            description: t("workspace-settings-revoke-sheet-description"),
            keep: t("workspace-settings-keep-permission"),
            review: t("workspace-settings-review-activity"),
            confirm: t("workspace-settings-revoke-permission"),
        },
    }
}

/// This machine's applications, as the workspace settings name them.
struct Directory<'a> {
    lists: &'a AppLists,
}

impl domain::AppDirectory for Directory<'_> {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        self.lists
            .applications
            .iter()
            .find(|app| app.id == app_id)
            .map(|app| app.title.clone())
            .or_else(|| {
                self.lists
                    .host_apps
                    .iter()
                    .find(|app| app.desktop_id == app_id)
                    .map(|app| app.name.clone())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each device is listed as its own label names it, the one this desk
    /// runs on saying so, with the toggle naming the choice it holds.
    #[test]
    fn a_devices_row_says_whether_the_workspace_shows_there() {
        let elsewhere = device_row(DeviceChoice {
            device: [0x21; 32],
            label: "The laptop".to_owned(),
            shown: true,
            here: false,
        });
        assert_eq!(elsewhere.key, "21".repeat(32), "asked for by its key");
        assert_eq!(
            (
                elsewhere.label.as_str(),
                elsewhere.toggle.as_str(),
                elsewhere.shown
            ),
            ("The laptop", "Shown", true)
        );

        let here = device_row(DeviceChoice {
            device: [0x22; 32],
            label: "The desktop".to_owned(),
            shown: false,
            here: true,
        });
        assert_eq!(
            (here.label.as_str(), here.toggle.as_str(), here.shown),
            ("The desktop (this device)", "Kept off", false)
        );
    }
}
