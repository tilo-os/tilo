//! Publisher-signed artifact uploads.
//!
//! A publisher sends a `.tapp` to `PUT /uploads/<blake3>.tapp` with its public
//! key in [`UPLOAD_PUBLISHER_HEADER`] and, in [`UPLOAD_SIGNATURE_HEADER`], an
//! Ed25519 signature over [`upload_signing_message`] of the file's BLAKE3.
//! The registry keeps the bytes privately until review approves a version
//! that binds them, then serves them at `/artifacts/<blake3>.tapp`.

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};

/// Domain separator of an upload signature.
pub const UPLOAD_DOMAIN: &[u8] = b"tilo-registry-upload-v1\0";

/// Header carrying the uploader's Ed25519 public key, hex.
pub const UPLOAD_PUBLISHER_HEADER: &str = "tilo-publisher";

/// Header carrying the upload signature, hex.
pub const UPLOAD_SIGNATURE_HEADER: &str = "tilo-signature";

/// The bytes an upload signature covers: the domain and the lowercase BLAKE3
/// hex of the exact file.
#[must_use]
pub fn upload_signing_message(blake3_hex: &str) -> Vec<u8> {
    let mut message = UPLOAD_DOMAIN.to_vec();
    message.extend_from_slice(blake3_hex.to_ascii_lowercase().as_bytes());
    message
}

/// Sign an upload of the file whose BLAKE3 is `blake3_hex`; hex signature.
#[must_use]
pub fn sign_upload(key: &SigningKey, blake3_hex: &str) -> String {
    hex::encode(key.sign(&upload_signing_message(blake3_hex)).to_bytes())
}

/// Whether `signature_hex` is `publisher`'s signature over an upload of
/// `blake3_hex`.
#[must_use]
pub fn verify_upload(publisher: &[u8; 32], blake3_hex: &str, signature_hex: &str) -> bool {
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
        &upload_signing_message(blake3_hex),
        &Signature::from_bytes(&signature),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_signature_binds_the_key_and_the_digest() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let publisher = key.verifying_key().to_bytes();
        let digest = "ab".repeat(32);
        let signature = sign_upload(&key, &digest);
        assert!(verify_upload(&publisher, &digest, &signature));
        assert!(verify_upload(
            &publisher,
            &digest.to_uppercase(),
            &signature
        ));
        assert!(!verify_upload(&publisher, &"cd".repeat(32), &signature));
        let other = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(!verify_upload(&other, &digest, &signature));
        assert!(!verify_upload(&publisher, &digest, "00"));
    }
}
