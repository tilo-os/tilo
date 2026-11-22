//! The workspace's accounts as this app keeps them: the record its External
//! Accounts panel edits and Mail, Calendar and the file apps read, beside this
//! device's vault of their secrets and its record of their sync health.

use std::path::PathBuf;

#[cfg(feature = "tilo-os")]
use accounts::email::AccountConfig;
use serde::{Deserialize, Serialize};

/// What a person reads when an account is changed before the workspace's
/// accounts reached this device.
pub const ACCOUNTS_ARRIVING: &str =
    "This workspace's accounts are still arriving on this device. Try again in a moment.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountMutationJournal {
    /// The workspace's accounts as they were, each without its secret.
    #[serde(rename = "previous_accounts")]
    accounts: Vec<accounts::Account>,
    #[serde(rename = "previous_statuses")]
    statuses: accounts::AccountSyncStatusStore,
    #[serde(rename = "previous_vault_ciphertext")]
    vault_ciphertext: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountRoot {
    explicit: Option<PathBuf>,
    person: Option<AccountListResource>,
    #[cfg(test)]
    vault_hold: VaultHold,
    #[cfg(test)]
    window: KeptWindow,
}

/// The test window a scratch root's record lives in, kept open with it.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct KeptWindow(
    #[allow(dead_code, reason = "held, never read: it keeps the window open")]
    Option<std::rc::Rc<dyn std::any::Any>>,
);

#[cfg(test)]
impl std::fmt::Debug for KeptWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeptWindow")
    }
}

#[cfg(test)]
impl PartialEq for KeptWindow {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[cfg(test)]
impl Eq for KeptWindow {}

#[cfg(test)]
#[derive(Clone, Default)]
pub struct VaultHold(Option<std::sync::Arc<dyn Fn() + Send + Sync>>);

#[cfg(test)]
impl std::fmt::Debug for VaultHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultHold")
    }
}

/// A root is told apart by where its files live, not by what its vault
/// waits for.
#[cfg(test)]
impl PartialEq for VaultHold {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[cfg(test)]
impl Eq for VaultHold {}

#[derive(Clone)]
pub struct VaultOpener {
    explicit: Option<PathBuf>,
    #[cfg(test)]
    check: VaultHold,
}

impl VaultOpener {
    pub fn open(&self) -> accounts::SecretVault {
        self.reach();
        accounts::SecretVault::open_person(self.explicit.as_deref())
    }

    #[cfg(feature = "tilo-os")]
    pub fn credential_store(&self) -> account_credentials::CredentialStore {
        account_credentials::CredentialStore::for_root(self.explicit.clone())
    }

    #[cfg(feature = "tilo-os")]
    pub fn oauth_granted(&self, ids: Vec<String>) -> std::collections::BTreeSet<String> {
        self.reach();
        let credential_store = self.credential_store();
        ids.into_iter()
            .filter(|id| {
                credential_store
                    .oauth_scopes(&accounts::AccountId::from(id.clone()))
                    .is_ok()
            })
            .collect()
    }

    /// About to reach the vault: never on the UI thread, and in a test only
    /// once its check lets it.
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "only a test's check is kept on it")
    )]
    fn reach(&self) {
        crate::device_read::off_ui_thread();
        #[cfg(test)]
        {
            if let Some(check) = &self.check.0 {
                check();
            }
        }
    }
}

#[derive(Clone)]
struct AccountListResource(ui::framework::WorkspaceResource<accounts::person::AccountList>);

impl std::fmt::Debug for AccountListResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountList")
    }
}

impl PartialEq for AccountListResource {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for AccountListResource {}

impl AccountRoot {
    pub fn live() -> Self {
        Self::default()
    }

