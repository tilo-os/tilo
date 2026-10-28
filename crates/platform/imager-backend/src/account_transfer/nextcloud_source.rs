//! Read-only native Nextcloud HTTP credential migration after exact account selection.
//! Upstream custody key: <https://github.com/nextcloud/desktop/blob/master/src/libsync/creds/abstractcredentials.cpp>
use super::{Candidate, ConnectionError, Connector, Origin, Provider, SourceRoots};
use std::path::Path;
use zeroize::Zeroizing;

use crate::account_sources::{
    NextcloudAccount as NativeAccount, nextcloud_accounts as accounts,
    nextcloud_source_id as source_id,
};

pub(super) trait Keychain {
    fn read(&self, account: &NativeAccount) -> Result<Zeroizing<String>, ConnectionError>;
}
struct NativeKeychain;
impl Keychain for NativeKeychain {
    fn read(&self, account: &NativeAccount) -> Result<Zeroizing<String>, ConnectionError> {
        let request = tilo_secrets::owner_migration::NextcloudCredential::new(
            &account.server,
            &account.username,
            &account.account_id,
        )
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let secret = request
            .read_blocking()
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let value = std::str::from_utf8(secret.expose())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        Ok(Zeroizing::new(value.to_owned()))
    }
}
impl Connector {
    /// Read one owner-selected native Nextcloud HTTP credential into sealed Tilo custody.
    ///
    /// # Errors
    /// Refuses changed/foreign source configuration, unsupported custody or missing credentials.
    pub fn import_nextcloud(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
    ) -> Result<accounts::AccountId, ConnectionError> {
        self.import_nextcloud_with(roots, selected, &NativeKeychain)
    }
    pub(super) fn import_nextcloud_with(
        &self,
        roots: &SourceRoots,
        selected: &Candidate,
        keychain: &impl Keychain,
    ) -> Result<accounts::AccountId, ConnectionError> {
        if selected.provider != Provider::Nextcloud
            || selected.origin != Origin::Nextcloud
            || !selected.can_import_credentials
            || selected.profile != roots.config.join("Nextcloud/nextcloud.cfg")
        {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let source = crate::migration_directory::PinnedDirectory::open_absolute(&roots.config)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let before = super::discovery::metadata(&roots.config, &selected.profile)
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::AccountMissing)?;
        use cap_primitives::fs::MetadataExt;
        let metadata =
            crate::nofollow::metadata_nofollow(&source.file, Path::new("Nextcloud/nextcloud.cfg"))
                .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let found = accounts(&before)
            .into_iter()
            .filter(|account| source_id(account) == selected.id)
            .collect::<Vec<_>>();
        let [account] = found.as_slice() else {
            return Err(ConnectionError::AccountMissing);
        };
        if !account.supported
            || selected.identity.as_deref() != Some(&account.username)
            || selected.server.as_deref() != Some(&account.server)
            || selected.label != account.label
        {
            return Err(ConnectionError::AccountMissing);
        }
        source
            .validate()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let password = keychain.read(account)?;
        let after = super::discovery::metadata(&roots.config, &selected.profile)
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::AccountMissing)?;
        source
            .validate()
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if *before != *after || password.is_empty() || password.len() > 4096 {
            return Err(ConnectionError::CredentialUnavailable);
        }
        self.password_account(
            accounts::cloud_types::NEXTCLOUD,
            &account.label,
            &account.server,
            &account.username,
            password,
            Some(accounts::AccountId::new(source_id(account))),
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::Inventory;
    use super::*;
    use crate::account_sources::discover_nextcloud as discover;
    use accounts::{AccountStore, SecretVault};
    use std::{cell::Cell, fs, os::unix::fs::PermissionsExt};

    const CONFIG: &str = "[Accounts]\n0\\url=https://cloud.example.test/base\n0\\http_user=ada\n0\\authType=http\n0\\displayName=My files\n";
    struct Fixture {
        _temporary: tempfile::TempDir,
        roots: SourceRoots,
        candidate: Candidate,
        target: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let home = temporary.path().join("home");
            let config = home.join(".config");
            fs::create_dir_all(config.join("Nextcloud")).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
            let path = config.join("Nextcloud/nextcloud.cfg");
            fs::write(&path, CONFIG).unwrap();
            let mut inventory = Inventory::default();
            discover(CONFIG, &path, &mut inventory);
            let candidate = inventory.candidates.pop().unwrap();
            let target = temporary.path().join("target");
            Self {
                _temporary: temporary,
                roots: SourceRoots { home, config },
                candidate,
                target,
            }
        }
    }
    struct SelectedKeychain<'a> {
        calls: Cell<usize>,
        change: Option<&'a Path>,
        unavailable: bool,
    }
    impl Keychain for SelectedKeychain<'_> {
        fn read(&self, account: &NativeAccount) -> Result<Zeroizing<String>, ConnectionError> {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(account.account_id, "0");
            assert_eq!(account.server, "https://cloud.example.test/base");
            assert_eq!(account.username, "ada");
            if let Some(path) = self.change {
                fs::write(path, CONFIG.replace("ada", "other")).unwrap();
            }
            if self.unavailable {
                return Err(ConnectionError::CredentialUnavailable);
            }
            Ok(Zeroizing::new("private-native-password".into()))
        }
    }
    #[test]
    fn selected_native_nextcloud_credential_enters_canonical_vault_once_without_source_changes() {
        let fixture = Fixture::new();
        let source_before = fs::read(&fixture.candidate.profile).unwrap();
        let mode_before = fs::metadata(&fixture.candidate.profile)
            .unwrap()
            .permissions()
            .mode();
        let keychain = SelectedKeychain {
            calls: Cell::new(0),
            change: None,
            unavailable: false,
        };
        assert!(fixture.candidate.can_import_credentials);
        assert!(fixture.candidate.needs_sign_in); // discovery has not proved authentication
        assert_eq!(keychain.calls.get(), 0);
        let connector = Connector::new(fixture.target.clone());
        let id = connector
            .import_nextcloud_with(&fixture.roots, &fixture.candidate, &keychain)
            .unwrap();
        assert_eq!(
            connector
                .import_nextcloud_with(&fixture.roots, &fixture.candidate, &keychain)
                .unwrap(),
            id
        );
        let store = AccountStore::load_in(&fixture.target);
        let account = store.get(&id).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(account.type_id.as_str(), accounts::cloud_types::NEXTCLOUD);
        assert_eq!(account.config["username"], "ada");
        assert_eq!(
            account.config["server_url"],
            "https://cloud.example.test/base"
        );
        assert_eq!(account.config["use_files"], true);
        assert!(account.config.get("app_password").is_none());
        assert!(account_custody::is_copied_secret(
            &SecretVault::open_in(&fixture.target).get(&id).unwrap()
        ));
        assert_eq!(
            account_custody::password_credential(
                &SecretVault::open_in(&fixture.target).get(&id).unwrap()
            )
            .to_string(),
            "private-native-password"
        );
        assert!(
            !fs::read_to_string(accounts::store_path_in(&fixture.target))
                .unwrap()
                .contains("private-native-password")
        );
        assert_eq!(fs::read(&fixture.candidate.profile).unwrap(), source_before);
        assert_eq!(
            fs::metadata(&fixture.candidate.profile)
                .unwrap()
                .permissions()
                .mode(),
            mode_before
        );
        assert_eq!(
            fs::read_dir(fixture.roots.config.join("Nextcloud"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn changed_selected_metadata_and_change_during_keychain_lookup_refuse_target_publication() {
        let fixture = Fixture::new();
        let keychain = SelectedKeychain {
            calls: Cell::new(0),
            change: None,
            unavailable: false,
        };
        fs::write(&fixture.candidate.profile, CONFIG.replace("ada", "other")).unwrap();
        let connector = Connector::new(fixture.target.clone());
        assert!(
            connector
                .import_nextcloud_with(&fixture.roots, &fixture.candidate, &keychain)
                .is_err()
        );
        assert_eq!(keychain.calls.get(), 0);
        assert!(!fixture.target.exists());
        fs::write(&fixture.candidate.profile, CONFIG).unwrap();
        let keychain = SelectedKeychain {
            calls: Cell::new(0),
            change: Some(&fixture.candidate.profile),
            unavailable: false,
        };
        assert!(
            connector
                .import_nextcloud_with(&fixture.roots, &fixture.candidate, &keychain)
                .is_err()
        );
        assert_eq!(keychain.calls.get(), 1);
        assert!(!fixture.target.exists());
    }
    #[test]
    fn unavailable_keychain_and_client_certificate_dependencies_require_reconnection() {
        let fixture = Fixture::new();
        let keychain = SelectedKeychain {
            calls: Cell::new(0),
            change: None,
            unavailable: true,
        };
        let connector = Connector::new(fixture.target.clone());
        assert!(matches!(
            connector.import_nextcloud_with(&fixture.roots, &fixture.candidate, &keychain),
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert!(!fixture.target.exists());
        let configuration = format!("{CONFIG}0\\http_clientCertPkcs12=certificate.p12\n");
        let mut inventory = Inventory::default();
        discover(&configuration, &fixture.candidate.profile, &mut inventory);
        let selected = inventory.candidates.pop().unwrap();
        assert!(!selected.can_import_credentials);
        assert!(
            connector
                .import_nextcloud_with(&fixture.roots, &selected, &keychain)
                .is_err()
        );
        assert_eq!(keychain.calls.get(), 1);
        assert_eq!(
            fs::read_to_string(&fixture.candidate.profile).unwrap(),
            CONFIG
        );
    }
}
