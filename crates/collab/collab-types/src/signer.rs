//! The device key as a signer rather than as bytes.
//!
//! A browser keeps its device key as a non-extractable `WebCrypto` key that
//! signs through a promise, so code that signs with the device key (the relay
//! handshake, certification, a steward's governance frames) asks a
//! [`DeviceSigner`] and awaits the signature. Native devices pass their
//! `SigningKey`, which implements the trait and answers at once.

use std::future::Future;
use std::pin::Pin;

/// `Send + Sync` where the platform has threads; nothing on the web, where
/// signer handles are JavaScript objects bound to the page's one thread.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSendSync: Send + Sync {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}
/// `Send + Sync` where the platform has threads; nothing on the web, where
/// signer handles are JavaScript objects bound to the page's one thread.
#[cfg(target_family = "wasm")]
pub trait MaybeSendSync {}
#[cfg(target_family = "wasm")]
impl<T: ?Sized> MaybeSendSync for T {}

/// A pending Ed25519 signature.
#[cfg(not(target_family = "wasm"))]
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<[u8; 64], SignError>> + Send + 'a>>;
/// A pending Ed25519 signature.
#[cfg(target_family = "wasm")]
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<[u8; 64], SignError>> + 'a>>;

/// The signer refused or failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("device signer failed: {0}")]
pub struct SignError(pub String);

/// Signs with one device key.
///
/// Every message a caller passes is domain-separated: a digest built from a
/// frozen tag (handshake challenges, certification requests) or the
/// canonical signing bytes of a signed frame. The signer holds the key;
/// nothing it signs can grant it away.
pub trait DeviceSigner: MaybeSendSync {
    /// The Ed25519 public key of the device.
    fn device_public_key(&self) -> [u8; 32];
    /// Ed25519-sign `message`.
    fn sign(&self, message: Vec<u8>) -> SignFuture<'_>;
}

impl DeviceSigner for ed25519_dalek::SigningKey {
    fn device_public_key(&self) -> [u8; 32] {
        self.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(self, &message).to_bytes();
        Box::pin(std::future::ready(Ok(signature)))
    }
}
