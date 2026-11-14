//! The person's mail accounts, kept where their workspace runs.
//!
//! A page has no mail connection and no vault of its own. Adding a mail
//! account from a browser therefore asks the host that runs the workspace:
//! it opens the server's session with the password once, keeps the account
//! and its secret in the workspace's own vault, and answers what it kept.
//! Nothing here carries a secret back: a reading names an account's address
//! and its servers, never its password.
//!
//! The same actions answer a device that keeps its own accounts, so the page
//! and the device read and change accounts through one vocabulary.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the host answers the person's mail accounts under.
pub const PROVIDER: &str = "tilo.accounts";
/// The resource the accounts are actions of.
pub const RESOURCE: &str = "accounts";
/// The mail accounts the workspace's vault keeps, as a [`MailAccounts`].
pub const LIST: &str = "list";
/// Add a mail account, once its server accepted its sign-in
/// ([`AddMailAccount`]), and answer it as a [`MailAccount`].
pub const ADD_MAIL: &str = "add-mail";
/// Remove a mail account and its secret (`account`), answering the accounts
/// that remain as a [`MailAccounts`].
pub const REMOVE: &str = "remove";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// One mail account, as [`LIST`] and [`ADD_MAIL`] answer it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailAccount {
    /// The account's id, as [`REMOVE`] names it.
    pub id: String,
    pub address: String,
    pub display_name: String,
    pub incoming_host: String,
    pub incoming_port: u16,
    pub outgoing_host: String,
    pub outgoing_port: u16,
    /// Whether the workspace's vault keeps the account's sign-in. A host
    /// never answers the secret itself.
    pub has_secret: bool,
}

/// Every mail account the workspace's vault keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailAccounts {
    pub accounts: Vec<MailAccount>,
}

/// A mail account to add. The password travels here and nowhere back: the
/// host keeps it in the workspace's vault and answers only [`MailAccount`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMailAccount {
    pub address: String,
    #[serde(default)]
    pub display_name: String,
    /// The sign-in name, when it is not the address itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub incoming_host: String,
    #[serde(default = "default_imaps_port")]
    pub incoming_port: u16,
    pub outgoing_host: String,
    #[serde(default = "default_submissions_port")]
    pub outgoing_port: u16,
    /// The account's sign-in.
    pub password: String,
}

impl std::fmt::Debug for AddMailAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddMailAccount")
            .field("address", &self.address)
            .field("display_name", &self.display_name)
            .field("username", &self.username)
            .field("incoming_host", &self.incoming_host)
            .field("incoming_port", &self.incoming_port)
            .field("outgoing_host", &self.outgoing_host)
            .field("outgoing_port", &self.outgoing_port)
            .finish_non_exhaustive()
    }
}

const fn default_imaps_port() -> u16 {
    993
}

const fn default_submissions_port() -> u16 {
    465
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A password never reaches a log: the account read for a person hides
    /// it, and the reading the host answers has no field to carry one.
    #[test]
    fn nothing_read_back_carries_the_sign_in() {
        let adding = AddMailAccount {
            address: "ada@example.com".into(),
            display_name: "Ada".into(),
            username: None,
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.com".into(),
            outgoing_port: 465,
            password: "hunter2".into(),
        };
        let printed = format!("{adding:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("imap.example.com"), "{printed}");

        let read = MailAccount {
            id: "ada@example.com".into(),
            address: "ada@example.com".into(),
            display_name: "Ada".into(),
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.com".into(),
            outgoing_port: 465,
            has_secret: true,
        };
        let json = serde_json::to_value(&read).unwrap();
        assert!(json.get("password").is_none());
        assert_eq!(json["hasSecret"], serde_json::json!(true));
    }

    /// A host names its actions without having read a catalogue, as it names
    /// its billing account, and the name is the provider's resource.
    #[test]
    fn the_actions_are_addressed_without_a_catalogue() {
        assert_eq!(target(ADD_MAIL).provider, PROVIDER);
        assert_eq!(target(ADD_MAIL).address.resource, RESOURCE);
        assert_eq!(target(ADD_MAIL).address.action, ADD_MAIL);
    }

    /// An account added without a port signs in over the standard secure
    /// ports; a caller that names none is not refused.
    #[test]
    fn a_mail_account_defaults_to_the_secure_ports() {
        let adding: AddMailAccount = serde_json::from_value(serde_json::json!({
            "address": "ada@example.com",
            "incomingHost": "imap.example.com",
            "outgoingHost": "smtp.example.com",
            "password": "hunter2",
        }))
        .unwrap();
        assert_eq!(adding.incoming_port, 993);
        assert_eq!(adding.outgoing_port, 465);
    }
}
