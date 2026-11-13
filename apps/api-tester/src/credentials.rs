//! Authentication is device-local; collaborative requests carry only opaque keys.
//! A document holds references into this device's secret store, so a copy of it
//! (a peer's, an export, a backup) never carries a credential.

use crate::model::Auth;
use ui::secrets::{PutOptions, SecretStore};

const UNAVAILABLE: &str = "Authentication secret unavailable; unlock the host secret store";
/// A staged browser write has not yet received durable acknowledgement.
pub const PENDING: &str = "Saving authentication securely; waiting for browser storage";

/// Seal authentication, returning a reference only after storage acknowledges it.
pub fn seal(auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
    let Some(value) = credential_bytes(auth) else {
        return Ok(auth.clone());
    };
    let key = format!("request-auth/{}", uuid::Uuid::new_v4());
    let id = store
        .seal_blocking(&key, &value, PutOptions::default())
        .map_err(|_| UNAVAILABLE)?
        .ok_or(PENDING)?;
    Ok(Auth::SecretRef {
        id,
        scheme: auth.scheme().into(),
    })
}

/// Seal authentication and wait for the backend's durable acknowledgement.
#[allow(
    clippy::future_not_send,
    reason = "the secret store's future is not Send on the web, where its promises run on the page's single thread"
)]
pub async fn seal_async(auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
    let Some(value) = credential_bytes(auth) else {
        return Ok(auth.clone());
    };
    let id = format!("request-auth/{}", uuid::Uuid::new_v4());
    store
        .put(&id, &value, PutOptions::default())
        .await
        .map_err(|_| UNAVAILABLE)?;
    Ok(Auth::SecretRef {
        id,
        scheme: auth.scheme().into(),
    })
}

/// Keep the local envelope encoding identical for synchronous and async custody.
fn credential_bytes(auth: &Auth) -> Option<Vec<u8>> {
    let value = match auth {
        Auth::None | Auth::SecretRef { .. } => return None,
        Auth::Bearer { token } => serde_json::json!({"Bearer": {"token": token}}),
        Auth::Basic { username, password } => {
            serde_json::json!({"Basic": {"username": username, "password": password}})
        }
        Auth::ApiKey { header, value } => {
            serde_json::json!({"ApiKey": {"header": header, "value": value}})
        }
    };
    Some(value.to_string().into_bytes())
}

/// Whether storage has unacknowledged work.
pub fn pending() -> bool {
    SecretStore::pending()
}

/// Resolve only an API Tester authentication reference from this device's store.
pub fn resolve(auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
    let Auth::SecretRef { id, .. } = auth else {
        return Ok(auth.clone());
    };
    // Do not allow a collaborative reference to address another consumer's keys.
    if id
        .strip_prefix("request-auth/")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_none()
    {
        return Err(UNAVAILABLE);
    }
    let secret = store
        .get_blocking(id)
        .map_err(|_| UNAVAILABLE)?
        .ok_or(UNAVAILABLE)?;
    let auth: Auth = serde_json::from_slice(secret.expose()).map_err(|_| UNAVAILABLE)?;
    if matches!(auth, Auth::SecretRef { .. } | Auth::None) {
        return Err(UNAVAILABLE);
    }
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_is_reference_only_and_resolves_from_injected_store() {
        let store = SecretStore::memory("so.tilo.api-tester");
        for auth in [
            Auth::Bearer {
                token: "private-token".into(),
            },
            Auth::Basic {
                username: "private-user".into(),
                password: "private-password".into(),
            },
            Auth::ApiKey {
                header: "X-API-Key".into(),
                value: "private-key".into(),
            },
        ] {
            assert!(serde_json::to_string(&auth).is_err());
            assert!(!format!("{auth:?}").contains("private-"));
            let reference = seal(&auth, &store).unwrap();
            let json = serde_json::to_string(&reference).unwrap();
            assert!(!json.contains("private-"));
            assert_eq!(
                resolve(&serde_json::from_str(&json).unwrap(), &store).unwrap(),
                auth
            );
            assert!(resolve(&reference, &SecretStore::memory("other-host")).is_err());
        }
    }

    #[test]
    fn a_plaintext_credential_is_sealed_and_resolves_from_the_store() {
        let legacy: Auth =
            serde_json::from_str(r#"{"Bearer":{"token":"legacy-plaintext"}}"#).unwrap();
        let store = SecretStore::memory("test");
        let reference = seal(&legacy, &store).unwrap();
        assert_eq!(resolve(&reference, &store).unwrap(), legacy);
        assert!(
            !serde_json::to_string(&reference)
                .unwrap()
                .contains("legacy-plaintext")
        );
    }
}