    pub fn at(config_root: impl Into<PathBuf>) -> Self {
        Self {
            explicit: Some(config_root.into()),
            ..Self::default()
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_vault_hold(mut self, check: impl Fn() + Send + Sync + 'static) -> Self {
        self.vault_hold = VaultHold(Some(std::sync::Arc::new(check)));
        self
    }

    #[must_use]
    pub fn with_person(
        mut self,
        record: ui::framework::WorkspaceResource<accounts::person::AccountList>,
    ) -> Self {
        self.person = Some(AccountListResource(record));
        self
    }

    /// A root at `config_root` over a new workspace's accounts record, the
    /// one a test's Settings window opens.
    #[cfg(test)]
    pub(crate) fn scratch_at(config_root: impl Into<PathBuf>) -> Self {
        let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new()
            .expect("settings opens headlessly");
        let record = app
            .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
            .expect("Settings keeps the workspace's accounts");
        let mut root = Self::at(config_root).with_person(record);
        root.window = KeptWindow(Some(std::rc::Rc::new(app)));
        root
    }

    /// The workspace's accounts record this root keeps them in.
    #[cfg(test)]
    pub(crate) fn record_for_test(
        &self,
    ) -> ui::framework::WorkspaceResource<accounts::person::AccountList> {
        let Some(AccountListResource(record)) = &self.person else {
            panic!("a scratch root keeps its accounts in a record");
        };
        record.clone()
    }

    pub fn explicit(&self) -> Option<&std::path::Path> {
        self.explicit.as_deref()
    }

    #[cfg(feature = "tilo-os")]
    pub fn credential_store(&self) -> account_credentials::CredentialStore {
        account_credentials::CredentialStore::for_root(self.explicit.clone())
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.explicit.clone().or_else(accounts::config_root)
    }

    fn require(&self) -> Result<PathBuf, String> {
        self.path().ok_or_else(|| "no config dir".to_string())
    }

    fn require_io(&self) -> std::io::Result<PathBuf> {
        self.path()
            .ok_or_else(|| std::io::Error::other("no config dir"))
    }

    /// The workspace's accounts, as its record holds them: none while the
    /// record has not reached this device.
    pub fn store(&self) -> accounts::person::AccountList {
        self.person
            .as_ref()
            .map(|AccountListResource(record)| (*record.get()).clone())
            .unwrap_or_default()
    }

    pub fn revision(&self) -> Option<u64> {
        self.person
            .as_ref()
            .map(|AccountListResource(record)| record.revision())
    }

    pub fn publish_browser_identity(
        &self,
        root: String,
        origin: String,
        display_name: String,
    ) -> Result<(), String> {
        let Some(AccountListResource(record)) = &self.person else {
            return Err("Settings' personal account resource is unavailable".into());
        };
        record
            .invoke(
                &accounts::person::AccountList::PUBLISH_BROWSER_IDENTITY.with((
                    root,
                    origin,
                    display_name,
                )),
            )
            .map_err(|error| error.to_string())
    }

    pub fn set_browser_identity_enabled(
        &self,
        account: String,
        enabled: bool,
    ) -> Result<(), String> {
        let Some(AccountListResource(record)) = &self.person else {
            return Err("Settings' personal account resource is unavailable".into());
        };
        let current = record.get().account(&account).ok_or("No such account")?;
        if current.type_id.as_str() != accounts::person::TILO_IDENTITY_TYPE {
            return Err("Not a browser identity account".into());
        }
        record
            .invoke(&accounts::person::AccountList::SAVE_ACCOUNT.with((
                account,
                current.type_id.as_str().to_owned(),
                current.display_name,
                current.config,
                enabled,
                current.tint,
            )))
            .map_err(|error| error.to_string())
    }

    pub fn remove_browser_identity(&self, account: String) -> Result<(), String> {
        let Some(AccountListResource(record)) = &self.person else {
            return Err("Settings' personal account resource is unavailable".into());
        };
        let current = record.get().account(&account).ok_or("No such account")?;
        if current.type_id.as_str() != accounts::person::TILO_IDENTITY_TYPE {
            return Err("Not a browser identity account".into());
        }
        record
            .invoke(
                &accounts::person::AccountList::REMOVE_ACCOUNT
                    .with(ui::tilo_resource::Id::from_raw(account)),
            )
            .map_err(|error| error.to_string())
    }

    /// Add or replace the email account `config` in the workspace's accounts,
    /// keyed by its address, with the other names and addresses it sends as
    /// (`identities`, each `Name <address>`). Its secret is sealed apart
    /// ([`Self::seal_password`] or the session's ownership), never here.
    pub fn save_email_account(
        &self,
        config: &accounts::email::AccountConfig,
        identities: &[String],
        password_item: Option<&str>,
    ) -> Result<(), String> {
        let id = accounts::AccountId::from(config.email.clone());
        let mut account = accounts::Account::new(id.clone(), accounts::email::TYPE_ID);
        config.label().clone_into(&mut account.display_name);
        let mut config = config.clone();
        identities.clone_into(&mut config.identities);
        account.config = config.to_value().map_err(|error| error.to_string())?;
        let mut store = self.store();
        // The vault entry its password is in: the one just kept, or the one it named before.
        let named = password_item.map(str::to_owned).or_else(|| {
            store
                .account(id.as_str())
                .and_then(|kept| accounts::email::password_item(&kept.config).map(str::to_owned))
        });
        if let Some(item) = named {
            accounts::email::set_password_item(&mut account.config, Some(&item));
        }
        store.keep(&account).map_err(|error| error.to_string())?;
        self.save_store(&store).map_err(|error| error.to_string())
    }

    pub fn seal_password(&self, id: &accounts::AccountId, password: &str) -> Result<(), String> {
        let Some(root) = &self.explicit else {
            return Err("This device's account credentials is unavailable".into());
        };
        let mut credentials = accounts::AccountCredentials::open_in(root)?;
        let token = credentials.get(id).and_then(|secret| secret.token);
        // Typed just now: bound to the servers its account names where it
        // is first joined.
        credentials.put_anew(
            id,
            &accounts::Secret {
                password: password.to_owned(),
                token,
            },
        );
        credentials.try_save()
    }

    /// Make the workspace's accounts record hold `store`'s accounts: each
    /// one it lacks or holds otherwise saved, each one `store` lacks
    /// removed. Refused while the record has not reached this device: an
    /// account is the workspace's, never this device's alone.
    pub fn save_store(&self, store: &accounts::person::AccountList) -> std::io::Result<()> {
        let Some(AccountListResource(record)) = &self.person else {
            return Err(std::io::Error::other(ACCOUNTS_ARRIVING));
        };
        let refused =
            |error: ui::tilo_actions::ActionError| std::io::Error::other(error.to_string());
        let current = record.get();
        for account in current.all() {
            if store.account(account.id.as_str()).is_none() {
                record
                    .invoke(
                        &accounts::person::AccountList::REMOVE_ACCOUNT
                            .with(ui::tilo_resource::Id::from_raw(account.id.0.clone())),
                    )
                    .map_err(refused)?;
            }
        }
        for account in store.all() {
            let mut account = account;
            if account.config.is_null() {
                account.config = serde_json::json!({});
            }
            if current.account(account.id.as_str()).as_ref() != Some(&account) {
                record
                    .invoke(&accounts::person::AccountList::saving(&account))
                    .map_err(refused)?;
            }
        }
        Ok(())
    }

    pub fn statuses(&self) -> accounts::AccountSyncStatusStore {
        self.path()
            .map(|root| accounts::AccountSyncStatusStore::load_in(&root))
            .unwrap_or_default()
    }

    pub fn save_statuses(
        &self,
        statuses: &accounts::AccountSyncStatusStore,
    ) -> std::io::Result<()> {
        statuses.save_in(&self.require_io()?)
    }

    /// The encrypted credential vault. Opening it asks the Secret Service
    /// for its key: never on the UI thread.
    pub fn vault(&self) -> accounts::SecretVault {
        self.vault_opener().open()
    }

    pub fn vault_opener(&self) -> VaultOpener {
        VaultOpener {
            explicit: self.explicit.clone(),
            #[cfg(test)]
            check: self.vault_hold.clone(),
        }
    }

    pub fn sync_reporter(&self, id: accounts::AccountId) -> accounts::AccountSyncReporter {
        match &self.explicit {
            Some(root) => accounts::AccountSyncReporter::in_root(id, root.clone()),
            None => accounts::AccountSyncReporter::new(id),
        }
    }

    fn mutation_journal_path(&self) -> Result<PathBuf, String> {
        Ok(accounts::accounts_dir_in(&self.require()?).join("mutation-journal.json"))
    }

    /// Journal the pre-mutation stores and vault ciphertext so an
    /// interrupted metadata/vault/status mutation can be rolled back.
    pub fn begin_mutation(
        &self,
        previous_accounts: &accounts::person::AccountList,
        previous_statuses: &accounts::AccountSyncStatusStore,
    ) -> Result<(), String> {
        self.recover_mutation()?;
        let root = self.require()?;
        let vault_ciphertext =
            read_file(&accounts::vault_dir_in(&root).join("vault.enc")).unwrap_or_default();
        let journal = AccountMutationJournal {
            accounts: previous_accounts.all(),
            statuses: previous_statuses.clone(),
            vault_ciphertext,
        };
        let bytes = serde_json::to_vec(&journal)
            .map_err(|error| format!("could not serialize account transaction: {error}"))?;
        tilo_files::native::atomic::write_creating_parent_with_mode(
            &self.mutation_journal_path()?,
            &bytes,
            0o600,
        )
        .map_err(|error| format!("could not persist account transaction intent: {error}"))
    }

    pub fn finish_mutation(&self) -> Result<(), String> {
        remove_file(&self.mutation_journal_path()?)
            .map_err(|error| format!("could not clear account transaction intent: {error}"))
    }

    /// Roll back an interrupted metadata/vault/status mutation before
    /// presenting the account inventory. The journal contains ciphertext,
    /// never a plaintext password or token.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal or the recovered stores cannot be read
    /// or durably restored.
    pub fn recover_mutation(&self) -> Result<(), String> {
        let path = self.mutation_journal_path()?;
        let bytes = match read_file(&path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "could not read account transaction intent: {error}"
                ));
            }
        };
        let journal: AccountMutationJournal = serde_json::from_slice(&bytes)
            .map_err(|error| format!("account transaction intent is invalid: {error}"))?;
        let mut previous = accounts::person::AccountList::default();
        for account in &journal.accounts {
            previous
                .keep(account)
                .map_err(|error| format!("could not recover account metadata: {error}"))?;
        }
        self.save_store(&previous)
            .map_err(|error| format!("could not recover account metadata: {error}"))?;
        self.save_statuses(&journal.statuses)
            .map_err(|error| format!("could not recover account sync status: {error}"))?;
        let vault_path = accounts::vault_dir_in(&self.require()?).join("vault.enc");
        match journal.vault_ciphertext {
            Some(ciphertext) => {
                tilo_files::native::atomic::write_creating_parent_with_mode(
                    &vault_path,
                    &ciphertext,
                    0o600,
                )
                .map_err(|error| format!("could not recover credential vault: {error}"))?;
            }
            None => remove_file(&vault_path)
                .map_err(|error| format!("could not recover credential vault: {error}"))?,
        }
        self.finish_mutation()
    }

