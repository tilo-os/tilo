//! The mailbox's controls, bound to what the mailbox publishes: each runs a
//! published action, so a press, a screen reader, the palette and an agent
//! reach the same handler under the same contract.

use std::sync::LazyLock;

use tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{Effect, Resource};

use crate::actions::MailEffect;
use crate::resource::Mail;

/// The mailbox resource as the palette, an agent and a control see it: what
/// the mailbox publishes, with its effects beside it.
static MAILBOX: LazyLock<ResourceSpec> = LazyLock::new(|| {
    let mut mailbox =
        ui::tilo_resource::publish::<Mail>(Mail::KIND, Mail::LABEL, &Mail::default());
    mailbox.actions.extend(MailEffect::specs());
    mailbox
});

/// A section header's button, taking the section it was pressed on and
/// opening Settings' External Accounts page.
pub fn account_settings() -> Option<ActionBinding<String>> {
    let effect = MailEffect::AccountSettings;
    let spec = MAILBOX
        .actions
        .iter()
        .find(|spec| spec.id == effect.id())?;
    ActionBinding::new(&MAILBOX, spec, |_section: String| {
        serde_json::Map::new()
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section header's button is bound to a published, non-document
    /// action of the mailbox.
    #[test]
    fn the_section_header_binds_the_published_settings_action() {
        let binding = account_settings().expect("the mailbox publishes account settings");
        assert_eq!(binding.address().action.as_str(), "account-settings");
        assert_eq!(binding.address().resource.as_str(), Mail::KIND);
        let spec = MAILBOX
            .actions
            .iter()
            .find(|spec| spec.id == "account-settings")
            .expect("published");
        assert_eq!(spec.kind, tilo_actions::OperationKind::View);
    }
}
