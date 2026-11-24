//! The stand-in a server that holds a person's root keeps for them: a key
//! the server holds, certified by that root as an approver of new member
//! devices of theirs ([`tilo_user_identity::DeviceCapabilities::stand_in`]).
//! It is how the browser sessions the server places
//! ([`crate::hosted_browser`]) join a workspace as devices of the person,
//! without the root itself signing every session. Signing a person in
//! through their agent never goes through it: the device that claims the
//! code certifies the tab itself ([`super::sign_in`]).
//!
//! The key and its certificate are kept in the server's state directory,
//! readable by the server alone; a certificate that ran out is replaced the
//! next time the server keeps one.

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

fn key_path(dir: &Path, user: &[u8; 32]) -> PathBuf {
    dir.join(format!("{}.key", hex::encode(user)))
}

fn cert_path(dir: &Path, user: &[u8; 32]) -> PathBuf {
    dir.join(format!("{}.cert", hex::encode(user)))
}

/// Make this server the stand-in approver of the person whose root `root`
/// is: the key kept for them in `dir`, and the certificate their root signs
/// for it. A stand-in already live in `dir` is left as it is.
///
/// # Errors
///
/// The directory cannot be read or written, or the root would not sign it.
pub(crate) fn keep(dir: &Path, root: &SigningKey) -> Result<(), String> {
    let user = root.verifying_key().to_bytes();
    if kept(dir, &user).is_some() {
        return Ok(());
    }
    let seed = std::fs::read(key_path(dir, &user))
        .ok()
        .and_then(|seed| <[u8; 32]>::try_from(seed.as_slice()).ok());
    let key = if let Some(seed) = seed {
        SigningKey::from_bytes(&seed)
    } else {
        let key = SigningKey::from_bytes(&rand::random());
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let seed = zeroize::Zeroizing::new(key.to_bytes());
        tilo_files::native::atomic::write_with_mode(&key_path(dir, &user), seed.as_slice(), 0o600)
            .map_err(|error| error.to_string())?;
        key
    };
    let now = tilo_clock::unix_now_ns();
    let expires =
        now.saturating_add(tilo_user_identity::STAND_IN_LIFETIME_NS.saturating_sub(60_000_000_000));
    let mut cert = tilo_user_identity::prepare_stand_in_cert(
        user,
        key.verifying_key().to_bytes(),
        "Browser sessions",
        now,
        expires,
    );
    tilo_user_identity::sign_device_cert(&mut cert, root).map_err(|error| error.to_string())?;
    let bytes = postcard::to_allocvec(&cert).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_with_mode(&cert_path(dir, &user), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// The stand-in key and certificate this server keeps in `dir` for `user`,
/// when its certificate is live.
pub(crate) fn kept(dir: &Path, user: &[u8; 32]) -> Option<(SigningKey, DeviceCert)> {
    let seed: [u8; 32] = std::fs::read(key_path(dir, user))
        .ok()?
        .as_slice()
        .try_into()
        .ok()?;
    let key = SigningKey::from_bytes(&seed);
    let cert: DeviceCert = postcard::from_bytes(&std::fs::read(cert_path(dir, user)).ok()?).ok()?;
    (cert.device_public_key == key.verifying_key().to_bytes()
        && tilo_user_identity::verify_stand_in_cert_at(&cert, tilo_clock::unix_now_ns()).is_ok())
    .then_some((key, cert))
}

#[cfg(test)]
mod tests {
    use super::{keep, kept};

    /// The root a server holds signs one stand-in for it, kept across starts.
    #[test]
    fn a_server_holding_the_root_makes_itself_the_persons_stand_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let user = root.verifying_key().to_bytes();
        assert!(kept(dir.path(), &user).is_none(), "nothing kept yet");

        keep(dir.path(), &root).unwrap();
        let (key, cert) = kept(dir.path(), &user).expect("the stand-in is kept");
        assert_eq!(cert.user_public_key, user);
        assert_eq!(cert.device_public_key, key.verifying_key().to_bytes());
        assert!(
            cert.capabilities.stand_in,
            "it is an approver, nothing else"
        );
        assert_eq!(cert.issuer, None, "the root signed it");

        // A later start keeps the one it has, key and certificate.
        keep(dir.path(), &root).unwrap();
        assert_eq!(kept(dir.path(), &user).unwrap().1, cert);
    }
}
