//! The workspace's accounts as one record: the mail, calendar and messaging
//! accounts configured in it, which every member and device of the
//! workspace reads as the workspace's grants allow. Their secrets are never
//! in it.

use std::collections::BTreeMap;
use std::path::Path;

use tilo_resource::{
    ActionCx, Collection, Id, Invoke, Model, ModelError, Refusal, SharedResource, resource,
};

use crate::{Account, AccountId, AccountStore, AccountTypeId};

/// The app that keeps the person's accounts.
pub use tilo_app_id::apps::SETTINGS as ACCOUNTS_OWNER;

/// The other apps that change the person's accounts: Mail adds the mail
/// accounts its provider sign-in sets up.
pub const ACCOUNTS_EDITORS: &[&str] = &["so.tilo.mail"];

/// The app that keeps accounts of type `type_id` and is released their
/// secrets by the session's ownership: Mail for email. Settings, the keeper of
/// every account, seals secrets and is released none.
#[must_use]
pub fn secret_keeper(type_id: &str) -> Option<&'static str> {
    (type_id == crate::email::TYPE_ID).then_some(crate::email::KEEPER)
}

/// A Tilo identity's public connection metadata, edited in shared accounts.
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
    /// shapes its own; the record keeps them as the JSON they are, a map of
    /// settings, so two devices changing different settings both keep theirs.
    pub config: serde_json::Value,
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

/// Bring the person's accounts of version 1 up to this one: an account's
/// connection settings were one JSON string, and are the JSON value itself
/// now. Every account and every setting is kept.
///
/// # Errors
/// Returns why a stored account's settings do not read as JSON.
pub fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    use tilo_resource::evolution::{collection_items, retype_json};
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "no accounts migration from {from}"
        )));
    }
    for account in collection_items(doc, &tilo_resource::__private::ROOT, "accounts")? {
        retype_json::<serde_json::Value>(doc, &account, "config")?;
    }
    Ok(())
}

impl PersonAccounts {
    /// What the record holds: the accounts the person configured, without their secrets, and
    /// the one each type uses.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "tilo.accounts",
    version = 2,
    migrate = upgrade,
    read_content = PersonAccounts::read_document
)]
impl PersonAccounts {
    /// Project public browser choices from the existing typed account document.
    pub fn browser_accounts_from_doc(
        doc: &tilo_resource::Doc,
        root: &str,
        origin: &str,
    ) -> Result<Vec<BrowserAccount>, tilo_resource::ModelError> {
        use tilo_resource::Resource as _;
        Ok(Self::read(doc)?.browser_accounts(root, origin))
    }

