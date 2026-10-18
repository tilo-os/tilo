//! The person's accounts as a record of their own: the accounts they
//! configured follow them to every device they sign in on, never to a
//! collaborator.
//!
//! [`PersonAccounts`] is a resource kept in the person's own room, a
//! [`SharedRecord`]: its owner app ([`ACCOUNTS_OWNER`], Settings) and Mail,
//! whose provider sign-in adds mail accounts, edit it through its actions;
//! every other app reads it. Each app keeps its copy of the one record. It holds what an account *is* — its type, its name,
//! its connection settings — and never a credential: secrets stay in this
//! device's [`SecretVault`](crate::SecretVault) under the account's id, so a
//! device that has not been signed in yet shows the account and asks for its
//! secret.
//!
//! Services that run outside any app (the storage daemon, the credential
//! custodian, the file manager's remote places) read this device's
//! [`AccountStore`] file. The editors keep that file a projection of the
//! record ([`PersonAccounts::project_in`]); it is never edited directly. A
//! file that is not a projection yet holds accounts configured on this
//! device before the record kept them: the first editor to open takes them
//! in once ([`PersonAccounts::taking_in`]).

use std::collections::BTreeMap;
use std::path::Path;

use tilo_resource::{Collection, Id, Invoke, Json, Model, Refusal, SharedRecord, resource};

use crate::{Account, AccountId, AccountStore, AccountTypeId};

/// The app that keeps the person's accounts.
pub const ACCOUNTS_OWNER: &str = "so.tilo.settings";

/// The other apps that change the person's accounts: Mail adds the mail
/// accounts its provider sign-in sets up.
pub const ACCOUNTS_EDITORS: &[&str] = &["so.tilo.mail"];

/// A Tilo identity's public connection metadata, edited in shared accounts.
/// Its config contains `userPublicKey` and an HTTPS `origin`, never credentials.
pub const TILO_IDENTITY_TYPE: &str = "tilo.identity";

/// Public fields needed to choose an existing browser identity. This is a
/// projection of Settings' account record, not a credential or admission grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAccount {
    /// The shared account's stable id.
    pub id: String,
    /// The person's chosen account name.
    pub display_name: String,
    /// Public root key matching the profile that holds the account record.
    pub user_public_key: String,
}

/// One account of the person's, without its secret.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PersonAccount {
    /// The kind of account (`"email"`, `"caldav"`, …).
    pub type_id: String,
    /// The name shown for it ("Work — Fastmail").
    pub display_name: String,
    /// Type-specific connection settings. Never a secret. Each account type
    /// shapes its own, so to this record they are one opaque value (`Json`),
    /// written whole by the account's editor.
    pub config: Json<serde_json::Value>,
    /// Whether the person uses it now.
    pub enabled: bool,
    /// Tints the account's avatar so several read as distinct.
    pub tint: u8,
}

/// Every account the person configured, and the one each type uses.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PersonAccounts {
    /// Keyed by the account's id, the id its secret is kept under.
    pub accounts: Collection<PersonAccount>,
    /// The active account id per type; a type without one uses its first.
    pub active: BTreeMap<String, String>,
}

#[resource(kind = "tilo.accounts", label = "Accounts")]
impl PersonAccounts {
    /// Project public browser choices from the existing typed account document.
    ///
    /// # Errors
    /// Refuses a document that cannot be read as Settings' account resource.
    pub fn browser_accounts_from_doc(
        doc: &tilo_resource::Doc,
        root: &str,
        origin: &str,
    ) -> Result<Vec<BrowserAccount>, tilo_resource::ModelError> {
        use tilo_resource::Resource as _;
        Ok(Self::read(doc)?.browser_accounts(root, origin))
    }

