//! Publisher-signed reads of a publisher's own pending entry.
//!
//! A pending entry names uploads that are still private, so only its
//! publisher may read it: `GET /entries/<package id>/pending` with the
//! publisher key in [`UPLOAD_PUBLISHER_HEADER`](crate::upload::UPLOAD_PUBLISHER_HEADER),
//! the Unix time in [`PENDING_TIME_HEADER`], and in
//! [`UPLOAD_SIGNATURE_HEADER`](crate::upload::UPLOAD_SIGNATURE_HEADER) an
//! Ed25519 signature over [`pending_read_message`]. A new release builds on
//! it, since a replacement pending entry must keep every version the one it
//! replaces lists.

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};

/// Domain separator of a pending-read signature.
pub const PENDING_READ_DOMAIN: &[u8] = b"tilo-registry-pending-read-v1\0";

/// Header carrying the Unix time the read was signed at.
pub const PENDING_TIME_HEADER: &str = "tilo-signed-at";

/// How far a read's signing time may be from the registry's clock.
pub const PENDING_READ_SKEW_SECS: u64 = 300;

/// The bytes a pending-read signature covers.
#[must_use]
pub fn pending_read_message(package_id: &str, signed_at_unix: u64) -> Vec<u8> {
    let mut message = PENDING_READ_DOMAIN.to_vec();
    message.extend_from_slice(package_id.as_bytes());
    message.push(0);
    message.extend_from_slice(signed_at_unix.to_string().as_bytes());
    message
}

/// Sign a read of `package_id`'s pending entry at `signed_at_unix`; hex.
#[must_use]
pub fn sign_pending_read(key: &SigningKey, package_id: &str, signed_at_unix: u64) -> String {
    hex::encode(
        key.sign(&pending_read_message(package_id, signed_at_unix))
            .to_bytes(),
    )
}

/// Whether `signature_hex` is `publisher`'s read of `package_id` signed at
/// `signed_at_unix`, and that time is within [`PENDING_READ_SKEW_SECS`] of
/// `now_unix`.
#[must_use]
pub fn verify_pending_read(
    publisher: &[u8; 32],
    package_id: &str,
    signed_at_unix: u64,
    signature_hex: &str,
    now_unix: u64,
) -> bool {
    if signed_at_unix.abs_diff(now_unix) > PENDING_READ_SKEW_SECS {
        return false;
    }
    let Ok(key) = VerifyingKey::from_bytes(publisher) else {
        return false;
    };
    let Some(signature) = hex::decode(signature_hex)
        .ok()
        .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
    else {
        return false;
    };
    key.verify(
        &pending_read_message(package_id, signed_at_unix),
        &Signature::from_bytes(&signature),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_read_binds_the_key_the_id_and_the_time() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let publisher = key.verifying_key().to_bytes();
        let signature = sign_pending_read(&key, "com.example.editor", 1_000);
        assert!(verify_pending_read(
            &publisher,
            "com.example.editor",
            1_000,
            &signature,
            1_100
        ));
        assert!(!verify_pending_read(
            &publisher,
            "com.example.other",
            1_000,
            &signature,
            1_100
        ));
        assert!(!verify_pending_read(
            &publisher,
            "com.example.editor",
            1_001,
            &signature,
            1_100
        ));
        assert!(
            !verify_pending_read(&publisher, "com.example.editor", 1_000, &signature, 1_301),
            "a stale signature is refused"
        );
        let other = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(!verify_pending_read(
            &other,
            "com.example.editor",
            1_000,
            &signature,
            1_100
        ));
    }
}
