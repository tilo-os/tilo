//! Explicit owner-selected website sessions in canonical encrypted account custody.
use crate::{Custodian, CustodyError};
use accounts::{AccountId, Secret, SecretVault};
use browser_continuity::sessions::SessionCookies;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

pub const SERVICE_SESSION_TYPE: &str = "browser-service-session";
const MAX_SECRET: usize = browser_continuity::sessions::MAX_SESSION_BYTES + 64 * 1024;

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

impl Custodian {
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
    use browser_continuity::sessions::{Cookie, SameSite, Service};
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
        session.origin_storage = vec![browser_continuity::sessions::OriginStorage {
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
    fn malformed_selected_session_refuses_before_secret_publication() {
        let mut session = selected();
        session.cookies[0].host = "foreign.example".into();
        assert!(service_session_secret(&session).is_err());
        let mut session = selected();
        session.cookies[0].name = "__Host-session".into();
        assert!(service_session_secret(&session).is_err());
    }
}
