//! What the People pane offers: the ownership action named by what it does,
//! and which of a member's controls can be taken. The pane itself is laid
//! out by the lens's People page.

use vello::kurbo::Rect;

use super::{MemberRow, UserId, Workspace, public_key_member_id};
use crate::i18n::t;

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
    let (role, remove) = member_row_offers(row.is_workspace_owner, &row.user, local_user);
    (
        if role { row.role_rect } else { Rect::ZERO },
        if remove { row.remove_rect } else { Rect::ZERO },
    )
}

/// Whether a member's row offers their role and Remove, by the rules
/// [`member_row_controls`] states.
#[must_use]
pub fn member_row_offers(is_workspace_owner: bool, user: &str, local_user: &str) -> (bool, bool) {
    (
        !is_workspace_owner,
        !is_workspace_owner && user != local_user,
    )
}

#[cfg(test)]
mod tests {
    use super::super::{CapRole, members_pane_rows};
    use super::*;
    use crate::i18n::t_args;
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
}
