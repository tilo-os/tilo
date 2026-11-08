//! The Tilo `MoQ` carrier preface: how a participant is admitted to a live
//! plane's session before `moq-lite` starts on it.
//!
//! The carrier is generic over its transport ([`Carrier`], `moq-lite`'s poll
//! session interface); a device reaches it over a direct iroh connection
//! ([`crate::native`]). The admission preface, the `moq-lite` session and its
//! SETUP are the same code on every transport.
//!
//! # Admission preface (version 1)
//!
//! Client to host, on the admission stream, exactly once:
//!
//! | Bytes | Field | Value |
//! | --- | --- | --- |
//! | 1 | version | [`PREFACE_VERSION`] |
//! | 2 | length | big-endian `u16`, `1..=`[`MAX_GRANT_BYTES`] |
//! | length | grant | opaque grant bytes issued by the host |
//!
//! The host reads the three header bytes and checks version and length before
//! it allocates anything for the grant, then reads exactly `length` bytes. The
//! whole exchange, from the session opening to the last grant byte, is bounded
//! by [`PREFACE_BUDGET`]. The host never reads further bytes from the client on
//! this stream.
//!
//! Host to client, on the same stream:
//!
//! | Byte | Meaning |
//! | --- | --- |
//! | [`STATUS_ADMITTED`] | Admitted. Origins are attached; start `moq-lite` now. |
//! | [`STATUS_DENIED`] | Refused. The host closes the session with [`CLOSE_DENIED`]. |
//! | [`STATUS_REVOKED`] | Sent after `ADMITTED` when authority lapses. The client closes the session; the host closes it itself after its grace period. |
//!
//! A session whose first bidirectional stream is not a well-formed preface
//! within the budget is closed with [`CLOSE_PREFACE`] and never reaches `MoQ`.
//! The end of the admission stream after `ADMITTED` (host shut down, stream
//! reset) means the same as `REVOKED`.

use std::time::Duration;

use moq_net::web_transport_trait::poll::SendStream as _;

use crate::admit::{closed, read_exact, write_all};

pub(crate) use crate::admit::close;

pub use crate::admit::{
    AdmissionError, Admitted, Carrier, Connected, Dial, Dialling, OriginTask, SETUP_BUDGET, Wire,
    WireStats, origin_config, present, start, version, wire,
};

/// The WebTransport subprotocol and `moq-lite` draft every Tilo `MoQ` route
/// negotiates. No other version is offered or accepted.
pub use crate::preface::PROTOCOL;
pub use crate::preface::{
    CLOSE_DENIED, CLOSE_PREFACE, CLOSE_REVOKED, MAX_GRANT_BYTES, PREFACE_VERSION, STATUS_ADMITTED,
    STATUS_DENIED, STATUS_REVOKED,
};

/// Time from session open to the complete preface.
pub const PREFACE_BUDGET: Duration = Duration::from_secs(5);
pub use crate::preface::{DENIED_GRACE, DENIED_REPORT};

/// Budget for QUIC, TLS and the HTTP/3 CONNECT of one pending session. A
/// satellite path (600 ms RTT) needs about four round trips plus one loss
/// recovery.
pub const HANDSHAKE_BUDGET: Duration = Duration::from_secs(8);
/// Handshakes a standalone listener lets run at once; further connection
/// attempts are refused until one completes or times out.
pub const MAX_PENDING_HANDSHAKES: usize = 16;
/// Upper bound on concurrent QUIC streams per direction on a standalone
/// listener.
pub const MAX_STREAMS: u32 = 32;

/// Why a preface was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrefaceError {
    #[error("admission preface timed out")]
    Timeout,
    #[error("malformed admission preface")]
    Malformed,
    #[error("session closed before the admission preface")]
    Closed,
}

/// Host side of an admission stream, held for the session's lifetime.
pub struct Admission<S: Carrier> {
    send: S::SendStream,
    _recv: S::RecvStream,
}

