//! What each member sheet says: its title, what its choice does, and its
//! choices.

use collab_types::role::Role as CapRole;

use super::{MemberSheet, WorkspaceSettingsModel};
use crate::i18n::{t, t_args};

/// A member as the sheets name them: as the People pane does.
fn sheet_name(model: &WorkspaceSettingsModel<'_>, user: &str) -> String {
    model.desktop.active_workspace().map_or_else(
        || super::super::short_id(user),
        |ws| super::super::member_row_name(model.people, ws, model.acting_user, user),
    )
}

/// A member sheet's title, what its choice does, and whom or what it is
/// about when that is not in the title.
pub(super) fn member_sheet_text(
    sheet: &MemberSheet,
    model: &WorkspaceSettingsModel<'_>,
) -> (String, String, Option<String>) {
    match sheet {
        MemberSheet::Role(user) => {
            let current = model
                .desktop
                .active_workspace()
                .and_then(|workspace| workspace.role_of(user))
                .unwrap_or(CapRole::Viewer);
            (
                t("workspace-settings-role-sheet-title"),
                t_args(
                    "workspace-settings-role-sheet-description",
                    &[
                        ("person", super::super::short_id(user).into()),
                        ("role", super::super::role_label(current).into()),
                    ],
                ),
                None,
            )
        }
        MemberSheet::Remove(user) => (
            t("workspace-settings-remove-sheet-title"),
            t(remove_sheet_description(model, user)),
            Some(super::super::short_id(user)),
        ),
        MemberSheet::Revoke(cap_id) => (
            t("workspace-settings-revoke-sheet-title"),
            t("workspace-settings-revoke-sheet-description"),
            Some(super::super::short_id(cap_id)),
        ),
        MemberSheet::ChooseRecipient(_) => (
            t("workspace-settings-ownership-propose"),
            t("workspace-settings-ownership-choose-description"),
            None,
        ),
        MemberSheet::OfferTo(user) => {
            let person = sheet_name(model, user);
            (
                t_args(
                    "workspace-settings-ownership-offer-title",
                    &[("person", person.clone().into())],
                ),
                t_args(
                    "workspace-settings-ownership-offer-description",
                    &[("person", person.into())],
                ),
                None,
            )
        }
        MemberSheet::TakeBack => (
            t("workspace-settings-ownership-cancel"),
            t("workspace-settings-ownership-cancel-description"),
            None,
        ),
        MemberSheet::Accept => {
            let owner = model
                .desktop
                .active_workspace()
                .and_then(|ws| match &ws.ownership.step {
                    tilo_desktop_model::OwnershipStep::Offered { owner, .. } => {
                        Some(sheet_name(model, &hex::encode(owner)))
                    }
                    _ => None,
                })
                .unwrap_or_default();
            (
                t("workspace-settings-ownership-accept-title"),
                t_args(
                    "workspace-settings-ownership-accept-description",
                    &[("owner", owner.into())],
                ),
                None,
            )
        }
    }
}

/// A member sheet's choices, in order, and what each one does (shown on
/// the role sheet).
pub(super) fn member_sheet_options(
    sheet: &MemberSheet,
    model: &WorkspaceSettingsModel<'_>,
) -> (Vec<String>, Vec<String>) {
    let confirm = |keep: &str, go: &str| (vec![t(keep), t(go)], vec![String::new(), String::new()]);
    match sheet {
        MemberSheet::Role(_) => {
            let roles = [CapRole::Viewer, CapRole::Commenter, CapRole::Editor];
            (
                roles
                    .iter()
                    .map(|role| super::super::role_label(*role))
                    .collect(),
                roles
                    .iter()
                    .map(|role| super::super::role_capability_preview(*role))
                    .collect(),
            )
        }
        MemberSheet::Remove(_) => (
            vec![
                t("workspace-settings-keep-access"),
                t("workspace-settings-review-activity"),
                t("workspace-settings-remove-person"),
            ],
            vec![
                t("workspace-settings-no-changes"),
                t("workspace-settings-open-history"),
                t("workspace-settings-remove-person-detail"),
            ],
        ),
        MemberSheet::Revoke(_) => (
            vec![
                t("workspace-settings-keep-permission"),
                t("workspace-settings-review-activity"),
                t("workspace-settings-revoke-permission"),
            ],
            vec![
                t("workspace-settings-no-changes"),
                t("workspace-settings-open-history"),
                t("workspace-settings-revoke-permission-detail"),
            ],
        ),
        MemberSheet::ChooseRecipient(people) => {
            let labels: Vec<String> = std::iter::once(t("workspace-settings-hint-cancel"))
                .chain(people.iter().map(|user| sheet_name(model, user)))
                .collect();
            let previews = vec![String::new(); labels.len()];
            (labels, previews)
        }
        MemberSheet::OfferTo(_) => confirm(
            "workspace-settings-hint-cancel",
            "workspace-settings-ownership-propose",
        ),
        MemberSheet::TakeBack => confirm(
            "workspace-settings-ownership-keep-offer",
            "workspace-settings-ownership-cancel-short",
        ),
        MemberSheet::Accept => confirm(
            "workspace-settings-ownership-not-now",
            "workspace-settings-ownership-accept",
        ),
    }
}

/// What removing `user` does, as the Remove sheet says it: removing the
/// person the room is offered to closes the offer too.
fn remove_sheet_description(model: &WorkspaceSettingsModel<'_>, user: &str) -> &'static str {
    let offered = model.desktop.active_workspace().is_some_and(|ws| {
        matches!(
            &ws.ownership.step,
            tilo_desktop_model::OwnershipStep::Offered { recipient, .. }
                if hex::encode(recipient) == user
        )
    });
    if offered {
        "workspace-settings-remove-sheet-offer-description"
    } else {
        "workspace-settings-remove-sheet-description"
    }
}
