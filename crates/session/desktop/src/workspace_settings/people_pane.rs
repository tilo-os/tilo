//! The People pane of Workspace Settings, built from the design system's
//! primitives: the summary is a list row with the ownership actions named
//! by what they do, the people are list rows under a section header,
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
    ListRowFace, RowLeading, SectionHeaderFace, Tone,
};

use super::{
    CapRole, MemberRow, PANE_INTRO_GAP, PANE_INTRO_HEIGHT, PANE_PAD_TOP, PANE_PAD_X,
    SECTION_HEADER_HEIGHT, UserId, Workspace, draw_authority_note, draw_empty_state,
    member_row_name, members_authority_note, members_pane_rows, ordered_roster,
    ownership_transfer_status, role_label,
};
use crate::i18n::{t, t_args};

/// One of the People pane's ownership buttons, by what it does: each but
/// Decline asks to confirm first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipControl {
    /// Choose a member to offer the workspace to.
    Offer,
    /// Take the open offer back.
    TakeBack,
    /// Accept the offer made to this person.
    Accept,
    /// Decline the offer made to this person.
    Decline,
}

impl OwnershipControl {
    /// The button's label.
    #[must_use]
    pub fn label(self) -> String {
        t(match self {
            Self::Offer => "workspace-settings-ownership-transfer",
            Self::TakeBack => "workspace-settings-ownership-cancel-short",
            Self::Accept => "workspace-settings-ownership-accept",
            Self::Decline => "workspace-settings-ownership-decline",
        })
    }
}

/// The People pane's ownership buttons for `local_user` now, in reading
/// order: none when they can do nothing about ownership.
#[must_use]
pub fn ownership_buttons(ws: &Workspace, local_user: &UserId) -> Vec<OwnershipControl> {
    use tilo_desktop_model::{
        OWNERSHIP_ACCEPT_ACTION_ID, OWNERSHIP_CANCEL_ACTION_ID, OWNERSHIP_OFFER_ACTION_ID,
    };
    if ws.may_move(local_user, OWNERSHIP_ACCEPT_ACTION_ID) {
        vec![OwnershipControl::Decline, OwnershipControl::Accept]
    } else if ws.may_move(local_user, OWNERSHIP_CANCEL_ACTION_ID) {
        vec![OwnershipControl::TakeBack]
    } else if ws.may_move(local_user, OWNERSHIP_OFFER_ACTION_ID) {
        vec![OwnershipControl::Offer]
    } else {
        Vec::new()
    }
}

/// Whether the people of `ws` stay as they are for now: while the room is
/// on its way to its new owner's device, its old host takes no changes.
#[must_use]
pub fn members_frozen(ws: &Workspace) -> bool {
    matches!(
        ws.ownership.step,
        tilo_desktop_model::OwnershipStep::Moving { .. }
    )
}

