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
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::{Answer, Role};
use tilo_desktop::authority::WorkspaceAuthority as _;
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::collaborators::PeopleNames;
use tilo_desktop::desktop_actions::DESK_RESOURCE;
use tilo_desktop::workspace_settings::{self as domain, agent_access};
use tilo_desktop_model::{DesktopTree, UserId, Workspace, role_from_id, role_id};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::workspace_settings as sheet;

use crate::keys::{Invocation, Invocations};
use crate::lists::AppLists;
use crate::workspaces::{DeviceChoice, Personal};

mod emoji;

pub(crate) use emoji::Emoji as EmojiTab;

/// What the sheet is fed from: the desk in front, the lists its tabs offer,
/// and the workspace's own emoji.
pub(crate) struct Feeding<'a> {
    handle: &'a DesktopHandle,
    person: &'a UserId,
    /// The machine's application lists, as its every search pass reads them.
    lists: std::sync::Arc<AppLists>,
    /// The desk revision the panes were last fed at.
    fed: &'a Cell<Option<u64>>,
    /// The workspace's emoji, read and changed through Chat's record.
    emoji: &'a EmojiTab,
    /// The session's file chooser, where a picture to add is chosen.
    picker: Option<&'a FilePickerSeat>,
    personal: Option<&'a RefCell<Personal>>,
}

impl<'a> Feeding<'a> {
    pub(crate) fn new(
        handle: &'a DesktopHandle,
        person: &'a UserId,
        lists: std::sync::Arc<AppLists>,
        fed: &'a Cell<Option<u64>>,
        emoji: &'a EmojiTab,
        picker: Option<&'a FilePickerSeat>,
        personal: Option<&'a RefCell<Personal>>,
    ) -> Self {
        Self {
            handle,
            person,
            lists,
            fed,
            emoji,
            picker,
            personal,
        }
    }
}

/// Open the sheet on the workspace in front: its panes as the workspace
/// stands, and its name field ready for a new name.
pub(crate) fn open(seat: Option<&sheet::WorkspaceSettingsSeat>, fed: &Feeding<'_>) {
    let Some(seat) = seat else {
        return;
    };
    show(seat, fed, sheet::Tab::General, None);
    seat.reset();
    seat.presentation().open(false);
}

/// Keep the sheet's panes current while it is up: the workspace changes
/// while the sheet is open (a role set, a grant given), and so does what
/// other people do in it — the workspace's emoji among them.
pub(crate) fn refresh(seat: Option<&sheet::WorkspaceSettingsSeat>, fed: &Feeding<'_>) {
    let Some(seat) = seat else {
        return;
    };
    if !seat.presentation().is_open() {
        return;
    }
    let shown = seat.settings();
    let emoji_moved =
        shown.tab == sheet::Tab::Emoji && fed.handle.read(|tree| fed.emoji.moved(tree, fed.person));
    if !emoji_moved && fed.fed.get() == Some(fed.handle.revision()) {
        return;
    }
    let review = shown
        .agents
        .review
        .as_ref()
        .map(|review| review.agent.clone());
    show(seat, fed, shown.tab, review.as_deref());
}

/// Show the sheet's panes as the desk stands for the person, on `tab`.
fn show(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    tab: sheet::Tab,
    review: Option<&str>,
) {
    let shown = settings(
        &fed.handle.read(Clone::clone),
        fed.person,
        &fed.lists,
        tab,
        review,
        fed.personal,
        fed.emoji,
    );
    seat.show(shown);
    fed.fed.set(Some(fed.handle.revision()));
}

/// Show the workspace in front on one of the person's devices, or keep it
/// off there: the row says the choice at once, since a workspace kept off this
/// device leaves the desk under the sheet.
fn toggle_workspace_device(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, device: &str) {
    if !toggle_device(fed.handle, fed.personal, device) {
        return;
    }
    let shown = seat.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(seat, fed, shown.tab, review.as_deref());
}

/// Show one agent's review, or the roster again for `None`, on the tab the
/// sheet was on.
fn show_review(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, agent: Option<&str>) {
    show(seat, fed, seat.settings().tab, agent);
}

/// Show the tab the person picked, and keep it for the next opening.
fn show_picked_tab(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, tab: &str) {
    let Some(tab) = sheet::Tab::from_key(tab) else {
        return;
    };
    let shown = seat.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(seat, fed, tab, review.as_deref());
}