    /// Existing enabled identity accounts for this profile and exact host.
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
                        .get("userPublicKey")
                        .and_then(serde_json::Value::as_str)
                        == Some(root)
                    && account
                        .config
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
        self.keep_account(
            id,
            TILO_IDENTITY_TYPE.into(),
            display_name,
            serde_json::json!({"userPublicKey": root, "origin": origin}),
            true,
            0,
        )
    }

    /// Save an account: add it, or replace the one with its id.
    ///
    /// What an account that is kept is, and how it connects, is its person's
    /// own to change, at a device of theirs. Its secret is kept under its id
    /// and follows it to whatever server its settings name, and a calendar
    /// account connected for reading only says so in those settings. So
    /// whoever acts for them gives an account another name and turns one off
    /// or on, and is refused anything else of an account that is kept
    /// ([`theirs_alone`]). It adds none either: under the id of an account
    /// that was removed a secret may still be kept, which the accounts do not
    /// know.
    #[action]
    pub fn save_account(
        &mut self,
        cx: &mut ActionCx,
        #[param("The account's id, which its secret is kept under")] id: String,
        #[param("The kind of account, such as email or caldav")] type_id: String,
        #[param("The name shown for it")] display_name: String,
        #[param("Its connection settings, without any secret")] config: serde_json::Value,
        #[param("Whether it is in use")] enabled: bool,
        #[param("Avatar tint")] tint: u8,
    ) -> Result<(), Refusal> {
        if acts_for_its_person(cx) {
            let refused = match self.accounts.get(&Id::from_raw(id.clone())) {
                Some(kept) => theirs_alone(kept, &type_id, &config),
                None => Some(ADDED_BY_ITS_PERSON),
            };
            if let Some(why) = refused {
                return Err(Refusal::from(why));
            }
        }
        self.keep_account(id, type_id, display_name, config, enabled, tint)
    }

    /// Keep an account as [`Self::save_account`] does once it may: for code
    /// of this device that fills the person's accounts itself.
    ///
    /// # Errors
    /// The account names no id or no kind.
    pub fn keep_account(
        &mut self,
        id: String,
        type_id: String,
        display_name: String,
        config: serde_json::Value,
        enabled: bool,
        tint: u8,
    ) -> Result<(), Refusal> {
        if id.trim().is_empty() || type_id.trim().is_empty() {
            return Err(Refusal::from("an account needs an id and a type"));
        }
        let key = Id::from_raw(id.clone());
        let account = PersonAccount {
            type_id: type_id.clone(),
            display_name,
            config,
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

    /// Remove account.
    ///
    /// An account that is kept is its person's own to remove, at a device of
    /// theirs: removed and saved anew it would be another account under the
    /// id its secret is kept under.
    #[action]
    pub fn remove_account(&mut self, cx: &mut ActionCx, account: Id) -> Result<(), Refusal> {
        if acts_for_its_person(cx) && self.accounts.contains(&account) {
            return Err(Refusal::from(REMOVED_BY_ITS_PERSON));
        }
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

/// What whoever acts for a person is told when it would change what an
/// account of theirs is or how it connects.
const THEIRS_ALONE: &str = "What an account is and how it connects is its person's own to change, \
                            at a device of theirs. Whoever acts for them names one anew or turns \
                            one off";
/// What it is told when it would add an account.
const ADDED_BY_ITS_PERSON: &str = "An account is its person's own to add, at a device of theirs: a \
                                   secret may still be kept under its id";
/// What it is told when that would lift an account's reading only.
const READING_ONLY: &str = "This account is connected for reading only, which is its person's \
                            own to change: only they, at a device of theirs, save it without \
                            that word or remove it";
/// What it is told when it would remove an account.
const REMOVED_BY_ITS_PERSON: &str =
    "An account is its person's own to remove, at a device of theirs";

/// Whether the action applying now was invoked by somebody acting for the
/// person (an agent, a bot, an automation, a device that is not theirs) and
/// not by the person themselves at a device of theirs. An action nobody
/// invoked is this device's own code filling the person's accounts, and is
/// not.
fn acts_for_its_person(cx: &ActionCx) -> bool {
    cx.invoked_by()
        .is_some_and(|by| by.direct_person().is_none())
}

/// Whether `config` says its account is connected for reading only: at the
/// account, or under its calendar settings.
fn reading_only(config: &serde_json::Value) -> bool {
    ["/read_only", "/calendar/read_only"]
        .iter()
        .any(|at| config.pointer(at).and_then(serde_json::Value::as_bool) == Some(true))
}

/// Why making the account `kept` one of `type_id` with `config` is its
/// person's own to do, where it is: none for a change that leaves what the
/// account is and how it connects as they are.
fn theirs_alone(
    kept: &PersonAccount,
    type_id: &str,
    config: &serde_json::Value,
) -> Option<&'static str> {
    if kept.type_id == type_id && kept.config == *config {
        return None;
    }
    Some(if reading_only(&kept.config) && !reading_only(config) {
        READING_ONLY
    } else {
        THEIRS_ALONE
    })
}

impl SharedResource for PersonAccounts {
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
                config: account.config.clone(),
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
        // action API refuses a missing value.
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

    /// The person's accounts as version 1 kept them, each account's
    /// connection settings one JSON string, open with every account and
    /// every setting as it was.
    #[test]
    fn accounts_of_version_one_keep_every_account_and_setting() {
        use tilo_resource::{Slot, evolution};
        let root = tilo_resource::__private::ROOT;
        let config = serde_json::json!({
            "host": "imap.example",
            "port": 993,
            "tls": true,
            "folders": ["INBOX", "Sent"],
        });
        let mut replica = Replica::<PersonAccounts>::new();
        let doc = replica.doc_mut();
        evolution::fixture(
            doc,
            &root,
            &serde_json::json!({
                "accounts": {
                    "mail-1": {
                        "k": "a",
                        "v": {
                            "type_id": {"$text": "email"},
                            "display_name": {"$text": "Work"},
                            "config": config.to_string(),
                            "enabled": true,
                            "tint": 3,
                        },
                    },
                    "dav-1": {
                        "k": "b",
                        "v": {
                            "type_id": {"$text": "caldav"},
                            "display_name": {"$text": "Calendar"},
                            "config": "null",
                            "enabled": false,
                            "tint": 0,
                        },
                    },
                },
                "active": {"email": {"$text": "mail-1"}},
            }),
        )
        .unwrap();
        Slot::Key(&root, evolution::VERSION_KEY)
            .put_scalar(doc, tilo_resource::__private::ScalarValue::Uint(1))
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(evolution::migrate::<PersonAccounts>(&mut old).unwrap());
        let accounts = evolution::read_checked::<PersonAccounts>(&old).unwrap();
        assert_eq!(accounts.accounts.len(), 2);
        let mail = accounts.accounts.get(&Id::from_raw("mail-1")).unwrap();
        assert_eq!(mail.type_id, "email");
        assert_eq!(mail.display_name, "Work");
        assert_eq!(mail.config, config);
        assert!(mail.enabled);
        assert_eq!(mail.tint, 3);
        let dav = accounts.accounts.get(&Id::from_raw("dav-1")).unwrap();
        assert_eq!(dav.config, serde_json::Value::Null);
        assert_eq!(
            accounts.active.get("email").map(String::as_str),
            Some("mail-1")
        );
        assert_eq!(
            accounts.store().get(&"mail-1".into()).unwrap().config["port"],
            993
        );
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

    /// A calendar account as its person keeps it for reading only.
    fn reading_only(more: &[(&str, serde_json::Value)]) -> serde_json::Value {
        let mut config = serde_json::json!({
            "server_url": "https://dav.example/", "username": "ada", "read_only": true,
        });
        for (name, value) in more {
            config[*name] = value.clone();
        }
        config
    }

    /// The edit that saves the account `id` of `type_id` with `config`.
    fn saving(id: &str, type_id: &str, config: serde_json::Value) -> Invoke<PersonAccounts> {
        PersonAccounts::SAVE_ACCOUNT.with((
            id.into(),
            type_id.into(),
            "Work".into(),
            config,
            true,
            0,
        ))
    }

    /// What the account `id` keeps as its settings in `replica`; none when
    /// it is gone.
    fn kept(replica: &Replica<PersonAccounts>, id: &str) -> Option<serde_json::Value> {
        replica
            .state()
            .unwrap()
            .accounts
            .get(&Id::from_raw(id))
            .map(|account| account.config.clone())
    }

    /// An account connected for reading only stays so against whoever acts
    /// for its person. An agent on a device of theirs does not save it again
    /// without that word, at the account or under its calendar settings, and
    /// does not remove it, which would be the same once it saved it anew.
    /// The person themselves, at a device of theirs, does both.
    #[test]
    fn only_its_person_lifts_reading_only_or_removes_such_an_account() {
        let person = "ab".repeat(32);
        let mut hers = Replica::<PersonAccounts>::new().as_invoker(&person);
        hers.apply(saving("work", "caldav", reading_only(&[])))
            .unwrap();
        hers.apply(saving(
            "cloud",
            "nextcloud",
            serde_json::json!({
                "server_url": "https://cloud.example/",
                "calendar": {"read_only": true, "calendars": []},
            }),
        ))
        .unwrap();

        // An agent on a device of hers, acting for her.
        let mut agent = Replica::<PersonAccounts>::load(&hers.save())
            .unwrap()
            .as_agent_for("helper", &person);
        let said = agent
            .apply(saving(
                "work",
                "caldav",
                serde_json::json!({"server_url": "https://dav.example/", "username": "ada"}),
            ))
            .err()
            .map(|refusal| format!("{refusal:?}"));
        let said = said.expect("an agent saved a read-only account again without the word");
        assert!(
            said.contains("reading only"),
            "it is refused in words: {said}"
        );
        let unsaid = agent.apply(saving(
            "work",
            "caldav",
            reading_only(&[("read_only", serde_json::json!(false))]),
        ));
        assert!(unsaid.is_err(), "nor with the word said to be false");
        let under = agent.apply(saving(
            "cloud",
            "nextcloud",
            serde_json::json!({
                "server_url": "https://cloud.example/",
                "calendar": {"calendars": []},
            }),
        ));
        assert!(
            under.is_err(),
            "nor where the word stands under its calendar settings"
        );
        let removed = agent.apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("work")));
        assert!(
            removed.is_err(),
            "an agent removed a read-only account, to save it anew without the word"
        );
        assert_eq!(kept(&agent, "work"), Some(reading_only(&[])));
        assert_eq!(
            kept(&agent, "cloud").unwrap()["calendar"]["read_only"],
            true
        );
        // What keeps the word, it may save: the name shown.
        agent
            .apply(PersonAccounts::SAVE_ACCOUNT.with((
                "work".into(),
                "caldav".into(),
                "Work calendar".into(),
                reading_only(&[]),
                true,
                0,
            )))
            .unwrap();

        // She herself, at a device of hers, saves it without the word, and
        // removes it.
        hers.apply(saving(
            "work",
            "caldav",
            serde_json::json!({"server_url": "https://dav.example/", "username": "ada"}),
        ))
        .unwrap();
        assert_eq!(kept(&hers, "work").unwrap().get("read_only"), None);
        hers.apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("cloud")))
            .unwrap();
        assert_eq!(kept(&hers, "cloud"), None);
    }

    /// A mail account's settings, reading at `incoming`.
    fn mail(incoming: &str) -> serde_json::Value {
        serde_json::json!({"incoming": incoming, "outgoing": "smtp.example", "username": "ada"})
    }

    /// A calendar account's settings, at `address`.
    fn calendar(address: &str) -> serde_json::Value {
        serde_json::json!({"server_url": address, "username": "ada"})
    }

    /// Her accounts as she keeps them at a device of hers: a mail account,
    /// a calendar account, and one she added and removed again. Whether a
    /// secret is still kept under that one's id is not her accounts' to
    /// know.
    fn her_accounts(person: &str) -> Replica<PersonAccounts> {
        let mut hers = Replica::<PersonAccounts>::new().as_invoker(person);
        hers.apply(saving("ada@example.test", "email", mail("imap.example")))
            .unwrap();
        hers.apply(saving("work", "caldav", calendar("https://dav.example/")))
            .unwrap();
        hers.apply(saving("old@example.test", "email", mail("imap.example")))
            .unwrap();
        hers.apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("old@example.test")))
            .unwrap();
        hers
    }

    /// What an account is and where it signs in is its person's own to
    /// change. Its password is kept under its id and follows it, so an agent
    /// on a device of theirs that saved a mail or a calendar account whole
    /// with another server under the same id would have that password sent
    /// there. It is refused that, another kind under the same id, the
    /// removal that would let it save the account anew, and adding an
    /// account at all: under the id of an account that was removed a secret
    /// may still be kept.
    #[test]
    fn only_its_person_changes_what_an_account_is_and_where_it_signs_in() {
        let person = "ab".repeat(32);
        let mut hers = her_accounts(&person);
        // An agent on a device of hers, acting for her.
        let mut agent = Replica::<PersonAccounts>::load(&hers.save())
            .unwrap()
            .as_agent_for("helper", &person);
        let elsewhere = "imap.elsewhere.example";
        let mut taken = Vec::new();
        for (what, id, edit) in [
            (
                "a mail account saved whole with another server",
                "ada@example.test",
                saving("ada@example.test", "email", mail(elsewhere)),
            ),
            (
                "a calendar account saved whole with another address",
                "work",
                saving("work", "caldav", calendar("https://dav.elsewhere.example/")),
            ),
            (
                "an account saved as another kind",
                "work",
                saving("work", "webdav", calendar("https://dav.example/")),
            ),
            (
                "an account added under the id of one she removed",
                "old@example.test",
                saving("old@example.test", "email", mail(elsewhere)),
            ),
            (
                "an account added under a new id",
                "news@example.test",
                saving("news@example.test", "email", mail(elsewhere)),
            ),
            (
                "an account removed",
                "ada@example.test",
                PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("ada@example.test")),
            ),
        ] {
            let refused = agent
                .apply(edit)
                .err()
                .map(|refusal| format!("{refusal:?}"));
            match refused {
                Some(said) => assert!(said.contains("its person's own"), "{what}: {said}"),
                None => taken.push(format!("{what}, and it then keeps {:?}", kept(&agent, id))),
            }
        }
        assert!(
            taken.is_empty(),
            "an agent acting for her changed her accounts: {taken:#?}"
        );
        assert_eq!(kept(&agent, "ada@example.test"), Some(mail("imap.example")));
        assert_eq!(kept(&agent, "work"), Some(calendar("https://dav.example/")));
        assert_eq!(kept(&agent, "old@example.test"), None);
    }

    /// Whoever acts for a person gives an account another name and turns
    /// one off. The device's own code, with nobody invoking, and the person
    /// themselves, at a device of theirs, change their accounts as before.
    #[test]
    fn a_name_is_anyones_to_give_and_the_person_changes_their_accounts_as_before() {
        let person = "ab".repeat(32);
        let mut hers = her_accounts(&person);
        let mut agent = Replica::<PersonAccounts>::load(&hers.save())
            .unwrap()
            .as_agent_for("helper", &person);
        agent
            .apply(PersonAccounts::SAVE_ACCOUNT.with((
                "work".into(),
                "caldav".into(),
                "Team calendar".into(),
                calendar("https://dav.example/"),
                false,
                3,
            )))
            .unwrap();
        let renamed = agent.state().unwrap();
        let work = renamed.accounts.get(&Id::from_raw("work")).unwrap();
        assert_eq!(
            (work.display_name.as_str(), work.enabled),
            ("Team calendar", false)
        );

        // The device's own code, with nobody invoking.
        let mut device = Replica::<PersonAccounts>::load(&agent.save()).unwrap();
        device
            .apply(saving("news@example.test", "email", mail("imap.example")))
            .unwrap();
        device
            .apply(saving(
                "news@example.test",
                "email",
                mail("imap.moved.example"),
            ))
            .unwrap();
        device
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("news@example.test")))
            .unwrap();

        // She herself, at a device of hers.
        hers.apply(saving(
            "ada@example.test",
            "email",
            mail("imap.moved.example"),
        ))
        .unwrap();
        assert_eq!(
            kept(&hers, "ada@example.test"),
            Some(mail("imap.moved.example"))
        );
        hers.apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("work")))
            .unwrap();
        assert_eq!(kept(&hers, "work"), None);
    }
}
