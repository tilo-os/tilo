//! WebSocket clients: tokio-websockets on the desktop, the page's
//! `WebSocket` on the web. Pings and pongs are answered by the backend and
//! never surface. A page cannot set request headers on the upgrade, so
//! [`Options`] carries only what both targets can send.

use bytes::Bytes;

use crate::NetError;

#[cfg(not(target_family = "wasm"))]
use crate::native::ws as backend;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use crate::web::ws as backend;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Bytes),
}

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Subprotocols to offer (`Sec-WebSocket-Protocol`), in preference order.
    pub protocols: Vec<String>,
}

impl Options {
    #[must_use]
    pub fn protocols<I, S>(protocols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            protocols: protocols.into_iter().map(Into::into).collect(),
        }
    }
}

pub struct WebSocket {
    inner: backend::Socket,
    protocol: Option<String>,
}

impl std::fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocket")
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

pub fn connect(url: &str) -> impl Future<Output = Result<WebSocket, NetError>> + Send + use<> {
    connect_with(url, Options::default())
}

pub fn connect_with(
    url: &str,
    options: Options,
) -> impl Future<Output = Result<WebSocket, NetError>> + Send + use<> {
    let url = url::Url::parse(url)
        .map_err(|error| NetError::InvalidUrl(format!("{url}: {error}")))
        .and_then(|url| {
            if matches!(url.scheme(), "ws" | "wss") {
                Ok(url)
            } else {
                Err(NetError::InvalidUrl(format!(
                    "{url}: a WebSocket URL starts with ws: or wss:"
                )))
            }
        });
    async move {
        let (inner, protocol) = backend::connect(url?, options).await?;
        Ok(WebSocket { inner, protocol })
    }
}

impl WebSocket {
    #[must_use]
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    pub fn send(
        &mut self,
        message: Message,
    ) -> impl Future<Output = Result<(), NetError>> + Send + '_ {
        self.inner.send(message)
    }

    pub fn recv(&mut self) -> impl Future<Output = Option<Result<Message, NetError>>> + Send + '_ {
        self.inner.recv()
    }

    pub async fn close(mut self) -> Result<(), NetError> {
        self.inner.close().await
    }
}
