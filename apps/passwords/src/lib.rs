//! Passwords — the workspace's vault of logins, keys and codes.
//!
//! A workspace has one vault: Passwords' workspace vault document, whose entries any
//! member the workspace grants read opens under the document's own keys. A window shows that document and
//! nothing else, whether it stands on a shared workspace or on the person's own desktop.
//!
//! Two kinds of secret stay on the device and never enter the workspace:
//!
//! - the device's authentication material, items carrying the messaging profile scope
//!   (`SecretItem::may_share_with_workspace`), and
//! - the Wi-Fi networks of this one device (`SecretItemKind::Wifi`).
//!
//! They stay in the device's own secret service because they belong to one device rather
//! than the workspace: a workspace shares the logins its members use, not the sign-in
//! material of a particular machine or the networks that machine joined. They are neither
//! shown nor converted.
//!
//! A window that opens in a workspace converts, once, what its device vault holds that a
//! workspace may share: each such item is sealed into the workspace document, and taken
//! out of the device vault only once the document holds it. The conversion is idempotent —
//! an item the document already holds (or once held) is only taken out of the device
//! vault — so opening the window again converts nothing twice.

use accounts::{CredentialGrant, CredentialOwner, SecretId, SecretItem};
use std::collections::HashMap;

pub mod actions;
mod bindings;
mod demo;
mod list_pane;
mod pages;
pub mod resource;
mod search;
#[cfg(test)]
mod test_vault;
mod transfer;
mod vault;
mod window;
mod workspace_vault;
pub use crate::vault::Screen;
pub use actions::PasswordsEffect;
pub use resource::{Passwords, PasswordsShell};
pub use window::PasswordsWindow;

/// The items of the workspace vault, or none for a window that must not show one (a
/// past version, a window outside every workspace).
pub struct PasswordsModel {
    vault: Option<Workspace>,
}

struct Workspace {
    entries: workspace_vault::WorkspaceVault,
    /// The device's own vault, lent by the session for this person on this device: its app
    /// grants and proposals, and what the conversion reads.
    owner: Option<CredentialOwner>,
    /// Whether the conversion may read this device's own secret store when the session lent
    /// no owner: a window in an app of its own, or a development launch.
    local_device_vault: bool,
    /// What is left of the one conversion.
    conversion: Conversion,
}

