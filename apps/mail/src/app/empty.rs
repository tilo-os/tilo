//! A window with no mail: the account route it names, so an empty Mail still
//! leads to where an account is added and checked.

use super::{ACCOUNT_ROUTE, MailApp};
use ui::widget::Widget;

impl MailApp {
    /// The semantics of a window that has no mail yet. The onboarding page is
    /// the window's content, and its own accessible node names the heading and
    /// the action a person can take; publishing the frame alone would leave an
    /// empty window that names no control at all.
    pub(super) fn empty_window_accessibility(&self) -> ui::a11y::AccessNode {
        let page = self.first_run.accessibility();
        ui::a11y::AccessNode::new(ui::a11y::Role::Window)
            .with_name(crate::i18n::t("mail-title"))
            .with_children(page.into_iter().collect())
    }

    /// The account route an empty mailbox names: the one control that leads
    /// out of a mailbox no mail has arrived in.
    pub(super) fn account_route_node() -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        AccessNode::new(Role::Button)
            .with_name(crate::i18n::t("mail-account-settings"))
            .with_action_target(ACCOUNT_ROUTE)
            .with_state(AccessState::new().focusable(true))
    }
}
