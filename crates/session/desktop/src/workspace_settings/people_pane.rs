//! Which of a member's controls can be taken. The tab itself is drawn by the sheet.
//! A workspace's owner is structural: it is set when the workspace is made and is not a role the
//! workspace hands out, so the tab offers no ownership control of its own.

/// Whether a member's row offers their role and Remove: never for the owner, whose role is a fact
/// of the workspace, and never Remove for oneself.
#[must_use]
pub fn member_row_offers(is_workspace_owner: bool, user: &str, local_user: &str) -> (bool, bool) {
    (
        !is_workspace_owner,
        !is_workspace_owner && user != local_user,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{t, t_args};

    #[test]
    fn a_row_offers_only_what_can_be_done_and_states_facts_apart() {
        // The owner's role is a fact of their row, and no one removes the owner or themselves here.
        assert_eq!(member_row_offers(true, "owner", "owner"), (false, false));
        assert_eq!(member_row_offers(false, "ada", "owner"), (true, true));
        assert_eq!(member_row_offers(false, "ada", "ada"), (true, false));

        // Extra permissions are one fact of their own, never joined to the role; there is no line
        // for none.
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