/// The Emoji tab's own effects: the picture to add, an emoji to remove, the
/// notice dismissed. Whether `effect` was one of them.
fn follow_emoji(effect: &sheet::WorkspaceSettingsEffect, fed: &Feeding<'_>) -> bool {
    if !matches!(
        effect,
        sheet::WorkspaceSettingsEffect::AddEmoji
            | sheet::WorkspaceSettingsEffect::RemoveEmoji { .. }
            | sheet::WorkspaceSettingsEffect::DismissEmojiNotice
    ) {
        return false;
    }
    fed.handle
        .read(|tree| fed.emoji.asked(effect, tree, fed.person, fed.picker));
    true
}

/// The agents tab's asks: a review shown or ended, an agent answered, given
/// another app or model, or removed. Whether `effect` was one of them.
fn follow_agent(
    seat: &sheet::WorkspaceSettingsSeat,
    effect: &sheet::WorkspaceSettingsEffect,
    fed: &Feeding<'_>,
) -> bool {
    let handle = fed.handle;
    let person = fed.person;
    match effect {
        sheet::WorkspaceSettingsEffect::ReviewAgent { agent } => {
            show_review(seat, fed, Some(agent));
        }
        sheet::WorkspaceSettingsEffect::EndReview => show_review(seat, fed, None),
        sheet::WorkspaceSettingsEffect::SetAgentAnswer {
            agent,
            scope,
            kind,
            answer,
        } => answer_agent(handle, person, agent, scope, kind, answer),
        sheet::WorkspaceSettingsEffect::ResetAgentApp { agent, scope } => {
            reset_agent_app(handle, person, agent, scope);
        }
        sheet::WorkspaceSettingsEffect::RemoveAgent { agent } => {
            edit(handle, person, |ws, actor| {
                domain::remove_agent(ws, actor, agent)
            });
        }
        sheet::WorkspaceSettingsEffect::SetAgentModel { agent, model } => {
            set_agent_model(seat, fed, agent, model);
        }
        _ => return false,
    }
    true
}

/// Carry out what the person asked for in the sheet.
pub(crate) fn follow(
    seat: Option<&sheet::WorkspaceSettingsSeat>,
    fed: &Feeding<'_>,
    intents: &RefCell<Vec<ChromeIntent>>,
    invocations: &Invocations,
) {
    let Some(seat) = seat else {
        return;
    };
    let handle = fed.handle;
    let person = fed.person;
    // What came of the chooser and of the pictures waiting for the image
    // tools, before what the person asks now.
    handle.read(|tree| fed.emoji.settle(tree, person, fed.picker));
    for effect in seat.take_asked() {
        if follow_emoji(&effect, fed) || follow_agent(seat, &effect, fed) {
            continue;
        }
        match effect {
            sheet::WorkspaceSettingsEffect::ShowTab { tab } => {
                show_picked_tab(seat, fed, &tab);
            }
            sheet::WorkspaceSettingsEffect::Close => {}
            sheet::WorkspaceSettingsEffect::ToggleDevice { device } => {
                toggle_workspace_device(seat, fed, &device);
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
                open_search(invocations, ownership_query(handle, person));
            }
            sheet::WorkspaceSettingsEffect::SavedRoles => {
                open_search(invocations, t("workspace-settings-query-saved-roles"));
            }
            sheet::WorkspaceSettingsEffect::NextWorkspaceDefault { resource } => {
                next_workspace_default(handle, person, &fed.lists, &resource);
            }
            sheet::WorkspaceSettingsEffect::NextPersonalDefault { resource } => {
                next_personal_default(handle, person, &fed.lists, &resource);
            }
            other => tracing::debug!(
                ?other,
                "workspace settings asks what the session cannot do yet"
            ),
        }
    }
}

/// What the ownership button does here, as the search opens on it.
fn ownership_query(handle: &DesktopHandle, person: &UserId) -> String {
    // The button is named by what it does; search opens on that action,
    // which asks before it transfers anything.
    handle
        .read(|tree| {
            tree.active_workspace()
                .and_then(|ws| domain::ownership_button(ws, person))
                .map(|(_, action)| action)
        })
        .unwrap_or_else(|| t("workspace-settings-ownership-transfer"))
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
    edit_changed(handle, person, |workspace, actor| {
        !edit(workspace, actor).is_empty()
    });
}

