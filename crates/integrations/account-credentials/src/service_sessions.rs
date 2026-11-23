//! Explicit owner-selected website sessions in canonical encrypted account credentials.
use crate::{CredentialError, CredentialStore};
use accounts::{AccountId, Secret, SecretVault};
use browser_continuity_core::sessions::SessionCookies;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

pub const SERVICE_SESSION_TYPE: &str = "browser-service-session";
const MAX_SECRET: usize = browser_continuity_core::sessions::MAX_SESSION_BYTES + 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    kind: String,
    session: SessionCookies,
}

/// Sealable secret with an explicitly unknown-to-legacy protected kind.
/// # Errors
/// Rejects invalid or oversized selections before producing credential bytes.
pub fn service_session_secret(session: &SessionCookies) -> Result<Secret, CredentialError> {
    session
        .validate()
        .map_err(|_| CredentialError::NeedsAuthentication)?;
    let encoded = Zeroizing::new(
        serde_json::to_string(&Envelope {
            kind: "protected_service_session".into(),
            session: session.clone(),
        })
        .map_err(|_| CredentialError::NeedsAuthentication)?,
    );
    if encoded.len() > MAX_SECRET {
        return Err(CredentialError::NeedsAuthentication);
    }
    Ok(Secret {
        password: String::new(),
        token: Some(encoded.to_string()),
    })
}

/// Authenticate the selected protected secret against a concrete person/setup.
pub fn service_session_from_secret(
    secret: &Secret,
    id: &AccountId,
    identity: [u8; 32],
    binding: &str,
) -> Result<SessionCookies, CredentialError> {
    if !secret.password.is_empty() {
        return Err(CredentialError::NeedsAuthentication);
    }
    let encoded = secret
        .token
        .as_deref()
        .filter(|v| v.len() <= MAX_SECRET)
        .ok_or(CredentialError::NeedsAuthentication)?;
    let envelope: Envelope =
        serde_json::from_str(encoded).map_err(|_| CredentialError::NeedsAuthentication)?;
    envelope
        .session
        .validate()
        .map_err(|_| CredentialError::NeedsAuthentication)?;
    if envelope.kind != "protected_service_session"
        || envelope.session.session_id != id.as_str()
        || envelope.session.identity_public_key != identity
        || envelope.session.setup_binding != binding
    {
        return Err(CredentialError::NeedsAuthentication);
    }
    Ok(envelope.session)
}

/// Exercise the protected codec without opening a vault or publishing credentials.
///
/// Checks exact selection, person and setup bindings and redacted diagnostics.
pub fn verify_service_session_codec(session: &SessionCookies) -> Result<(), CredentialError> {
    let secret = service_session_secret(session)?;
    let id = AccountId::new(&session.session_id);
    let restored = service_session_from_secret(
        &secret,
        &id,
        session.identity_public_key,
        &session.setup_binding,
    )?;
    let debug = format!("{secret:?}");
    let leaks_credentials = session
        .cookies
        .iter()
        .any(|cookie| !cookie.value.is_empty() && debug.contains(&cookie.value))
        || session
            .origin_storage
            .iter()
            .any(|entry| !entry.value.is_empty() && debug.contains(&entry.value));
    if restored != *session
        || leaks_credentials
        || service_session_from_secret(
            &secret,
            &id,
            session.identity_public_key.map(|byte| byte ^ 1),
            &session.setup_binding,
        )
        .is_ok()
        || service_session_from_secret(
            &secret,
            &id,
            session.identity_public_key,
            &format!("{}.foreign", session.setup_binding),
        )
        .is_ok()
        || service_session_from_secret(
            &secret,
            &AccountId::new(format!("{}.foreign", session.session_id)),
            session.identity_public_key,
            &session.setup_binding,
        )
        .is_ok()
    {
        return Err(CredentialError::NeedsAuthentication);
    }
    Ok(())
}

/// Encrypted, credential-free receipt retained in the original vault. It binds
/// retries to the person and selection even after account metadata is removed.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgottenSelection {
    kind: String,
    account: accounts::Account,
    id: String,
    identity_public_key: [u8; 32],
    setup_binding: String,
}

/// A fresh selection that passed secret and account checks.
/// Retry receipts never enter this credential-replacement stage.
struct AuthenticatedForget {
    selection: ForgottenSelection,
}

