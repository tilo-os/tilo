//! Passwords — the person's logins, keys and codes: kept in their workspace's vault on its
//! server, which every device of theirs reads, or in the vault a native device lends it.

use accounts::{CredentialGrant, CredentialOwner, SecretId, SecretItem, SecretStore, VaultBackend};
use std::collections::HashMap;
use workspace_vault::WorkspaceVault;

pub mod actions;
mod bindings;
mod demo;
mod list_pane;
mod pages;
pub mod resource;
mod search;
#[cfg(test)]
mod served_vault;
mod transfer;
mod vault;
mod window;
mod workspace_vault;
pub use crate::vault::Screen;
pub use actions::PasswordsEffect;
pub use resource::{Passwords, PasswordsShell};
pub use window::PasswordsWindow;

/// The items of the person's vault, or none for a window that must not open it (a past
/// version: the vault keeps no history).
pub struct PasswordsModel {
    store: Option<Vault>,
}
enum Vault {
    /// This device's own vault, which it lends the app: a native desktop's.
    Shared(CredentialOwner),
    /// The person's vault on their workspace's server, which every device of theirs reads.
    Workspace(WorkspaceVault),
}

const CLOSED: &str = "the vault is not open in this window";
const OPENING: &str = "the vault is still opening";

ui::embedded_catalog!(pub(crate) static CATALOG, ["de-DE", "en-US"]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VaultHealth {
    pub total: usize,
    pub reused: usize,
    pub weak: usize,
    pub older_than_year: usize,
}

/// Whether an item holds a password a person chose: a login or a Wi-Fi key, not an issued
/// API key or licence code.
fn is_password(item: &SecretItem) -> bool {
    matches!(
        item.kind,
        accounts::SecretItemKind::Login | accounts::SecretItemKind::Wifi
    ) && !item.password.is_empty()
}

/// Per-item facts of the password review: which passwords are shared, weak or old.
pub(crate) fn findings(items: &[SecretItem]) -> Vec<vault::Finding> {
    let candidates: Vec<&SecretItem> = items.iter().filter(|item| is_password(item)).collect();
    let mut frequencies = HashMap::<&str, usize>::new();
    for password in candidates.iter().map(|item| item.password.as_str()) {
        let count = frequencies.entry(password).or_default();
        *count = count.saturating_add(1);
    }
    let now = vault::now_unix();
    let one_year = 365 * 24 * 60 * 60;
    candidates
        .iter()
        .map(|item| {
            let password = item.password.as_str();
            vault::Finding {
                id: item.id.clone(),
                reused: frequencies.get(password).copied().unwrap_or(0) > 1,
                weak: password.len() < 12
                    || !password.chars().any(char::is_uppercase)
                    || !password.chars().any(char::is_lowercase)
                    || !password.chars().any(|character| character.is_ascii_digit()),
                old: now.saturating_sub(item.updated_unix) > one_year,
            }
        })
        .filter(|finding| finding.reused || finding.weak || finding.old)
        .collect()
}

pub(crate) fn analyze_health(items: &[SecretItem]) -> VaultHealth {
    let found = findings(items);
    VaultHealth {
        total: items.iter().filter(|item| is_password(item)).count(),
        reused: found.iter().filter(|finding| finding.reused).count(),
        weak: found.iter().filter(|finding| finding.weak).count(),
        older_than_year: found.iter().filter(|finding| finding.old).count(),
    }
}

impl PasswordsModel {
    /// The person's vault on their workspace's server, as the window's actions reach it: a
    /// browser page's, and any window's whose device lends it no vault of its own.
    pub fn open() -> Self {
        Self {
            store: Some(Vault::Workspace(WorkspaceVault::new(
                ui::RemoteActions::current(),
            ))),
        }
    }
    /// The matching native device's original vault and its permission broker.
    pub fn shared(owner: CredentialOwner) -> Self {
        Self {
            store: Some(Vault::Shared(owner)),
        }
    }
    pub fn closed() -> Self {
        Self { store: None }
    }
    /// Whether this window opens the vault at all: a past version or another person's
    /// window does not, and says nothing about its lock.
    pub fn opens_vault(&self) -> bool {
        self.store.is_some()
    }
    /// Whether the vault can be read now. A window on the compositor's frame loop, which
    /// never waits on the disk, opens the person's vault on a thread of its own, which
    /// calls `ready` once it is open; anywhere else it is opened by the first read. The
    /// workspace's vault is read once its server answers, which wakes the window.
    pub fn open_without_waiting(&self, ready: impl FnOnce() + Send + 'static) -> bool {
        match &self.store {
            Some(Vault::Shared(owner)) if ui::storage::waiting::forbidden() => {
                owner.open_without_waiting(ready)
            }
            Some(Vault::Workspace(vault)) => vault.is_open() || vault.trouble().is_some(),
            _ => true,
        }
    }
    /// Refuse to open the vault on a thread that must never wait on the disk: there it is
    /// opened by [`Self::open_without_waiting`].
    fn opened(&self) -> Result<(), String> {
        match &self.store {
            Some(Vault::Shared(owner)) if !owner.is_open() && ui::storage::waiting::forbidden() => {
                Err(OPENING.to_owned())
            }
            Some(Vault::Workspace(vault)) if !vault.is_open() => {
                Err(vault.trouble().unwrap_or_else(|| OPENING.to_owned()))
            }
            _ => Ok(()),
        }
    }
    /// Whether what this window lists is the vault's: refused with why while it is still opening
    /// or its server can't be read, so an empty list is never taken for an empty vault.
    ///
    /// # Errors
    ///
    /// Why the vault can't be listed yet.
    pub fn readable(&self) -> Result<(), String> {
        self.opened()
    }
    /// Read this device's vault.
    fn read<T>(&self, read: impl FnOnce(&SecretStore) -> T) -> Result<T, String> {
        self.opened()?;
        match self.store.as_ref().ok_or_else(|| CLOSED.to_owned())? {
            Vault::Shared(owner) => owner
                .read(read)
                .map_err(|error| format!("could not access vault: {error:?}")),
            Vault::Workspace(_) => Err(CLOSED.to_owned()),
        }
    }
    /// Change this device's vault.
    fn edit<T>(&mut self, edit: impl FnOnce(&mut SecretStore) -> T) -> Result<T, String> {
        self.opened()?;
        match self.store.as_mut().ok_or_else(|| CLOSED.to_owned())? {
            Vault::Shared(owner) => owner
                .edit(edit)
                .map_err(|error| format!("could not access vault: {error:?}")),
            Vault::Workspace(_) => Err(CLOSED.to_owned()),
        }
    }
    /// The workspace's vault, once it is open.
    fn workspace(&self) -> Result<Option<&WorkspaceVault>, String> {
        match &self.store {
            Some(Vault::Workspace(vault)) => {
                self.opened()?;
                Ok(Some(vault))
            }
            _ => Ok(None),
        }
    }
    pub fn recovery_error(&self) -> Option<String> {
        match self.store.as_ref()? {
            Vault::Shared(_) => self
                .read(|store| store.recovery_error().map(str::to_owned))
                .unwrap_or_else(Some),
            Vault::Workspace(_) => None,
        }
    }
    /// Why the workspace's vault can't be read from here now, in words for the person.
    pub fn unreachable(&self) -> Option<String> {
        match &self.store {
            Some(Vault::Workspace(vault)) if !vault.is_open() => vault.trouble(),
            _ => None,
        }
    }
    pub fn items(&self) -> Vec<SecretItem> {
        match self.workspace() {
            Ok(Some(vault)) => vault.list(),
            Ok(None) => self.read(SecretStore::list).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }
    pub fn app_grants(&self) -> Vec<CredentialGrant> {
        if self.opened().is_err() {
            return Vec::new();
        }
        match &self.store {
            Some(Vault::Shared(owner)) => owner.grants().unwrap_or_default(),
            _ => Vec::new(),
        }
    }
    pub fn app_proposals(&self) -> Vec<accounts::CredentialProposal> {
        if self.opened().is_err() {
            return Vec::new();
        }
        match &self.store {
            Some(Vault::Shared(owner)) => owner.proposals().unwrap_or_default(),
            Some(Vault::Workspace(_)) => tilo_messaging::service::proposals(),
            None => Vec::new(),
        }
    }
    pub fn proposal_wake_after(&self) -> Option<std::time::Duration> {
        match &self.store {
            Some(Vault::Shared(owner)) => owner.proposal_wake_after(),
            _ => None,
        }
    }
    /// Decide a proposal an app made for access to an item.
    ///
    /// # Errors
    ///
    /// Refuses missing original ownership, stale proposals or retired admission.
    pub fn decide_app_proposal(
        &self,
        id: &str,
        decision: accounts::AccessDecision,
    ) -> Result<(), String> {
        match &self.store {
            Some(Vault::Shared(owner)) => owner
                .decide_proposal(id, decision)
                .map(drop)
                .map_err(|_| CATALOG.t("passwords-request-expired")),
            Some(Vault::Workspace(_)) => tilo_messaging::service::decide(id, decision)
                .map_err(|_| CATALOG.t("passwords-request-expired")),
            None => Err(CATALOG.t("passwords-request-expired")),
        }
    }
    /// Read the vault again: the workspace's is asked again once what it showed grew stale.
    pub fn reload(&mut self) {}
    pub fn revision(&self) -> Option<u64> {
        match &self.store {
            Some(Vault::Shared(owner)) => owner.revision(),
            Some(Vault::Workspace(vault)) => Some(
                vault
                    .revision()
                    .wrapping_mul(1 << 20)
                    .wrapping_add(tilo_messaging::service::proposals_revision().unwrap_or(0)),
            ),
            None => None,
        }
    }
    /// Where the encryption key for this device's vault is held; `None` for the workspace's,
    /// which its server keeps. A `LocalFile` result is still encrypted storage, but it is not
    /// a system keyring and the UI must not present it as one.
    pub fn backend(&self) -> Option<VaultBackend> {
        match &self.store {
            Some(Vault::Workspace(_)) => None,
            _ => Some(
                self.read(SecretStore::backend)
                    .unwrap_or_else(|_| VaultBackend::for_session()),
            ),
        }
    }
    /// Revoke one exact app, account and origin relationship.
    ///
    /// # Errors
    ///
    /// Why the vault cannot durably store the revocation.
    pub fn revoke_app_grant(&mut self, grant: &CredentialGrant) -> Result<bool, String> {
        self.opened()?;
        match &mut self.store {
            Some(Vault::Shared(owner)) => owner
                .revoke_grant(grant)
                .map_err(|error| format!("could not revoke app access: {error:?}")),
            _ => Ok(false),
        }
    }
    pub fn search(&self, query: &str) -> Vec<SecretItem> {
        let query = query.trim().to_lowercase();
        self.items()
            .into_iter()
            .filter(|item| {
                query.is_empty()
                    || item.title.to_lowercase().contains(&query)
                    || item.username.to_lowercase().contains(&query)
                    || item
                        .origins
                        .iter()
                        .any(|origin| origin.to_lowercase().contains(&query))
                    || item
                        .fields
                        .keys()
                        .any(|name| name.to_lowercase().contains(&query))
                    || item
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .collect()
    }
    pub fn health(&self) -> VaultHealth {
        analyze_health(&self.items())
    }
    pub fn save_item(&mut self, item: &mut SecretItem) -> Result<SecretId, String> {
        let id = item.id.clone();
        self.update(item)?;
        Ok(id)
    }
    /// Finish writing what [`Self::save_item`] and [`Self::remove`] left in flight:
    /// `Ok(false)` while the frame loop's writer thread or the workspace's server is still
    /// at it, `Ok(true)` once it is kept.
    ///
    /// # Errors
    ///
    /// Why the store refused the write.
    pub fn flush(&mut self) -> Result<bool, String> {
        match &self.store {
            None => Ok(true),
            Some(Vault::Workspace(vault)) => vault.flush(),
            Some(Vault::Shared(_)) => self
                .read(SecretStore::flush)?
                .map(|saved| saved == accounts::Saved::Durable)
                .map_err(|error| format!("{error:?}")),
        }
    }
    /// Remove one stored login.
    ///
    /// # Errors
    ///
    /// Why the secret store cannot complete the removal.
    pub fn remove(&mut self, id: &SecretId) -> Result<bool, String> {
        if let Some(vault) = self.workspace()? {
            return vault.remove(id);
        }
        self.edit(|store| store.remove(id))?
            .map_err(|e| format!("could not remove login: {e:?}"))
    }
    /// Persist an edited login.
    ///
    /// # Errors
    ///
    /// Why the secret store cannot persist the update.
    pub fn update(&mut self, item: &mut SecretItem) -> Result<(), String> {
        if let Some(vault) = self.workspace()? {
            return vault.put(item);
        }
        self.edit(|store| store.put(item))?
            .map_err(|error| format!("could not save item: {error:?}"))
    }
    /// Reveal a password into zeroizing memory for an explicit user action.
    ///
    /// # Errors
    ///
    /// Why the item is missing or the store cannot reveal it.
    pub fn reveal(&self, id: &SecretId) -> Result<zeroize::Zeroizing<String>, String> {
        if let Some(vault) = self.workspace()? {
            return vault
                .get(id)
                .map(|item| zeroize::Zeroizing::new(item.password))
                .ok_or_else(|| "item no longer exists".to_owned());
        }
        self.read(|store| store.reveal_password(id))?
            .map_err(|error| format!("could not reveal login: {error:?}"))
    }
    pub fn reveal_value(&self, id: &SecretId) -> Result<zeroize::Zeroizing<String>, String> {
        let item = match self.workspace()? {
            Some(vault) => vault.get(id),
            None => self.read(|store| store.get(id))?,
        }
        .ok_or_else(|| "item no longer exists".to_string())?;
        let value = if item.kind == accounts::SecretItemKind::SecureNote {
            item.notes
        } else {
            item.password
        };
        Ok(zeroize::Zeroizing::new(value))
    }
}

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(PasswordsShell)
}

#[cfg(test)]
mod tests {
    use super::*;
    tilo_test_env::scenario! {
        fn the_model_persists_secrets_and_searches_only_their_metadata;

        phase every_secret_kind_persists_and_round_trips;
        {
            eprintln!("phase a login's crud round trips");
            {
                let _vault = crate::served_vault::served();
                let mut model = PasswordsModel::open();
                let mut item = SecretItem::login("Example", "ada", "original secret");
                item.add_origin("https://example.com/login").unwrap();
                let id = model.save_item(&mut item).unwrap();
                let item = model.items().pop().unwrap();
                assert_eq!(item.id, id);
                assert!(item.origins.contains("https://example.com"));
                assert_eq!(model.search("EXAMP").len(), 1);
                let mut edited = item;
                edited.title = "Renamed".into();
                edited.password = "replacement secret".into();
                model.update(&mut edited).unwrap();
                assert_eq!(model.reveal(&id).unwrap().as_str(), "replacement secret");
                assert!(model.remove(&id).unwrap());
            }

            eprintln!("phase every_supported_secret_kind_persists_and_reveals_its_primary_value");
            {
                use accounts::SecretItemKind;

                let _vault = crate::served_vault::served();
                let mut model = PasswordsModel::open();
                let cases = [
                    (SecretItemKind::Login, "Login", "login-secret", ""),
                    (SecretItemKind::ApiKey, "API key", "api-secret", ""),
                    (SecretItemKind::SecureNote, "Note", "", "note-secret"),
                    (SecretItemKind::Wifi, "Wi-Fi", "wifi-secret", ""),
                    (SecretItemKind::Other, "Other", "other-secret", ""),
                ];

                for (kind, title, password, notes) in cases {
                    let mut item = SecretItem::login(title, "owner", password);
                    item.kind = kind;
                    item.notes = notes.into();
                    let id = model.save_item(&mut item).unwrap();
                    let expected = if kind == SecretItemKind::SecureNote {
                        notes
                    } else {
                        password
                    };
                    assert_eq!(model.reveal_value(&id).unwrap().as_str(), expected);
                }

                let stored = model.items();
                assert_eq!(stored.len(), cases.len());
                for (kind, ..) in cases {
                    assert!(stored.iter().any(|item| item.kind == kind));
                }
            }
        }

        phase search_and_health_use_only_local_non_secret_metadata;
        {
            eprintln!("phase search_indexes_only_non_secret_local_metadata");
            {
                let _vault = crate::served_vault::served();
                let mut model = PasswordsModel::open();
                let mut item = SecretItem::login("Payroll", "ada", "DoNotIndexThis7");
                item.add_origin("https://people.example/pay").unwrap();
                item.notes = "Managed by Finance".into();
                item.fields.insert("account number".into(), "A-1942".into());
                item.tags.insert("work".into());
                model.save_item(&mut item).unwrap();

                for query in ["people.example", "account number", "work"] {
                    assert_eq!(model.search(query).len(), 1, "query: {query}");
                }
                for secret in ["DoNotIndexThis7", "managed by finance", "a-1942"] {
                    assert!(model.search(secret).is_empty(), "secret query: {secret}");
                }
            }

            eprintln!("phase health_reports_only_locally_provable_findings");
            {
                let mut reused_a = SecretItem::login("A", "ada", "SharedPassword7");
                let reused_b = SecretItem::login("B", "ada", "SharedPassword7");
                let weak = SecretItem::login("C", "ada", "tiny");
                reused_a.updated_unix = 1;
                let health = analyze_health(&[reused_a, reused_b, weak]);
                assert_eq!(health.total, 3);
                assert_eq!(health.reused, 2);
                assert_eq!(health.weak, 1);
                assert_eq!(health.older_than_year, 1);
            }
        }
    }
}

/// An entry saved in one window is kept on the workspace's server and nowhere else, and a
/// window another device of the person's opens reads it there; one forgotten there is gone
/// from the next window too.
#[cfg(test)]
mod workspace_vault_tests {
    use super::*;

    #[test]
    fn an_entry_saved_in_one_window_is_read_in_another() {
        let served = crate::served_vault::served();
        let mut here = PasswordsModel::open();
        let mut entry = SecretItem::login("Bank", "ada", "kept-on-the-server-7Q");
        entry.add_origin("https://bank.example").unwrap();
        let id = here.save_item(&mut entry).unwrap();
        assert_eq!(here.flush(), Ok(true), "the server kept it");
        let kept = served.kept.lock().unwrap().clone();
        assert_eq!(kept.len(), 1, "the server keeps the entry");
        assert_eq!(kept[&id.0]["password"], "kept-on-the-server-7Q");

        let there = PasswordsModel::open();
        let read = there.items();
        assert_eq!(read.len(), 1, "another window reads it");
        assert_eq!(read[0].title, "Bank");
        assert_eq!(there.reveal(&id).unwrap().as_str(), "kept-on-the-server-7Q");

        assert!(here.remove(&id).unwrap());
        assert!(
            served.kept.lock().unwrap().is_empty(),
            "forgotten on the server"
        );
        assert!(PasswordsModel::open().items().is_empty());
    }
}

/// German is one of the languages the app ships, and it reads in German: the copy comes
/// from `locales/de-DE.ftl`, never from the English source catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("passwords-new-item"), "Neuer Eintrag");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("passwords-new-item"), "New item");
    }
}