/// Apply `change` to the workspace in front as `person`, and keep the tree
/// when it answers that something changed.
fn edit_changed(
    handle: &DesktopHandle,
    person: &UserId,
    change: impl FnOnce(&mut Workspace, &UserId) -> bool,
) {
    let mut tree = handle.read(Clone::clone);
    let Some(workspace) = tree.active_workspace_mut() else {
        return;
    };
    if !change(workspace, person) {
        return;
    }
    handle.replace_tree(tree);
}

/// Move what opens a kind of file or link in the workspace on to the next
/// app that can.
fn next_workspace_default(
    handle: &DesktopHandle,
    person: &UserId,
    lists: &AppLists,
    resource: &str,
) {
    let Some(resource) = desktop_policy::ResourceKind::parse(resource) else {
        tracing::debug!(%resource, "no such kind of file or link: nothing changed");
        return;
    };
    let candidates = lists.candidates.clone();
    let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
    edit_changed(handle, person, |ws, actor| {
        let next = domain::next_workspace_default(
            ws.default_apps.policy.get(&resource),
            &resource,
            &candidates,
        );
        if ws.default_apps.policy.get(&resource) == Some(&next) {
            return false;
        }
        match ws.set_default_app_policy_as(actor, resource, next, &resolver) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "workspace default refused");
                false
            }
        }
    });
}

/// Move what opens a kind of file or link for `person` themselves on to the
/// next app that can.
fn next_personal_default(
    handle: &DesktopHandle,
    person: &UserId,
    lists: &AppLists,
    resource: &str,
) {
    let Some(resource) = desktop_policy::ResourceKind::parse(resource) else {
        tracing::debug!(%resource, "no such kind of file or link: nothing changed");
        return;
    };
    let candidates = lists.candidates.clone();
    let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
    edit_changed(handle, person, |ws, actor| {
        let current = ws
            .default_apps
            .user_choices
            .get(actor)
            .and_then(|choices| choices.get(&resource));
        let next = domain::next_user_default(current, &resource, &candidates);
        if current == Some(&next) {
            return false;
        }
        match ws.set_default_app_choice_as(actor, actor, resource, next, &resolver) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "personal default refused");
                false
            }
        }
    });
}

/// Set what an agent may do for one kind of action at one app, and nothing
/// else: the effect carries the answer the person chose, and an empty one
/// takes the agent back to what its admission says.
fn answer_agent(
    handle: &DesktopHandle,
    person: &UserId,
    agent: &str,
    scope: &str,
    kind: &str,
    answer: &str,
) {
    let (Some(scope), Some(kind)) = (AnswerScope::parse(scope), agent_access::kind_from_key(kind))
    else {
        tracing::debug!(%scope, %kind, "no such answer: nothing changed");
        return;
    };
    let answer = if answer.is_empty() {
        None
    } else {
        let Some(answer) = Answer::parse(answer) else {
            tracing::debug!(%answer, "no such answer: nothing changed");
            return;
        };
        Some(answer)
    };
    edit(handle, person, |ws, actor| {
        agent_access::set_agent_answers(ws, actor, agent, &[(scope, kind, answer)])
    });
}

/// Take every answer the person set for an agent at one app back to what the
/// agent's admission says.
fn reset_agent_app(handle: &DesktopHandle, person: &UserId, agent: &str, scope: &str) {
    let Some(scope) = AnswerScope::parse(scope) else {
        tracing::debug!(%scope, "no such app: nothing changed");
        return;
    };
    edit(handle, person, |ws, actor| {
        let answers: Vec<_> = agent_access::KINDS
            .into_iter()
            .map(|kind| (scope.clone(), kind, None))
            .collect();
        agent_access::set_agent_answers(ws, actor, agent, &answers)
    });
}

