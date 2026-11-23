//! Explicit owner-selected website sessions in canonical encrypted account credentials, as a
//! prepared setup seals them for the system it installs. Browser opens a workspace's website
//! sessions from entries of the workspace's vault instead.
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

impl CredentialStore {
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
