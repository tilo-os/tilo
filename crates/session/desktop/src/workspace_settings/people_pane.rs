//! The People pane of Workspace Settings, built from the design system's
//! primitives: the summary is a list row with the one ownership action
//! named by what it does, the people are list rows under a section header,
//! each with their role's dropdown and, where it can be taken, Remove.
//! Facts stand on their own: a person's role is their dropdown, extra
//! permissions are a line of their own only when there are any, and "You"
//! is a tag, never a joined string.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AvatarFace, Button, ButtonFace, ButtonRole, ChipStyle, ControlSize, DropdownFace, Interaction,
    ListRowFace, RowLeading, SectionHeaderFace,
};

use super::{
    CapRole, MemberRow, PANE_INTRO_GAP, PANE_INTRO_HEIGHT, PANE_PAD_TOP, PANE_PAD_X,
    SECTION_HEADER_HEIGHT, UserId, Workspace, draw_authority_note, draw_empty_state,
    member_row_name, members_authority_note, members_pane_rows, ordered_roster,
    ownership_transfer_status, public_key_member_id, role_label,
};
use crate::i18n::{t, t_args};

/// What the People pane's ownership button does for `local_user` now, by
/// outcome: its label, and the label of the ownership action it leads to.
/// `None` when they can do nothing about ownership.
#[must_use]
pub fn ownership_button(ws: &Workspace, local_user: &UserId) -> Option<(String, String)> {
    let owner = ws.owner() == Some(local_user);
    match &ws.pending_ownership_transfer {
        Some(pending) if public_key_member_id(pending.recipient) == *local_user => Some((
            t("workspace-settings-ownership-accept-phrase"),
            t("workspace-settings-ownership-accept"),
        )),
        Some(_) if owner => Some((
            t("workspace-settings-ownership-cancel-short"),
            t("workspace-settings-ownership-cancel"),
        )),
        None if owner => Some((
            t("workspace-settings-ownership-transfer"),
            t("workspace-settings-ownership-propose"),
        )),
        _ => None,
    }
}

/// The controls a member's row offers, as `(role, remove)` rects, zero for
/// none: the owner's role is structural, so it is shown, not offered; no
/// one removes the owner, and no one removes themselves here.
#[must_use]
pub fn member_row_controls(row: &MemberRow, local_user: &str) -> (Rect, Rect) {
    let role = if row.is_workspace_owner {
        Rect::ZERO
    } else {
        row.role_rect
    };
    let remove = if row.is_workspace_owner || row.user == local_user {
        Rect::ZERO
    } else {
        row.remove_rect
    };
    (role, remove)
}

/// The role a row shows: an owner by role who does not own the workspace
/// reads as Admin.
fn shown_role(row: &MemberRow) -> String {
    if row.role == CapRole::Owner && !row.is_workspace_owner {
        t("workspace-settings-role-admin")
    } else {
        role_label(row.role)
    }
}

/// `local_user` is the person acting in this workspace and `is_owner` their
/// authority: a non-owner sees every control, disabled, and the note saying
/// why. People are named as the roster names them (`names`).
pub fn draw_members_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    (ws, names): (&Workspace, &crate::collaborators::PeopleNames),
    local_user: &UserId,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
    draw_summary(&mut ctx, pane_rect, (ws, names), local_user);

    let rows = members_pane_rows(ws, pane_rect);
    if rows.is_empty() {
        let empty = Rect::new(
            pane_rect.x0,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
            pane_rect.x1,
            pane_rect.y1,
        );
        draw_empty_state(
            ctx.scene,
            transform,
            empty,
            Icon::Shield,
            (
                &t("workspace-settings-members-empty-title"),
                &t("workspace-settings-members-empty-detail"),
            ),
            ctx.text,
            theme,
        );
        return;
    }

    let header_y = pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP;
    SectionHeaderFace::heading(&t("workspace-settings-people-header")).paint(
        &mut ctx,
        Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            header_y,
            pane_rect.x1 - PANE_PAD_X,
            header_y + SECTION_HEADER_HEIGHT,
        ),
        Interaction::default(),
    );

    let last = rows.len().saturating_sub(1);
    for (index, row) in rows.iter().enumerate() {
        let name = member_row_name(names, ws, local_user, &row.user);
        let can_manage = is_owner && !row.is_workspace_owner && row.user != *local_user;
        draw_member_row(
            &mut ctx,
            row,
            &MemberRowView {
                name: &name,
                is_you: row.user == *local_user,
                can_manage,
                separator: index < last,
                local_user,
            },
        );
    }

    if let Some(note) = members_authority_note(is_owner) {
        draw_authority_note(ctx.scene, transform, pane_rect, &note, ctx.text, theme);
    }
}

