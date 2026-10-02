//! Passwords — the user-facing view of Tilo credential custody.
//!
//! The live view starts without sample entries and reveals secrets only on request.
//! Apps obtain credentials through `accounts::CredentialBroker`; this process
//! is for organizing, reviewing and revoking them.

use accounts::{
    CredentialGrant, GeneratedPasswordPolicy, SecretCustody, SecretId, SecretItem, SecretStore,
};
use std::collections::HashMap;
use ui::widget::Widget;

mod actions;
mod doc;
pub mod resource;
mod showcase;
mod transfer;
pub use showcase::{PasswordsSurface, ShowcaseMode};

pub struct PasswordsModel {
    store: SecretStore,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VaultHealth {
    pub total: usize,
    pub reused: usize,
    pub weak: usize,
    pub older_than_year: usize,
}

pub(crate) fn analyze_health(items: &[SecretItem]) -> VaultHealth {
    let candidates: Vec<&SecretItem> = items
        .iter()
        .filter(|item| !item.password.is_empty())
        .collect();
    let mut frequencies = HashMap::<&str, usize>::new();
    for password in candidates.iter().map(|item| item.password.as_str()) {
        let count = frequencies.entry(password).or_default();
        *count = count.saturating_add(1);
    }
    let reused = candidates
        .iter()
        .filter(|item| {
            frequencies
                .get(item.password.as_str())
                .copied()
                .unwrap_or(0)
                > 1
        })
        .count();
    let weak = candidates
        .iter()
        .filter(|item| {
            let password = item.password.as_str();
            password.len() < 12
                || !password.chars().any(char::is_uppercase)
                || !password.chars().any(char::is_lowercase)
                || !password.chars().any(|character| character.is_ascii_digit())
        })
        .count();
    let now = ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let one_year = 365 * 24 * 60 * 60;
    VaultHealth {
        total: candidates.len(),
        reused,
        weak,
        older_than_year: candidates
            .iter()
            .filter(|item| now.saturating_sub(item.updated_unix) > one_year)
            .count(),
    }
}

impl PasswordsModel {
    pub fn open() -> Self {
        Self {
            store: SecretStore::open(),
        }
    }
    pub fn open_in(root: &std::path::Path) -> Self {
        Self {
            store: SecretStore::open_in(root),
        }
    }
    pub fn recovery_error(&self) -> Option<&str> {
        self.store.recovery_error()
    }
    pub fn items(&self) -> Vec<SecretItem> {
        self.store.list()
    }
    /// Durable application access shown in the vault's management UI.
    pub fn app_grants(&self) -> Vec<CredentialGrant> {
        self.store.grants()
    }
    /// Where the encryption key for this vault is currently held. A
    /// `LocalFile` result is still encrypted storage, but it is not a system
    /// keyring and the UI must not present it as one.
    pub fn custody(&self) -> SecretCustody {
        self.store.custody()
    }
    /// Revoke one exact app/account/origin relationship.
    ///
    /// # Errors
    /// Returns an error if the vault cannot durably store the revocation.
    pub fn revoke_app_grant(&mut self, grant: &CredentialGrant) -> Result<bool, String> {
        self.store
            .revoke_grant(grant)
            .map_err(|error| format!("could not revoke app access: {error:?}"))
    }
    /// Revoke all durable vault access for an application.
    ///
    /// # Errors
    /// Returns an error if the vault cannot durably store the revocations.
    pub fn revoke_all_app_grants(&mut self, app_id: &str) -> Result<usize, String> {
        self.store
            .revoke_app_grants(app_id)
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
                    // Search is local to the open vault, but notes and custom
                    // values frequently *are* secrets. Never make them a
                    // search index, which also keeps them out of search
                    // history/provider integrations.
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
    /// Analyze only facts available in the local vault. This deliberately
    /// makes no breach claim: that requires a separately configured service.
    pub fn health(&self) -> VaultHealth {
        analyze_health(&self.items())
    }
    /// Save a login, generating a password when `password` is empty.
    ///
    /// # Errors
    ///
    /// Returns a people-facing error when the origin is invalid, password
    /// generation fails, or the secret store cannot persist the item.
    pub fn save_login(
        &mut self,
        title: &str,
        username: &str,
        password: &str,
        origin: Option<&str>,
    ) -> Result<SecretId, String> {
        let password = if password.is_empty() {
            GeneratedPasswordPolicy::default()
                .generate()
                .map_err(|e| format!("password generation failed: {e:?}"))?
                .to_string()
        } else {
            password.to_string()
        };
        let mut item = SecretItem::login(title, username, password);
        if let Some(origin) = origin {
            item.add_origin(origin)
                .map_err(|e| format!("invalid sign-in address: {e:?}"))?;
        }
        let id = item.id.clone();
        self.store
            .put(&mut item)
            .map_err(|e| format!("could not save login: {e:?}"))?;
        Ok(id)
    }
    /// Persist any supported secret item.
    ///
    /// # Errors
    /// Returns an error when serialization or durable credential storage fails.
    pub fn save_item(&mut self, item: &mut SecretItem) -> Result<SecretId, String> {
        let id = item.id.clone();
        self.store
            .put(item)
            .map_err(|error| format!("could not save item: {error:?}"))?;
        Ok(id)
    }
    /// Remove one stored login.
    ///
    /// # Errors
    ///
    /// Returns an error when the secret store cannot complete the removal.
    pub fn remove(&mut self, id: &SecretId) -> Result<bool, String> {
        self.store
            .remove(id)
            .map_err(|e| format!("could not remove login: {e:?}"))
    }
    /// Persist an edited login.
    ///
    /// # Errors
    ///
    /// Returns an error when the secret store cannot persist the update.
    pub fn update(&mut self, item: &mut SecretItem) -> Result<(), String> {
        self.store
            .put(item)
            .map_err(|error| format!("could not update login: {error:?}"))
    }
    /// Reveal a password into zeroizing memory for an explicit user action.
    ///
    /// # Errors
    ///
    /// Returns an error when the item is missing or the store cannot reveal it.
    pub fn reveal(&self, id: &SecretId) -> Result<zeroize::Zeroizing<String>, String> {
        self.store
            .reveal_password(id)
            .map_err(|error| format!("could not reveal login: {error:?}"))
    }
    /// Return the primary copyable value for the item's type.
    ///
    /// # Errors
    /// Returns an error if the item no longer exists.
    pub fn reveal_value(&self, id: &SecretId) -> Result<zeroize::Zeroizing<String>, String> {
        let item = self
            .store
            .get(id)
            .ok_or_else(|| "item no longer exists".to_string())?;
        let value = if item.kind == accounts::SecretItemKind::SecureNote {
            item.notes
        } else {
            item.password
        };
        Ok(zeroize::Zeroizing::new(value))
    }
}

pub fn app_root() -> Box<dyn Widget> {
    Box::new(PasswordsSurface::live())
}

/// The app as every host mounts it: the live root.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.passwords", app_root).as_definition("Passwords")
}
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(PasswordsSurface::demo(ShowcaseMode::Vault))
}

