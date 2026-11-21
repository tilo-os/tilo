//! The workspace's accounts as one record: the mail, calendar and messaging
//! accounts configured in it, which every member and device of the
//! workspace reads as the workspace's grants allow. Their secrets are never
//! in it.

use std::path::Path;

use tilo_resource::{Collection, Id, Invoke, Model, ModelError, Refusal, SharedResource, resource};

use crate::{Account, AccountId, AccountStore, AccountTypeId};

/// The app that keeps the workspace's accounts.
pub use tilo_app_id::apps::SETTINGS as ACCOUNTS_OWNER;

/// A Tilo identity's public connection metadata, edited in shared accounts.
pub const TILO_IDENTITY_TYPE: &str = "tilo.identity";

/// Public fields needed to choose an existing browser identity. This is a
/// projection of Settings' account record, not a credential or admission grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAccount {
    /// The shared account's stable id.
    pub id: String,
    /// The chosen account name.
    pub display_name: String,
    /// Public root key matching the profile that holds the account record.
    pub user_public_key: String,
}

/// One account of the workspace's, without its secret.
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
    /// Whether it is in use.
    pub enabled: bool,
    /// Tints the account's avatar so several read as distinct.
    pub tint: u8,
}

/// Every account the workspace has. Which one a member's window shows is
/// that member's own preference, kept beside their Mail settings.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PersonAccounts {
    /// Keyed by the account's id, the id its secret is kept under.
    pub accounts: Collection<PersonAccount>,
}

/// Bring the workspace's accounts of an older version up to this one: what
/// an account's connection settings are, and the account each member's
/// window showed, which is their own preference now. Every account and every
/// setting is kept.
///
/// # Errors
/// Returns why a stored account's settings do not read as JSON.
pub fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    use automerge::transaction::Transactable as _;
    use tilo_resource::evolution::{collection_items, retype_json};
    if from == 1 {
        // Version 1 kept an account's connection settings as one JSON
        // string; version 2 keeps the JSON value itself.
        for account in collection_items(doc, &tilo_resource::__private::ROOT, "accounts")? {
            retype_json::<serde_json::Value>(doc, &account, "config")?;
        }
    } else if from != 2 {
        return Err(ModelError::invalid(format!(
            "no accounts migration from {from}"
        )));
    }
    // Version 2 also kept, per account type, the account a window showed:
    // one member's choice, which changed for every member. It is gone, and
    // each member's Mail preferences keep their own.
    doc.delete(&tilo_resource::__private::ROOT, "active")?;
    Ok(())
}

impl PersonAccounts {
    /// What the record holds: the accounts the workspace has, without their secrets.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "tilo.accounts",
    version = 3,
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
    /// Whoever the workspace's policy lets change its accounts, a person or
    /// an agent acting for one, may save an account or remove it.
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
        self.keep_account(id, type_id, display_name, config, enabled, tint)
    }

    /// Keep an account as [`Self::save_account`] does once it may: for code
    /// of this device that fills the workspace's accounts itself.
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
        let key = Id::from_raw(id);
        let account = PersonAccount {
            type_id,
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
        Ok(())
    }

    /// Remove account.
    #[action]
    pub fn remove_account(&mut self, account: Id) -> Result<(), Refusal> {
        self.accounts
            .remove(&account)
            .map(|_| ())
            .ok_or_else(|| Refusal::from("there is no such account"))
    }
}

impl SharedResource for PersonAccounts {
    const OWNER: &'static str = ACCOUNTS_OWNER;
}

impl PersonAccounts {
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
        store
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
    /// of the workspace's accounts, for the services that read it. Writes
    /// only when it differs. Whether it wrote.
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

