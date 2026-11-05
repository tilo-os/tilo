//! What the People tab offers: the ownership action named by what it does,
//! and which of a member's controls can be taken. The tab itself is drawn by
//! the sheet.

use super::{UserId, Workspace, public_key_member_id};
use crate::i18n::t;

/// What the People tab's ownership button does for `local_user` now, by
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

/// Whether a member's row offers their role and Remove: never for the
/// owner, whose role is a fact of the workspace, and never Remove for
/// oneself.
#[must_use]
pub fn member_row_offers(is_workspace_owner: bool, user: &str, local_user: &str) -> (bool, bool) {
    (
        !is_workspace_owner,
        !is_workspace_owner && user != local_user,
    )
}

#[cfg(test)]
mod tests {
    use super::super::CapRole;
    use super::*;
    use crate::i18n::t_args;
    use tilo_desktop_model::WorkspaceSlot;

    #[test]
    fn the_ownership_button_says_what_it_will_do() {
        let (owner_key, owner_public) = collab_types::generate_keypair();
        let (_, recipient_public) = collab_types::generate_keypair();
        let owner = hex::encode(owner_public.as_bytes());
        let recipient = hex::encode(recipient_public.as_bytes());
        let mut ws = Workspace::new(WorkspaceSlot(92), "Studio");
        ws.set_owner(owner.clone());
        ws.admit(recipient.clone(), CapRole::Editor);
        let label =
            |ws: &Workspace, who: &String| ownership_button(ws, who).map(|(label, _)| label);

        assert_eq!(label(&ws, &owner).as_deref(), Some("Transfer ownership"));
        assert_eq!(label(&ws, &recipient), None, "a member has nothing to do");

        ws.pending_ownership_transfer = collab_types::OwnershipTransferProposalV1::new(
            ws.id().to_string(),
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
        // The owner's role is a fact of their row, and no one removes the
        // owner or themselves here.
        assert_eq!(member_row_offers(true, "owner", "owner"), (false, false));
        assert_eq!(member_row_offers(false, "ada", "owner"), (true, true));
        assert_eq!(member_row_offers(false, "ada", "ada"), (true, false));

        // Extra permissions are one fact of their own, never joined to the
        // role; there is no line for none.
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