    /// Existing enabled identity accounts for this profile and exact host.
    /// Unknown config keys are not exported; secrets are never consulted.
    #[must_use]
    pub fn browser_accounts(&self, root: &str, origin: &str) -> Vec<BrowserAccount> {
        if root.len() != 64 || !root.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Vec::new();
        }
        let Ok(url) = url::Url::parse(origin) else {
            return Vec::new();
        };
        if url.scheme() != "https" || url.origin().ascii_serialization() != origin {
            return Vec::new();
        }
        self.accounts
            .iter()
            .filter(|(_, account)| {
                account.enabled
                    && account.type_id == TILO_IDENTITY_TYPE
                    && account
                        .config
                        .0
                        .get("userPublicKey")
                        .and_then(serde_json::Value::as_str)
                        == Some(root)
                    && account
                        .config
                        .0
                        .get("origin")
                        .and_then(serde_json::Value::as_str)
                        == Some(origin)
            })
            .take(64)
            .map(|(id, account)| BrowserAccount {
                id: id.as_str().to_owned(),
                display_name: account.display_name.chars().take(160).collect(),
                user_public_key: root.to_owned(),
            })
            .collect()
    }

    /// Publish this signed-in identity's public browser connection metadata.
    /// This changes the Settings account record only; it creates no credential,
    /// certificate, room membership or content key. Republishing preserves a
    /// person's disabled choice and chosen account name.
    ///
    /// # Errors
    /// Refuses malformed public metadata or an occupied identity account id.
    #[action]
    pub fn publish_browser_identity(
        &mut self,
        root: String,
        origin: String,
        display_name: String,
    ) -> Result<(), Refusal> {
        if root.len() != 64
            || !root.bytes().all(|byte| byte.is_ascii_hexdigit())
            || root != root.to_ascii_lowercase()
            || display_name.trim().is_empty()
            || display_name.chars().count() > 160
            || display_name.chars().any(char::is_control)
        {
            return Err(Refusal::from(
                "a bounded public identity and name are required",
            ));
        }
        let url =
            url::Url::parse(&origin).map_err(|_| Refusal::from("an HTTPS origin is required"))?;
        if url.scheme() != "https"
            || url.origin().ascii_serialization() != origin
            || origin.len() > 2048
        {
            return Err(Refusal::from("a canonical HTTPS origin is required"));
        }
        let id = format!("tilo-identity:{root}@{origin}");
        let key = Id::from_raw(id.clone());
        if let Some(existing) = self.accounts.get(&key) {
            if existing.type_id != TILO_IDENTITY_TYPE {
                return Err(Refusal::from("the identity account id is already in use"));
            }
            // No overwrite: removal/disable/name edits are ordinary Settings
            // actions. A normal sign-in must not silently reverse them.
            return Ok(());
        }
        self.save_account(
            id,
            TILO_IDENTITY_TYPE.into(),
            display_name,
            serde_json::json!({"userPublicKey": root, "origin": origin}),
            true,
            0,
        )
    }

    /// Save an account: add it, or replace the one with its id.
    #[action]
    pub fn save_account(
        &mut self,
        #[param("The account's id, which its secret is kept under")] id: String,
        #[param("The kind of account, such as email or caldav")] type_id: String,
        #[param("The name shown for it")] display_name: String,
        #[param("Its connection settings, without any secret")] config: serde_json::Value,
        #[param("Whether it is in use")] enabled: bool,
        #[param("Avatar tint")] tint: u8,
    ) -> Result<(), Refusal> {
        if id.trim().is_empty() || type_id.trim().is_empty() {
            return Err(Refusal::from("an account needs an id and a type"));
        }
        let key = Id::from_raw(id.clone());
        let account = PersonAccount {
            type_id: type_id.clone(),
            display_name,
            config: Json(config),
            enabled,
            tint,
        };
        if let Some(existing) = self.accounts.get_mut(&key) {
            *existing = account;
        } else {
            self.accounts.push(key, account);
        }
        self.active.entry(type_id).or_insert(id);
        Ok(())
    }

    /// Publish a reviewed new Signal identity without replacing another
    /// account. Names are labels; a configured own ACI requires its existing
    /// relinking path. This is public metadata, never a credential or grant.
    /// # Errors
    /// Refuses malformed identities, duplicate scopes or an occupied account ID.
    #[action]
    pub fn publish_signal_account(
        &mut self,
        id: String,
        display_name: String,
        aci: String,
        generation: u64,
    ) -> Result<(), Refusal> {
        for value in [&id, &aci] {
            if uuid::Uuid::parse_str(value)
                .map_err(|_| Refusal::from("a canonical Signal account identity is required"))?
                .to_string()
                != *value
            {
                return Err(Refusal::from(
                    "a canonical Signal account identity is required",
                ));
            }
        }
        if generation == 0
            || display_name.trim().is_empty()
            || display_name.chars().count() > 160
            || display_name.chars().any(char::is_control)
        {
            return Err(Refusal::from(
                "a bounded account name and link generation are required",
            ));
        }
        let scope = format!("signal:{aci}");
        if self.accounts.get(&Id::from_raw(&id)).is_some()
            || self.accounts.iter().any(|(_, account)| {
                account.type_id == "signal"
                    && account
                        .config
                        .0
                        .get("messaging_scope")
                        .and_then(serde_json::Value::as_str)
                        == Some(scope.as_str())
            })
        {
            return Err(Refusal::from(
                "this identity already has an account; use its existing setup",
            ));
        }
        self.save_account(
            id,
            "signal".into(),
            display_name,
            serde_json::json!({"messaging_scope": scope, "linking_generation": generation}),
            true,
            0,
        )
    }

    /// Publish a protected native SIM account. The scope is an opaque identity;
    /// modem paths, phone numbers and SIM identifiers stay in private custody.
    /// # Errors
    /// Refuses malformed identities, duplicate accounts or an invalid name/generation.
    #[action]
    pub fn publish_sms_account(
        &mut self,
        id: String,
        display_name: String,
        scope: String,
        generation: u64,
    ) -> Result<(), Refusal> {
        if uuid::Uuid::parse_str(&id)
            .map_err(|_| Refusal::from("a canonical SMS account identity is required"))?
            .to_string()
            != id
            || scope != format!("sms:{id}")
            || generation == 0
            || display_name.trim().is_empty()
            || display_name.chars().count() > 160
            || display_name.chars().any(char::is_control)
        {
            return Err(Refusal::from(
                "a canonical SIM account, bounded name and generation are required",
            ));
        }
        if self.accounts.get(&Id::from_raw(&id)).is_some() {
            return Err(Refusal::from(
                "this account already exists; use its existing setup",
            ));
        }
        self.save_account(
            id,
            "sms".into(),
            display_name,
            serde_json::json!({"messaging_scope": scope, "linking_generation": generation}),
            true,
            0,
        )
    }

    /// Publish the explicitly reviewed self identity of one native `WhatsApp`
    /// profile. The caller retains provider proof and credential authority;
    /// this record contains no credential, pairing payload or messaging state.
    /// # Errors
    /// Refuses invalid self identity, generation, name or an occupied account.
    #[action]
    pub fn publish_whatsapp_account(
        &mut self,
        id: String,
        display_name: String,
        jid: String,
        generation: u64,
    ) -> Result<(), Refusal> {
        if !uuid::Uuid::parse_str(&id).is_ok_and(|parsed| parsed.to_string() == id)
            || generation == 0
            || display_name.trim().is_empty()
            || display_name.chars().count() > 160
            || display_name.chars().any(char::is_control)
            || jid.len() > 128
            || !jid.split_once('@').is_some_and(|(user, server)| {
                server == "s.whatsapp.net"
                    && !user.is_empty()
                    && user.bytes().all(|c| c.is_ascii_digit())
            })
            || self.accounts.get(&Id::from_raw(&id)).is_some()
            || self.accounts.iter().any(|(_, row)| {
                row.type_id == "whatsapp"
                    && row
                        .config
                        .0
                        .get("whatsapp_jid")
                        .and_then(serde_json::Value::as_str)
                        == Some(jid.as_str())
            })
        {
            return Err(Refusal::from(
                "a fresh account and reviewed WhatsApp self identity are required",
            ));
        }
        let scope = format!("whatsapp:{id}");
        self.save_account(
            id, "whatsapp".into(), display_name,
            serde_json::json!({"messaging_scope":scope, "linking_generation":generation, "whatsapp_jid":jid}),
            true, 0,
        )
    }

    /// Publish the original personal Tilo sender with an opaque private scope.
    /// Neither workspace membership nor a label authorizes this account.
    /// # Errors
    /// Refuses malformed identities, duplicate accounts or an invalid name/generation.
    #[action]
    pub fn publish_tilo_account(
        &mut self,
        id: String,
        display_name: String,
        scope: String,
        generation: u64,
    ) -> Result<(), Refusal> {
        if uuid::Uuid::parse_str(&id).is_ok_and(|uuid| uuid.is_nil())
            || uuid::Uuid::parse_str(&id)
                .map_err(|_| Refusal::from("a canonical Tilo account identity is required"))?
                .to_string()
                != id
            || scope != format!("tilo:{id}")
            || generation == 0
            || display_name.trim().is_empty()
            || display_name.chars().count() > 160
            || display_name.chars().any(char::is_control)
        {
            return Err(Refusal::from(
                "a canonical Tilo sender, bounded name and generation are required",
            ));
        }
        if self
            .accounts
            .iter()
            .any(|(_, account)| account.type_id == "tilo")
        {
            return Err(Refusal::from("the original Tilo sender already exists"));
        }
        if self.accounts.get(&Id::from_raw(&id)).is_some() {
            return Err(Refusal::from(
                "this account already exists; use its existing setup",
            ));
        }
        self.save_account(
            id,
            "tilo".into(),
            display_name,
            serde_json::json!({"messaging_scope": scope, "linking_generation": generation}),
            true,
            0,
        )
    }

    /// Remove account.
    #[action]
    pub fn remove_account(&mut self, account: Id) -> Result<(), Refusal> {
        let removed = self
            .accounts
            .remove(&account)
            .ok_or_else(|| Refusal::from("there is no such account"))?;
        if self.active.get(&removed.type_id).map(String::as_str) == Some(account.as_str()) {
            match self
                .accounts
                .iter()
                .find(|(_, other)| other.type_id == removed.type_id)
            {
                Some((next, _)) => {
                    self.active
                        .insert(removed.type_id.clone(), next.as_str().to_owned());
                }
                None => {
                    self.active.remove(&removed.type_id);
                }
            }
        }
        Ok(())
    }

    /// Use an account for its type.
    #[action]
    pub fn set_active_account(&mut self, account: Id) -> Result<(), Refusal> {
        let type_id = self
            .accounts
            .get(&account)
            .ok_or_else(|| Refusal::from("there is no such account"))?
            .type_id
            .clone();
        self.active.insert(type_id, account.as_str().to_owned());
        Ok(())
    }
}