/// Read the admission preface from a freshly opened session.
///
/// Only the first bidirectional stream is accepted. Version and length are
/// checked before the grant buffer is allocated, and the whole read is bounded
/// by [`PREFACE_BUDGET`].
///
/// # Errors
///
/// Returns [`PrefaceError`] when the preface is late, malformed or the session
/// closed. The caller must then close the session with [`refuse`].
pub async fn read_preface<S: Carrier>(
    session: &S,
) -> Result<(Vec<u8>, Admission<S>), PrefaceError> {
    let mut session = session.clone();
    tokio::time::timeout(PREFACE_BUDGET, async {
        let (send, mut recv) = session
            .accept_bi()
            .await
            .map_err(|_| PrefaceError::Closed)?;
        let mut header = [0u8; 3];
        if !read_exact(&mut recv, &mut header).await {
            return Err(PrefaceError::Malformed);
        }
        let length = crate::preface::grant_length(header).ok_or(PrefaceError::Malformed)?;
        let mut grant = vec![0u8; length];
        if !read_exact(&mut recv, &mut grant).await {
            return Err(PrefaceError::Malformed);
        }
        Ok((grant, Admission { send, _recv: recv }))
    })
    .await
    .map_err(|_| PrefaceError::Timeout)?
}

/// Close a session whose preface was missing, malformed or late, telling the
/// client which in the close reason.
pub fn refuse<S: Carrier>(session: &S, error: PrefaceError) {
    close(session, CLOSE_PREFACE, &error.to_string());
}

impl<S: Carrier> Admission<S> {
    /// Tell the client it is admitted. Call only after origins are attached.
    pub async fn admit(&mut self) -> Result<(), PrefaceError> {
        write_all(&mut self.send, &[STATUS_ADMITTED])
            .await
            .then_some(())
            .ok_or(PrefaceError::Closed)
    }

    /// Refuse the grant and close the session.
    pub async fn deny(mut self, session: &S) {
        let _ = tokio::time::timeout(Duration::from_millis(100), async {
            let _ = write_all(&mut self.send, &[STATUS_DENIED]).await;
            let _ = self.send.finish();
        })
        .await;
        // Some browsers (Firefox) drop a stream's unread bytes when the
        // session closes under it, and report no close code either: the
        // client closes the session once it read the refusal, or the host
        // after a moment.
        let _ = tokio::time::timeout(DENIED_GRACE, closed(session)).await;
        close(session, CLOSE_DENIED, "admission denied");
    }

    /// Tell an admitted client that its authority lapsed. Never blocks for
    /// more than a moment; the caller closes the session after its grace.
    pub async fn revoke(&mut self) {
        let _ = tokio::time::timeout(Duration::from_millis(100), async {
            let _ = write_all(&mut self.send, &[STATUS_REVOKED]).await;
            let _ = self.send.finish();
        })
        .await;
    }
}

/// Host side of one admitted `moq-lite` session.
pub(crate) struct Accepted<S: Carrier> {
    pub session: moq_net::Session,
    pub driver: moq_net::Driver<S>,
    pub admission: Admission<S>,
    pub transport: S,
}

/// Admit `transport` with `admit`, which returns the configured server and the
/// caller's admission state (or `None` to refuse), then run the `moq-lite` SETUP. The client reaches `MoQ`
/// only after `admit` succeeded.
pub(crate) async fn accept<S, F, Fut, T>(
    transport: S,
    admit: F,
) -> Result<(Accepted<S>, T), PrefaceError>
where
    S: Carrier,
    F: FnOnce(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Option<(moq_net::Server, T)>>,
{
    if transport.protocol() != Some(PROTOCOL) {
        refuse(&transport, PrefaceError::Malformed);
        return Err(PrefaceError::Malformed);
    }
    let (grant, mut admission) = match read_preface(&transport).await {
        Ok(preface) => preface,
        Err(error) => {
            refuse(&transport, error);
            return Err(error);
        }
    };
    let Some((server, admitted)) = admit(grant).await else {
        admission.deny(&transport).await;
        return Err(PrefaceError::Closed);
    };
    let Some(version) = version() else {
        admission.deny(&transport).await;
        return Err(PrefaceError::Closed);
    };
    if admission.admit().await.is_err() {
        close(&transport, CLOSE_PREFACE, "admission stream closed");
        return Err(PrefaceError::Closed);
    }
    let accepted = tokio::time::timeout(
        SETUP_BUDGET,
        server
            .with_versions(version.into())
            .accept_lite(moq_net::time::Instant::now(), transport.clone()),
    )
    .await;
    let Ok(Ok((session, driver))) = accepted else {
        close(&transport, CLOSE_PREFACE, "setup failed");
        return Err(PrefaceError::Closed);
    };
    Ok((
        Accepted {
            session,
            driver,
            admission,
            transport,
        },
        admitted,
    ))
}