impl AuthenticatedForget {
    fn seal_receipt(self, root: &std::path::Path, id: &AccountId) -> Result<(), CredentialError> {
        let mut vault = SecretVault::open_person(Some(root));
        if vault.recovery_error().is_some() {
            return Err(CredentialError::NeedsAuthentication);
        }
        let mut current = vault.get(id).ok_or(CredentialError::NeedsAuthentication)?;
        let checked = service_session_from_secret(
            &current,
            id,
            self.selection.identity_public_key,
            &self.selection.setup_binding,
        );
        current.password.zeroize();
        if let Some(token) = current.token.as_mut() {
            token.zeroize();
        }
        checked?;
        let encoded = Zeroizing::new(
            serde_json::to_string(&self.selection)
                .map_err(|_| CredentialError::NeedsAuthentication)?,
        );
        let mut receipt = Secret {
            password: String::new(),
            token: Some(encoded.to_string()),
        };
        vault.put(id, &receipt);
        if let Some(token) = receipt.token.as_mut() {
            token.zeroize();
        }
        vault.try_save().map_err(CredentialError::Storage)
    }
}

impl CredentialStore {
    /// Durably forget precisely one authenticated selection, never another account: `current`
    /// is the workspace's account under `id` now, if it still has one. The vault replaces
    /// credentials with a bound receipt first, so the caller removes the account from the
    /// workspace's accounts only once this answered, and a removal that failed cannot make the
    /// cookies usable again.
    pub fn forget_service_session(
        &self,
        id: &AccountId,
        current: Option<&accounts::Account>,
        identity: [u8; 32],
        binding: &str,
    ) -> Result<(), CredentialError> {
        use CredentialError::NeedsAuthentication as Refused;
        static REMOVAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = REMOVAL.lock().map_err(|_| Refused)?;
        let root = self
            .config_root()
            .map(std::path::Path::to_owned)
            .or_else(accounts::config_root)
            .ok_or(Refused)?;
        let existing = SecretVault::open_person(Some(&root));
        if existing.recovery_error().is_some() {
            return Err(Refused);
        }
        let mut secret = existing.get(id).ok_or(Refused)?;
        let receipt = secret
            .token
            .as_deref()
            .and_then(|encoded| serde_json::from_str::<ForgottenSelection>(encoded).ok());
        let retry = receipt.as_ref().is_some_and(|receipt| {
            secret.password.is_empty()
                && receipt.kind == "forgotten_service_session"
                && receipt.id == id.as_str()
                && receipt.identity_public_key == identity
                && receipt.setup_binding == binding
        });
        let authenticated = if retry {
            Ok(())
        } else {
            service_session_from_secret(&secret, id, identity, binding).and_then(|session| {
                let account = current.ok_or(Refused)?;
                if account
                    .config
                    .get("service")
                    .and_then(serde_json::Value::as_str)
                    != Some(session.service.as_str())
                {
                    return Err(Refused);
                }
                Ok(())
            })
        };
        secret.password.zeroize();
        if let Some(token) = secret.token.as_mut() {
            token.zeroize();
        }
        authenticated?;
        if let Some(account) = current {
            if retry
                && receipt
                    .as_ref()
                    .is_none_or(|receipt| &receipt.account != account)
            {
                return Err(Refused);
            }
            if !is_bound_service_session(account, binding) {
                return Err(Refused);
            }
        } else if !retry {
            return Err(Refused);
        }
        if !retry {
            AuthenticatedForget {
                selection: ForgottenSelection {
                    kind: "forgotten_service_session".into(),
                    account: current.ok_or(Refused)?.clone(),
                    id: id.as_str().into(),
                    identity_public_key: identity,
                    setup_binding: binding.into(),
                },
            }
            .seal_receipt(&root, id)?;
        }
        Ok(())
    }

    /// Resolve only the selected website session `account`, the workspace's account of it,
    /// from private current-person ownership. Cookie installation is not proof of provider
    /// authentication.
    pub fn service_session(
        &self,
        account: &accounts::Account,
        identity: [u8; 32],
        binding: &str,
    ) -> Result<SessionCookies, CredentialError> {
        let id = &account.id;
        if account.type_id.as_str() != SERVICE_SESSION_TYPE
            || !account.enabled
            || account
                .config
                .get("setup_binding")
                .and_then(serde_json::Value::as_str)
                != Some(binding)
        {
            return Err(CredentialError::NeedsAuthentication);
        }
        let vault = SecretVault::open_person(self.config_root());
        if vault.recovery_error().is_some() {
            return Err(CredentialError::NeedsAuthentication);
        }
        let mut secret = vault.get(id).ok_or(CredentialError::NeedsAuthentication)?;
        let result = service_session_from_secret(&secret, id, identity, binding);
        secret.password.zeroize();
        if let Some(token) = secret.token.as_mut() {
            token.zeroize();
        }
        let session = result?;
        if account
            .config
            .get("service")
            .and_then(serde_json::Value::as_str)
            != Some(session.service.as_str())
        {
            return Err(CredentialError::NeedsAuthentication);
        }
        Ok(session)
    }
}

