//! The device key a participant signs its governance frames with: a
//! [`DeviceSigner`], so a browser's `WebCrypto` key works as well as a native
//! `SigningKey`.

use std::future::Future;
use std::task::{Context, Poll, Waker};

pub use collab_types::signer::{DeviceSigner, MaybeSendSync, SignFuture};

use crate::{Error, Result};

/// Run a peer operation whose signer answers at once, such as a
/// `SigningKey`. An operation that would wait on its signer fails instead,
/// and the change it staged is dropped.
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
