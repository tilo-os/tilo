//! What Passwords does with the vault, as a typed effect vocabulary, and the rows it
//! publishes for the vault's items. The vault is the person's encrypted credential store, on
//! their workspace's server or lent by their device, never a document: every change to it is an effect the
//! surface answers, and every row is an item of a declared type that offers that type's
//! effects, bound to the item.

use accounts::{SecretItem, SecretItemKind};
use ui::framework::LiveRow;
use ui::tilo_actions::ActionBinding;
use ui::tilo_resource;

pub const ITEM_PREFIX: &str = "passwords:item:";

/// A saved login: an item row of its kind, whose effects are
/// [`PasswordsEffect`]'s.
#[derive(tilo_resource::Row)]
#[row(kind = "passwords.login", label = "Login", effects = PasswordsEffect)]
pub struct Login;

/// A saved API key.
#[derive(tilo_resource::Row)]
#[row(kind = "passwords.api-key", label = "API key", effects = PasswordsEffect)]
pub struct ApiKey;

/// A saved secure note.
#[derive(tilo_resource::Row)]
#[row(kind = "passwords.note", label = "Secure note", effects = PasswordsEffect)]
pub struct SecureNote;

/// A saved Wi-Fi password.
#[derive(tilo_resource::Row)]
#[row(kind = "passwords.wifi", label = "Wi-Fi network", effects = PasswordsEffect)]
pub struct WifiNetwork;

/// A saved secret of any other kind.
#[derive(tilo_resource::Row)]
#[row(kind = "passwords.secret", label = "Secret", effects = PasswordsEffect)]
pub struct Secret;

/// An app's request for one vault item, which the person allows or declines.
#[derive(tilo_resource::Row)]
#[row(
    kind = "passwords.credential-request",
    label = "Credential request",
    effects = PasswordsEffect
)]
pub struct CredentialRequest;

/// One app's permission to an item until the person revokes it.
#[derive(tilo_resource::Row)]
#[row(
    kind = "passwords.credential-permission",
    label = "Credential permission",
    effects = PasswordsEffect
)]
pub struct CredentialPermission;

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "passwords.effect")]
pub enum PasswordsEffect {
    #[effect(view, idempotent, kind = "read", label = "passwords-show-access")]
    ShowAccess,
    #[effect(
        kind = "outside",
        on = CredentialPermission
    )]
    #[effect(destructive, idempotent, label = "passwords-revoke-access")]
    RevokeAccess { grant: String },
    #[effect(kind = "outside")]
    #[effect(attended, privileged, label = "passwords-approve-access")]
    ApproveAccess { proposal: String, lifetime: String },
    /// Allow the requesting app this session, as the request's row offers it.
    #[effect(
        kind = "outside",
        on = CredentialRequest
    )]
    #[effect(attended, privileged, label = "passwords-approve-access")]
    ApproveForSession { proposal: String },
    #[effect(
        idempotent,
        attended,
        kind = "read",
        on = CredentialRequest
    )]
    #[effect(label = "passwords-deny-access")]
    DenyAccess { proposal: String },
    #[effect(screen)]
    Add,
    #[effect(kind = "outside")]
    Save,
    /// The password is a secret parameter: it is never shown, logged or listed.
    #[effect(kind = "outside")]
    SaveLogin {
        #[param("What the login is called")]
        name: String,
        #[param("The address it signs in to")]
        origin: String,
        #[param("The username")]
        username: String,
        #[param(secret, "The password")]
        password: String,
    },
    #[effect(returns, kind = "read", idempotent)]
    ListLogins,
    #[effect(screen)]
    Cancel,
    #[effect(kind = "outside")]
    Generate,
    #[effect(row, screen, on = Login, on = ApiKey, on = SecureNote, on = WifiNetwork, on = Secret)]
    Open {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Adds the item to the person's favorites, or takes it out: an item's row offers
    /// whichever this item is not.
    #[effect(
        kind = "outside",
        idempotent,
        on = Login,
        on = ApiKey,
        on = SecureNote,
        on = WifiNetwork,
        on = Secret
    )]
    ToggleFavorite {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    #[effect(kind = "outside", on = Login, on = ApiKey, on = SecureNote, on = WifiNetwork, on = Secret)]
    Edit {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Copies the item's password, key or note to the clipboard, which clears after 30
    /// seconds.
    #[effect(
        attended,
        privileged,
        kind = "read",
        on = Login,
        on = ApiKey,
        on = SecureNote,
        on = WifiNetwork,
        on = Secret
    )]
    Copy {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Copies what identifies the person to the account — a login's username, an API key's
    /// account, a Wi-Fi network's name — to the clipboard, which clears after 30 seconds.
    #[effect(attended, kind = "read")]
    CopyIdentity {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Copies the addresses the item signs in to, one per line, to the clipboard, which
    /// clears after 30 seconds.
    #[effect(attended, kind = "read")]
    CopyOrigins {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    /// Reveal secret for 10 seconds.
    #[effect(
        attended,
        privileged,
        kind = "read",
        on = Login,
        on = ApiKey,
        on = SecureNote,
        on = WifiNetwork,
        on = Secret
    )]
    Reveal {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    #[effect(view, kind = "read", idempotent)]
    Hide {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    #[effect(
        kind = "outside",
        on = Login,
        on = ApiKey,
        on = SecureNote,
        on = WifiNetwork,
        on = Secret
    )]
    #[effect(destructive)]
    Delete {
        #[param("Opaque id of the vault item")]
        item: String,
    },
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    Favorite {
        #[param("Opaque id of the vault item")]
        item: String,
        #[param("Whether the item is a favorite")]
        favorite: bool,
    },
    #[effect(kind = "outside")]
    #[effect(attended)]
    Import,
    #[effect(
        kind = "outside",
        attended,
        privileged,
        returns,
        label = "passwords-import-device-vault"
    )]
    ImportDeviceVault,
    #[effect(kind = "outside")]
    #[effect(attended)]
    Export,
    #[effect(screen)]
    Filter {
        #[param("all | favorites | login | api-key | note | wifi | other")]
        filter: String,
    },
}

