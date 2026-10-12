//! Explicit owner migration reads from a recognized native application keychain.
//! Ordinary `SecretStore` remains app-scoped. This API neither writes nor deletes
//! foreign items and must be called only after the owner selects this account.
use crate::{Secret, SecretError};

mod chromium;
mod goa;
pub use chromium::{ChromiumBrand, ChromiumKey};
pub use goa::{GoaCredential, GoaCredentials, GoaProvider, GoaRegistration};

/// One exact HTTP credential used by the native Nextcloud client.
/// The key format is defined by Nextcloud's `AbstractCredentials::keychainKey`:
/// https://github.com/nextcloud/desktop/blob/master/src/libsync/creds/abstractcredentials.cpp
/// QtKeychain's libsecret attributes are `user`, `server`, and `type`:
/// https://github.com/frankosterfeld/qtkeychain/blob/main/qtkeychain/libsecret.cpp
pub struct NextcloudCredential {
    key: String,
}
impl NextcloudCredential {
    /// Construct the precise key for the selected native account; no wildcard search.
    ///
    /// # Errors
    /// Refuses missing or oversized account fields, unsafe URL forms and controls.
    pub fn new(server: &str, username: &str, account_id: &str) -> Result<Self, SecretError> {
        if !server.starts_with("https://")
            || server.len() > 2048
            || server.contains(['?', '#', '@'])
            || username.is_empty()
            || username.len() > 256
            || username.contains(':')
            || account_id.is_empty()
            || account_id.len() > 128
            || !account_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || server.chars().chain(username.chars()).any(char::is_control)
        {
            return Err(SecretError::Denied);
        }
        let separator = if server.ends_with('/') { "" } else { "/" };
        Ok(Self {
            key: format!("{username}:{server}{separator}:{account_id}"),
        })
    }
    /// Read only this credential from the current owner's Secret Service.
    /// The service may show its own unlock prompt; no collection or item is created.
    ///
    /// # Errors
    /// Refuses ambiguous matches, unavailable/locked custody, or an invalid secret.
    pub fn read_blocking(&self) -> Result<Option<Secret>, SecretError> {
        crate::linux::read_nextcloud(&self.key)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_account_key_is_exact_and_rejects_unbound_queries() {
        assert_eq!(
            NextcloudCredential::new("https://cloud.test/nextcloud", "ada", "0")
                .unwrap()
                .key,
            "ada:https://cloud.test/nextcloud/:0"
        );
        assert_eq!(
            NextcloudCredential::new("https://cloud.test/", "ada", "7")
                .unwrap()
                .key,
            "ada:https://cloud.test/:7"
        );
        for (server, user, id) in [
            ("http://cloud.test", "ada", "0"),
            ("https://cloud.test/#x", "ada", "0"),
            ("https://cloud.test", "", "0"),
            ("https://cloud.test", "ada", ""),
        ] {
            assert!(NextcloudCredential::new(server, user, id).is_err());
        }
    }
}