impl SharedRecord for PersonAccounts {
    const OWNER: &'static str = ACCOUNTS_OWNER;
    const EDITORS: &'static [&'static str] = ACCOUNTS_EDITORS;
}

impl PersonAccounts {
    /// The edits that take in the accounts of `device`, a store this device
    /// kept before the record did: each account the record lacks, and the
    /// active one of a type the record has none of. Once the file is the
    /// record's projection it brings nothing back — an account removed on
    /// another device stays removed — except into a copy of the record that
    /// holds no account yet, which takes what this device shows.
    #[must_use]
    pub fn taking_in(&self, device: &AccountStore) -> Vec<Invoke<Self>> {
        if device.is_projection() && !self.accounts.is_empty() {
            return Vec::new();
        }
        let mut edits = Vec::new();
        for account in device.all() {
            if !self.accounts.contains(&Id::from_raw(account.id.0.clone())) {
                edits.push(Self::saving(account));
            }
        }
        for account in device.all() {
            let type_id = account.type_id.as_str();
            if !self.active.contains_key(type_id)
                && device.active_id(&account.type_id) == Some(&account.id)
            {
                edits.push(Self::SET_ACTIVE_ACCOUNT.with(Id::from_raw(account.id.0.clone())));
            }
        }
        edits
    }