    /// Whether a mutation's intent is on disk, so loading the stores
    /// recovers it first. An intent that cannot be ruled out counts.
    pub fn mutation_pending(&self) -> bool {
        self.path().is_some()
            && self
                .mutation_journal_path()
                .is_ok_and(|path| path.try_exists().unwrap_or(true))
    }

    /// Recover an interrupted mutation, logging rather than failing: the
    /// inventory is still worth presenting.
    pub fn recover_mutation_or_warn(&self) {
        if self.path().is_some()
            && let Err(error) = self.recover_mutation()
        {
            tracing::warn!(target: "tilo::settings", %error, "could not recover interrupted account mutation");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountKind {
    Mail,
    CalDav,
    WebDav,
    Nextcloud,
}

fn read_file(path: &std::path::Path) -> Result<Option<Vec<u8>>, tilo_files::FsError> {
    let (dir, file) = tilo_files::file_at(path)?;
    match tilo_files::blocking::read(&dir, &file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(tilo_files::FsError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Remove the file at `path`, durably: its directory is synced after. A file
/// that is not there is already removed.
fn remove_file(path: &std::path::Path) -> Result<(), tilo_files::FsError> {
    let (dir, file) = tilo_files::file_at(path)?;
    match tilo_files::blocking::remove(&dir, &file) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                let _ = tilo_files::native::atomic::sync_directory(parent);
            }
            Ok(())
        }
        Err(tilo_files::FsError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

impl AccountKind {
    pub const ALL: [Self; 4] = [Self::Mail, Self::CalDav, Self::WebDav, Self::Nextcloud];

    pub fn slug(self) -> &'static str {
        match self {
            AccountKind::Mail => "mail",
            AccountKind::CalDav => "caldav",
            AccountKind::WebDav => "webdav",
            AccountKind::Nextcloud => "nextcloud",
        }
    }

    #[cfg(feature = "tilo-os")]
    pub fn type_id(self) -> &'static str {
        match self {
            AccountKind::Mail => MAIL_TYPE,
            AccountKind::CalDav => CALDAV_TYPE,
            AccountKind::WebDav => WEBDAV_TYPE,
            AccountKind::Nextcloud => NEXTCLOUD_TYPE,
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.slug() == slug)
    }

    #[cfg(feature = "tilo-os")]
    pub fn from_type_id(type_id: &str) -> Option<Self> {
        if type_id == accounts::thunderbird::PENDING_TYPE {
            return Some(Self::Mail);
        }
        Self::ALL.into_iter().find(|kind| kind.type_id() == type_id)
    }

    /// Whether the account is signed in to on its server's own page rather than with a
    /// password typed here.
    #[cfg(feature = "tilo-os")]
    pub fn signs_in(self) -> bool {
        self == AccountKind::Nextcloud
    }
}

#[cfg(feature = "tilo-os")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView {
    pub id: String,
    pub kind: AccountKind,
    pub label: String,
    pub summary: String,
    pub enabled: bool,
    pub tint: u8,
}

#[cfg(feature = "tilo-os")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountDirectory {
    pub accounts: Vec<accounts::Account>,
}

#[cfg(feature = "tilo-os")]
impl AccountDirectory {
    pub fn load_in(root: &AccountRoot) -> Self {
        root.recover_mutation_or_warn();
        Self {
            accounts: root
                .store()
                .all()
                .into_iter()
                .filter(|account| AccountKind::from_type_id(account.type_id.as_str()).is_some())
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn of(&self, kind: AccountKind) -> impl Iterator<Item = &accounts::Account> {
        self.accounts.iter().filter(move |account| {
            AccountKind::from_type_id(account.type_id.as_str()) == Some(kind)
        })
    }

    pub fn mail_config(&self, id: &str) -> Option<AccountConfig> {
        self.of(AccountKind::Mail)
            .find(|account| account.id.as_str() == id)
            .and_then(|account| AccountConfig::from_value(account.config.clone()).ok())
    }

    pub fn views_of(&self, kind: AccountKind) -> Vec<AccountView> {
        self.of(kind)
            .filter_map(|account| account_view(kind, account))
            .collect()
    }
}

#[cfg(feature = "tilo-os")]
fn config_str<'a>(account: &'a accounts::Account, key: &str) -> &'a str {
    account
        .config
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

#[cfg(feature = "tilo-os")]
fn account_view(kind: AccountKind, account: &accounts::Account) -> Option<AccountView> {
    let id = account.id.to_string();
    Some(match kind {
        AccountKind::Mail => {
            let config = AccountConfig::from_value(account.config.clone()).ok()?;
            AccountView {
                id,
                kind,
                label: config.label().to_string(),
                summary: config.email,
                enabled: true,
                tint: account.tint,
            }
        }
        AccountKind::CalDav | AccountKind::Nextcloud | AccountKind::WebDav => {
            let url = if kind == AccountKind::WebDav {
                "base_url"
            } else {
                "server_url"
            };
            AccountView {
                id,
                kind,
                // The user it signs in as, not the kind its section already names.
                label: match config_str(account, "username") {
                    "" => account.display_name.clone(),
                    user => user.to_owned(),
                },
                summary: short_url(config_str(account, url)),
                enabled: account.enabled,
                tint: account.tint,
            }
        }
    })
}

#[cfg(feature = "tilo-os")]
pub fn short_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(feature = "tilo-os")]
const MAIL_TYPE: &str = "email";
#[cfg(feature = "tilo-os")]
pub(crate) const CALDAV_TYPE: &str = "caldav";
pub use accounts::cloud_types::{NEXTCLOUD as NEXTCLOUD_TYPE, WEBDAV as WEBDAV_TYPE};

#[cfg(feature = "tilo-os")]
/// A deterministic, populated directory for `--shots` and tests so the
/// panel renders a realistic accounts list without touching disk.
pub fn sample_directory() -> AccountDirectory {
    fn account(
        id: &str,
        type_id: &str,
        name: &str,
        tint: u8,
        config: serde_json::Value,
    ) -> accounts::Account {
        let mut account = accounts::Account::new(id, type_id);
        account.display_name = name.into();
        account.tint = tint;
        account.config = config;
        account
    }
    let mail = |id: &str, name: &str, provider: &str, tint: u8| {
        let config = AccountConfig {
            email: id.into(),
            display_name: name.into(),
            imap_host: format!("imap.{provider}"),
            imap_port: 993,
            imap_tls: true,
            smtp_host: format!("smtp.{provider}"),
            smtp_port: 465,
            smtp_tls: true,
            username: id.into(),
            ..Default::default()
        };
        account(
            id,
            MAIL_TYPE,
            name,
            tint,
            config.to_value().unwrap_or_default(),
        )
    };
    AccountDirectory {
        accounts: vec![
            mail("ada@example.com", "Work email", "fastmail.com", 2),
            mail("ada.personal@gmail.com", "Personal email", "gmail.com", 5),
            account(
                "dav-team",
                CALDAV_TYPE,
                "Team calendar",
                3,
                serde_json::json!({
                    "server_url": "https://dav.example.com/calendars/ada/",
                    "username": "ada",
                }),
            ),
            account(
                "nc-home",
                NEXTCLOUD_TYPE,
                "Home Nextcloud",
                6,
                serde_json::json!({
                    "server_url": "https://cloud.example.com", "username": "ada",
                    "use_files": true, "use_calendar": true, "use_mail": false,
                }),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_profiles_do_not_take_in_another_profiles_browser_identity() {
        use ui::framework::testing::TestApp;

        let mut before = TestApp::<crate::resource::SettingsShell>::new().unwrap();
        let key = "ef".repeat(32);
        let origin = "https://isolated-profile.example";
        let id = format!("settings.browser-identity:tilo-identity:{key}@{origin}");
        assert!(
            before
                .actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != id)
        );

        let mut publisher = TestApp::<crate::resource::SettingsShell>::for_person(&key).unwrap();
        let record = publisher
            .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        AccountRoot::at(directory.path())
            .with_person(record)
            .publish_browser_identity(key, origin.into(), "Private profile".into())
            .unwrap();
        publisher.settle();
        assert!(
            publisher
                .actions()
                .unwrap()
                .iter()
                .any(|action| action.address.resource == id)
        );

        let mut after = TestApp::<crate::resource::SettingsShell>::new().unwrap();
        assert!(
            after
                .actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != id),
            "a fresh profile must not import another profile's browser identity through the session account file"
        );
        assert!(
            before
                .actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != id)
        );
    }

    #[test]
    fn browser_identity_publication_is_a_settings_owned_personal_resource_action() {
        use accounts::person::AccountList;
        let root_key = "ab".repeat(32);
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::SettingsShell>::for_person(
                &root_key,
            )
            .unwrap();
        let record = app
            .shared_workspace::<AccountList>(accounts::person::ACCOUNTS_OWNER)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path()).with_person(record.clone());
        root.publish_browser_identity(
            root_key.clone(),
            "https://host.example".into(),
            "Work".into(),
        )
        .unwrap();
        assert_eq!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .len(),
            1
        );
        assert!(
            record
                .get()
                .browser_accounts(&"cd".repeat(32), "https://host.example")
                .is_empty()
        );
        let choice = record
            .get()
            .browser_accounts(&root_key, "https://host.example")[0]
            .id
            .clone();
        let actions = app.actions().unwrap();
        assert!(actions.iter().any(|action| action.address.resource
            == format!("settings.browser-identity:{choice}")
            && action.address.action == "set-browser-identity-enabled"));
        app.call_on(
            &format!("settings.browser-identity:{choice}"),
            "set-browser-identity-enabled",
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .is_empty()
        );
        root.publish_browser_identity(
            root_key.clone(),
            "https://host.example".into(),
            "Another name".into(),
        )
        .unwrap();
        assert!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .is_empty()
        );
        app.call_on(
            &format!("settings.browser-identity:{choice}"),
            "set-browser-identity-enabled",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .len(),
            1
        );
        app.call_confirmed_on(
            &format!("settings.browser-identity:{choice}"),
            "remove-browser-identity",
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            record
                .get()
                .browser_accounts(&root_key, "https://host.example")
                .is_empty()
        );
        assert!(!dir.path().join("accounts").exists());
        assert!(
            AccountRoot::at(dir.path())
                .publish_browser_identity(root_key, "https://host.example".into(), "Work".into())
                .is_err()
        );
    }

    /// In a host the accounts are the workspace's record: saving edits it and
    /// reading reads it, on every device.
    #[test]
    fn in_a_host_the_accounts_are_the_workspace_record() {
        use accounts::person::AccountList;
        let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new()
            .expect("settings opens headlessly");
        let record = app
            .shared_workspace::<AccountList>(accounts::person::ACCOUNTS_OWNER)
            .expect("Settings keeps the workspace's accounts");
        assert_eq!(
            record.record().route().expect("the accounts sync").app_id,
            accounts::person::ACCOUNTS_OWNER,
            "Settings keeps the workspace's accounts"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path()).with_person(record.clone());

        let mut account = accounts::Account::new("a@x.example", "email");
        account.display_name = "Work".into();
        account.config = serde_json::json!({ "imap_host": "imap.x.example" });
        let mut store = AccountList::default();
        store.keep(&account).unwrap();
        root.save_store(&store).unwrap();
        assert_eq!(
            record.get().account(account.id.as_str()),
            Some(account.clone())
        );
        assert_eq!(
            root.store().account(account.id.as_str()),
            Some(account.clone())
        );

        store.remove(&account.id);
        root.save_store(&store).unwrap();
        assert!(record.get().is_empty());
        assert!(root.store().is_empty());
    }

    #[cfg(feature = "tilo-os")]
    #[test]
    fn account_directory_views_summarise_and_autofill() {
        let dir = sample_directory();
        assert!(!dir.is_empty());
        assert!(AccountDirectory::default().is_empty());
        for (kind, want) in [
            (AccountKind::Mail, 2),
            (AccountKind::CalDav, 1),
            (AccountKind::WebDav, 0),
            (AccountKind::Nextcloud, 1),
        ] {
            assert_eq!(dir.views_of(kind).len(), want, "{kind:?} views");
        }
        let mail = &dir.views_of(AccountKind::Mail)[0];
        assert_eq!(mail.summary, "ada@example.com");
        assert_eq!(
            dir.mail_config("ada@example.com")
                .map(|config| config.imap_host),
            Some("imap.fastmail.com".into())
        );
        let cloud = &dir.views_of(AccountKind::Nextcloud)[0];
        assert_eq!(cloud.summary, "cloud.example.com");

        for (url, short) in [
            ("https://cloud.example.com/", "cloud.example.com"),
            (
                "http://dav.example.com/calendars/ada/",
                "dav.example.com/calendars/ada",
            ),
        ] {
            assert_eq!(short_url(url), short, "{url}");
        }
    }

    #[cfg(feature = "tilo-os")]
    #[test]
    fn journal_recovers_each_interrupted_removal_phase() {
        for phase in 1..=3 {
            let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new()
                .expect("settings opens headlessly");
            let record = app
                .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
                .expect("Settings keeps the workspace's accounts");
            let dir = tempfile::tempdir().expect("scratch config root");
            let root = AccountRoot::at(dir.path().join("tilo")).with_person(record);
            let id = accounts::AccountId::from(format!("recover-{phase}"));
            let mut original_store = accounts::person::AccountList::default();
            let mut account = accounts::Account::new(id.clone(), NEXTCLOUD_TYPE);
            account.display_name = "Recover me".into();
            account.config = serde_json::json!({
                "id": id.as_str(), "label": "Recover me", "server_url": "https://cloud.example",
                "username": "ada", "use_files": true, "use_calendar": true, "use_mail": true
            });
            original_store.keep(&account).unwrap();
            root.save_store(&original_store).expect("seed metadata");
            let mut vault = root.vault();
            vault.put(&id, &accounts::Secret::password("sealed-secret"));
            vault.try_save().expect("seed vault");
            root.sync_reporter(id.clone())
                .succeeded()
                .expect("seed status");
            let original_statuses = root.statuses();

            root.begin_mutation(&original_store, &original_statuses)
                .expect("journal intent");
            let mut vault = root.vault();
            vault.remove(&id);
            vault.try_save().expect("phase one");
            if phase >= 2 {
                let mut store = root.store();
                store.remove(&id);
                root.save_store(&store).expect("phase two");
            }
            if phase >= 3 {
                let mut statuses = root.statuses();
                statuses.remove(&id);
                root.save_statuses(&statuses).expect("phase three");
            }

            let recovered = AccountDirectory::load_in(&root);
            assert_eq!(
                recovered.of(AccountKind::Nextcloud).count(),
                1,
                "phase {phase}"
            );
            assert_eq!(
                root.vault().get(&id).map(|secret| secret.password),
                Some("sealed-secret".into()),
                "phase {phase} restores encrypted credential"
            );
            assert!(
                root.statuses().get(&id).is_some(),
                "phase {phase} restores sync status"
            );
            assert!(!root.mutation_journal_path().expect("journal path").exists());
        }
    }

    #[test]
    fn the_window_bars_sidebar_button_flips_the_sidebar() {
        ui::framework::testing::check_window_sidebar_button(
            ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new,
        )
        .unwrap_or_else(|error| panic!("{error}"));
    }
}
