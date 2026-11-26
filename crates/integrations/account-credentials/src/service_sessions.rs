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