    /// The accounts as the device's [`AccountStore`] holds them: what
    /// readers that work with the store's types take.
    #[must_use]
    pub fn store(&self) -> AccountStore {
        let mut store = AccountStore::new();
        for (id, account) in self.accounts.iter() {
            store.upsert(Account {
                id: AccountId::new(id.as_str()),
                type_id: AccountTypeId::new(account.type_id.clone()),
                display_name: account.display_name.clone(),
                config: account.config.0.clone(),
                enabled: account.enabled,
                tint: account.tint,
                owner: None,
            });
        }
        for (type_id, id) in &self.active {
            store.set_active(
                &AccountTypeId::new(type_id.clone()),
                &AccountId::new(id.clone()),
            );
        }
        store.as_projection()
    }

    /// The edit that saves `account` as it is.
    #[must_use]
    pub fn saving(account: &Account) -> Invoke<Self> {
        // An account without connection settings is saved with none: the
        // door refuses a missing value.
        let config = if account.config.is_null() {
            serde_json::json!({})
        } else {
            account.config.clone()
        };
        Self::SAVE_ACCOUNT.with((
            account.id.0.clone(),
            account.type_id.0.clone(),
            account.display_name.clone(),
            config,
            account.enabled,
            account.tint,
        ))
    }

    /// Keep this device's account file under `config_root` the projection
    /// of the person's accounts, for the services that read it. Writes only
    /// when it differs. Whether it wrote.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn project_in(&self, config_root: &Path) -> std::io::Result<bool> {
        let store = self.store();
        if AccountStore::load_in(config_root) == store {
            return Ok(false);
        }
        store.save_in(config_root)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Replica;

