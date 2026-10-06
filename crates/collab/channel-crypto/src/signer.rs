//! The device key a steward signs its governance frames with.
//!
//! A native device holds its `SigningKey` and signs at once. A browser's
//! device key is a non-extractable `WebCrypto` key, which signs through a
//! promise, so a signature is a future in both cases.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use collab_types::Ed25519PublicKey;
use ed25519_dalek::SigningKey;

use crate::{Error, Result};

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
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<[u8; 64]>> + Send + 'a>>;
/// A pending Ed25519 signature.
#[cfg(target_family = "wasm")]
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<[u8; 64]>> + 'a>>;

/// Signs a steward's governance frames with its device key.
pub trait StewardSigner: MaybeSendSync {
    /// The device key's public half, which names the steward.
    fn device(&self) -> Ed25519PublicKey;
    /// Ed25519-sign `message`, the signing bytes of one governance frame.
    fn sign(&self, message: Vec<u8>) -> SignFuture<'_>;
}

impl StewardSigner for SigningKey {
    fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.verifying_key().to_bytes())
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(self, &message).to_bytes();
        Box::pin(std::future::ready(Ok(signature)))
    }
}

/// Run a steward operation whose signer answers at once, such as a
/// `SigningKey`. An operation that would wait on its signer fails instead;
/// the steward finishes or discards the change it left staged the next time
/// it is used.
pub(crate) fn complete_now<T>(operation: impl Future<Output = Result<T>>) -> Result<T> {
    let mut operation = std::pin::pin!(operation);
    match operation
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => Err(Error::Signer(
            "the signer did not answer at once; use the asynchronous call".into(),
        )),
    }
}
