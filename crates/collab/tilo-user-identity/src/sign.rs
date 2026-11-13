//! Who signs a statement: the one seam between a statement's bytes and the key
//! that approves them.
//!
//! Everything this crate signs — a device certificate, a revocation, a
//! certification request — signs the 32-byte digest of a frozen preimage with
//! an Ed25519 key (see the module docs of the crate). A [`StatementSigner`] is
//! the key side of that: native code holds the key and answers at once, and a
//! page keeps its key in `WebCrypto` custody and answers later, which is what
//! the `_async` twin of each signing function is for. Both spellings run the
//! same checks over the same bytes.

pub use collab_types::signer::SignError;

/// The key that signs a person's statements: their root's, a stand-in's, or an
/// agent's grant's.
///
/// Implemented on the keys themselves, so that nothing has to know which kind
/// of custody a key lives in to sign with it.
pub trait StatementSigner {
    /// The public key the statement names as its signer: a certificate's
    /// `user_public_key`, a revocation's, or the stand-in's or grant's key that
    /// issued another certificate.
    fn public_key(&self) -> [u8; 32];

    /// Ed25519-sign `digest`, a statement's domain-separated digest.
    ///
    /// # Errors
    ///
    /// The key is locked, was refused by whoever holds it, or failed.
    fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 64], SignError>;
}

/// A digest in, a signature out: what a vault holding a key offers instead of
/// the key.
pub type DigestCallback<'a, E> = &'a dyn Fn(&[u8]) -> Result<[u8; 64], E>;

/// A key a vault holds: it signs a digest and names the public key it signs
/// under, and nothing here sees the key itself. What a prepared-identity vault,
/// an identity record or a secret store is handed as.
pub struct CallbackSigner<'a, E> {
    public_key: [u8; 32],
    sign: DigestCallback<'a, E>,
}

impl<'a, E> CallbackSigner<'a, E> {
    /// `sign` as the signer of `public_key`.
    #[must_use]
    pub fn new(public_key: [u8; 32], sign: DigestCallback<'a, E>) -> Self {
        Self { public_key, sign }
    }
}

impl<E: std::fmt::Display> StatementSigner for CallbackSigner<'_, E> {
    fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 64], SignError> {
        (self.sign)(digest).map_err(|error| SignError(error.to_string()))
    }
}

impl StatementSigner for ed25519_dalek::SigningKey {
    fn public_key(&self) -> [u8; 32] {
        self.verifying_key().to_bytes()
    }

    fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 64], SignError> {
        use ed25519_dalek::Signer as _;
        Ok(self.sign(digest).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_key_and_a_vault_callback_sign_the_same_digest() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let digest = [3u8; 32];
        let held = StatementSigner::sign_digest(&key, &digest).unwrap();
        let callback = |digest: &[u8]| {
            let digest: [u8; 32] = digest.try_into().expect("a digest");
            StatementSigner::sign_digest(&key, &digest).map_err(|error| error.to_string())
        };
        let vault = CallbackSigner::new(StatementSigner::public_key(&key), &callback);
        assert_eq!(vault.public_key(), StatementSigner::public_key(&key));
        assert_eq!(vault.sign_digest(&digest).unwrap(), held);
    }
}