    #[test]
    fn publish_signal_account_keeps_names_separate_and_scope_exact() {
        let mut record = Replica::<PersonAccounts>::new();
        for (id, aci) in [
            (
                "11111111-1111-4111-8111-111111111111",
                "22222222-2222-4222-8222-222222222222",
            ),
            (
                "33333333-3333-4333-8333-333333333333",
                "44444444-4444-4444-8444-444444444444",
            ),
        ] {
            record
                .apply(PersonAccounts::PUBLISH_SIGNAL_ACCOUNT.with((
                    id.into(),
                    "Same name".into(),
                    aci.into(),
                    7,
                )))
                .unwrap();
            let state = record.state().unwrap();
            let account = state.accounts.get(&Id::from_raw(id)).unwrap();
            assert_eq!(account.type_id, "signal");
            assert_eq!(
                account.config.0,
                serde_json::json!({"messaging_scope":format!("signal:{aci}"),"linking_generation":7})
            );
        }
        assert_eq!(record.state().unwrap().accounts.len(), 2);
    }

    #[test]
    fn publish_signal_account_refuses_duplicate_id_or_scope_without_replacing_state() {
        let mut record = Replica::<PersonAccounts>::new();
        let id = "11111111-1111-4111-8111-111111111111";
        let aci = "22222222-2222-4222-8222-222222222222";
        record
            .apply(PersonAccounts::PUBLISH_SIGNAL_ACCOUNT.with((
                id.into(),
                "Original".into(),
                aci.into(),
                1,
            )))
            .unwrap();
        let before = record.state().unwrap();
        for (next_id, next_aci) in [
            (id, "33333333-3333-4333-8333-333333333333"),
            ("44444444-4444-4444-8444-444444444444", aci),
        ] {
            assert!(
                record
                    .apply(PersonAccounts::PUBLISH_SIGNAL_ACCOUNT.with((
                        next_id.into(),
                        "Replacement".into(),
                        next_aci.into(),
                        2
                    )))
                    .is_err()
            );
            assert_eq!(record.state().unwrap(), before);
        }
    }