/// How many share the workspace, where ownership stands (or what removing
/// someone does), and the ownership action this person can take.
fn draw_summary(
    ctx: &mut PaintCtx<'_>,
    pane_rect: Rect,
    (ws, names): (&Workspace, &crate::collaborators::PeopleNames),
    local_user: &UserId,
) {
    let theme = ctx.theme;
    let rect = Rect::new(
        pane_rect.x0 + PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP,
        pane_rect.x1 - PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT,
    );
    let count = t_args(
        "workspace-settings-members-count",
        &[("count", ordered_roster(ws).len().into())],
    );
    let status = ownership_transfer_status(ws, names, local_user)
        .unwrap_or_else(|| t("workspace-settings-members-help"));
    let button = ownership_button(ws, local_user);
    let action_rect = super::ownership_action_rect(pane_rect);
    let mut face = ListRowFace::new(&count)
        .leading(RowLeading::Icon(Icon::Users))
        .secondary(&status)
        .secondary_lines(2);
    if button.is_some() {
        face = face.trailing_width(rect.x1 - action_rect.x0);
    }
    face.paint(ctx, rect);
    if let Some((label, _)) = &button {
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let slot = Rect::new(
            action_rect.x0,
            action_rect.center().y - height / 2.0,
            action_rect.x1,
            action_rect.center().y + height / 2.0,
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(slot, label).role(ButtonRole::Secondary),
        );
    }
}

/// What one member's row shows beyond its geometry.
struct MemberRowView<'a> {
    name: &'a str,
    is_you: bool,
    /// The acting person may change this member's role and remove them.
    can_manage: bool,
    separator: bool,
    local_user: &'a str,
}

/// One person: avatar and name, "You" on the acting person's own row, their
/// extra permissions when they have any, and their role's dropdown and
/// Remove where those can be offered.
fn draw_member_row(ctx: &mut PaintCtx<'_>, row: &MemberRow, view: &MemberRowView<'_>) {
    let theme = ctx.theme;
    let you = [(t("workspace-settings-you"), ChipStyle::Neutral)];
    let you_tag: Vec<(&str, ChipStyle)> = you
        .iter()
        .map(|(label, style)| (label.as_str(), *style))
        .collect();
    let extra = (row.grant_count > 0).then(|| {
        t_args(
            "workspace-settings-member-extra",
            &[("count", row.grant_count.into())],
        )
    });
    let role = shown_role(row);
    let (role_rect, remove_rect) = member_row_controls(row, view.local_user);
    let mut face = ListRowFace::new(view.name)
        .leading(RowLeading::Avatar(AvatarFace::new(view.name, &row.user)));
    if view.separator {
        face = face.separator(false);
    }
    if view.is_you {
        face = face.tags(&you_tag);
    }
    if let Some(extra) = &extra {
        face = face.secondary(extra);
    }
    if role_rect.area() > 0.0 {
        face = face.trailing_width(row.row_rect.x1 - row.role_rect.x0);
    } else {
        // The owner's role is a fact of the row, not a choice.
        face = face.value(&role);
    }
    face.paint(ctx, row.row_rect);

    if role_rect.area() > 0.0 {
        let height = DropdownFace::height(ControlSize::Small, theme);
        DropdownFace::new(centred(role_rect, height), &role)
            .control_size(ControlSize::Small)
            .disabled(!view.can_manage)
            .paint(ctx.scene, ctx.transform, ctx.text, theme);
    }
    if remove_rect.area() > 0.0 {
        let height = ButtonFace::height(ControlSize::Small, theme);
        let remove = t("workspace-settings-remove");
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(centred(remove_rect, height), &remove)
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .disabled(!view.can_manage),
        );
    }
}

