//! Linux and macOS backends.

#[cfg(feature = "http")]
pub(crate) mod http;
pub(crate) mod tls;
#[cfg(feature = "webtransport")]
pub(crate) mod webtransport;
#[cfg(feature = "ws")]
pub(crate) mod ws;

#[cfg(feature = "sockets")]
pub(crate) async fn deadline<T>(
    timeout: Option<std::time::Duration>,
    future: impl Future<Output = Result<T, crate::NetError>>,
) -> Result<T, crate::NetError> {
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| crate::NetError::Timeout)?,
        None => future.await,
    }
}

/// An error's whole source chain: the top-level text alone hides the cause.
#[cfg(any(feature = "http", feature = "ws", feature = "webtransport"))]
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