    #[test]
    fn publish_signal_account_refuses_invalid_publication_without_creating_an_account() {
        let id = "11111111-1111-4111-8111-111111111111";
        let aci = "22222222-2222-4222-8222-222222222222";
        let mut record = Replica::<PersonAccounts>::new();
        for (next_id, name, next_aci, generation) in [
            ("account-name", "Work", aci, 1),
            (id, "Work", "not-an-aci", 1),
            (id, "Work", aci, 0),
            (id, "", aci, 1),
            (id, "line\nname", aci, 1),
        ] {
            assert!(
                record
                    .apply(PersonAccounts::PUBLISH_SIGNAL_ACCOUNT.with((
                        next_id.into(),
                        name.into(),
                        next_aci.into(),
                        generation
                    )))
                    .is_err()
            );
            assert!(record.state().unwrap().accounts.is_empty());
        }
    }

    #[test]
    fn browser_picker_projects_settings_accounts_without_crossing_identity_or_origin() {
        let root = "ab".repeat(32);
        let origin = "https://host.example";
        let mut record = Replica::<PersonAccounts>::new();
        for (id, key, host, enabled) in [
            ("work", root.clone(), origin, true),
            ("disabled", root.clone(), origin, false),
            ("other-root", "cd".repeat(32), origin, true),
            ("other-host", root.clone(), "https://other.example", true),
        ] {
            record
                .apply(PersonAccounts::SAVE_ACCOUNT.with((
                    id.into(),
                    TILO_IDENTITY_TYPE.into(),
                    id.into(),
                    serde_json::json!({
                        "userPublicKey": key, "origin": host, "privateField": "never-exported"
                    }),
                    enabled,
                    0,
                )))
                .unwrap();
        }
        let state = record.state().unwrap();
        let rows = state.browser_accounts(&root, origin);
        assert_eq!(
            rows,
            vec![BrowserAccount {
                id: "work".into(),
                display_name: "work".into(),
                user_public_key: root.clone()
            }]
        );
        assert!(
            !serde_json::to_string(&rows)
                .unwrap()
                .contains("privateField")
        );
        assert!(
            state
                .browser_accounts(&root, "http://host.example")
                .is_empty()
        );
        assert!(state.browser_accounts("invalid", origin).is_empty());
        record
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("work")))
            .unwrap();
        assert!(
            record
                .state()
                .unwrap()
                .browser_accounts(&root, origin)
                .is_empty()
        );
    }

    #[test]
    fn identity_publication_uses_the_shared_account_record_and_preserves_disabled_choices() {
        let root = "ab".repeat(32);
        let origin = "https://host.example";
        let mut record = Replica::<PersonAccounts>::new();
        let publication = PersonAccounts::PUBLISH_BROWSER_IDENTITY.with((
            root.clone(),
            origin.into(),
            "Work".into(),
        ));
        record.apply(publication.clone()).unwrap();
        record.apply(publication.clone()).unwrap();
        let choices = record.state().unwrap().browser_accounts(&root, origin);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].display_name, "Work");
        let id = choices[0].id.clone();
        let account = record
            .state()
            .unwrap()
            .store()
            .get(&id.clone().into())
            .unwrap()
            .clone();
        assert_eq!(
            account.config,
            serde_json::json!({"userPublicKey": root, "origin": origin})
        );
        record
            .apply(PersonAccounts::SAVE_ACCOUNT.with((
                id.clone(),
                TILO_IDENTITY_TYPE.into(),
                "Private".into(),
                account.config,
                false,
                0,
            )))
            .unwrap();
        record.apply(publication).unwrap();
        assert!(
            record
                .state()
                .unwrap()
                .browser_accounts(&root, origin)
                .is_empty()
        );
        assert_eq!(
            record
                .state()
                .unwrap()
                .store()
                .get(&id.into())
                .unwrap()
                .display_name,
            "Private"
        );
        for invalid in [
            "http://host.example",
            "https://host.example/path",
            "https://name:secret@host.example",
            "https://host.example/?key=value",
        ] {
            assert!(
                record
                    .apply(PersonAccounts::PUBLISH_BROWSER_IDENTITY.with((
                        root.clone(),
                        invalid.into(),
                        "Work".into()
                    )))
                    .is_err()
            );
        }
        assert_eq!(record.state().unwrap().store().all().len(), 1);
    }

    fn save(replica: &mut Replica<PersonAccounts>, id: &str, type_id: &str, host: &str) {
        replica
            .apply(PersonAccounts::SAVE_ACCOUNT.with((
                id.into(),
                type_id.into(),
                format!("{id} inbox"),
                serde_json::json!({ "host": host }),
                true,
                2,
            )))
            .unwrap();
    }

    #[test]
    fn accounts_are_saved_by_id_and_each_type_keeps_an_active_one() {
        let mut replica = Replica::<PersonAccounts>::new();
        save(&mut replica, "a@x.example", "email", "imap.x");
        save(&mut replica, "b@y.example", "email", "imap.y");
        save(&mut replica, "work", "caldav", "dav.example");
        save(&mut replica, "a@x.example", "email", "imap.new");

        let store = replica.state().unwrap().store();
        assert_eq!(
            store.list(&"email".into()).len(),
            2,
            "saving again replaces"
        );
        assert_eq!(
            store.get(&"a@x.example".into()).unwrap().config["host"],
            "imap.new"
        );
        assert_eq!(
            store.active(&"email".into()).unwrap().id,
            "a@x.example".into()
        );

        replica
            .apply(PersonAccounts::SET_ACTIVE_ACCOUNT.with(Id::from_raw("b@y.example")))
            .unwrap();
        assert_eq!(
            replica
                .state()
                .unwrap()
                .store()
                .active(&"email".into())
                .unwrap()
                .id,
            "b@y.example".into()
        );
        replica
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("b@y.example")))
            .unwrap();
        let store = replica.state().unwrap().store();
        assert_eq!(
            store.active(&"email".into()).unwrap().id,
            "a@x.example".into(),
            "removing the active account falls back to another"
        );
        assert!(
            replica
                .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("gone")))
                .is_err()
        );
    }

    #[test]
    fn the_device_file_is_a_projection_without_secrets() {
        let mut replica = Replica::<PersonAccounts>::new();
        save(&mut replica, "a@x.example", "email", "imap.x");
        let root =
            std::env::temp_dir().join(format!("tilo-person-accounts-{}", uuid::Uuid::new_v4()));
        let accounts = replica.state().unwrap();
        assert!(accounts.project_in(&root).unwrap());
        assert!(
            !accounts.project_in(&root).unwrap(),
            "unchanged, not rewritten"
        );
        assert_eq!(AccountStore::load_in(&root), accounts.store());
        let raw = std::fs::read_to_string(crate::store_path_in(&root)).unwrap();
        assert!(!raw.to_lowercase().contains("password"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn saving_a_store_account_round_trips() {
        let mut account = Account::new("cal-1", "caldav");
        account.display_name = "Work".into();
        account.config = serde_json::json!({ "url": "https://dav.example" });
        let mut replica = Replica::<PersonAccounts>::new();
        replica.apply(PersonAccounts::saving(&account)).unwrap();
        assert_eq!(
            replica.state().unwrap().store().get(&"cal-1".into()),
            Some(&account)
        );
    }

    #[test]
    fn a_device_file_is_taken_in_once_then_follows_the_record() {
        let mut replica = Replica::<PersonAccounts>::new();
        save(&mut replica, "roamed", "caldav", "dav.example");
        let mut device = AccountStore::new();
        device.upsert(Account::new("local@x.example", "email"));
        device.upsert(Account::new("roamed", "caldav"));
        for edit in replica.state().unwrap().taking_in(&device) {
            replica.apply(edit).unwrap();
        }
        let person = replica.state().unwrap().store();
        assert!(person.get(&"local@x.example".into()).is_some(), "taken in");
        assert_eq!(
            person.active(&"email".into()).unwrap().id,
            "local@x.example".into()
        );
        assert_eq!(
            person.get(&"roamed".into()).unwrap().config["host"],
            "dav.example",
            "the record's own account is kept as it is"
        );

        // Removed on another device: the projected file never brings it back.
        replica
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("local@x.example")))
            .unwrap();
        let projected = person;
        assert!(projected.is_projection());
        assert!(replica.state().unwrap().taking_in(&projected).is_empty());
    }
}
