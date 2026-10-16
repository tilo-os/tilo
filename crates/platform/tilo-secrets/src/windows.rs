//! The Windows backend: generic credentials in the user's Credential
//! Manager, which keeps them encrypted with the user's DPAPI key, readable
//! by that user on this machine and by no one else.
//!
//! Each secret is a credential named `tilo:<app>/<key>` (as Credential
//! Manager lists it under Windows Credentials), persisted for this machine
//! only so it never roams to another; a secret larger than one credential
//! holds continues in numbered ones (see the `credential` module).

use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_NO_SUCH_LOGON_SESSION, ERROR_NOT_FOUND, GetLastError, WIN32_ERROR,
};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};
use zeroize::Zeroizing;

use crate::credential::{further_parts, join, split, target_name};
use crate::{Protection, Secret, SecretBackend, SecretError};

pub(crate) struct CredentialManager;

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failure(call: &str, code: WIN32_ERROR) -> SecretError {
    match code {
        ERROR_ACCESS_DENIED => SecretError::Denied,
        // A service or a session without a loaded user profile.
        ERROR_NO_SUCH_LOGON_SESSION => {
            SecretError::Unavailable("Credential Manager needs a signed-in user's session".into())
        }
        _ => SecretError::Failed(format!("Credential Manager: {call} failed (error {code})")),
    }
}

/// The blob of the credential `target`, or `None` where there is none.
fn read(target: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
    let name = wide(target);
    let mut found: *mut CREDENTIALW = ptr::null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call; `found` is
    // written only on success.
    if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut found) } == 0 {
        // SAFETY: reads the calling thread's last error.
        return match unsafe { GetLastError() } {
            ERROR_NOT_FOUND => Ok(None),
            code => Err(failure("CredReadW", code)),
        };
    }
    // SAFETY: on success `found` points at a credential the system
    // allocated, whose blob holds `CredentialBlobSize` bytes; it is freed
    // once copied.
    let blob = unsafe {
        let credential = &*found;
        let bytes = if credential.CredentialBlob.is_null() || credential.CredentialBlobSize == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
            .to_vec()
        };
        CredFree(found.cast());
        bytes
    };
    Ok(Some(Zeroizing::new(blob)))
}

/// Store `blob` as the credential `target`, replacing it.
fn write(target: &str, blob: &[u8], label: &str) -> Result<(), SecretError> {
    let mut name = wide(target);
    let mut comment = wide(label);
    let size = u32::try_from(blob.len())
        .map_err(|_| SecretError::Failed("a credential part is too large".into()))?;
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: name.as_mut_ptr(),
        Comment: comment.as_mut_ptr(),
        CredentialBlobSize: size,
        // The system copies the blob; it only reads through this pointer.
        CredentialBlob: blob.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..CREDENTIALW::default()
    };
    // SAFETY: every pointer in `credential` is valid for the call.
    if unsafe { CredWriteW(&raw const credential, 0) } == 0 {
        // SAFETY: reads the calling thread's last error.
        return Err(failure("CredWriteW", unsafe { GetLastError() }));
    }
    Ok(())
}

/// Delete the credential `target`; a missing one is already deleted.
fn remove(target: &str) -> Result<(), SecretError> {
    let name = wide(target);
    // SAFETY: `name` is NUL-terminated and outlives the call.
    if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        // SAFETY: reads the calling thread's last error.
        return match unsafe { GetLastError() } {
            ERROR_NOT_FOUND => Ok(()),
            code => Err(failure("CredDeleteW", code)),
        };
    }
    Ok(())
}

/// How many further parts the stored secret has; none where there is no
/// secret or the first part is not one Tilo wrote.
fn stored_parts(app: &str, key: &str) -> Result<usize, SecretError> {
    Ok(read(&target_name(app, key, 0))?
        .and_then(|first| further_parts(&first).ok())
        .unwrap_or(0))
}

impl SecretBackend for CredentialManager {
    fn protection(&self) -> Protection {
        Protection::OsKeychain
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        let Some(first) = read(&target_name(app, key, 0))? else {
            return Ok(None);
        };
        let count = further_parts(&first).map_err(SecretError::Failed)?;
        let mut rest = Zeroizing::new(Vec::with_capacity(count));
        for part in 1..=count {
            let blob = read(&target_name(app, key, part))?.ok_or_else(|| {
                SecretError::Failed(format!("the secret {app} {key} is missing part {part}"))
            })?;
            rest.push(blob.to_vec());
        }
        let value = Zeroizing::new(join(&first, &rest).map_err(SecretError::Failed)?);
        Ok(Some(Secret::new(value.as_slice())))
    }

    /// The further parts are written before the first, which names how many
    /// there are, so a reader never finds a first part without its parts;
    /// parts an earlier, larger secret left are removed last.
    fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
        let before = stored_parts(app, key)?;
        let parts = split(value).map_err(SecretError::Failed)?;
        let Some((first, rest)) = parts.split_first() else {
            return Err(SecretError::Failed("a secret has no parts".into()));
        };
        for (blob, part) in rest.iter().zip(1..) {
            write(&target_name(app, key, part), blob, label)?;
        }
        write(&target_name(app, key, 0), first, label)?;
        for part in parts.len()..=before {
            remove(&target_name(app, key, part))?;
        }
        Ok(())
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        let parts = stored_parts(app, key)?;
        remove(&target_name(app, key, 0))?;
        for part in 1..=parts {
            remove(&target_name(app, key, part))?;
        }
        Ok(())
    }

    /// Reading a credential nobody stores reaches Credential Manager without
    /// touching a secret.
    fn probe(&self) -> Result<(), SecretError> {
        read("tilo:probe").map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes, overwrites (smaller and larger), reads and deletes through
    /// the signed-in user's Credential Manager, only where a developer asks
    /// for it: it writes their real store.
    #[test]
    fn credential_manager_keeps_the_store_contract_when_asked() {
        if std::env::var_os("TILO_SECRETS_SYSTEM_TEST").is_none() {
            return;
        }
        let store = crate::SecretStore::with_backend(
            "so.tilo.secrets-test",
            std::sync::Arc::new(CredentialManager),
        );
        store.probe_blocking().unwrap();
        crate::conformance::check_store_blocking(&store);
        let large: Vec<u8> = (0..9_000).map(|i| u8::try_from(i % 253).unwrap()).collect();
        let options = crate::PutOptions::default;
        store.put_blocking("large", &large, options()).unwrap();
        let read_back = store.get_blocking("large").unwrap().unwrap();
        assert_eq!(read_back.expose(), &large[..]);
        store.put_blocking("large", b"small", options()).unwrap();
        assert_eq!(
            store.get_blocking("large").unwrap().unwrap().expose(),
            b"small"
        );
        assert_eq!(
            read(&target_name("so.tilo.secrets-test", "large", 1)).unwrap(),
            None
        );
        store.delete_blocking("large").unwrap();
        assert_eq!(store.get_blocking("large").unwrap(), None);
    }
}