/// Whether `account` is a service session set up under `binding`.
fn is_bound_service_session(account: &accounts::Account, binding: &str) -> bool {
    account.type_id.as_str() == SERVICE_SESSION_TYPE
        && account
            .config
            .get("setup_binding")
            .and_then(serde_json::Value::as_str)
            == Some(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_continuity_core::sessions::{Cookie, SameSite, Service};
    fn selected() -> SessionCookies {
        SessionCookies {
            schema: 1,
            session_id: "selected-slack".into(),
            service: Service::Slack,
            identity_public_key: [7; 32],
            setup_binding: "setup-operation".into(),
            source_snapshot_digest: [9; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "slack.com".into(),
                host_only: false,
                name: "d".into(),
                value: "private-cookie-marker".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::None,
            }],
        }
    }
    /// The workspace's account of the selected session.
    fn selected_account() -> accounts::Account {
        let mut account =
            accounts::Account::new(AccountId::from(selected().session_id), SERVICE_SESSION_TYPE);
        account.config = serde_json::json!({"service":"slack","setup_binding":"setup-operation"});
        account
    }

    /// A device that sealed the selected session and an unrelated account's password, with the
    /// workspace's accounts of both.
    fn ownership_fixture() -> (
        tempfile::TempDir,
        CredentialStore,
        accounts::Account,
        accounts::Account,
    ) {
        let root = tempfile::tempdir().unwrap();
        let session = selected();
        let account = selected_account();
        let unrelated = accounts::Account::new("unrelated", "imap");
        let mut vault = SecretVault::open_in(root.path());
        vault.put(&account.id, &service_session_secret(&session).unwrap());
        vault.put(
            &unrelated.id,
            &Secret {
                password: "unrelated-password".into(),
                token: None,
            },
        );
        vault.try_save().unwrap();
        let credential_store = CredentialStore::for_root(Some(root.path().to_owned()));
        (root, credential_store, account, unrelated)
    }

    /// A selection sealed before the person's vault had one ownership: under
    /// the owner-only key beside the ciphertext, as the local path wrote it.
    #[cfg(unix)]
    #[test]
    fn a_selection_sealed_under_an_owner_only_key_is_read_and_its_key_migrates() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config");
        let person = config.join("tilo");
        std::fs::create_dir_all(&person).unwrap();
        std::fs::set_permissions(&person, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &config)
            .set("HOME", home.path())
            .set(tilo_secrets::SECRETS_DIR_ENV, home.path().join("secrets"));

        let session = selected();
        let account = selected_account();
        let mut legacy = SecretVault::open_in(&person);
        legacy.put(&account.id, &service_session_secret(&session).unwrap());
        legacy.try_save().unwrap();
        let key = accounts::vault_dir_in(&person).join("vault.key");
        assert!(key.exists(), "the old path kept the vault key beside it");

        let resolved = CredentialStore::live()
            .service_session(
                &account,
                session.identity_public_key,
                &session.setup_binding,
            )
            .expect("a keyring that answers reads the selection the person kept");
        assert_eq!(resolved, session, "byte for byte the selection they made");
        assert!(
            !key.exists(),
            "the key moved into the keyring; nothing reads the vault beside it"
        );
        let vault = SecretVault::open_person(Some(&person));
        assert_eq!(vault.backend(), accounts::VaultBackend::System);
        assert!(
            vault.get(&account.id).is_some(),
            "the selection stayed in the vault"
        );
    }

    tilo_test_env::scenario! {
        /// `forget`: what it removes, and what it refuses to touch.
        fn forgetting_a_service_session_removes_only_what_was_selected;

        phase forget_removes_only_selected_canonical_credentials_and_binds_retries;
        {
            let (root, credential_store, account, unrelated) = ownership_fixture();
            let id = account.id.clone();
            for (key, binding, target) in [
                ([8; 32], "setup-operation", &account),
                ([7; 32], "foreign-operation", &account),
                ([7; 32], "setup-operation", &unrelated),
            ] {
                assert!(
                    credential_store
                        .forget_service_session(&target.id, Some(target), key, binding)
                        .is_err()
                );
                assert_eq!(
                    credential_store
                        .service_session(&account, [7; 32], "setup-operation")
                        .unwrap(),
                    selected()
                );
            }
            credential_store
                .forget_service_session(&id, Some(&account), [7; 32], "setup-operation")
                .unwrap();
            assert!(
                credential_store
                    .service_session(&account, [7; 32], "setup-operation")
                    .is_err()
            );
            let vault = SecretVault::open_read_only_in(root.path());
            let mut receipt = vault.get(&id).unwrap();
            assert!(
                !receipt
                    .token
                    .as_deref()
                    .unwrap()
                    .contains("private-cookie-marker")
            );
            assert!(crate::password_credential(&receipt).is_empty());
            receipt.token.as_mut().unwrap().zeroize();
            assert_eq!(
                vault.get(&unrelated.id).unwrap().password,
                "unrelated-password"
            );
            // A retry while the workspace still has the account, its removal not yet made,
            // and once it is gone, changes nothing more in the vault.
            let bytes = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
            for current in [Some(&account), None] {
                credential_store
                    .forget_service_session(&id, current, [7; 32], "setup-operation")
                    .unwrap();
                assert_eq!(
                    std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
                    bytes
                );
            }
            assert!(
                credential_store
                    .forget_service_session(&id, None, [8; 32], "setup-operation")
                    .is_err()
            );
            assert!(
                credential_store
                    .forget_service_session(&id, None, [7; 32], "foreign-operation")
                    .is_err()
            );
            // Reusing a removed id for another kind is never covered by the receipt.
            let reused = accounts::Account::new(id.clone(), "imap");
            assert!(
                credential_store
                    .forget_service_session(&id, Some(&reused), [7; 32], "setup-operation")
                    .is_err()
            );
        }

        phase forget_with_an_unreadable_vault_fails_without_adopting_ownership;
        {
            let (root, credential_store, account, _) = ownership_fixture();
            std::fs::write(root.path().join("accounts/vault/vault.enc"), b"corrupt").unwrap();
            assert!(
                credential_store
                    .forget_service_session(&account.id, Some(&account), [7; 32], "setup-operation")
                    .is_err()
            );
            assert_eq!(
                std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
                b"corrupt"
            );
        }
    }

    tilo_test_env::scenario! {
        /// The selected session and its codec.
        fn a_selected_service_session_is_bound_private_and_strictly_decoded;

        phase selected_session_private_vault_binding_and_redacted_debug;
        {
            let root = tempfile::tempdir().unwrap();
            let session = selected();
            let id = AccountId::from(session.session_id.clone());
            let secret = service_session_secret(&session).unwrap();
            assert!(!format!("{secret:?} {session:?}").contains("private-cookie-marker"));
            let account = selected_account();
            let mut vault = SecretVault::open_in(root.path());
            vault.put(&id, &secret);
            vault.try_save().unwrap();
            let credential_store = CredentialStore::for_root(Some(root.path().to_owned()));
            assert_eq!(
                credential_store
                    .service_session(&account, [7; 32], "setup-operation")
                    .unwrap(),
                session
            );
            assert!(
                credential_store
                    .service_session(&account, [8; 32], "setup-operation")
                    .is_err()
            );
            assert!(
                credential_store
                    .service_session(&account, [7; 32], "another-operation")
                    .is_err()
            );
            assert!(
                crate::password_credential(&secret).is_empty(),
                "Canonical password consumer must refuse session kind"
            );
            let bytes = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
            assert!(
                !bytes
                    .windows(b"private-cookie-marker".len())
                    .any(|v| v == b"private-cookie-marker")
            );
        }

        phase protected_origin_only_secret_preserves_raw_dom_and_exact_person_binding;
        {
            let mut session = selected();
            session.schema = 2;
            session.service = Service::Discord;
            session.cookies.clear();
            session.origin_storage = vec![browser_continuity_core::sessions::OriginStorage {
                origin: "https://discord.com".into(),
                key: "token".into(),
                value: "\"private-origin-marker\"".into(),
                source_snapshot_digest: [8; 32],
            }];
            let id = AccountId::from(session.session_id.clone());
            let secret = service_session_secret(&session).unwrap();
            assert_eq!(
                service_session_from_secret(&secret, &id, [7; 32], "setup-operation").unwrap(),
                session
            );
            assert!(service_session_from_secret(&secret, &id, [8; 32], "setup-operation").is_err());
            assert!(!format!("{secret:?}").contains("private-origin-marker"));
            assert!(crate::password_credential(&secret).is_empty());
        }

        phase codec_probe_checks_ownership_contract_and_refuses_malformed_selection;
        {
            let session = selected();
            verify_service_session_codec(&session).unwrap();
            let mut foreign = session;
            foreign.cookies[0].host = "foreign.example".into();
            assert!(verify_service_session_codec(&foreign).is_err());
        }

        phase malformed_selected_session_refuses_before_secret_publication;
        {
            let mut session = selected();
            session.cookies[0].host = "foreign.example".into();
            assert!(service_session_secret(&session).is_err());
            let mut session = selected();
            session.cookies[0].name = "__Host-session".into();
            assert!(service_session_secret(&session).is_err());
        }
    }
}
