//! The streams one device-to-device link carries, whatever transports it.
//!
//! A native device dials another on its own iroh connection; a page reaches a
//! host over the browser's WebTransport session. Both carry the same records:
//! one unidirectional stream each way of length-prefixed frames, and the
//! bidirectional stream that opens a link and answers what it asks for. The
//! engine speaks to either through [`Transport`], so a workspace's records,
//! the link to its host and the page that reads them run one implementation.
//!
//! The transport proves which device is at the other end: an iroh connection's
//! endpoint key, or the device whose handshake the session's first stream
//! authenticated ([`Transport::remote`]). Nothing above a link trusts anything
//! else about who sends its frames.

use std::future::Future;
use std::pin::Pin;

use collab_types::signer::MaybeSendSync;
use tilo_server_protocol::refusal::Refusal;
use tokio::io::{AsyncRead, AsyncWrite};

/// The sending half of one stream of a link.
#[cfg(not(target_arch = "wasm32"))]
pub type StreamSend = Box<dyn AsyncWrite + Send + Unpin>;
/// The sending half of one stream of a link.
#[cfg(target_arch = "wasm32")]
pub type StreamSend = Box<dyn AsyncWrite + Unpin>;

/// The receiving half of one stream of a link.
#[cfg(not(target_arch = "wasm32"))]
pub type StreamRecv = Box<dyn AsyncRead + Send + Unpin>;
/// The receiving half of one stream of a link.
#[cfg(target_arch = "wasm32")]
pub type StreamRecv = Box<dyn AsyncRead + Unpin>;

/// A stream a transport is opening, answered once the other end is ready for
/// it.
#[cfg(not(target_arch = "wasm32"))]
pub type StreamFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;
/// A stream a transport is opening, answered once the other end is ready for
/// it.
#[cfg(target_arch = "wasm32")]
pub type StreamFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + 'a>>;

/// What carries one link between two devices: an iroh connection or a
/// WebTransport session.
///
/// Methods take `&self`, so one task sends while another receives, and a
/// transport clones cheaply into the tasks a link runs.
pub trait Transport: MaybeSendSync {
    /// Open a unidirectional stream, for what the other end only reads.
    fn open_uni(&self) -> StreamFuture<'_, StreamSend>;
    /// Take the next unidirectional stream the other end opens.
    fn accept_uni(&self) -> StreamFuture<'_, StreamRecv>;
    /// Open a bidirectional stream, the link's control stream first of all.
    fn open_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)>;
    /// Take the next bidirectional stream the other end opens.
    fn accept_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)>;
    /// The device at the other end, as the transport proved it.
    fn remote(&self) -> [u8; 32];
    /// Close the link at once, telling the other end `reason` under `code`. A
    /// transport that carries a close reason as text is given `reason` as it
    /// reads, lossily: a refusal's typed frame reaches a device over iroh, and
    /// a page is told the words.
    fn close(&self, code: u32, reason: &[u8]);
    /// Why the other end closed the link, when it closed it as a refusal
    /// (`code` [`crate::frames::REFUSED`]).
    fn close_reason(&self) -> Option<Refusal> {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for iroh::endpoint::Connection {
    fn open_uni(&self) -> StreamFuture<'_, StreamSend> {
        let connection = self.clone();
        Box::pin(async move {
            let send = connection
                .open_uni()
                .await
                .map_err(|error| error.to_string())?;
            Ok(Box::new(send) as StreamSend)
        })
    }

    fn accept_uni(&self) -> StreamFuture<'_, StreamRecv> {
        let connection = self.clone();
        Box::pin(async move {
            let recv = connection
                .accept_uni()
                .await
                .map_err(|error| error.to_string())?;
            Ok(Box::new(recv) as StreamRecv)
        })
    }

    fn open_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
        let connection = self.clone();
        Box::pin(async move {
            let (send, recv) = connection
                .open_bi()
                .await
                .map_err(|error| error.to_string())?;
            Ok((Box::new(send) as StreamSend, Box::new(recv) as StreamRecv))
        })
    }

    fn accept_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
        let connection = self.clone();
        Box::pin(async move {
            let (send, recv) = connection
                .accept_bi()
                .await
                .map_err(|error| error.to_string())?;
            Ok((Box::new(send) as StreamSend, Box::new(recv) as StreamRecv))
        })
    }

    fn remote(&self) -> [u8; 32] {
        *self.remote_id().as_bytes()
    }

    fn close(&self, code: u32, reason: &[u8]) {
        self.close(code.into(), reason);
    }

    fn close_reason(&self) -> Option<Refusal> {
        match iroh::endpoint::Connection::close_reason(self)? {
            iroh::endpoint::ConnectionError::ApplicationClosed(close)
                if close.error_code == crate::frames::REFUSED.into() =>
            {
                Some(match postcard::from_bytes(&close.reason) {
                    Ok(refusal) => refusal,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            "a device's refusal did not decode; kept as its text"
                        );
                        Refusal::Other(String::from_utf8_lossy(&close.reason).into_owned())
                    }
                })
            }
            _ => None,
        }
    }
}

/// A page's link to its host is one more transport the workspace's records
/// travel on ([`tilo_server_client::link::Link`]): the same streams over the
/// browser's WebTransport, or over an iroh connection the same code dialled.
impl Transport for tilo_server_client::link::Link {
    fn open_uni(&self) -> StreamFuture<'_, StreamSend> {
        let link = self.clone();
        Box::pin(async move {
            let send = link.open_uni().await.map_err(|error| error.to_string())?;
            Ok(Box::new(send) as StreamSend)
        })
    }

    fn accept_uni(&self) -> StreamFuture<'_, StreamRecv> {
        let link = self.clone();
        Box::pin(async move {
            let recv = link.accept_uni().await.map_err(|error| error.to_string())?;
            Ok(Box::new(recv) as StreamRecv)
        })
    }

    fn open_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
        let link = self.clone();
        Box::pin(async move {
            let (send, recv) = link.open_bi().await.map_err(|error| error.to_string())?;
            Ok((Box::new(send) as StreamSend, Box::new(recv) as StreamRecv))
        })
    }

    fn accept_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
        let link = self.clone();
        Box::pin(async move {
            let (send, recv) = link.accept_bi().await.map_err(|error| error.to_string())?;
            Ok((Box::new(send) as StreamSend, Box::new(recv) as StreamRecv))
        })
    }

    fn remote(&self) -> [u8; 32] {
        // A link handed to a workspace's engine names the device at its other
        // end; one built without a device answers a key no device holds, so
        // the workspace's admission refuses it.
        self.remote().unwrap_or_default()
    }

    fn close(&self, code: u32, reason: &[u8]) {
        self.close_with(code, &String::from_utf8_lossy(reason));
    }

    // An iroh link carries the typed refusal its host closed with; a WebTransport session names
    // its reason in words, and none is typed.
    #[cfg(not(target_arch = "wasm32"))]
    fn close_reason(&self) -> Option<Refusal> {
        self.connection().and_then(Transport::close_reason)
    }
}