fn centred(slot: Rect, height: f64) -> Rect {
    Rect::new(
        slot.x0,
        slot.center().y - height / 2.0,
        slot.x1,
        slot.center().y + height / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::WorkspaceId;

    fn pane() -> Rect {
        Rect::new(0.0, 0.0, 640.0, 480.0)
    }

    #[test]
    fn the_ownership_button_says_what_it_will_do() {
        let (owner_key, owner_public) = collab_types::generate_keypair();
        let (_, recipient_public) = collab_types::generate_keypair();
        let owner = hex::encode(owner_public.as_bytes());
        let recipient = hex::encode(recipient_public.as_bytes());
        let mut ws = Workspace::new(WorkspaceId(92), "Studio");
        ws.set_owner(owner.clone());
        ws.admit(recipient.clone(), CapRole::Editor);
        let label =
            |ws: &Workspace, who: &String| ownership_button(ws, who).map(|(label, _)| label);

        assert_eq!(label(&ws, &owner).as_deref(), Some("Transfer ownership"));
        assert_eq!(label(&ws, &recipient), None, "a member has nothing to do");

        ws.pending_ownership_transfer = collab_types::OwnershipTransferProposalV1::new(
            ws.room().to_string(),
            [9; 32],
            owner_public,
            recipient_public,
            u64::MAX,
            &owner_key,
        )
        .ok();
        assert_eq!(label(&ws, &owner).as_deref(), Some("Cancel transfer"));
        assert_eq!(label(&ws, &recipient).as_deref(), Some("Accept ownership"));
        let actions: Vec<String> = super::super::ownership_transfer_resource(&ws, &owner)
            .actions
            .iter()
            .map(|action| action.label.clone())
            .collect();
        let (_, leads_to) = ownership_button(&ws, &owner).expect("the owner may cancel");
        assert!(actions.contains(&leads_to), "{leads_to:?} in {actions:?}");
    }

    #[test]
    fn a_row_offers_only_what_can_be_done_and_states_facts_apart() {
        let mut ws = Workspace::new(WorkspaceId(93), "Studio");
        ws.set_owner("owner");
        ws.admit("ada".to_string(), CapRole::Editor);
        let rows = members_pane_rows(&ws, pane());
        let owner = rows.iter().find(|row| row.user == "owner").expect("owner");
        let ada = rows.iter().find(|row| row.user == "ada").expect("ada");

        // The owner, looking at their own row: neither a role choice nor
        // Remove.
        assert_eq!(
            member_row_controls(owner, "owner"),
            (Rect::ZERO, Rect::ZERO)
        );
        let (role, remove) = member_row_controls(ada, "owner");
        assert!(role.area() > 0.0 && remove.area() > 0.0);
        // Ada, looking at her own row, cannot remove herself here.
        let (_, remove) = member_row_controls(ada, "ada");
        assert_eq!(remove, Rect::ZERO);

        // Extra permissions are one fact of their own, never joined to the
        // role; there is no line for none.
        assert_eq!(ada.grant_count, 0);
        let extra = t_args("workspace-settings-member-extra", &[("count", 2.into())]);
        assert_eq!(extra, "2 extra permissions");
        for text in [
            extra,
            t("workspace-settings-people-header"),
            t_args("workspace-settings-members-count", &[("count", 4.into())]),
        ] {
            assert!(!text.contains('·'), "{text:?} joins facts");
        }
    }

    /// People stand in list rows, one after another, not cards with gaps.
    #[test]
    fn people_are_rows_of_one_list() {
        let mut ws = Workspace::new(WorkspaceId(94), "Studio");
        ws.set_owner("owner");
        ws.admit("ada".to_string(), CapRole::Editor);
        ws.admit("bo".to_string(), CapRole::Viewer);
        let rows = members_pane_rows(&ws, pane());
        for pair in rows.windows(2) {
            assert!((pair[0].row_rect.y1 - pair[1].row_rect.y0).abs() < f64::EPSILON);
        }
    }
}
