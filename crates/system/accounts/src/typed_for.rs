//! Where an account's secret was typed for.
//!
//! A person types a password for the servers their account names at that
//! moment. The account's settings can come to name other servers later: by
//! their own edit, on another device of theirs, by whoever is let change
//! the account. A secret kept under the account's id alone would follow it
//! there and be sent to a server it was never typed for.
//!
//! So the vault keeps, beside an account's secret, the servers it was typed
//! for ([`SecretVault::put_for`]), and joins the secret to an account only
//! while every server the account's settings name is one of them
//! ([`SecretVault::get_for`]). Whatever signs in with a kept secret joins
//! it so.
//!
//! A secret put with no word of where it was typed for (one kept before the
//! vault remembered, one an import brought) has no servers on it. It is
//! bound the first time its account is joined by code that can write the
//! vault ([`SecretVault::bind_first`]), to the servers the account names
//! then, and goes to no other from then on. Beside the servers the vault
//! keeps when it bound them, so that the person can read where and since
//! which day a secret of theirs is bound ([`SecretVault::typed_for`]).

use std::collections::BTreeSet;

use crate::vault_items::Place;
use crate::{Account, AccountId, Secret, SecretVault};

/// Under this, beside an account's secret, the vault keeps the servers the
/// secret was typed for.
const TYPED_FOR: &str = "tilo-secret-typed-for:";

fn beside(id: &AccountId) -> AccountId {
    AccountId::new(format!("{TYPED_FOR}{}", id.as_str()))
}

/// What the vault keeps beside an account's secret: the servers the secret
/// was typed for, and when it was bound to them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypedFor {
    /// Each as [`servers_of`] names it.
    pub servers: BTreeSet<String>,
    /// When it was bound, in seconds since 1970: its day is what the
    /// account's form says.
    #[serde(default)]
    pub since: u64,
}

/// The servers the settings of `account` send its secret to: a mail
/// account's incoming and outgoing server, each by name and port, and the
/// origin of the address a calendar, `WebDAV` or Nextcloud account names.
/// An address that cannot be read as one is a server of its own, which only
/// the same writing names again.
#[must_use]
pub fn servers_of(account: &Account) -> BTreeSet<String> {
    let settings = &account.config;
    let mut servers = BTreeSet::new();
    for (scheme, name, port_setting, usual) in [
        ("imap", "imap_host", "imap_port", 993),
        ("smtp", "smtp_host", "smtp_port", 465),
    ] {
        let Some(server) = settings
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|server| !server.trim().is_empty())
        else {
            continue;
        };
        let port = settings
            .get(port_setting)
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(usual);
        servers.insert(
            Place::server(scheme, server, port)
                .map_or_else(|_| format!("unread:{scheme}:{server}:{port}"), String::from),
        );
    }
    for at in ["/server_url", "/base_url", "/calendar/server_url"] {
        let Some(address) = settings
            .pointer(at)
            .and_then(serde_json::Value::as_str)
            .filter(|address| !address.trim().is_empty())
        else {
            continue;
        };
        servers.insert(
            Place::parse(address)
                .ok()
                .filter(Place::is_web)
                .map_or_else(|| format!("unread:{address}"), String::from),
        );
    }
    servers
}

impl SecretVault {
    /// Keep `secret` as the secret of `account`, typed for the servers its
    /// settings name now. In memory until the vault is saved.
    pub fn put_for(&mut self, account: &Account, secret: &Secret) {
        self.put(&account.id, secret);
        self.bind(account);
    }

    /// Keep `secret` under `id` as typed just now, with no word yet of the
    /// servers it was typed for: it is bound where its account is first
    /// joined ([`Self::bind_first`]), also where the same secret was kept
    /// for other servers before.
    pub fn put_anew(&mut self, id: &AccountId, secret: &Secret) {
        self.typed_for_nowhere(id);
        self.put(id, secret);
    }

    /// The secret of `account`, while every server its settings name is one
    /// the secret was typed for. A secret with no servers on it yet is
    /// given: [`Self::bind_first`] binds it.
    #[must_use]
    pub fn get_for(&self, account: &Account) -> Option<Secret> {
        if self.typed_elsewhere(account) {
            return None;
        }
        self.get(&account.id)
    }

    /// Whether a secret is kept for `account` that was typed for other
    /// servers than its settings name now.
    #[must_use]
    pub fn typed_elsewhere(&self, account: &Account) -> bool {
        self.typed_for(&account.id)
            .is_some_and(|typed| !servers_of(account).is_subset(&typed.servers))
    }

    /// Bind the secret of `account`, where one is kept with no servers on
    /// it yet, to the servers the account's settings name now. Whether it
    /// bound one; in memory until the vault is saved.
    pub fn bind_first(&mut self, account: &Account) -> bool {
        if !self.contains(&account.id) || self.contains(&beside(&account.id)) {
            return false;
        }
        self.bind(account);
        true
    }

    /// Forget where the secret under `id` was typed for.
    pub(crate) fn typed_for_nowhere(&mut self, id: &AccountId) {
        if id.as_str().starts_with(TYPED_FOR) {
            return;
        }
        let beside = beside(id);
        if self.contains(&beside) {
            self.remove(&beside);
        }
    }

    fn bind(&mut self, account: &Account) {
        let typed = TypedFor {
            servers: servers_of(account),
            since: tilo_clock::unix_now(),
        };
        self.put(
            &beside(&account.id),
            &Secret {
                password: String::new(),
                token: serde_json::to_string(&typed).ok(),
            },
        );
    }

