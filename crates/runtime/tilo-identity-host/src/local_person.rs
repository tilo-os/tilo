//! Which account on this device is the person the device is bound to.
//!
//! The device is bound to a person's identity: its certificate is issued by
//! that identity, whose public key is `/var/lib/tilo/user-identity/pubkey.bin`.
//! Which login account is that person is what the system's own services ask
//! (the storage daemon serves the workspace as theirs, storage.md §6.2), and
//! saying so takes the identity's key: the account that holds it signs a
//! **local person proof** naming its uid and this device's key, and keeps it
//! in its identity directory ([`PROOF_FILE`]). Anyone can read the proof, and
//! nobody without the key can make one: another account that copies it gets
//! a proof of someone else's uid, and one that names the identity in its own
//! records proves nothing. A claim without a proof that verifies is ignored.
//!
//! The session writes the proof whenever it provisions the identity that owns
//! the device ([`ensure_proof`]), so a device bound before proofs existed
//! gets one at the person's next sign-in.

use std::io;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tilo_user_identity::IdentityPaths;
use tilo_user_identity::persist;

/// The proof's file in an account's identity directory: the 64-byte
/// Ed25519 signature of [`proof_message`] by the device's identity.
pub const PROOF_FILE: &str = "local-person.sig";

/// Where a login account keeps its identity records, relative to its home.
pub const IDENTITY_DIR_IN_HOME: &str = ".config/tilo/identity";

const DOMAIN: &[u8] = b"tilo local person v1\0";

/// What the identity `user_public_key` signs to say that the account with
/// `uid` on the device with key `device_public_key` is its person.
#[must_use]
pub fn proof_message(
    user_public_key: &[u8; 32],
    device_public_key: &[u8; 32],
    uid: u32,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len().saturating_add(68));
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(user_public_key);
    message.extend_from_slice(device_public_key);
    message.extend_from_slice(&uid.to_le_bytes());
    message
}

/// The identity this device is bound to and the device's own key, from its
/// certificate, which that identity must have issued to it.
///
/// # Errors
///
/// The binding cannot be read, or the certificate is not the identity's.
pub fn device_binding(paths: &IdentityPaths) -> io::Result<([u8; 32], [u8; 32])> {
    let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidData, message);
    let owner = persist::read_public_key(&paths.user_pubkey)
        .map_err(|error| invalid(format!("this device's identity: {error}")))?;
    let cert = persist::read_device_cert(&paths.device_cert)
        .map_err(|error| invalid(format!("this device's certificate: {error}")))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| invalid(format!("this device's certificate: {error}")))?;
    if cert.user_public_key != owner {
        return Err(invalid(
            "this device's certificate was issued by another identity than the one it is bound to"
                .into(),
        ));
    }
    Ok((owner, cert.device_public_key))
}

/// Whether `signature` proves that the account `uid` is the person of the
/// device bound to `user_public_key` with key `device_public_key`.
#[must_use]
pub fn proves(
    user_public_key: &[u8; 32],
    device_public_key: &[u8; 32],
    uid: u32,
    signature: &[u8],
) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(user_public_key) else {
        return false;
    };
    key.verify(
        &proof_message(user_public_key, device_public_key, uid),
        &Signature::from_bytes(&signature),
    )
    .is_ok()
}

/// A login account, as the system's account database lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// The regular accounts (uid 1000 to 59999) in `passwd`-format `text`.
#[must_use]
pub fn regular_accounts(text: &str) -> Vec<Account> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let [name, _, uid, gid, _, home, ..] = fields.as_slice() else {
                return None;
            };
            let uid: u32 = uid.parse().ok()?;
            let gid: u32 = gid.parse().ok()?;
            ((1000..60000).contains(&uid) && !name.is_empty()).then(|| Account {
                name: (*name).to_owned(),
                uid,
                gid,
                home: PathBuf::from(home),
            })
        })
        .collect()
}

/// Who the device's person is among `accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Person {
    /// Exactly one account proved it.
    One(Account),
    /// No account proved it.
    Nobody,
    /// Several accounts proved it: each holds the identity's key.
    Several(Vec<Account>),
}

/// The account among `accounts` that proves it is the person of the device
/// whose binding `paths` holds. Accounts without a proof, or whose proof does
/// not verify for their own uid, are not considered.
///
/// # Errors
///
/// The device's binding cannot be read ([`device_binding`]).
pub fn find(paths: &IdentityPaths, accounts: &[Account]) -> io::Result<Person> {
    let (user_public_key, device_public_key) = device_binding(paths)?;
    let mut proven: Vec<Account> = accounts
        .iter()
        .filter(|account| {
            std::fs::read(account.home.join(IDENTITY_DIR_IN_HOME).join(PROOF_FILE)).is_ok_and(
                |proof| proves(&user_public_key, &device_public_key, account.uid, &proof),
            )
        })
        .cloned()
        .collect();
    Ok(match proven.len() {
        0 => Person::Nobody,
        1 => Person::One(proven.remove(0)),
        _ => Person::Several(proven),
    })
}

/// Write this account's proof into `identity_dir` when `identity` is the one
/// the device is bound to and can sign here, and the proof held is missing or
/// does not verify. Answers whether a proof is held now.
///
/// # Errors
///
/// The proof could not be signed or written. An identity that is not the
/// device's, or a device with no binding yet, is `Ok(false)`.
pub fn ensure_proof(
    identity_dir: &Path,
    identity: &runtime_identity::SigningIdentity,
    paths: &IdentityPaths,
    uid: u32,
) -> io::Result<bool> {
    let Ok((user_public_key, device_public_key)) = device_binding(paths) else {
        return Ok(false);
    };
    if identity.group_public_key() != user_public_key {
        return Ok(false);
    }
    let path = identity_dir.join(PROOF_FILE);
    if std::fs::read(&path)
        .is_ok_and(|held| proves(&user_public_key, &device_public_key, uid, &held))
    {
        return Ok(true);
    }
    let signature = identity
        .sign(&proof_message(&user_public_key, &device_public_key, uid))
        .map_err(|error| io::Error::other(error.to_string()))?;
    let staged = identity_dir.join(format!(".{PROOF_FILE}.new"));
    std::fs::write(&staged, signature)?;
    {
        use std::os::unix::fs::PermissionsExt;
        // Public: the system reads it from every account's directory.
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&staged, &path)?;
    Ok(true)
}

#[cfg(test)]
#[path = "local_person_tests.rs"]
mod tests;