pub fn app_root_showcase(mode: ShowcaseMode) -> Box<dyn Widget> {
    Box::new(PasswordsSurface::demo(mode))
}

pub fn app_root_long_list_showcase() -> Box<dyn Widget> {
    Box::new(PasswordsSurface::demo_long_list())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_secret_kind_persists_round_trips_and_generates_missing_passwords() {
        eprintln!("phase empty_password_is_generated_and_crud_round_trips");
        {
            let root =
                std::env::temp_dir().join(format!("tilo-passwords-test-{}", std::process::id()));
            let mut model = PasswordsModel::open_in(&root);
            let id = model
                .save_login("Example", "ada", "", Some("https://example.com/login"))
                .unwrap();
            let item = model.items().pop().unwrap();
            assert_eq!(item.id, id);
            assert!(item.password.len() >= 20);
            assert!(item.origins.contains("https://example.com"));
            assert_eq!(model.search("EXAMP").len(), 1);
            let mut edited = item;
            edited.title = "Renamed".into();
            edited.password = "replacement secret".into();
            model.update(&mut edited).unwrap();
            assert_eq!(model.reveal(&id).unwrap().as_str(), "replacement secret");
            assert!(model.remove(&id).unwrap());
            std::fs::remove_dir_all(root).ok();
        }

        eprintln!("phase every_supported_secret_kind_persists_and_reveals_its_primary_value");
        {
            use accounts::SecretItemKind;

            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-kinds-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = PasswordsModel::open_in(&root);
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
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn search_and_health_use_only_local_non_secret_metadata() {
        eprintln!("phase search_indexes_only_non_secret_local_metadata");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-search-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = PasswordsModel::open_in(&root);
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
            let _ = std::fs::remove_dir_all(root);
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
#[cfg(target_arch = "wasm32")]
pub use resource::{Passwords, PasswordsShell};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{Passwords, PasswordsShell, launch_spec};
