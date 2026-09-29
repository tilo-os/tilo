//! WebSocket on the desktop: tokio-websockets over rustls on the system
//! trust bundle.

use std::sync::Arc;

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_websockets::{ClientBuilder, Connector, MaybeTlsStream, WebSocketStream};
use url::Url;

use super::chain;
use crate::NetError;
use crate::ws::{Message, Options};

pub(crate) struct Socket(WebSocketStream<MaybeTlsStream<TcpStream>>);

pub(crate) async fn connect(
    url: Url,
    options: Options,
) -> Result<(Socket, Option<String>), NetError> {
    let uri = url
        .as_str()
        .parse::<http::Uri>()
        .map_err(|error| NetError::InvalidUrl(format!("{url}: {error}")))?;
    let mut tls =
        super::tls::client_config().map_err(|error| NetError::Protocol(format!("TLS: {error}")))?;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = Connector::Rustls(tokio_rustls::TlsConnector::from(Arc::new(tls)));
    let mut builder = ClientBuilder::from_uri(uri).connector(&connector);
    if !options.protocols.is_empty() {
        let offered = http::HeaderValue::try_from(options.protocols.join(", "))
            .map_err(|error| NetError::InvalidRequest(format!("subprotocols: {error}")))?;
        builder = builder
            .add_header(http::header::SEC_WEBSOCKET_PROTOCOL, offered)
            .map_err(|error| NetError::InvalidRequest(error.to_string()))?;
    }
    let (stream, response) = builder
        .connect()
        .await
        .map_err(|error| NetError::connect(chain(&error)))?;
    let protocol = response
        .headers()
        .get(http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok((Socket(stream), protocol))
}

impl Socket {
    pub(crate) async fn send(&mut self, message: Message) -> Result<(), NetError> {
        let frame = match message {
            Message::Text(text) => tokio_websockets::Message::text(text),
            Message::Binary(bytes) => tokio_websockets::Message::binary(bytes),
        };
        self.0.send(frame).await.map_err(map_error)
    }

    pub(crate) async fn recv(&mut self) -> Option<Result<Message, NetError>> {
        loop {
            let frame = match self.0.next().await? {
                Ok(frame) => frame,
                Err(error) => return Some(Err(map_error(error))),
            };
            if frame.is_close() {
                return None;
            }
            if let Some(text) = frame.as_text() {
                return Some(Ok(Message::Text(text.to_owned())));
            }
            if frame.is_binary() {
                return Some(Ok(Message::Binary(frame.into_payload().into())));
            }
        }
    }

    pub(crate) async fn close(&mut self) -> Result<(), NetError> {
        self.0.close().await.map_err(map_error)
    }
}

fn map_error(error: tokio_websockets::Error) -> NetError {
    match error {
        tokio_websockets::Error::AlreadyClosed => NetError::Closed,
        tokio_websockets::Error::Io(error) => NetError::connect(error.to_string()),
        other => NetError::Protocol(chain(&other)),
    }
}