/// Choose the model an agent plans with when `person` launches it, in the
/// person's own Settings.
fn set_agent_model(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    agent: &str,
    model: &str,
) {
    let Some(models) =
        agent_access::agent_models(&agent_provider_config::ProviderConfig::load(), agent)
    else {
        tracing::debug!(%agent, "Settings names no model: nothing changed");
        return;
    };
    let chosen = if model.is_empty() {
        None
    } else {
        let Some(option) = models
            .offered
            .into_iter()
            .find(|option| option.key() == model)
        else {
            tracing::debug!(%model, "no such model: nothing changed");
            return;
        };
        Some(option)
    };
    match agent_access::choose_agent_model(agent, chosen) {
        Ok(_) => {
            let shown = seat.settings();
            let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
            show(seat, fed, shown.tab, review.as_deref());
        }
        Err(error) => tracing::warn!(%error, "agent model not saved"),
    }
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
    review: Option<&str>,
    personal: Option<&RefCell<Personal>>,
    emoji: &EmojiTab,
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
        agents: workspace.map_or_else(sheet::Agents::default, |ws| {
            agents_pane(ws, person, &apps, review)
        }),
        apps: workspace.map_or_else(sheet::Apps::default, |ws| apps_pane(ws, person, lists)),
        // The workspace's emoji are Chat's record, opened when the tab is
        // first shown and nowhere else: no other tab reads it.
        emoji: if tab == sheet::Tab::Emoji {
            emoji.pane(tree, person, &names)
        } else {
            sheet::EmojiPane::default()
        },
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
    if !shown {
        return personal.set_shown(room, device, true);
    }
    // The desk keeps the person one workspace: keeping this device's last
    // one off it would leave them with none, and the workspace would stay
    // whatever the choice said.
    if personal.is_this_device(&device) && handle.read(|tree| tree.workspaces.len()) <= 1 {
        return false;
    }
    personal.set_shown(room, device, false)
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

/// The Agents tab: the agents admitted here, each with what it may do, and
/// what one of them may do app by app while it is reviewed.
fn agents_pane(
    ws: &Workspace,
    person: &UserId,
    apps: &dyn domain::AppDirectory,
    review: Option<&str>,
) -> sheet::Agents {
    let summaries = domain::agent_summaries(ws);
    let manage = ws.may_administer(person);
    sheet::Agents {
        intro: sheet::Intro {
            title: t_args(
                "workspace-settings-agents-count",
                &[("count", summaries.len().into())],
            ),
            help: t("workspace-settings-agents-help"),
            authority: if manage {
                t("workspace-settings-you-manage")
            } else {
                t("workspace-settings-view-only")
            },
        },
        add: t("workspace-settings-add-agent"),
        manage,
        header: t("workspace-settings-agents-header"),
        agents: summaries
            .iter()
            .map(|row| sheet::Agent {
                key: row.agent.clone(),
                name: row.name.clone(),
                badge: row.badge.clone(),
                detail: row.detail.clone(),
                admitted: t("workspace-settings-admitted"),
            })
            .collect(),
        review_label: t("workspace-settings-review-access"),
        remove_label: t("workspace-settings-revoke"),
        empty: (
            t("workspace-settings-agents-empty-title"),
            t("workspace-settings-agents-empty-detail"),
        ),
        bots_header: t("workspace-settings-bots-header"),
        add_bot: t("workspace-settings-add-bot"),
        bot_tag: t("workspace-settings-bot-tag"),
        remove_bot: t("workspace-settings-remove-bot"),
        bots: ws
            .bots
            .iter()
            .map(|bot| sheet::Bot {
                name: bot.name.clone(),
                entry: bot.requested.then(|| bot.entry.clone()).flatten(),
                detail: domain::bot_detail(bot),
            })
            .collect(),
        bots_empty: t("workspace-settings-bots-empty"),
        footnote: (!manage).then(|| t("workspace-settings-owner-only")),
        review: review.and_then(|agent| reviewed(ws, agent, manage, apps)),
    }
}

/// Which model the agent plans with by default, as the review's models row
/// reads it.
fn model_summary(models: Option<&agent_access::AgentModels>) -> String {
    let model = models
        .and_then(|models| models.default.clone())
        .unwrap_or_else(|| t("workspace-settings-agent-model-none"));
    t_args(
        "workspace-settings-agent-model-default",
        &[("model", model.into())],
    )
}

/// One agent's review: what it may do in each app, the model it plans with
/// and the actions it may always take. `None` once it is no longer admitted
/// here, where the tab shows the roster again.
fn reviewed(
    ws: &Workspace,
    agent: &str,
    editable: bool,
    apps: &dyn domain::AppDirectory,
) -> Option<sheet::AgentReview> {
    let summary = domain::agent_summaries(ws)
        .into_iter()
        .find(|row| row.agent == agent)?;
    let domain::AgentSummary {
        agent: key,
        name,
        badge,
        ..
    } = summary;
    let answer_index = |answer: Answer| {
        agent_access::ANSWERS
            .iter()
            .position(|candidate| *candidate == answer)
            .unwrap_or_default()
    };
    let rows = agent_access::agent_access_rows(ws, agent, apps);
    let models = agent_access::agent_models(&agent_provider_config::ProviderConfig::load(), agent);
    let exceptions = agent_access::agent_exceptions(ws, agent, apps);
    let summary_label = model_summary(models.as_ref());
    Some(sheet::AgentReview {
        agent: key,
        name,
        badge,
        back: t("workspace-settings-agent-access-back"),
        note: t("workspace-settings-agent-access-note"),
        editable,
        answers: [
            t("workspace-settings-answer-allow"),
            t("workspace-settings-answer-ask"),
            t("workspace-settings-answer-deny"),
        ],
        reset: t("workspace-settings-agent-access-reset"),
        custom: t("workspace-settings-agent-access-custom"),
        apps: rows
            .iter()
            .map(|row| sheet::AccessApp {
                scope: row.scope.key(),
                label: row.label.clone(),
                customised: row.customised(),
                detail: t("workspace-settings-agent-access-custom-detail"),
                kinds: row
                    .cells
                    .iter()
                    .map(|cell| sheet::KindLine {
                        kind: agent_access::kind_key(cell.kind).to_owned(),
                        label: agent_access::kind_label(cell.kind),
                        origin: t(if cell.explicit {
                            "workspace-settings-agent-access-set-here"
                        } else {
                            "workspace-settings-agent-access-inherited"
                        }),
                        answer: answer_index(cell.answer),
                    })
                    .collect(),
            })
            .collect(),
        empty: t("workspace-settings-agent-access-empty"),
        models: models.map(|models| sheet::Models {
            title: t("workspace-settings-agent-model"),
            summary: summary_label.clone(),
            choices: std::iter::once(sheet::ModelChoice {
                key: String::new(),
                label: t("workspace-settings-agent-model-none"),
                in_use: models.chosen.is_none(),
            })
            .chain(models.offered.iter().map(|option| sheet::ModelChoice {
                key: option.key(),
                label: option.label.clone(),
                in_use: models.chosen.as_ref() == Some(option),
            }))
            .collect(),
        }),
        use_model: t("workspace-settings-agent-model-use"),
        in_use: t("workspace-settings-agent-model-in-use"),
        exceptions_title: t_args(
            "workspace-settings-agent-access-always",
            &[("count", exceptions.len().into())],
        ),
        exceptions: exceptions
            .into_iter()
            .map(|exception| sheet::Exception {
                scope: exception.scope.key(),
                kind: agent_access::kind_key(exception.kind).to_owned(),
                label: exception.label,
            })
            .collect(),
        stop: t("workspace-settings-agent-access-stop"),
    })
}

/// The Apps & Files tab: which app opens each kind of file and link here,
/// the workspace's choice and the person's own.
fn apps_pane(ws: &Workspace, person: &UserId, lists: &AppLists) -> sheet::Apps {
    let manage = ws.may_administer(person);
    sheet::Apps {
        intro: sheet::Intro {
            title: t("workspace-settings-apps-intro-title"),
            help: t("workspace-settings-apps-intro-detail"),
            authority: domain::default_apps_status(ws, &lists.candidates),
        },
        header: t("workspace-settings-apps-header"),
        workspace_choice: t("workspace-settings-workspace-choice"),
        your_choice: t("workspace-settings-your-choice"),
        rows: domain::default_app_choices(ws, person, &lists.candidates)
            .into_iter()
            .map(|choice| sheet::AppRow {
                resource: choice.resource.as_str().to_owned(),
                label: choice.label,
                workspace: choice.workspace,
                yours: choice.yours,
            })
            .collect(),
        manage,
        empty: (
            t("workspace-settings-apps-empty-title"),
            t("workspace-settings-apps-empty-detail"),
        ),
        footnote: (!manage).then(|| t("workspace-settings-owner-only")),
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
