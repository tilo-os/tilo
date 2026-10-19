//! Explicit owner-selected website sessions in canonical encrypted account custody.
use crate::{Custodian, CustodyError};
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
pub fn service_session_secret(session: &SessionCookies) -> Result<Secret, CustodyError> {
    session
        .validate()
        .map_err(|_| CustodyError::NeedsAuthentication)?;
    let encoded = Zeroizing::new(
        serde_json::to_string(&Envelope {
            kind: "protected_service_session".into(),
            session: session.clone(),
        })
        .map_err(|_| CustodyError::NeedsAuthentication)?,
    );
    if encoded.len() > MAX_SECRET {
        return Err(CustodyError::NeedsAuthentication);
    }
    Ok(Secret {
        password: String::new(),
        token: Some(encoded.to_string()),
    })
}

/// Authenticate the selected protected secret against a concrete person/setup.
/// # Errors
/// Rejects unsupported secret shapes, foreign bindings, or malformed credentials.
pub fn service_session_from_secret(
    secret: &Secret,
    id: &AccountId,
    identity: [u8; 32],
    binding: &str,
) -> Result<SessionCookies, CustodyError> {
    if !secret.password.is_empty() {
        return Err(CustodyError::NeedsAuthentication);
    }
    let encoded = secret
        .token
        .as_deref()
        .filter(|v| v.len() <= MAX_SECRET)
        .ok_or(CustodyError::NeedsAuthentication)?;
    let envelope: Envelope =
        serde_json::from_str(encoded).map_err(|_| CustodyError::NeedsAuthentication)?;
    envelope
        .session
        .validate()
        .map_err(|_| CustodyError::NeedsAuthentication)?;
    if envelope.kind != "protected_service_session"
        || envelope.session.session_id != id.as_str()
        || envelope.session.identity_public_key != identity
        || envelope.session.setup_binding != binding
    {
        return Err(CustodyError::NeedsAuthentication);
    }
    Ok(envelope.session)
}

/// Exercise the protected codec without opening a vault or publishing credentials.
///
/// Checks exact selection, person and setup bindings and redacted diagnostics.
/// # Errors
/// Rejects malformed sessions or a failed codec/binding contract.
pub fn verify_service_session_codec(session: &SessionCookies) -> Result<(), CustodyError> {
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
        return Err(CustodyError::NeedsAuthentication);
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

/// A fresh selection that passed secret, account and store-snapshot checks.
/// Retry receipts never enter this credential-replacement stage.
struct AuthenticatedForget {
    selection: ForgottenSelection,
}

impl AuthenticatedForget {
    fn seal_receipt(self, root: &std::path::Path, id: &AccountId) -> Result<(), CustodyError> {
        let mut vault = SecretVault::open_in(root);
        if vault.recovery_error().is_some() {
            return Err(CustodyError::NeedsAuthentication);
        }
        let mut current = vault.get(id).ok_or(CustodyError::NeedsAuthentication)?;
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
                .map_err(|_| CustodyError::NeedsAuthentication)?,
        );
        let mut receipt = Secret {
            password: String::new(),
            token: Some(encoded.to_string()),
        };
        vault.put(id, &receipt);
        if let Some(token) = receipt.token.as_mut() {
            token.zeroize();
        }
        vault.try_save().map_err(CustodyError::Storage)
    }
}

