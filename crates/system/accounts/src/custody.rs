//! What the session's account custody decides for an app asking on its own
//! route: Settings, the keeper of the person's accounts, seals an account's
//! secret; the app that keeps an account's type
//! ([`crate::person::secret_keeper`], Mail for email) is released that one
//! account's secret. Nothing else is sealed or released.
//!
//! The session holds the person's vault under session custody and answers
//! with these; a sandboxed app never opens the vault
//! (docs/security/passwords-and-secrets.md).

use zeroize::Zeroizing;

use crate::{AccountCredentials, AccountId, AccountStore, Secret};

/// Why custody did not seal or release a secret, as the app is told.
pub const REFUSED: &str = "This account's secret is not available to this app";

/// Seal `password` as account `account`'s secret, for `app`: only the
/// accounts' keeper may. A password replaces the password; a sign-in's
/// sealed grant stays.
///
/// # Errors
/// Refuses any other app; returns why the vault could not be saved.
pub fn seal(
    credentials: &mut AccountCredentials,
    app: &str,
    account: &str,
    password: &str,
) -> Result<(), String> {
    if app != crate::person::ACCOUNTS_OWNER {
        return Err(REFUSED.into());
    }
    let id = AccountId::from(account.to_owned());
    let token = credentials.get(&id).and_then(|secret| secret.token);
    credentials.put(
        &id,
        &Secret {
            password: password.to_owned(),
            token,
        },
    );
    credentials.try_save()
}

/// Account `account`'s secret for `app`: only for an enabled account of the
/// person's (`store`) whose type `app` keeps, with a secret sealed.
///
/// # Errors
/// Refuses any other app, account or type, and an account with no secret.
pub fn release(
    store: &AccountStore,
    credentials: &AccountCredentials,
    app: &str,
    account: &str,
) -> Result<Zeroizing<String>, String> {
    let id = AccountId::from(account.to_owned());
    let kept = store.get(&id).ok_or(REFUSED)?;
    if !kept.enabled || crate::person::secret_keeper(kept.type_id.as_str()) != Some(app) {
        return Err(REFUSED.into());
    }
    credentials
        .get(&id)
        .map(|secret| Zeroizing::new(secret.password))
        .filter(|password| !password.is_empty())
        .ok_or_else(|| REFUSED.into())
}