/// The one conversion of the device vault's shareable items into the workspace document.
enum Conversion {
    /// Not done: the next refresh reads the device vault and seals what it may share.
    Pending,
    /// Sealed into the document, but the save is not kept yet: these entries leave the
    /// device vault once it is, never before.
    Saving(Vec<SecretId>),
    Done,
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

/// Whether an item of the device vault belongs in the workspace: what a workspace may share,
/// which leaves the device's authentication material, and the Wi-Fi networks that device
/// joined, where they are.
fn travels_to_workspace(item: &SecretItem) -> bool {
    item.may_share_with_workspace() && item.kind != accounts::SecretItemKind::Wifi
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
    /// The vault in `workspace`: what a live window shows.
    pub(crate) fn workspace(
        opener: ui::framework::ResourceOpener<passwords_model::WorkspaceVaultEntries>,
        device_owner: Option<CredentialOwner>,
        local_device_vault: bool,
    ) -> Self {
        Self {
            vault: Some(Workspace {
                entries: workspace_vault::WorkspaceVault::new(opener),
                owner: device_owner,
                local_device_vault,
                conversion: Conversion::Pending,
            }),
        }
    }
    pub fn closed() -> Self {
        Self { vault: None }
    }
    /// Whether this window shows the vault at all: a past version or another person's
    /// window shows none, and says nothing about its lock.
    pub fn opens_vault(&self) -> bool {
        self.vault.is_some()
    }
    /// Whether the vault can be read now. A window on the compositor's frame loop, which
    /// never waits on the disk, opens the device's credential owner on a thread of its own,
    /// which calls `ready` once it is open; the workspace vault is read where it is.
    pub fn open_without_waiting(&self, ready: impl FnOnce() + Send + 'static) -> bool {
        if let Some(owner) = self.credential_owner()
            && ui::storage::waiting::forbidden()
        {
            owner.open_without_waiting(ready);
        }
        true
    }
    fn credential_owner(&self) -> Option<&CredentialOwner> {
        self.vault.as_ref()?.owner.as_ref()
    }
    /// Whether the lent device vault may be read now.
    fn owner_readable(&self) -> bool {
        self.credential_owner()
            .is_none_or(|owner| owner.is_open() || !ui::storage::waiting::forbidden())
    }
    /// Convert what the device vault holds that may be shared into the workspace document,
    /// once per window.
    ///
    /// Each converted item is removed from the device vault only after the document has
    /// kept it: the removal waits for the save, and the next refresh finishes it. The
    /// device's authentication material and its Wi-Fi networks stay where they are
    /// ([`travels_to_workspace`]).
    ///
    /// # Errors
    ///
    /// Why the device vault cannot be read or the document cannot be written. What was
    /// converted stays converted; the next refresh converts the rest.
    pub(crate) fn convert_device_vault(&mut self) -> Result<usize, String> {
        let Some(workspace) = &mut self.vault else {
            return Err(CLOSED.to_owned());
        };
        match std::mem::replace(&mut workspace.conversion, Conversion::Done) {
            Conversion::Done => Ok(0),
            Conversion::Saving(ids) => {
                if !workspace.entries.flush()? {
                    workspace.conversion = Conversion::Saving(ids);
                    return Ok(0);
                }
                workspace_vault::remove_device_items(
                    workspace.owner.as_ref(),
                    workspace.local_device_vault,
                    &ids,
                )?;
                Ok(0)
            }
            Conversion::Pending => {
                let Some((ids, sealed)) = workspace
                    .entries
                    .convert_device_vault(workspace.owner.as_ref(), workspace.local_device_vault)?
                else {
                    // Waiting for the lent device vault to open on its own thread: the
                    // window that woke for it converts on its next refresh.
                    let waiting = workspace
                        .owner
                        .as_ref()
                        .is_some_and(|owner| !owner.is_open() && ui::storage::waiting::forbidden());
                    if waiting {
                        workspace.conversion = Conversion::Pending;
                        return Err(OPENING.to_owned());
                    }
                    // This window cannot read a device vault on this thread: an embedded
                    // window with none lent, or another device's. It has nothing to convert,
                    // and says nothing more about it.
                    return Ok(0);
                };
                if ids.is_empty() || !workspace.entries.flush()? {
                    // Nothing leaves the device vault: there is nothing to keep before
                    // finishing. Otherwise the removal waits for the document's save.
                    if !ids.is_empty() {
                        workspace.conversion = Conversion::Saving(ids);
                    }
                    return Ok(sealed);
                }
                workspace_vault::remove_device_items(
                    workspace.owner.as_ref(),
                    workspace.local_device_vault,
                    &ids,
                )?;
                Ok(sealed)
            }
        }
    }
    /// Whether the conversion still has work left, so the window keeps refreshing for it.
    pub(crate) fn conversion_pending(&self) -> bool {
        self.vault
            .as_ref()
            .is_some_and(|workspace| !matches!(workspace.conversion, Conversion::Done))
    }
    pub fn recovery_error(&self) -> Option<String> {
        self.vault.as_ref()?.entries.check_read().err()
    }
    pub fn unreachable(&self) -> Option<String> {
        self.vault.as_ref()?.entries.check_read().err()
    }
    pub fn items(&self) -> Vec<SecretItem> {
        self.vault
            .as_ref()
            .and_then(|workspace| workspace.entries.items().ok())
            .unwrap_or_default()
    }
    pub fn app_grants(&self) -> Vec<CredentialGrant> {
        if !self.owner_readable() {
            return Vec::new();
        }
        self.credential_owner()
            .and_then(|owner| owner.grants().ok())
            .unwrap_or_default()
    }
    pub fn app_proposals(&self) -> Vec<accounts::CredentialProposal> {
        if !self.owner_readable() {
            return Vec::new();
        }
        match self.credential_owner() {
            Some(owner) => owner.proposals().unwrap_or_default(),
            None => tilo_messaging::service::proposals(),
        }
    }
    pub fn proposal_wake_after(&self) -> Option<std::time::Duration> {
        self.credential_owner()
            .and_then(CredentialOwner::proposal_wake_after)
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
        if let Some(owner) = self.credential_owner() {
            return owner
                .decide_proposal(id, decision)
                .map(drop)
                .map_err(|_| CATALOG.t("passwords-request-expired"));
        }
        tilo_messaging::service::decide(id, decision)
            .map_err(|_| CATALOG.t("passwords-request-expired"))
    }
    pub fn revision(&self) -> Option<u64> {
        let workspace = self.vault.as_ref()?;
        Some(
            workspace.entries.revision().rotate_left(13)
                ^ workspace
                    .owner
                    .as_ref()
                    .and_then(CredentialOwner::revision)
                    .unwrap_or(0),
        )
    }
    /// Revoke one exact app, account and origin relationship.
    ///
    /// # Errors
    ///
    /// Why the vault cannot durably store the revocation.
    pub fn revoke_app_grant(&mut self, grant: &CredentialGrant) -> Result<bool, String> {
        let owner = self.credential_owner().ok_or_else(|| CLOSED.to_owned())?;
        owner
            .revoke_grant(grant)
            .map_err(|error| format!("could not revoke app access: {error:?}"))
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
        self.update(item)?;
        Ok(item.id.clone())
    }
    /// Finish writing what [`Self::save_item`] and [`Self::remove`] left in flight:
    /// `Ok(false)` while the workspace is still saving, `Ok(true)` once it is kept.
    ///
    /// # Errors
    ///
    /// Why the document refused the write.
    pub fn flush(&mut self) -> Result<bool, String> {
        match &self.vault {
            Some(workspace) => workspace.entries.flush(),
            None => Ok(true),
        }
    }
    /// Remove one stored login.
    ///
    /// # Errors
    ///
    /// Why the workspace vault cannot complete the removal.
    pub fn remove(&mut self, id: &SecretId) -> Result<bool, String> {
        self.vault
            .as_ref()
            .ok_or_else(|| CLOSED.to_owned())?
            .entries
            .remove(id)
    }
    /// Persist an edited login.
    ///
    /// # Errors
    ///
    /// Why the workspace vault cannot persist the update.
    pub fn update(&mut self, item: &mut SecretItem) -> Result<(), String> {
        let workspace = self.vault.as_ref().ok_or_else(|| CLOSED.to_owned())?;
        let last_updated = item.updated_unix;
        item.updated_unix = vault::now_unix();
        if let Err(error) = workspace.entries.put(item) {
            item.updated_unix = last_updated;
            return Err(error);
        }
        Ok(())
    }
    /// Reveal a password into zeroizing memory for an explicit user action.
    ///
    /// # Errors
    ///
    /// Why the item is missing or the workspace vault cannot open it.
    pub fn reveal(&self, id: &SecretId) -> Result<zeroize::Zeroizing<String>, String> {
        self.vault
            .as_ref()
            .ok_or_else(|| CLOSED.to_owned())?
            .entries
            .item(id)
            .map(|item| zeroize::Zeroizing::new(item.password))
    }
    pub fn reveal_value(&self, id: &SecretId) -> Result<zeroize::Zeroizing<String>, String> {
        let item = self
            .vault
            .as_ref()
            .ok_or_else(|| CLOSED.to_owned())?
            .entries
            .item(id)?;
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
                let mut app = crate::test_vault::workspace_window().unwrap();
                with_model(&mut app, |model| {
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
                });
            }

            eprintln!("phase every_supported_secret_kind_persists_and_reveals_its_primary_value");
            {
                use accounts::SecretItemKind;

                let mut app = crate::test_vault::workspace_window().unwrap();
                with_model(&mut app, |model| {
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
                });
            }
        }

        phase search_and_health_use_only_local_non_secret_metadata;
        {
            eprintln!("phase search_indexes_only_non_secret_local_metadata");
            {
                let mut app = crate::test_vault::workspace_window().unwrap();
                with_model(&mut app, |model| {
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
                });
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

    /// Do `act` with the vault model of the window `app` shows: the workspace document,
    /// through the production opener.
    fn with_model<T>(
        app: &mut ui::framework::testing::TestApp<PasswordsShell>,
        act: impl FnOnce(&mut PasswordsModel) -> T,
    ) -> T {
        let mut core = app.surface_mut::<PasswordsWindow>().core_mut();
        act(core.model.as_mut().expect("the workspace vault is mounted"))
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