    /// The workspace's accounts as version 1 kept them, each account's
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
        assert!(
            evolution::map_at(&old, &root, "active").unwrap().is_none(),
            "the account a window showed is each member's preference now"
        );
        assert_eq!(
            accounts.store().get(&"mail-1".into()).unwrap().config["port"],
            993
        );
    }

    /// The workspace's accounts as version 2 kept them: each account's
    /// connection settings the JSON value itself, and the account a window
    /// showed per type. Each account and setting stays; the shown account
    /// is gone, each member's Mail preferences keeping their own.
    #[test]
    fn accounts_of_version_two_keep_every_account_and_setting() {
        use tilo_resource::{Slot, evolution};
        let root = tilo_resource::__private::ROOT;
        let config = serde_json::json!({"host": "imap.example", "port": 993});
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
                            "config": config,
                            "enabled": true,
                            "tint": 3,
                        },
                    },
                },
                "active": {"email": {"$text": "mail-1"}},
            }),
        )
        .unwrap();
        Slot::Key(&root, evolution::VERSION_KEY)
            .put_scalar(doc, tilo_resource::__private::ScalarValue::Uint(2))
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(evolution::migrate::<PersonAccounts>(&mut old).unwrap());
        assert_eq!(evolution::stored_version(&old), 3);
        let accounts = evolution::read_checked::<PersonAccounts>(&old).unwrap();
        assert_eq!(accounts.accounts.len(), 1);
        let mail = accounts.accounts.get(&Id::from_raw("mail-1")).unwrap();
        assert_eq!(mail.config, config);
        assert!(
            evolution::map_at(&old, &root, "active").unwrap().is_none(),
            "the shown account is each member's preference now"
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
    fn accounts_are_saved_by_id_and_removed() {
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

        replica
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("b@y.example")))
            .unwrap();
        let store = replica.state().unwrap().store();
        assert!(store.get(&"b@y.example".into()).is_none());
        assert_eq!(
            store.list(&"email".into()).len(),
            1,
            "the other account stays"
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

    /// Whoever acts for a member, an agent included, changes the workspace's
    /// accounts as the member may: what an account is, where it signs in, a
    /// new account, a removal, and an account no longer connected for
    /// reading only.
    #[test]
    fn an_agent_acting_for_a_member_changes_the_workspaces_accounts() {
        let person = "ab".repeat(32);
        let mut theirs = workspace_accounts(&person);
        let mut agent = Replica::<PersonAccounts>::load(&theirs.save())
            .unwrap()
            .as_agent_for("helper", &person);
        let elsewhere = "imap.elsewhere.example";
        agent
            .apply(saving("ada@example.test", "email", mail(elsewhere)))
            .unwrap();
        assert_eq!(kept(&agent, "ada@example.test"), Some(mail(elsewhere)));
        agent
            .apply(saving(
                "work",
                "webdav",
                calendar("https://dav.elsewhere.example/"),
            ))
            .unwrap();
        assert_eq!(
            kept(&agent, "work"),
            Some(calendar("https://dav.elsewhere.example/"))
        );
        agent
            .apply(saving("news@example.test", "email", mail(elsewhere)))
            .unwrap();
        assert_eq!(kept(&agent, "news@example.test"), Some(mail(elsewhere)));
        agent
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("ada@example.test")))
            .unwrap();
        assert_eq!(kept(&agent, "ada@example.test"), None);
        let reading_only = serde_json::json!({
            "server_url": "https://dav.example/", "username": "ada", "read_only": true,
        });
        agent.apply(saving("read", "caldav", reading_only)).unwrap();
        agent
            .apply(saving("read", "caldav", calendar("https://dav.example/")))
            .unwrap();
        assert_eq!(kept(&agent, "read"), Some(calendar("https://dav.example/")));
    }

    /// A mail account's settings, reading at `incoming`.
    fn mail(incoming: &str) -> serde_json::Value {
        serde_json::json!({"incoming": incoming, "outgoing": "smtp.example", "username": "ada"})
    }

    /// A calendar account's settings, at `address`.
    fn calendar(address: &str) -> serde_json::Value {
        serde_json::json!({"server_url": address, "username": "ada"})
    }

    /// The workspace's accounts as a member's device keeps them: a mail
    /// account, a calendar account, and one added and removed again.
    fn workspace_accounts(person: &str) -> Replica<PersonAccounts> {
        let mut theirs = Replica::<PersonAccounts>::new().as_invoker(person);
        theirs
            .apply(saving("ada@example.test", "email", mail("imap.example")))
            .unwrap();
        theirs
            .apply(saving("work", "caldav", calendar("https://dav.example/")))
            .unwrap();
        theirs
            .apply(saving("old@example.test", "email", mail("imap.example")))
            .unwrap();
        theirs
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("old@example.test")))
            .unwrap();
        theirs
    }

    /// The device's own code, with nobody invoking, and a member at their
    /// device change the workspace's accounts as before.
    #[test]
    fn a_members_own_device_changes_the_accounts_as_before() {
        let person = "ab".repeat(32);
        let mut theirs = workspace_accounts(&person);
        let mut device = Replica::<PersonAccounts>::load(&theirs.save()).unwrap();
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
        assert_eq!(kept(&device, "news@example.test"), None);

        theirs
            .apply(saving(
                "ada@example.test",
                "email",
                mail("imap.moved.example"),
            ))
            .unwrap();
        assert_eq!(
            kept(&theirs, "ada@example.test"),
            Some(mail("imap.moved.example"))
        );
        theirs
            .apply(PersonAccounts::REMOVE_ACCOUNT.with(Id::from_raw("work")))
            .unwrap();
        assert_eq!(kept(&theirs, "work"), None);
    }

    /// The workspace's roles decide who changes its accounts, as they decide
    /// any workspace action: an Editor, whether their own device or an agent
    /// they grant Mail, adds and removes accounts; a Viewer is refused.
    #[test]
    fn an_editor_changes_the_accounts_and_a_viewer_is_refused() {
        use tilo_action_policy::{
            Acting, ActionKind, Answer, Authority, Context, Grant, Grantee, KindSet, Role,
            TreePath, WorkspacePolicy,
        };
        use tilo_resource::Resource as _;
        const WORKSPACE: &str = "w";
        const MAIL: &str = "so.tilo.mail";
        let editor = "ab".repeat(32);
        let viewer = "cd".repeat(32);
        let mut policy = WorkspacePolicy::new(WORKSPACE, &editor);
        policy.members.insert(editor.clone(), Role::Editor);
        policy.members.insert(viewer.clone(), Role::Viewer);
        // Mail publishes the accounts record, so its actions are Mail's.
        let accounts = TreePath::workspace(WORKSPACE)
            .app(MAIL)
            .kind(PersonAccounts::KIND);
        let cx = Context::at_secs(1);
        let answer = |acting: &Acting, granted: &[Grant]| {
            Authority::of_acting(acting, &policy, granted)
                .decide(&accounts, ActionKind::Undoable, &cx)
                .answer
        };
        assert_eq!(answer(&Acting::person(&editor), &[]), Answer::Allow);
        assert_eq!(answer(&Acting::person(&viewer), &[]), Answer::Deny);
        // The grant an agent's turn gives it, on Mail.
        let mail = [Grant::new(
            "turn:helper:0",
            Grantee::agent("helper"),
            TreePath::workspace(WORKSPACE).app(MAIL),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person(&editor),
        )];
        assert_eq!(
            answer(
                &Acting::agent("helper").acting_for(Acting::person(&editor)),
                &mail
            ),
            Answer::Allow
        );
        assert_eq!(
            answer(
                &Acting::agent("helper").acting_for(Acting::person(&viewer)),
                &mail
            ),
            Answer::Deny
        );
    }
}
