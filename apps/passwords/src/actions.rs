//! What Passwords does with the vault, as a typed effect vocabulary, and the
//! live rows it publishes for the vault's items.
//!
//! The vault is this device's encrypted credential store
//! ([`accounts::SecretStore`]), never a document: every change to it is an
//! effect the surface answers. The palette, agents and scripts reach the same
//! handler the window's controls do.

use accounts::{SecretItem, SecretItemKind};
use ui::framework::LiveRow;
use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{self, Effect};

/// The prefix of an item row's id; the rest is the item's opaque vault id.
pub const ITEM_PREFIX: &str = "passwords:item:";

/// What Passwords does with the vault.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "passwords.effect")]
pub enum PasswordsEffect {
    /// Show the current private credential permissions.
    #[effect(view, idempotent, kind = "read", label = "passwords-show-access")]
    ShowAccess,
    /// Revoke one current credential relationship by its opaque permission ID.
    #[effect(destructive, idempotent, label = "passwords-revoke-access")]
    RevokeAccess { grant: String },
    /// Approve one exact native request after protected confirmation.
    #[effect(attended, privileged, label = "passwords-approve-access")]
    ApproveAccess { proposal: String, lifetime: String },
    /// Decline one pending request without granting access.
    #[effect(idempotent, kind = "read", label = "passwords-deny-access")]
    DenyAccess { proposal: String },
    /// New item.
    #[effect(kind = "read")]
    Add,
    /// Save.
    ///
    /// Saves the item open in the editor to the vault.
    Save,
    /// Cancel editing.
    #[effect(kind = "read")]
    Cancel,
    /// Generate strong password.
    ///
    /// Fills the editor's password with a new strong one.
    Generate,
    /// Edit.
    Edit {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Copy secret.
    ///
    /// Copies the item's password, key or note to the clipboard, which
    /// clears after 30 seconds.
    #[effect(attended, privileged, kind = "read")]
    Copy {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Copy username.
    ///
    /// Copies what identifies the person to the account — a login's username,
    /// an API key's account, a Wi-Fi network's name — to the clipboard, which
    /// clears after 30 seconds.
    #[effect(attended, kind = "read")]
    CopyIdentity {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Copy addresses.
    ///
    /// Copies the addresses the item signs in to, one per line, to the
    /// clipboard, which clears after 30 seconds.
    #[effect(attended, kind = "read")]
    CopyOrigins {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Reveal secret for 10 seconds.
    #[effect(attended, privileged, kind = "read")]
    Reveal {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Hide secret.
    ///
    /// Hides a revealed secret again before its ten seconds run out. The item
    /// stays open and nothing else changes.
    #[effect(view, kind = "read", idempotent)]
    Hide {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Delete item.
    #[effect(destructive)]
    Delete {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Favorite.
    ///
    /// Adds the item to Favorites, or removes it.
    #[effect(idempotent)]
    Favorite {
        #[param("Opaque id of the vault item")]
        item: String,
        #[param("Whether the item is a favorite")]
        favorite: bool,
    },
    /// Import passwords.
    ///
    /// Opens the system file chooser for a browser password CSV.
    #[effect(attended)]
    Import,
    /// Export CSV…
    ///
    /// Asks for confirmation, then writes every item unencrypted to a file
    /// the person chooses.
    #[effect(attended)]
    Export,
    /// Filter vault.
    #[effect(kind = "read")]
    Filter {
        #[param("all | favorites | login | api-key | note | wifi | other")]
        filter: String,
    },
    /// Open.
    ///
    /// Shows the item in the window, clearing a filter that hides it.
    #[effect(row, kind = "read")]
    Open {
        #[param("Opaque id of the vault item")]
        item: String,
    },
}

pub(crate) const GRANT_PREFIX: &str = "passwords:grant:";
pub(crate) const PROPOSAL_PREFIX: &str = "passwords:request:";
pub(crate) fn proposal_row(proposal: &accounts::CredentialProposal) -> LiveRow {
    LiveRow::new(
        format!("{PROPOSAL_PREFIX}{}", proposal.id),
        "passwords.credential-request",
        crate::CATALOG.t("passwords-credential-request"),
    )
    .operation(
        PasswordsEffect::ApproveAccess {
            proposal: proposal.id.clone(),
            lifetime: "session".into(),
        }
        .bound(),
    )
    .operation(
        PasswordsEffect::DenyAccess {
            proposal: proposal.id.clone(),
        }
        .bound(),
    )
}
pub(crate) fn grant_row(grant: &accounts::CredentialGrant) -> LiveRow {
    LiveRow::new(
        format!("{GRANT_PREFIX}{}", grant.id()),
        "passwords.credential-permission",
        crate::CATALOG.t("passwords-credential-permission"),
    )
    .operation(
        PasswordsEffect::RevokeAccess {
            grant: grant.id().into(),
        }
        .bound(),
    )
}

/// The published id of the vault item `id`.
pub fn item_resource_id(id: &str) -> String {
    format!("{ITEM_PREFIX}{id}")
}

/// One vault item as a live row. Rows feed system-wide search and
/// collaboration, so the encrypted vault metadata (a title, an account, an
/// origin) is not copied into it: it names the item by its opaque id and
/// kind, and no operation carries a secret.
pub fn item_row(item: &SecretItem) -> LiveRow {
    let id = item.id.0.clone();
    let (kind, name, copy, reveal) = match item.kind {
        SecretItemKind::Login => (
            "passwords.login",
            "Saved login",
            "Copy password",
            "Reveal password for 10 seconds",
        ),
        SecretItemKind::ApiKey => (
            "passwords.api-key",
            "Saved API key",
            "Copy API key",
            "Reveal API key for 10 seconds",
        ),
        SecretItemKind::SecureNote => (
            "passwords.note",
            "Saved secure note",
            "Copy note",
            "Reveal note for 10 seconds",
        ),
        SecretItemKind::Wifi => (
            "passwords.wifi",
            "Saved Wi-Fi password",
            "Copy Wi-Fi password",
            "Reveal password for 10 seconds",
        ),
        SecretItemKind::Other => (
            "passwords.secret",
            "Saved secret",
            "Copy secret",
            "Reveal secret for 10 seconds",
        ),
    };
    let mut copy_spec = PasswordsEffect::Copy { item: id.clone() }.bound();
    copy.clone_into(&mut copy_spec.type_mut().label);
    let mut reveal_spec = PasswordsEffect::Reveal { item: id.clone() }.bound();
    reveal.clone_into(&mut reveal_spec.type_mut().label);
    let favorite = crate::vault::is_favorite(item);
    let mut favorite_spec = PasswordsEffect::Favorite {
        item: id.clone(),
        favorite: !favorite,
    }
    .bound();
    if favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    }
    .clone_into(&mut favorite_spec.type_mut().label);
    LiveRow::new(item_resource_id(&id), kind, name)
        .operation(PasswordsEffect::Open { item: id.clone() }.bound())
        .operation(favorite_spec)
        .operation(PasswordsEffect::Edit { item: id.clone() }.bound())
        .operation(copy_spec)
        .operation(reveal_spec)
        .operation(PasswordsEffect::Delete { item: id }.bound())
}

/// A control's binding to `action` of the item's published row
/// ([`item_row`]): pressing it is what an agent addressing the item by its
/// opaque id does, and it names the item by that id alone.
pub(crate) fn item_binding(row: &ResourceSpec, action: &str) -> Option<ActionBinding<()>> {
    let spec = row.actions.iter().find(|spec| spec.id == action)?;
    ActionBinding::static_json(row, spec, serde_json::Map::new()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::SecretItem;

    #[test]
    fn item_rows_keep_encrypted_metadata_out_of_public_descriptors() {
        let mut item = SecretItem::login("Private bank", "ada@private.example", "not-public");
        item.add_origin("https://private.example").unwrap();
        let resource = item_row(&item).into_resource();
        let mut public = format!("{} {} {}", resource.id.0, resource.kind, resource.label);
        for action in &resource.actions {
            public.push_str(&action.label);
            let encoded = action.build_json(&serde_json::Map::new()).unwrap();
            public.push_str(&String::from_utf8_lossy(&encoded.payload));
        }

        assert_eq!(resource.label, "Saved login");
        for private in [
            "Private bank",
            "ada@private.example",
            "private.example",
            "not-public",
        ] {
            assert!(!public.contains(private), "resource leaked {private:?}");
        }
        let ids: Vec<&str> = resource.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            ["open", "favorite", "edit", "copy", "reveal", "delete"]
        );
        assert!(public.contains("Reveal password"));
        let delete = resource.actions.iter().find(|a| a.id == "delete").unwrap();
        assert_eq!(
            delete.contract.risk,
            ui::tilo_actions::RiskLevel::Destructive,
            "deleting asks first"
        );
        let copy = resource.actions.iter().find(|a| a.id == "copy").unwrap();
        assert_eq!(
            copy.contract.confirmation,
            ui::tilo_actions::ConfirmationPolicy::Confirm,
            "handing out a secret asks first"
        );
    }
}