pub(crate) const GRANT_PREFIX: &str = "passwords:grant:";
pub(crate) const PROPOSAL_PREFIX: &str = "passwords:request:";

/// The item `value` names, whether it is the row's published id or the
/// vault's own.
pub(crate) fn item_of(value: &str) -> &str {
    value.strip_prefix(ITEM_PREFIX).unwrap_or(value)
}

/// The request `value` names, as a row's id or the request's own.
pub(crate) fn proposal_of(value: &str) -> &str {
    value.strip_prefix(PROPOSAL_PREFIX).unwrap_or(value)
}

/// The permission `value` names, as a row's id or the grant's own.
pub(crate) fn grant_of(value: &str) -> &str {
    value.strip_prefix(GRANT_PREFIX).unwrap_or(value)
}

pub(crate) fn proposal_row(proposal: &accounts::CredentialProposal) -> LiveRow {
    LiveRow::typed::<CredentialRequest>(
        format!("{PROPOSAL_PREFIX}{}", proposal.id),
        crate::CATALOG.t("passwords-credential-request"),
    )
}

pub(crate) fn grant_row(grant: &accounts::CredentialGrant) -> LiveRow {
    LiveRow::typed::<CredentialPermission>(
        format!("{GRANT_PREFIX}{}", grant.id()),
        crate::CATALOG.t("passwords-credential-permission"),
    )
}

pub fn item_resource_id(id: &str) -> String {
    format!("{ITEM_PREFIX}{id}")
}

/// How an item of `kind` names copying its secret and revealing it for a
/// while, which its row offers labelled.
pub(crate) fn copy_label(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::Login => "Copy password",
        SecretItemKind::ApiKey => "Copy API key",
        SecretItemKind::SecureNote => "Copy note",
        SecretItemKind::Wifi => "Copy Wi-Fi password",
        SecretItemKind::Other => "Copy secret",
    }
}

/// How an item of `kind` names revealing its secret for ten seconds.
pub(crate) fn reveal_label(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::Login | SecretItemKind::Wifi => "Reveal password for 10 seconds",
        SecretItemKind::ApiKey => "Reveal API key for 10 seconds",
        SecretItemKind::SecureNote => "Reveal note for 10 seconds",
        SecretItemKind::Other => "Reveal secret for 10 seconds",
    }
}

pub fn item_row(item: &SecretItem) -> LiveRow {
    let id = item_resource_id(&item.id.0);
    let row = match item.kind {
        SecretItemKind::Login => LiveRow::typed::<Login>(id, "Saved login"),
        SecretItemKind::ApiKey => LiveRow::typed::<ApiKey>(id, "Saved API key"),
        SecretItemKind::SecureNote => LiveRow::typed::<SecureNote>(id, "Saved secure note"),
        SecretItemKind::Wifi => LiveRow::typed::<WifiNetwork>(id, "Saved Wi-Fi password"),
        SecretItemKind::Other => LiveRow::typed::<Secret>(id, "Saved secret"),
    };
    // The row names its kind's secret, and the favorite press the way it goes.
    row.labelled(PasswordsEffect::COPY, copy_label(item.kind))
        .labelled(PasswordsEffect::REVEAL, reveal_label(item.kind))
        .labelled(
            PasswordsEffect::TOGGLE_FAVORITE,
            if crate::vault::is_favorite(item) {
                "Remove from favorites"
            } else {
                "Add to favorites"
            },
        )
}

/// A control's binding to `action` of the item's published row ([`item_row`]): pressing it
/// is what an agent addressing the item by its opaque id does, and it names the item by
/// that id alone.
pub(crate) fn item_binding(row: &LiveRow, action: &str) -> Option<ActionBinding<()>> {
    let spec = row.operations().iter().find(|spec| spec.id == action)?;
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
            [
                "open",
                "toggle-favorite",
                "edit",
                "copy",
                "reveal",
                "delete"
            ]
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
