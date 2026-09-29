//! Linux and macOS backends.

#![cfg_attr(
    not(all(
        feature = "http",
        feature = "ws",
        feature = "webtransport",
        feature = "sockets"
    )),
    allow(dead_code, reason = "the helpers serve feature-gated backends")
)]

#[cfg(feature = "http")]
pub(crate) mod http;
pub(crate) mod tls;
#[cfg(feature = "webtransport")]
pub(crate) mod webtransport;
#[cfg(feature = "ws")]
pub(crate) mod ws;

use std::time::Duration;

use crate::NetError;

/// Run `future` with an optional deadline.
pub(crate) async fn deadline<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = Result<T, NetError>>,
) -> Result<T, NetError> {
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| NetError::Timeout)?,
        None => future.await,
    }
}

/// The error and its sources, which carry the useful part (DNS, TLS,
/// refused).
pub(crate) fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}
