//! The relay's server side of the certified-device handshake
//! ([`crate::auth`]), once for every endpoint.
//!
//! Each endpoint names who it admits with an [`EndpointTrust`] and runs
//! [`LiveIdentity::authenticate`] over its stream, or, split across two HTTPS
//! requests, [`LiveIdentity::begin`] and [`PendingServer::finish`]. Anything an
//! endpoint checks beyond that (room membership, the owner gate on compute
//! control) is authorization and stays with the endpoint.

use std::sync::{Arc, PoisonError, RwLock};

use thiserror::Error;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::auth::{
    self, AuthError, AuthenticatedPeer, HandshakeTrust, Hello, HelloAck, HelloAck2, PendingServer,
    ServerIdentity,
};
use crate::frame::{read_frame, write_frame};
use crate::policy::TrustPolicy;

/// Who an endpoint admits at the handshake.
#[derive(Clone, Copy, Debug)]
pub(crate) enum EndpointTrust<'a> {
    /// [`HandshakeTrust::Live`], read from the relay's live policy when the
    /// hello is checked.
    Live(&'a RwLock<TrustPolicy>),
    /// A fixed, named requirement such as [`HandshakeTrust::OwnerDevicesOnly`].
    Fixed(HandshakeTrust<'static>),
}

/// Why a server handshake failed.
#[derive(Debug, Error)]
pub(crate) enum HandshakeError {
    /// The stream failed or a frame exceeded the endpoint's limit.
    #[error("{stage}: {source}")]
    Io {
        /// The step that failed.
        stage: &'static str,
        /// The stream error.
        source: std::io::Error,
    },
    /// A handshake message did not decode or encode.
    #[error("{stage}: {source}")]
    Codec {
        /// The step that failed.
        stage: &'static str,
        /// The codec error.
        source: postcard::Error,
    },
    /// The peer was refused: its certificate, trust or signature.
    #[error("auth reject: {0}")]
    Auth(#[from] AuthError),
}

/// The relay's own identity and its live revocation list, shared by every
/// endpoint and outbound dial.
#[derive(Clone)]
pub(crate) struct LiveIdentity {
    /// The user root this relay chains to.
    pub(crate) user_public_key: [u8; 32],
    pub(crate) cert: DeviceCert,
    pub(crate) signing_key: ed25519_dalek::SigningKey,
    pub(crate) revocations: Arc<RwLock<RevocationList>>,
}

impl LiveIdentity {
    /// First half without I/O: check `hello` under `trust` and the live
    /// revocations, both borrowed only for the check.
    ///
    /// # Errors
    ///
    /// As [`auth::server_begin`].
    pub(crate) fn begin(
        &self,
        trust: EndpointTrust<'_>,
        hello: &Hello,
    ) -> Result<(PendingServer, HelloAck), AuthError> {
        let identity = ServerIdentity {
            own_user: &self.user_public_key,
            cert: &self.cert,
            key: &self.signing_key,
        };
        let now_ns = tilo_clock::unix_now_ns();
        let server_nonce: [u8; 32] = rand::random();
        let revocations = self
            .revocations
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        match trust {
            EndpointTrust::Live(policy) => {
                let policy = policy.read().unwrap_or_else(PoisonError::into_inner);
                auth::server_begin(
                    &identity,
                    HandshakeTrust::Live(&policy),
                    &revocations,
                    hello,
                    now_ns,
                    server_nonce,
                )
            }
            EndpointTrust::Fixed(trust) => {
                auth::server_begin(&identity, trust, &revocations, hello, now_ns, server_nonce)
            }
        }
    }

    /// Run the whole handshake over a stream: read the [`Hello`], then as
    /// [`Self::authenticate_hello`].
    ///
    /// # Errors
    ///
    /// Returns [`HandshakeError`] for a failed stream, a malformed message or
    /// a refused peer.
    pub(crate) async fn authenticate<R, W>(
        &self,
        trust: EndpointTrust<'_>,
        recv: &mut R,
        send: &mut W,
        max_frame: usize,
    ) -> Result<AuthenticatedPeer, HandshakeError>
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send,
    {
        let hello: Hello = decode("decode hello", &read("read hello", recv, max_frame).await?)?;
        self.authenticate_hello(trust, &hello, recv, send, max_frame)
            .await
    }

    /// Finish the handshake for a [`Hello`] the endpoint already read: check
    /// it, send the [`HelloAck`], and verify the client's [`HelloAck2`].
    ///
    /// # Errors
    ///
    /// As [`Self::authenticate`].
    pub(crate) async fn authenticate_hello<R, W>(
        &self,
        trust: EndpointTrust<'_>,
        hello: &Hello,
        recv: &mut R,
        send: &mut W,
        max_frame: usize,
    ) -> Result<AuthenticatedPeer, HandshakeError>
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send,
    {
        let (pending, ack) = self.begin(trust, hello)?;
        let ack = postcard::to_allocvec(&ack).map_err(|source| HandshakeError::Codec {
            stage: "encode ack",
            source,
        })?;
        write_frame(send, &ack)
            .await
            .map_err(|source| HandshakeError::Io {
                stage: "write ack",
                source,
            })?;
        let ack2: HelloAck2 = decode("decode ack2", &read("read ack2", recv, max_frame).await?)?;
        Ok(pending.finish(&ack2)?)
    }
}

async fn read<R: AsyncRead + Unpin + Send>(
    stage: &'static str,
    recv: &mut R,
    max_frame: usize,
) -> Result<Vec<u8>, HandshakeError> {
    read_frame(recv, max_frame)
        .await
        .map_err(|source| HandshakeError::Io { stage, source })
}

fn decode<T: serde::de::DeserializeOwned>(
    stage: &'static str,
    bytes: &[u8],
) -> Result<T, HandshakeError> {
    postcard::from_bytes(bytes).map_err(|source| HandshakeError::Codec { stage, source })
}
