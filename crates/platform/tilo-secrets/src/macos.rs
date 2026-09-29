//! The macOS backend: generic passwords in the login Keychain, with the app
//! id as the service and the key as the account.
//!
//! An unsigned development binary gets its own Keychain access prompt the
//! first time it reads an item another build stored; a signed `.app` with a
//! stable identity does not.

use security_framework::passwords;

use crate::{Protection, Secret, SecretBackend, SecretError};

/// `errSecItemNotFound`.
const NOT_FOUND: i32 = -25300;
/// `errSecUserCanceled` and `errSecAuthFailed`.
const DENIED: [i32; 2] = [-128, -25293];

pub(crate) struct Keychain;

fn error(error: security_framework::base::Error) -> SecretError {
    if DENIED.contains(&error.code()) {
        SecretError::Denied
    } else {
        SecretError::Failed(format!("Keychain: {error}"))
    }
}

impl SecretBackend for Keychain {
    fn protection(&self) -> Protection {
        Protection::OsKeychain
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        match passwords::get_generic_password(app, key) {
            Ok(bytes) => Ok(Some(Secret::new(bytes))),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    fn put(&self, app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        passwords::set_generic_password(app, key, value).map_err(error)
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        match passwords::delete_generic_password(app, key) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(error(e)),
        }
    }
}