    /// The servers the secret under `id` was typed for and when it was
    /// bound to them, where the vault says. What cannot be read says no
    /// server, so that it is joined to no account that names one.
    #[must_use]
    pub fn typed_for(&self, id: &AccountId) -> Option<TypedFor> {
        let kept = self.get(&beside(id))?;
        Some(
            kept.token
                .as_deref()
                .and_then(|typed| serde_json::from_str(typed).ok())
                .unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, kind: &str, settings: serde_json::Value) -> Account {
        let mut account = Account::new(AccountId::from(id), kind);
        account.config = settings;
        account
    }

    fn calendar(address: &str) -> Account {
        account(
            "me@dav",
            "caldav",
            serde_json::json!({ "server_url": address, "username": "me" }),
        )
    }

    /// A mail account as an address this code does not know fills it in:
    /// `imap.` and `smtp.` of its domain.
    fn mail(address: &str) -> Account {
        let mut settings = crate::email::AccountConfig {
            email: address.into(),
            ..Default::default()
        };
        settings.autofill_from_email();
        account("me@example.test", "email", settings.to_value().unwrap())
    }

    fn servers(account: &Account) -> Vec<String> {
        servers_of(account).into_iter().collect()
    }

    #[test]
    fn an_accounts_servers_are_its_mail_servers_and_the_origin_of_its_address() {
        assert_eq!(
            servers(&mail("me@example.test")),
            [
                "imap://imap.example.test:993",
                "smtp://smtp.example.test:465"
            ]
        );
        assert_eq!(
            servers(&calendar("https://dav.example.test/dav/me/")),
            ["https://dav.example.test"]
        );
        assert_eq!(
            servers(&calendar("https://dav.example.test:8443/dav/me/")),
            ["https://dav.example.test:8443"]
        );
        assert_eq!(
            servers(&account(
                "files",
                "webdav",
                serde_json::json!({ "base_url": "http://files.example.test/remote.php" }),
            )),
            ["http://files.example.test"]
        );
        // A Nextcloud account with calendars names its own address and theirs.
        assert_eq!(
            servers(&account(
                "cloud",
                "nextcloud",
                serde_json::json!({
                    "server_url": "https://cloud.example.test",
                    "calendar": { "server_url": "https://dav.cloud.example.test/dav" },
                }),
            )),
            [
                "https://cloud.example.test",
                "https://dav.cloud.example.test"
            ]
        );
        // What cannot be read as an address is a server of its own.
        assert_eq!(
            servers(&calendar("dav.example.test/dav")),
            ["unread:dav.example.test/dav"]
        );
        assert!(servers(&account("id", "tilo.identity", serde_json::json!({}))).is_empty());
    }

    #[test]
    fn a_secret_is_joined_only_to_an_account_that_names_the_servers_it_was_typed_for() {
        let root = std::env::temp_dir().join(format!("tilo-typed-for-{}", uuid::Uuid::new_v4()));
        let typed = Secret::password("typed-for-dav-example");
        let at = calendar("https://dav.example.test/dav/me/");
        let elsewhere = calendar("https://dav.elsewhere.example/dav/me/");
        let mut vault = SecretVault::open_in(&root);
        assert_eq!(vault.typed_for(&at.id), None);
        tilo_clock::with_wall_ms(1_795_003_200_000, || vault.put_for(&at, &typed));
        vault.try_save().unwrap();

        // Read from the file again, as another process reads it.
        let mut vault = SecretVault::open_in(&root);
        assert_eq!(vault.get_for(&at), Some(typed.clone()));
        assert_eq!(
            vault.typed_for(&at.id),
            Some(TypedFor {
                servers: servers_of(&at),
                since: 1_795_003_200,
            }),
            "the person can read where it is bound, and since noon of 18 November 2026"
        );
        assert_eq!(
            vault.get_for(&calendar("https://dav.example.test/other/")),
            Some(typed.clone()),
            "another path on its server is still its server"
        );
        assert_eq!(vault.get_for(&elsewhere), None);
        assert!(vault.typed_elsewhere(&elsewhere));
        assert!(!vault.bind_first(&elsewhere), "it is bound once");
        assert_eq!(vault.get_for(&mail("me@example.test")), None);

        // Put again as it is, with no word of its servers, it stays theirs.
        vault.put(&at.id, &typed);
        assert_eq!(vault.get_for(&elsewhere), None);

        // Another secret under the id, with no word of its servers, is
        // typed for none yet. Where it is first joined, it is bound.
        let other = Secret::password("put-with-no-word");
        vault.put(&at.id, &other);
        assert!(!vault.typed_elsewhere(&elsewhere));
        assert!(vault.bind_first(&elsewhere));
        assert_eq!(vault.get_for(&elsewhere), Some(other.clone()));
        assert_eq!(vault.get_for(&at), None);

        // Typed anew, the same secret is bound where it is joined next.
        vault.put_anew(&at.id, &other);
        assert!(vault.bind_first(&at));
        assert_eq!(vault.get_for(&at), Some(other));
        assert_eq!(vault.get_for(&elsewhere), None);

        // Removed, nothing of it stays.
        vault.remove(&at.id);
        assert!(!vault.contains(&beside(&at.id)));
        assert_eq!(vault.get_for(&at), None);
        vault.try_save().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
