//! The vault window's controls on its items, bound to what the vault
//! publishes: a press, a screen reader, the palette and an agent reach the
//! same effect under the same contract. The vault is this device's, so every
//! one of them is an effect; none writes a document.

use std::sync::LazyLock;

use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{Effect, Resource};

use crate::actions::PasswordsEffect;
use crate::resource::Passwords;

/// The vault as the palette, an agent and a control see it: what the vault
/// publishes, with its effects beside it.
static VAULT: LazyLock<ResourceSpec> = LazyLock::new(|| {
    let mut vault =
        ui::tilo_resource::publish::<Passwords>(Passwords::KIND, Passwords::LABEL, &Passwords::default());
    vault.actions.extend(PasswordsEffect::specs());
    vault
});

/// A control invoking `effect`, which names the item it acts on.
pub(crate) fn effect(effect: &PasswordsEffect) -> Option<ActionBinding<()>> {
    let spec = VAULT.actions.iter().find(|spec| spec.id == effect.id())?;
    ActionBinding::static_json(&VAULT, spec, effect.arguments()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::{ConfirmationPolicy, OperationKind};

    /// The item's controls are bound to effects the vault publishes: none is
    /// a document change, and the two that hand out or destroy a secret ask
    /// the person first.
    #[test]
    fn item_controls_bind_published_effects() {
        let item = "the-item".to_owned();
        for (binding, id) in [
            (
                effect(&PasswordsEffect::CopyIdentity {
                    item: item.clone(),
                }),
                "copy-identity",
            ),
            (
                effect(&PasswordsEffect::CopyOrigins {
                    item: item.clone(),
                }),
                "copy-origins",
            ),
            (effect(&PasswordsEffect::Reveal { item: item.clone() }), "reveal"),
            (effect(&PasswordsEffect::Hide { item }), "hide"),
            (
                effect(&PasswordsEffect::RevokeAccess {
                    grant: "the-grant".to_owned(),
                }),
                "revoke-access",
            ),
        ] {
            let binding = binding.unwrap_or_else(|| panic!("the vault publishes `{id}`"));
            assert_eq!(binding.address().action.as_str(), id);
            assert_eq!(binding.address().resource.as_str(), Passwords::KIND);
            let spec = VAULT
                .actions
                .iter()
                .find(|spec| spec.id == id)
                .unwrap_or_else(|| panic!("`{id}` is published"));
            assert_ne!(spec.kind, OperationKind::Document, "{id}");
        }
        let confirmation = |id: &str| {
            VAULT
                .actions
                .iter()
                .find(|spec| spec.id == id)
                .map(|spec| spec.contract.confirmation.clone())
        };
        assert_eq!(confirmation("reveal"), Some(ConfirmationPolicy::Confirm));
        assert_eq!(
            confirmation("revoke-access"),
            Some(ConfirmationPolicy::Confirm)
        );
        assert_eq!(confirmation("copy-identity"), Some(ConfirmationPolicy::None));
        assert_eq!(confirmation("hide"), Some(ConfirmationPolicy::None));
    }
}
