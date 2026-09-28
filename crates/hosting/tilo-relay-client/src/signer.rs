//! The device key as a signer rather than as bytes.
//!
//! A browser keeps its device key as a non-extractable `WebCrypto` key, so
//! the relay handshake cannot take an `ed25519_dalek::SigningKey`. It asks a
//! [`DeviceSigner`] to sign the exact challenge digest instead. Native
//! devices pass their `SigningKey`, which implements the trait directly.

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

/// A pending signature.
#[cfg(not(target_family = "wasm"))]
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<[u8; 64], SignError>> + Send + 'a>>;
/// A pending signature.
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
/// canonical signing bytes of a signed collaboration envelope. The signer
/// holds the key; nothing it signs can grant it away.
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
        Box::pin(async move { Ok(signature) })
    }
}

/// The final handshake frame, signed by `signer`.
///
/// # Errors
///
/// The signer's refusal or failure, as its message.
pub async fn signed_ack2(
    signer: &dyn DeviceSigner,
    server_nonce: &[u8; 32],
) -> Result<tilo_relay_protocol::auth::HelloAck2, String> {
    let sig_over_server_nonce = signer
        .sign(tilo_relay_protocol::auth::challenge_digest(server_nonce).to_vec())
        .await
        .map_err(|error| error.0)?;
    Ok(tilo_relay_protocol::auth::HelloAck2 {
        sig_over_server_nonce,
    })
}
