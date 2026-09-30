//! The macOS backend: generic passwords in the login Keychain, with the app
//! id as the service and the key as the account.
//!
//! The login Keychain binds every item to the partition of the code that
//! stored it: `teamid:<team>` for a Developer ID signature, which every later
//! build of the same app shares, and `cdhash:<hash>` for an ad-hoc or linker
//! signature, which the next build does not. Another partition reads an item
//! only after the person confirms with their Keychain password, in a dialog
//! a process waits on for as long as nobody answers it.
//!
//! So a build without a Developer ID signature never lets the Keychain ask:
//! an item another build stored fails at once, saying why and what to do.

use std::sync::OnceLock;

use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};
use security_framework::passwords;

use crate::{Protection, Secret, SecretBackend, SecretError};

/// `errSecItemNotFound`.
const NOT_FOUND: i32 = -25300;
/// `errSecUserCanceled` and `errSecAuthFailed`.
const DENIED: [i32; 2] = [-128, -25293];
/// `errSecInteractionNotAllowed`: the Keychain wanted to ask.
const WOULD_ASK: i32 = -25308;
/// Code Signing Services' errors (`errSecCS*`): the Keychain could not check
/// the signature an item's access list names, or this process's own.
const CODE_SIGNING: std::ops::RangeInclusive<i32> = -67099..=-67000;

/// A Developer ID Application signature.
const DEVELOPER_ID: &str = "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] \
     exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists";

unsafe extern "C" {
    fn SecKeychainSetUserInteractionAllowed(allowed: u8) -> i32;
}

/// Whether this process carries a Developer ID signature, whose Keychain
/// items every later build of the same app reads. Anything else, including
/// a signature that cannot be checked, is not.
pub(crate) fn developer_id_signed() -> bool {
    static SIGNED: OnceLock<bool> = OnceLock::new();
    *SIGNED.get_or_init(|| {
        DEVELOPER_ID
            .parse::<SecRequirement>()
            .and_then(|requirement| {
                SecCode::for_self(Flags::NONE)?.check_validity(Flags::NONE, &requirement)
            })
            .is_ok()
    })
}

pub(crate) struct Keychain;

impl Keychain {
    /// The login Keychain, which from now on never asks the person anything
    /// in a process without a Developer ID signature.
    pub(crate) fn new() -> Self {
        if !developer_id_signed() {
            // SAFETY: a process-wide switch that takes a plain flag.
            unsafe { SecKeychainSetUserInteractionAllowed(0) };
        }
        Self
    }
}

fn error(error: security_framework::base::Error, app: &str, key: &str) -> SecretError {
    let code = error.code();
    let another_build = code == WOULD_ASK || DENIED.contains(&code) || CODE_SIGNING.contains(&code);
    if another_build && !developer_id_signed() {
        return SecretError::Failed(format!(
            "Keychain: {app} {key} belongs to another build ({error}). A build without a \
             Developer ID signature cannot use it without the Keychain password, and is \
             never allowed to ask. Use a Developer ID build, or delete the item to start \
             over: security delete-generic-password -s {app} -a {key}"
        ));
    }
    if DENIED.contains(&code) {
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
            Err(e) => Err(error(e, app, key)),
        }
    }

    fn put(&self, app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        passwords::set_generic_password(app, key, value).map_err(|e| error(e, app, key))
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        match passwords::delete_generic_password(app, key) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(error(e, app, key)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use security_framework::base::Error;

    /// Cargo's test binaries carry only the linker's ad-hoc signature.
    #[test]
    fn a_build_without_developer_id_names_the_item_instead_of_asking() {
        assert!(!developer_id_signed());
        // Asked, refused without asking, and a signature the Keychain could
        // not check (`errSecCSSignatureUnsupported`).
        for code in [-25308, -25293, -67045] {
            let SecretError::Failed(message) = error(Error::from_code(code), "so.tilo.x", "k")
            else {
                panic!("{code} is not a failure");
            };
            assert!(message.contains("another build"), "{message}");
            assert!(
                message.contains("security delete-generic-password -s so.tilo.x -a k"),
                "{message}"
            );
        }
        assert!(matches!(
            error(Error::from_code(-34018), "so.tilo.x", "k"),
            SecretError::Failed(message) if !message.contains("another build")
        ));
    }
}