impl Custodian {
    /// Durably forget precisely one authenticated selection, never another account.
    /// The vault replaces credentials with a bound receipt before metadata removal;
    /// a failed metadata save therefore cannot make the cookies usable again.
    /// # Errors
    /// Refuses foreign selections, missing/corrupt custody or failed durable saves.
    pub fn forget_service_session(
        &self,
        id: &AccountId,
        identity: [u8; 32],
        binding: &str,
    ) -> Result<(), CustodyError> {
        static REMOVAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = REMOVAL
            .lock()
            .map_err(|_| CustodyError::NeedsAuthentication)?;
        let root = self
            .config_root()
            .map(std::path::Path::to_owned)
            .or_else(accounts::config_root)
            .ok_or(CustodyError::NeedsAuthentication)?;
        let load = || -> Result<accounts::AccountStore, CustodyError> {
            let bytes = std::fs::read(root.join("accounts/store.json"))
                .map_err(|_| CustodyError::NeedsAuthentication)?;
            serde_json::from_slice(&bytes).map_err(|_| CustodyError::NeedsAuthentication)
        };
        let mut store = load()?;
        let existing = SecretVault::open_read_only_in(&root);
        if existing.recovery_error().is_some() {
            return Err(CustodyError::NeedsAuthentication);
        }
        let mut secret = existing.get(id).ok_or(CustodyError::NeedsAuthentication)?;
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
                let account = store.get(id).ok_or(CustodyError::NeedsAuthentication)?;
                if account
                    .config
                    .get("service")
                    .and_then(serde_json::Value::as_str)
                    != Some(session.service.as_str())
                {
                    return Err(CustodyError::NeedsAuthentication);
                }
                Ok(())
            })
        };
        secret.password.zeroize();
        if let Some(token) = secret.token.as_mut() {
            token.zeroize();
        }
        authenticated?;
        if let Some(account) = store.get(id) {
            if retry
                && receipt
                    .as_ref()
                    .is_none_or(|receipt| &receipt.account != account)
            {
                return Err(CustodyError::NeedsAuthentication);
            }
            if account.type_id.as_str() != SERVICE_SESSION_TYPE
                || account.owner.is_some()
                || account
                    .config
                    .get("setup_binding")
                    .and_then(serde_json::Value::as_str)
                    != Some(binding)
            {
                return Err(CustodyError::NeedsAuthentication);
            }
        } else if !retry {
            return Err(CustodyError::NeedsAuthentication);
        }
        if load()? != store {
            return Err(CustodyError::NeedsAuthentication);
        }
        if !retry {
            AuthenticatedForget {
                selection: ForgottenSelection {
                    kind: "forgotten_service_session".into(),
                    account: store
                        .get(id)
                        .ok_or(CustodyError::NeedsAuthentication)?
                        .clone(),
                    id: id.as_str().into(),
                    identity_public_key: identity,
                    setup_binding: binding.into(),
                },
            }
            .seal_receipt(&root, id)?;
        }
        if load()? != store {
            return Err(CustodyError::NeedsAuthentication);
        }
        if store.remove(id) {
            store
                .save_in(&root)
                .map_err(|error| CustodyError::Storage(error.to_string()))?;
        }
        Ok(())
    }

    /// Resolve only this selected website session from private current-person custody.
    /// Cookie installation is not proof of provider authentication.
    /// # Errors
    /// Missing custody, changed metadata, foreign identity/setup, or invalid protected secret.
    pub fn service_session(
        &self,
        id: &AccountId,
        identity: [u8; 32],
        binding: &str,
    ) -> Result<SessionCookies, CustodyError> {
        let store = self.config_root().map_or_else(
            accounts::AccountStore::load,
            accounts::AccountStore::load_in,
        );
        let account = store.get(id).ok_or(CustodyError::NeedsAuthentication)?;
        if account.type_id.as_str() != SERVICE_SESSION_TYPE
            || !account.enabled
            || account.owner.is_some()
            || account
                .config
                .get("setup_binding")
                .and_then(serde_json::Value::as_str)
                != Some(binding)
        {
            return Err(CustodyError::NeedsAuthentication);
        }
        let vault = self
            .config_root()
            .map_or_else(SecretVault::open_read_only, SecretVault::open_read_only_in);
        if vault.recovery_error().is_some() {
            return Err(CustodyError::NeedsAuthentication);
        }
        let mut secret = vault.get(id).ok_or(CustodyError::NeedsAuthentication)?;
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
            return Err(CustodyError::NeedsAuthentication);
        }
        let current = self.config_root().map_or_else(
            accounts::AccountStore::load,
            accounts::AccountStore::load_in,
        );
        if current.get(id) != Some(account) {
            return Err(CustodyError::NeedsAuthentication);
        }
        Ok(session)
    }
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
    fn custody_fixture() -> (tempfile::TempDir, Custodian, AccountId) {
        let root = tempfile::tempdir().unwrap();
        let session = selected();
        let id = AccountId::from(session.session_id.clone());
        let mut account = accounts::Account::new(id.clone(), SERVICE_SESSION_TYPE);
        account.config = serde_json::json!({"service":"slack","setup_binding":"setup-operation"});
        let mut store = accounts::AccountStore::new();
        store.upsert(account);
        store.upsert(accounts::Account::new("unrelated", "imap"));
        store.save_in(root.path()).unwrap();
        let mut vault = SecretVault::open_in(root.path());
        vault.put(&id, &service_session_secret(&session).unwrap());
        vault.put(
            &AccountId::from("unrelated"),
            &Secret {
                password: "unrelated-password".into(),
                token: None,
            },
        );
        vault.try_save().unwrap();
        let custodian = Custodian::for_root(Some(root.path().to_owned()));
        (root, custodian, id)
    }

    #[test]
    fn forget_removes_only_selected_canonical_credentials_and_binds_retries() {
        let (root, custodian, id) = custody_fixture();
        let unrelated = AccountId::from("unrelated");
        let before = accounts::AccountStore::load_in(root.path())
            .get(&unrelated)
            .unwrap()
            .clone();
        for (key, binding, target) in [
            ([8; 32], "setup-operation", id.clone()),
            ([7; 32], "foreign-operation", id.clone()),
            ([7; 32], "setup-operation", unrelated.clone()),
        ] {
            assert!(
                custodian
                    .forget_service_session(&target, key, binding)
                    .is_err()
            );
            assert_eq!(
                custodian
                    .service_session(&id, [7; 32], "setup-operation")
                    .unwrap(),
                selected()
            );
        }
        custodian
            .forget_service_session(&id, [7; 32], "setup-operation")
            .unwrap();
        assert!(
            accounts::AccountStore::load_in(root.path())
                .get(&id)
                .is_none()
        );
        assert!(
            custodian
                .service_session(&id, [7; 32], "setup-operation")
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
            vault.get(&unrelated).unwrap().password,
            "unrelated-password"
        );
        assert_eq!(
            accounts::AccountStore::load_in(root.path()).get(&unrelated),
            Some(&before)
        );
        let bytes = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
        custodian
            .forget_service_session(&id, [7; 32], "setup-operation")
            .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
            bytes
        );
        assert!(
            custodian
                .forget_service_session(&id, [8; 32], "setup-operation")
                .is_err()
        );
        assert!(
            custodian
                .forget_service_session(&id, [7; 32], "foreign-operation")
                .is_err()
        );
        // Reusing a removed id for another kind is never covered by the receipt.
        let mut store = accounts::AccountStore::load_in(root.path());
        store.upsert(accounts::Account::new(id.clone(), "imap"));
        store.save_in(root.path()).unwrap();
        assert!(
            custodian
                .forget_service_session(&id, [7; 32], "setup-operation")
                .is_err()
        );
        assert_eq!(
            accounts::AccountStore::load_in(root.path())
                .get(&id)
                .unwrap()
                .type_id
                .as_str(),
            "imap"
        );
    }

    #[cfg(unix)]
    #[test]
    fn forget_metadata_save_failure_revokes_first_and_retries_finish_removal() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_observer};
        let (root, custodian, id) = custody_fixture();
        let metadata = root.path().join("accounts/store.json");
        let backup = metadata.with_extension("backup");
        let destination = metadata.clone();
        let saved = backup.clone();
        let publications = std::cell::Cell::new(0);
        let result = with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::BeforePublish {
                    publications.set(publications.get() + 1);
                    if publications.get() == 2 {
                        // The vault publishes first. Keep metadata readable until
                        // its save reaches rename, then force a real failure even
                        // as root: a regular staging file cannot replace a directory.
                        std::fs::rename(&destination, &saved).unwrap();
                        std::fs::create_dir(&destination).unwrap();
                    }
                }
            },
            || custodian.forget_service_session(&id, [7; 32], "setup-operation"),
        );
        std::fs::remove_dir(&metadata).unwrap();
        std::fs::rename(&backup, &metadata).unwrap();
        assert!(
            matches!(result, Err(CustodyError::Storage(_))),
            "Metadata persistence must fail after vault revocation"
        );
        assert!(
            accounts::AccountStore::load_in(root.path())
                .get(&id)
                .is_some()
        );
        assert!(
            custodian
                .service_session(&id, [7; 32], "setup-operation")
                .is_err()
        );
        assert!(
            custodian
                .forget_service_session(&id, [8; 32], "setup-operation")
                .is_err()
        );
        custodian
            .forget_service_session(&id, [7; 32], "setup-operation")
            .unwrap();
        assert!(
            accounts::AccountStore::load_in(root.path())
                .get(&id)
                .is_none()
        );
        assert!(
            custodian
                .service_session(&id, [7; 32], "setup-operation")
                .is_err()
        );
    }

    #[test]
    fn forget_missing_key_or_corrupt_metadata_fails_without_adopting_custody() {
        let (root, custodian, id) = custody_fixture();
        let before = std::fs::read(root.path().join("accounts/store.json")).unwrap();
        std::fs::write(root.path().join("accounts/vault/vault.enc"), b"corrupt").unwrap();
        assert!(
            custodian
                .forget_service_session(&id, [7; 32], "setup-operation")
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("accounts/store.json")).unwrap(),
            before
        );
        let (root, custodian, id) = custody_fixture();
        let before = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
        std::fs::write(root.path().join("accounts/store.json"), b"corrupt").unwrap();
        assert!(
            custodian
                .forget_service_session(&id, [7; 32], "setup-operation")
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
            before
        );
    }

    #[test]
    fn selected_session_private_vault_binding_and_redacted_debug() {
        let root = tempfile::tempdir().unwrap();
        let session = selected();
        let id = AccountId::from(session.session_id.clone());
        let secret = service_session_secret(&session).unwrap();
        assert!(!format!("{secret:?} {session:?}").contains("private-cookie-marker"));
        let mut store = accounts::AccountStore::new();
        let mut account = accounts::Account::new(id.clone(), SERVICE_SESSION_TYPE);
        account.config = serde_json::json!({"service":"slack","setup_binding":"setup-operation"});
        store.upsert(account);
        store.save_in(root.path()).unwrap();
        let mut vault = SecretVault::open_in(root.path());
        vault.put(&id, &secret);
        vault.try_save().unwrap();
        let custodian = Custodian::for_root(Some(root.path().to_owned()));
        assert_eq!(
            custodian
                .service_session(&id, [7; 32], "setup-operation")
                .unwrap(),
            session
        );
        assert!(
            custodian
                .service_session(&id, [8; 32], "setup-operation")
                .is_err()
        );
        assert!(
            custodian
                .service_session(&id, [7; 32], "another-operation")
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
    #[test]
    fn protected_origin_only_secret_preserves_raw_dom_and_exact_person_binding() {
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
    #[test]
    fn codec_probe_checks_custody_contract_and_refuses_malformed_selection() {
        let session = selected();
        verify_service_session_codec(&session).unwrap();
        let mut foreign = session;
        foreign.cookies[0].host = "foreign.example".into();
        assert!(verify_service_session_codec(&foreign).is_err());
    }

    #[test]
    fn malformed_selected_session_refuses_before_secret_publication() {
        let mut session = selected();
        session.cookies[0].host = "foreign.example".into();
        assert!(service_session_secret(&session).is_err());
        let mut session = selected();
        session.cookies[0].name = "__Host-session".into();
        assert!(service_session_secret(&session).is_err());
    }
}