/// The open offer of the room to `user`, by when it lapses.
fn offered_to(ws: &Workspace, user: &str) -> Option<String> {
    match &ws.ownership.step {
        tilo_desktop_model::OwnershipStep::Offered {
            recipient,
            expires_at_ns,
            ..
        } if hex::encode(recipient) == user => Some(super::offer_expiry(*expires_at_ns)),
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
    let frozen = members_frozen(ws);
    for (index, row) in rows.iter().enumerate() {
        let name = member_row_name(names, ws, local_user, &row.user);
        let can_manage = is_owner && !frozen && !row.is_workspace_owner && row.user != *local_user;
        draw_member_row(
            &mut ctx,
            row,
            &MemberRowView {
                name: &name,
                is_you: row.user == *local_user,
                can_manage,
                separator: index < last,
                local_user,
                offer_expires: offered_to(ws, &row.user),
            },
        );
    }

    let note = if frozen {
        Some(t("workspace-settings-members-moving-note"))
    } else {
        members_authority_note(is_owner)
    };
    if let Some(note) = note {
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
    let buttons = ownership_buttons(ws, local_user);
    let rects = super::ownership_action_rects(pane_rect, buttons.len());
    let mut face = ListRowFace::new(&count)
        .leading(RowLeading::Icon(Icon::Users))
        .secondary(&status)
        .secondary_lines(3);
    if let Some(first) = rects.first() {
        face = face.trailing_width(rect.x1 - first.x0);
    }
    face.paint(ctx, rect);
    let height = ButtonFace::height(ControlSize::Regular, theme);
    for (control, slot) in buttons.iter().zip(&rects) {
        let label = control.label();
        let role = if *control == OwnershipControl::Accept {
            ButtonRole::Primary
        } else {
            ButtonRole::Secondary
        };
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(centred(*slot, height), &label).role(role),
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
    /// When the open offer of the room to this member lapses, while there
    /// is one.
    offer_expires: Option<String>,
}

/// One person: avatar and name, "You" on the acting person's own row, their
/// extra permissions when they have any, and their role's dropdown and
/// Remove where those can be offered.
fn draw_member_row(ctx: &mut PaintCtx<'_>, row: &MemberRow, view: &MemberRowView<'_>) {
    let theme = ctx.theme;
    let mut tags = Vec::new();
    if view.is_you {
        tags.push((t("workspace-settings-you"), ChipStyle::Neutral));
    }
    if view.offer_expires.is_some() {
        tags.push((
            t("workspace-settings-ownership-offered-tag"),
            ChipStyle::Tinted(Tone::Info),
        ));
    }
    let tags: Vec<(&str, ChipStyle)> = tags
        .iter()
        .map(|(label, style)| (label.as_str(), *style))
        .collect();
    let extra = view.offer_expires.as_ref().map_or_else(
        || {
            (row.grant_count > 0).then(|| {
                t_args(
                    "workspace-settings-member-extra",
                    &[("count", row.grant_count.into())],
                )
            })
        },
        |expires| {
            Some(t_args(
                "workspace-settings-ownership-offer-expires",
                &[("expires", expires.clone().into())],
            ))
        },
    );
    let role = shown_role(row);
    let (role_rect, remove_rect) = member_row_controls(row, view.local_user);
    let mut face = ListRowFace::new(view.name)
        .leading(RowLeading::Avatar(AvatarFace::new(view.name, &row.user)));
    if view.separator {
        face = face.separator(false);
    }
    if !tags.is_empty() {
        face = face.tags(&tags);
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
    fn the_ownership_buttons_say_what_they_will_do() {
        use tilo_desktop_model::{OwnershipStep, RoomOwnership};
        let owner = hex::encode([3; 32]);
        let recipient = hex::encode([4; 32]);
        let mut ws = Workspace::new(WorkspaceId(92), "Studio");
        ws.set_owner(owner.clone());
        ws.admit(recipient.clone(), CapRole::Editor);
        let labels = |ws: &Workspace, who: &String| -> Vec<String> {
            ownership_buttons(ws, who)
                .into_iter()
                .map(OwnershipControl::label)
                .collect()
        };

        assert_eq!(labels(&ws, &owner), ["Transfer ownership"]);
        assert!(
            labels(&ws, &recipient).is_empty(),
            "a member has nothing to do"
        );

        ws.ownership.step = OwnershipStep::Offered {
            offer_id: [9; 32],
            owner: [3; 32],
            recipient: [4; 32],
            expires_at_ns: 1_800_000_000_000_000_000,
        };
        assert_eq!(labels(&ws, &owner), ["Take back offer"]);
        assert_eq!(labels(&ws, &recipient), ["Decline", "Accept ownership"]);

        ws.ownership = RoomOwnership {
            step: OwnershipStep::Moving { recipient: [4; 32] },
            handed_at: 7,
        };
        assert!(
            labels(&ws, &owner).is_empty(),
            "nothing moves a room on its way"
        );
        assert!(
            members_frozen(&ws),
            "nobody's role or place changes on its way"
        );
        assert!(labels(&ws, &recipient).is_empty());
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
