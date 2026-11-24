//! The server's server side of the certified-device handshake
//! ([`crate::auth`]), once for every endpoint.
//!
//! Each endpoint names who it admits with an [`EndpointTrust`] and runs
//! [`LiveIdentity::authenticate`] over its stream, or, split across two HTTPS
//! requests, [`LiveIdentity::begin`] and [`PendingServer::finish`]. Anything an
//! endpoint checks beyond that (workspace membership, the owner check on compute
//! control) is authorization and stays with the endpoint.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use thiserror::Error;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Semaphore;

use crate::auth::{
    self, AuthError, AuthenticatedPeer, HandshakeTrust, Hello, HelloAck, HelloAck2, PendingServer,
    ServerIdentity,
};
use crate::frame::{read_frame, write_frame, write_refusal};
use crate::policy::TrustPolicy;
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::entering::{Entered, Entry};

/// The most bytes a pre-authentication frame may declare. The session's
/// 64 MiB limit would let a few hundred idle peers pin the host's memory.
const MAX_HANDSHAKE_FRAME_BYTES: usize = 64 * 1024;
/// One deadline covers reading the hello, writing the ack and verifying ack2.
///
/// A handshake that finds every budget slot taken waits for one inside this
/// deadline instead of being refused: a burst of devices arriving at once
/// queues for its turn, and only one that cannot start before the deadline
/// ends up timing out.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// One shared budget across endpoints, not one allowance per endpoint.
const MAX_PENDING_HANDSHAKES: usize = 256;
static PENDING_HANDSHAKES: Semaphore = Semaphore::const_new(MAX_PENDING_HANDSHAKES);
/// A future an entry awaits.
pub type EntryFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Where an entering device comes in: the host's workspaces, as their entry
/// and join answer it.
pub trait Admit: Send + Sync {
    /// Let `cert`'s person in through the entry `entry` names and have the
    /// workspace admit the device, which proved it holds its key on the
    /// connection carrying the entry; and once admitted, hand the
    /// workspace's channel key holders the device's request for the keys of
    /// its group. An entry the workspace already admitted (its answer was
    /// lost with an earlier connection) is answered again.
    fn enter<'a>(&'a self, cert: &'a DeviceCert, entry: Entry) -> EntryFuture<'a, Entered>;
}

#[derive(Clone, Copy, Debug)]
pub enum EndpointTrust<'a> {
    /// [`HandshakeTrust::Live`], read from the server's live policy when the
    /// hello is checked.
    Live(&'a RwLock<TrustPolicy>),
    /// A fixed, named requirement such as [`HandshakeTrust::OwnerDevicesOnly`].
    Fixed(HandshakeTrust<'static>),
}

/// Why a server handshake failed.
#[derive(Debug, Error)]
pub enum HandshakeError {
    /// The stream failed, a frame exceeded the endpoint's limit, the
    /// pre-authentication queue was closed or the deadline elapsed
    /// (`TimedOut`).
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
    /// The peer speaks another peer protocol version, `theirs`. It was told
    /// this server's ([`auth::version_answer`]) and nothing else was read.
    #[error(
        "peer speaks protocol {theirs}, this server {}",
        auth::PEER_PROTOCOL_VERSION
    )]
    Version {
        /// The peer's version.
        theirs: u32,
    },
    /// The hello asked to enter a workspace at an endpoint that admits no
    /// one ([`crate::entering`]): only the session endpoint does.
    #[error("this endpoint lets no one enter a workspace")]
    NotEntering,
}

/// The server's own identity and its live revocation list, shared by every
/// endpoint and outbound dial.
#[derive(Clone)]
pub struct LiveIdentity {
    /// The user root this server chains to.
    pub user_public_key: [u8; 32],
    pub cert: DeviceCert,
    pub signing_key: ed25519_dalek::SigningKey,
    pub revocations: Arc<RwLock<RevocationList>>,
}

impl LiveIdentity {
    /// First half without I/O: check `hello` under `trust` and the live
    /// revocations, both borrowed only for the check.
    ///
    /// # Errors
    ///
    /// As [`auth::server_begin`].
    pub fn begin(
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
        let revocations = self.revocations.read_recover();
        match trust {
            EndpointTrust::Live(policy) => {
                let policy = policy.read_recover();
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

    /// What answers `hello`, which [`Self::begin`] refused with `error` under
    /// `trust`, where this server says why ([`auth::HelloRefusal::said_of`]).
    fn refusal(
        &self,
        trust: EndpointTrust<'_>,
        hello: &Hello,
        error: &AuthError,
    ) -> Option<auth::HelloRefused> {
        let identity = ServerIdentity {
            own_user: &self.user_public_key,
            cert: &self.cert,
            key: &self.signing_key,
        };
        let revocations = self.revocations.read_recover().clone();
        match trust {
            EndpointTrust::Live(policy) => {
                let policy = policy.read_recover();
                auth::server_refusal(
                    &identity,
                    HandshakeTrust::Live(&policy),
                    &revocations,
                    hello,
                    error,
                )
            }
            EndpointTrust::Fixed(trust) => {
                auth::server_refusal(&identity, trust, &revocations, hello, error)
            }
        }
    }

    /// Run the whole handshake over a stream: read the [`Hello`], then as
    /// [`Self::authenticate_hello`].
    ///
    /// # Errors
    ///
    /// Returns [`HandshakeError`] for a failed stream, a malformed message,
    /// a refused peer, an exhausted pending-handshake budget or a timeout.
    /// Frames are limited to the smaller of `max_frame` and 64 KiB, and the
    /// whole exchange has one 10-second deadline.
    pub async fn authenticate<R, W>(
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
        authenticate_bounded(
            self,
            trust,
            None,
            recv,
            send,
            max_frame,
            &PENDING_HANDSHAKES,
        )
        .await
    }

    /// Finish the handshake for a [`Hello`] the endpoint already read: check
    /// it, send the [`HelloAck`], and verify the client's [`HelloAck2`].
    ///
    /// # Errors
    ///
    /// As [`Self::authenticate`].
    pub async fn authenticate_hello<R, W>(
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
        authenticate_bounded(
            self,
            trust,
            Some(hello),
            recv,
            send,
            max_frame,
            &PENDING_HANDSHAKES,
        )
        .await
    }
}

/// How long a device that was told why its hello is refused has to read it.
const REFUSAL_READ: Duration = Duration::from_secs(5);

/// Say why this server refused `hello` with `error` under `trust`, where it
/// says so ([`LiveIdentity::refusal`]): to a device its person removed, that
/// it was removed. `true` once that is written; the stream is then the
/// device's to read ([`refusal_read`]). A device that reads none of it has
/// what it had, a refused hello.
async fn say_refusal<W: AsyncWrite + Unpin + Send>(
    identity: &LiveIdentity,
    trust: EndpointTrust<'_>,
    hello: &Hello,
    error: &AuthError,
    send: &mut W,
) -> bool {
    let Some(refused) = identity.refusal(trust, hello, error) else {
        return false;
    };
    let Ok(refused) = postcard::to_allocvec(&refused) else {
        return false;
    };
    // The hello stays refused whether this reaches the device or not.
    write_refusal(send, &refused).await.is_ok()
}

/// Hold the stream of a device that was just told why its hello is refused
/// ([`say_refusal`]): finished, until the device lets go of its end or
/// [`REFUSAL_READ`] has passed. Whoever ran the handshake closes the
/// connection next, and a connection closed at once drops what is still on
/// its way to the device. The handshake is over by then, so this waits
/// outside its deadline and holds no place in its budget.
async fn refusal_read<R, W>(recv: &mut R, send: &mut W)
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let _ = send.shutdown().await;
    let let_go = async {
        let mut unread = [0_u8; 256];
        while matches!(recv.read(&mut unread).await, Ok(read) if read > 0) {}
    };
    let _ = n0_future::time::timeout(REFUSAL_READ, let_go).await;
}

fn queue_closed() -> HandshakeError {
    HandshakeError::Io {
        stage: "handshake",
        source: std::io::Error::other("the handshake queue closed"),
    }
}

// Both entry points use this path: one permit and one timeout, never an
// authenticate -> authenticate_hello nesting. Passing the budget explicitly
// also lets tests exercise exhaustion without racing other endpoints' tests.
async fn authenticate_bounded<R, W>(
    identity: &LiveIdentity,
    trust: EndpointTrust<'_>,
    hello: Option<&Hello>,
    recv: &mut R,
    send: &mut W,
    max_frame: usize,
    budget: &Semaphore,
) -> Result<AuthenticatedPeer, HandshakeError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let max_frame = max_frame.min(MAX_HANDSHAKE_FRAME_BYTES);
    let mut said = false;
    let authenticated = n0_future::time::timeout(HANDSHAKE_DEADLINE, async {
        // A burst past the budget queues for a slot instead of being refused;
        // the deadline bounds the wait and the exchange together. Errors,
        // timeouts and cancellation release the slot too, through RAII.
        let _permit = budget.acquire().await.map_err(|_| queue_closed())?;
        let received;
        let hello = if let Some(hello) = hello {
            hello
        } else {
            received = decode_hello(&read("read hello", recv, max_frame).await?, send).await?;
            &received
        };
        if hello.entering.is_some() {
            return Err(HandshakeError::NotEntering);
        }
        let (pending, ack) = match identity.begin(trust, hello) {
            Ok(begun) => begun,
            Err(error) => {
                said = say_refusal(identity, trust, hello, &error, send).await;
                return Err(error.into());
            }
        };
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
    })
    .await;
    if said {
        refusal_read(recv, send).await;
    }
    authenticated.map_err(|_| HandshakeError::Io {
        stage: "handshake",
        source: std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timed out"),
    })?
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

/// The client's hello; at another version, this server's version is the
/// answer and the handshake ends.
async fn decode_hello<W: AsyncWrite + Unpin + Send>(
    bytes: &[u8],
    send: &mut W,
) -> Result<Hello, HandshakeError> {
    match auth::decode_handshake::<Hello>(bytes) {
        Ok(hello) => Ok(hello),
        Err(auth::HandshakeFrameError::Version { theirs }) => {
            let answer = auth::version_answer().map_err(|source| HandshakeError::Codec {
                stage: "encode version answer",
                source,
            })?;
            // The peer learns this server's version even if the write fails;
            // there is nothing more to tell it either way.
            let _ = write_frame(send, &answer).await;
            tracing::info!(
                theirs,
                ours = auth::PEER_PROTOCOL_VERSION,
                "a peer at another protocol version was told this server's"
            );
            Err(HandshakeError::Version { theirs })
        }
        Err(auth::HandshakeFrameError::Codec(source)) => Err(HandshakeError::Codec {
            stage: "decode hello",
            source,
        }),
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    stage: &'static str,
    bytes: &[u8],
) -> Result<T, HandshakeError> {
    postcard::from_bytes(bytes).map_err(|source| HandshakeError::Codec { stage, source })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::task::Poll;
    use tilo_user_identity::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};
    use tokio::io::{AsyncWriteExt, DuplexStream, split};

    use crate::transport::MAX_FRAME_BYTES;

    fn identity(label: &str) -> Result<LiveIdentity, tilo_user_identity::DeviceCertError> {
        let (root, user) = UserIdentity::generate("Alice", 1);
        let (signing_key, device) = DeviceIdentity::generate(label, 100);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: device.public_key,
            device_label: device.label,
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root)?;
        Ok(LiveIdentity {
            user_public_key: user.public_key,
            cert,
            signing_key,
            revocations: Arc::new(RwLock::new(RevocationList::new())),
        })
    }

    fn hello(identity: &LiveIdentity) -> Hello {
        Hello::new(identity.cert.clone(), [1; 32])
    }

    fn trust() -> EndpointTrust<'static> {
        EndpointTrust::Fixed(HandshakeTrust::OwnerDevicesOnly)
    }

    // Drive an exchange exactly to its next stream wait, without spawning or
    // depending on the runtime's scheduling of other tests.
    async fn poll_pending<F: Future>(mut future: Pin<&mut F>) {
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    async fn oversized_prefix(client: &mut DuplexStream, limit: usize) -> std::io::Result<()> {
        let declared = u32::try_from(limit.saturating_add(1)).map_err(std::io::Error::other)?;
        // No payload or EOF: refusal must happen from the prefix alone.
        client.write_all(&declared.to_le_bytes()).await
    }

    fn assert_io(error: &HandshakeError, expected_stage: &str, kind: std::io::ErrorKind) {
        assert!(
            matches!(error, HandshakeError::Io { stage, source }
                if *stage == expected_stage && source.kind() == kind),
            "expected {expected_stage}: {kind:?}, got {error}",
        );
    }

    async fn answer_challenge(
        identity: &LiveIdentity,
        client: &mut DuplexStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ack: HelloAck =
            postcard::from_bytes(&read_frame(client, MAX_HANDSHAKE_FRAME_BYTES).await?)?;
        let ack2 = auth::build_hello_ack2(&identity.signing_key, &ack.server_nonce);
        write_frame(client, &postcard::to_allocvec(&ack2)?).await?;
        Ok(())
    }

    #[test]
    fn a_full_hello_fits_the_unauthenticated_frame_bound() {
        // A certificate with an outsized label must still fit under the cap.
        let identity = identity(&"label ".repeat(200)).unwrap();
        let bytes = postcard::to_allocvec(&hello(&identity)).unwrap();
        assert!(
            bytes.len() < MAX_HANDSHAKE_FRAME_BYTES,
            "a {}-byte hello must fit under the {MAX_HANDSHAKE_FRAME_BYTES}-byte handshake bound",
            bytes.len(),
        );
        const { assert!(MAX_HANDSHAKE_FRAME_BYTES < MAX_FRAME_BYTES) };
    }

    #[tokio::test(start_paused = true)]
    async fn oversized_hello_is_refused_before_reading_a_payload() {
        let identity = identity("host").unwrap();
        let budget = Semaphore::new(1);
        // Both the session limit and a smaller endpoint limit are enforced.
        for endpoint_limit in [MAX_FRAME_BYTES, 16 * 1024] {
            let (server, mut client) = tokio::io::duplex(4096);
            let (mut recv, mut send) = split(server);
            oversized_prefix(&mut client, endpoint_limit.min(MAX_HANDSHAKE_FRAME_BYTES))
                .await
                .unwrap();
            let error = authenticate_bounded(
                &identity,
                trust(),
                None,
                &mut recv,
                &mut send,
                endpoint_limit,
                &budget,
            )
            .await
            .unwrap_err();
            assert_io(&error, "read hello", std::io::ErrorKind::InvalidData);
            assert_eq!(budget.available_permits(), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn oversized_ack2_is_refused_for_both_entry_points() {
        let identity = identity("host").unwrap();
        let hello = hello(&identity);
        let budget = Semaphore::new(1);
        for supplied in [false, true] {
            for endpoint_limit in [MAX_FRAME_BYTES, 16 * 1024] {
                let (server, mut client) = tokio::io::duplex(4096);
                let (mut recv, mut send) = split(server);
                if !supplied {
                    write_frame(&mut client, &postcard::to_allocvec(&hello).unwrap())
                        .await
                        .unwrap();
                }
                let mut handshake = Box::pin(authenticate_bounded(
                    &identity,
                    trust(),
                    supplied.then_some(&hello),
                    &mut recv,
                    &mut send,
                    endpoint_limit,
                    &budget,
                ));
                poll_pending(handshake.as_mut()).await;
                read_frame(&mut client, MAX_HANDSHAKE_FRAME_BYTES)
                    .await
                    .unwrap();
                oversized_prefix(&mut client, endpoint_limit.min(MAX_HANDSHAKE_FRAME_BYTES))
                    .await
                    .unwrap();
                assert_io(
                    &handshake.await.unwrap_err(),
                    "read ack2",
                    std::io::ErrorKind::InvalidData,
                );
                assert_eq!(budget.available_permits(), 1);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn idle_handshakes_time_out_and_release_their_permits() {
        let identity = identity("host").unwrap();
        let hello = hello(&identity);
        let budget = Semaphore::new(1);
        for supplied in [false, true] {
            // With a supplied hello, also cover a stalled ack write (capacity
            // one) as well as an idle ack2 read (capacity 4096).
            for capacity in [1, 4096] {
                let (server, _client) = tokio::io::duplex(capacity);
                let (mut recv, mut send) = split(server);
                let mut handshake = Box::pin(authenticate_bounded(
                    &identity,
                    trust(),
                    supplied.then_some(&hello),
                    &mut recv,
                    &mut send,
                    MAX_FRAME_BYTES,
                    &budget,
                ));
                poll_pending(handshake.as_mut()).await;
                assert_eq!(budget.available_permits(), 0);
                tokio::time::advance(
                    HANDSHAKE_DEADLINE
                        .checked_sub(Duration::from_nanos(1))
                        .unwrap(),
                )
                .await;
                poll_pending(handshake.as_mut()).await;
                tokio::time::advance(Duration::from_nanos(1)).await;
                assert_io(
                    &handshake.await.unwrap_err(),
                    "handshake",
                    std::io::ErrorKind::TimedOut,
                );
                assert_eq!(budget.available_permits(), 1);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hello_and_ack2_share_one_deadline() {
        let identity = identity("host").unwrap();
        let hello = hello(&identity);
        let budget = Semaphore::new(1);
        let (server, mut client) = tokio::io::duplex(4096);
        let (mut recv, mut send) = split(server);
        let mut handshake = Box::pin(authenticate_bounded(
            &identity,
            trust(),
            None,
            &mut recv,
            &mut send,
            MAX_FRAME_BYTES,
            &budget,
        ));
        poll_pending(handshake.as_mut()).await;
        tokio::time::advance(Duration::from_secs(6)).await;
        write_frame(&mut client, &postcard::to_allocvec(&hello).unwrap())
            .await
            .unwrap();
        poll_pending(handshake.as_mut()).await;
        read_frame(&mut client, MAX_HANDSHAKE_FRAME_BYTES)
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_io(
            &handshake.await.unwrap_err(),
            "handshake",
            std::io::ErrorKind::TimedOut,
        );
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn pending_budget_queues_a_burst_and_releases_on_cancellation_and_success() {
        let identity = identity("host").unwrap();
        let hello = hello(&identity);
        // Local budgets isolate this test from unrelated server tests.
        let budget = Semaphore::new(MAX_PENDING_HANDSHAKES);
        let mut held: Vec<_> = (0..MAX_PENDING_HANDSHAKES)
            .map(|_| budget.try_acquire().unwrap())
            .collect();
        assert_eq!(budget.available_permits(), 0);

        // A handshake arriving while every slot is taken waits for one: it
        // neither fails nor starts reading until a slot frees.
        let (server, mut client) = tokio::io::duplex(4096);
        let (mut recv, mut send) = split(server);
        write_frame(&mut client, &postcard::to_allocvec(&hello).unwrap())
            .await
            .unwrap();
        let mut queued = Box::pin(authenticate_bounded(
            &identity,
            trust(),
            None,
            &mut recv,
            &mut send,
            MAX_FRAME_BYTES,
            &budget,
        ));
        poll_pending(queued.as_mut()).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        poll_pending(queued.as_mut()).await;
        assert_eq!(budget.available_permits(), 0);

        // Cancelling one held exchange hands its slot to the queue, which
        // then completes its handshake.
        drop(held.pop().unwrap());
        let (result, ()) = tokio::join!(queued, async {
            answer_challenge(&identity, &mut client).await.unwrap();
        });
        assert_eq!(
            result.unwrap().device_public_key,
            identity.cert.device_public_key
        );
        // A completed handshake frees its slot too.
        assert_eq!(budget.available_permits(), 1);
        drop(held);
        assert_eq!(budget.available_permits(), MAX_PENDING_HANDSHAKES);
    }

    #[tokio::test]
    async fn a_burst_past_the_budget_queues_and_every_handshake_completes() {
        let identity = identity("host").unwrap();
        let hello_bytes = postcard::to_allocvec(&hello(&identity)).unwrap();
        let budget = Arc::new(Semaphore::new(1));
        // The first handshake holds the only slot, waiting for its client.
        let (server, mut holding) = tokio::io::duplex(4096);
        let (mut hold_recv, mut hold_send) = split(server);
        let (hold_identity, hold_budget) = (identity.clone(), Arc::clone(&budget));
        let holding_task = tokio::spawn(async move {
            authenticate_bounded(
                &hold_identity,
                trust(),
                None,
                &mut hold_recv,
                &mut hold_send,
                MAX_FRAME_BYTES,
                &hold_budget,
            )
            .await
        });
        // Wait until it holds the slot.
        for _ in 0..1000 {
            tokio::task::yield_now().await;
            if budget.available_permits() == 0 {
                break;
            }
        }
        assert_eq!(budget.available_permits(), 0);
        // A second handshake finds the slot taken and queues for it.
        let (server, mut queued_client) = tokio::io::duplex(4096);
        let (mut queued_recv, mut queued_send) = split(server);
        write_frame(&mut queued_client, &hello_bytes).await.unwrap();
        let (queued_identity, queued_budget) = (identity.clone(), Arc::clone(&budget));
        let queued_task = tokio::spawn(async move {
            authenticate_bounded(
                &queued_identity,
                trust(),
                None,
                &mut queued_recv,
                &mut queued_send,
                MAX_FRAME_BYTES,
                &queued_budget,
            )
            .await
        });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!holding_task.is_finished());
        assert!(
            !queued_task.is_finished(),
            "a burst queues for a slot, it does not fail"
        );
        assert_eq!(budget.available_permits(), 0);
        // Completing the first handshake frees the slot, and the queued one
        // completes in its turn.
        write_frame(&mut holding, &hello_bytes).await.unwrap();
        let (first, ()) = tokio::join!(holding_task, async {
            answer_challenge(&identity, &mut holding).await.unwrap();
        });
        assert_eq!(
            first.unwrap().unwrap().device_public_key,
            identity.cert.device_public_key
        );
        let (second, ()) = tokio::join!(queued_task, async {
            answer_challenge(&identity, &mut queued_client)
                .await
                .unwrap();
        });
        assert_eq!(
            second.unwrap().unwrap().device_public_key,
            identity.cert.device_public_key
        );
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hello_asking_to_enter_is_refused_where_no_one_enters() {
        let identity = identity("host").unwrap();
        let budget = Semaphore::new(1);
        let entering = Hello::entering(
            identity.cert.clone(),
            [1; 32],
            Entry {
                entering: tilo_server_protocol::entering::Entering {
                    workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".to_owned(),
                    name: "Stranger".to_owned(),
                    request_id: [2; 32],
                    through: tilo_server_protocol::entering::Through::Entry,
                },
                access: None,
                proof: [0; 64],
            },
        );
        for supplied in [false, true] {
            let (server, mut client) = tokio::io::duplex(4096);
            let (mut recv, mut send) = split(server);
            if !supplied {
                write_frame(&mut client, &postcard::to_allocvec(&entering).unwrap())
                    .await
                    .unwrap();
            }
            assert!(matches!(
                authenticate_bounded(
                    &identity,
                    trust(),
                    supplied.then_some(&entering),
                    &mut recv,
                    &mut send,
                    MAX_FRAME_BYTES,
                    &budget,
                )
                .await,
                Err(HandshakeError::NotEntering)
            ));
            assert_eq!(budget.available_permits(), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_and_untrusted_hellos_release_the_permit() {
        let identity = identity("host").unwrap();
        let budget = Semaphore::new(1);
        let (server, mut client) = tokio::io::duplex(4096);
        let (mut recv, mut send) = split(server);
        write_frame(&mut client, &[255]).await.unwrap();
        assert!(matches!(
            authenticate_bounded(
                &identity,
                trust(),
                None,
                &mut recv,
                &mut send,
                MAX_FRAME_BYTES,
                &budget,
            )
            .await,
            Err(HandshakeError::Codec {
                stage: "decode hello",
                ..
            })
        ));
        assert_eq!(budget.available_permits(), 1);

        let mut untrusted = hello(&identity);
        untrusted.cert.signature = [0; 64];
        assert!(matches!(
            authenticate_bounded(
                &identity,
                trust(),
                Some(&untrusted),
                &mut recv,
                &mut send,
                MAX_FRAME_BYTES,
                &budget,
            )
            .await,
            Err(HandshakeError::Auth(_))
        ));
        assert_eq!(budget.available_permits(), 1);
    }

    /// What the tests of a refusal that says why share. They stand in a module of their own
    /// because the lints take only a plain `#[cfg(test)]` module for a test's.
    #[cfg(test)]
    mod removed {
        use super::*;

        /// A device of the person `root` is, named `label`: its key and certificate.
        pub(super) fn device_of(
            root: &ed25519_dalek::SigningKey,
            label: &str,
        ) -> (ed25519_dalek::SigningKey, DeviceCert) {
            let (signing_key, device) = DeviceIdentity::generate(label, 100);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.public_key,
                device_label: device.label,
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 200,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, root).unwrap();
            (signing_key, cert)
        }

        /// `root`'s removal of `device`, signed.
        pub(super) fn removal(
            root: &ed25519_dalek::SigningKey,
            device: &DeviceCert,
        ) -> tilo_user_identity::RevocationEntry {
            let mut removal = tilo_user_identity::RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: device.device_public_key,
                revoked_at_ns: 500,
                reason: tilo_user_identity::RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut removal, root).unwrap();
            removal
        }

        /// A person's server, and one more device of theirs which the person removed: the server's
        /// own list holds the removal, signed by the person's root.
        pub(super) fn server_and_removed_device()
        -> (LiveIdentity, DeviceCert, ed25519_dalek::SigningKey) {
            let (root, user) = UserIdentity::generate("Ada", 1);
            let (signing_key, cert) = device_of(&root, "server");
            let (removed_key, removed) = device_of(&root, "browser");
            let mut revocations = RevocationList::new();
            revocations.insert(removal(&root, &removed)).unwrap();
            let identity = LiveIdentity {
                user_public_key: user.public_key,
                cert,
                signing_key,
                revocations: Arc::new(RwLock::new(revocations)),
            };
            (identity, removed, removed_key)
        }

        /// Everything a device reads of `client` once its server let go of the stream.
        pub(super) async fn heard(mut client: DuplexStream) -> Vec<u8> {
            let mut said = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut client, &mut said)
                .await
                .unwrap();
            said
        }

        /// The refusal in `said`, as a device built since refusals said why reads it; nothing may
        /// follow it.
        pub(super) async fn refusal_in(said: Vec<u8>) -> auth::HelloRefused {
            use crate::frame::{Answer, read_answer};
            let mut said = std::io::Cursor::new(said);
            let Some(Answer::Refusal(frame)) =
                read_answer(&mut said, MAX_FRAME_BYTES).await.unwrap()
            else {
                panic!("the server said no refusal");
            };
            assert_eq!(
                read_answer(&mut said, MAX_FRAME_BYTES).await.unwrap(),
                None,
                "the server said more than its refusal"
            );
            auth::decode_handshake(&frame).unwrap()
        }

        /// What `hello` is answered at an endpoint that asks `trust`: how the handshake ended, and
        /// every byte the device read.
        pub(super) async fn answered(
            identity: &LiveIdentity,
            trust: EndpointTrust<'_>,
            hello: &Hello,
        ) -> (HandshakeError, Vec<u8>) {
            let budget = Semaphore::new(1);
            let (server, mut client) = tokio::io::duplex(4096);
            let (mut recv, mut send) = split(server);
            write_frame(&mut client, &postcard::to_allocvec(hello).unwrap())
                .await
                .unwrap();
            let error = authenticate_bounded(
                identity,
                trust,
                None,
                &mut recv,
                &mut send,
                MAX_FRAME_BYTES,
                &budget,
            )
            .await
            .unwrap_err();
            assert_eq!(budget.available_permits(), 1);
            drop((recv, send));
            (error, heard(client).await)
        }
    }

    use removed::{answered, device_of, refusal_in, removal, server_and_removed_device};

    #[tokio::test(start_paused = true)]
    async fn a_removed_device_is_told_so_before_its_hello_is_closed() {
        let (identity, removed, _removed_key) = server_and_removed_device();
        let (error, said) = answered(&identity, trust(), &Hello::new(removed, [1; 32])).await;
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::Revoked)),
            "{error}"
        );
        assert!(
            !said.is_empty(),
            "the server closed a removed device's hello without saying why"
        );

        // A device built before refusals said why reads frames one way: it refuses the first,
        // and so reads what it read before, a hello nobody answered.
        let mut before = std::io::Cursor::new(said.clone());
        assert!(
            crate::frame::read_frame_or_eof(&mut before, MAX_FRAME_BYTES)
                .await
                .is_err()
        );

        // A device built since reads why, signed by this server for this very hello: it
        // believes the refusal where it would have believed the ack.
        let refused = refusal_in(said).await;
        assert_eq!(refused.refusal, auth::HelloRefusal::DeviceRevoked);
        let checked = |nonce: [u8; 32]| {
            auth::verify_hello_ack(
                &refused,
                &identity.user_public_key,
                &RevocationList::new(),
                tilo_clock::unix_now_ns(),
                &nonce,
            )
        };
        assert_eq!(
            checked([1; 32]).unwrap().device_public_key,
            identity.cert.device_public_key
        );
        assert!(matches!(checked([2; 32]), Err(AuthError::BadChallengeSig)));
        // It names the server and the reason, and nothing of the person's other devices or of
        // whoever removed this one.
        assert_eq!(refused.cert, identity.cert);
        assert_eq!(refused.protocol, auth::PEER_PROTOCOL_VERSION);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hello_refused_for_anything_but_its_certificate_is_closed_with_nothing_said() {
        let (identity, removed, _removed_key) = server_and_removed_device();
        // A stranger: a device of a person this endpoint does not admit.
        let (stranger_root, _) = UserIdentity::generate("Bob", 1);
        let (_, stranger) = device_of(&stranger_root, "laptop");
        let (error, said) =
            answered(&identity, trust(), &Hello::new(stranger.clone(), [1; 32])).await;
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::PolicyReject)),
            "{error}"
        );
        assert!(said.is_empty(), "a stranger was told something");

        // A stranger's device that its own person removed, which this server knows: it hears
        // what that person's other devices hear here, nothing.
        identity
            .revocations
            .write()
            .unwrap()
            .insert(removal(&stranger_root, &stranger))
            .unwrap();
        let (error, said) =
            answered(&identity, trust(), &Hello::new(stranger.clone(), [1; 32])).await;
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::Revoked)),
            "{error}"
        );
        assert!(said.is_empty(), "a stranger learned of a removal");
        // Where its person is admitted, the removal alone refuses it, and it is told.
        let admitting = RwLock::new(TrustPolicy::AllowList {
            users: vec![hex::encode(stranger.user_public_key)],
        });
        let (error, said) = answered(
            &identity,
            EndpointTrust::Live(&admitting),
            &Hello::new(stranger, [1; 32]),
        )
        .await;
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::Revoked)),
            "{error}"
        );
        assert_eq!(
            refusal_in(said).await.refusal,
            auth::HelloRefusal::DeviceRevoked
        );

        // A certificate that does not verify is refused before any removal is looked at, and its
        // device is told: the certificate is the device's own, and the answer says nothing of the
        // people this server serves.
        let mut forged = removed;
        forged.signature = [0; 64];
        let (error, said) = answered(&identity, trust(), &Hello::new(forged, [1; 32])).await;
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::Cert(_))),
            "{error}"
        );
        assert_eq!(
            refusal_in(said).await.refusal,
            auth::HelloRefusal::CertificateRejected
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_that_says_why_is_held_until_its_device_let_go_of_the_stream() {
        let (identity, removed, _removed_key) = server_and_removed_device();
        let hello = postcard::to_allocvec(&Hello::new(removed, [1; 32])).unwrap();
        let budget = Semaphore::new(1);

        // A device that reads the refusal and lets go: its server lets go at once.
        let (server, mut client) = tokio::io::duplex(4096);
        let (mut recv, mut send) = split(server);
        write_frame(&mut client, &hello).await.unwrap();
        let began = tokio::time::Instant::now();
        let mut handshake = Box::pin(authenticate_bounded(
            &identity,
            trust(),
            None,
            &mut recv,
            &mut send,
            MAX_FRAME_BYTES,
            &budget,
        ));
        poll_pending(handshake.as_mut()).await;
        // The refusal is written and the handshake over, its place in the budget given back,
        // while the stream is still the device's to read.
        assert_eq!(budget.available_permits(), 1);
        let Some(crate::frame::Answer::Refusal(_)) =
            crate::frame::read_answer(&mut client, MAX_FRAME_BYTES)
                .await
                .unwrap()
        else {
            panic!("the server said no refusal");
        };
        poll_pending(handshake.as_mut()).await;
        client.shutdown().await.unwrap();
        let error = handshake.await.unwrap_err();
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::Revoked)),
            "{error}"
        );
        assert!(
            began.elapsed() < Duration::from_millis(100),
            "it waited {:?} for a device that had let go",
            began.elapsed()
        );

        // A device that never lets go is held no longer than the refusal is given to be read.
        let (server, mut client) = tokio::io::duplex(4096);
        let (mut recv, mut send) = split(server);
        write_frame(&mut client, &hello).await.unwrap();
        let began = tokio::time::Instant::now();
        let error = authenticate_bounded(
            &identity,
            trust(),
            None,
            &mut recv,
            &mut send,
            MAX_FRAME_BYTES,
            &budget,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, HandshakeError::Auth(AuthError::Revoked)),
            "{error}"
        );
        let held = began.elapsed();
        assert!(
            held >= REFUSAL_READ && held < REFUSAL_READ + Duration::from_millis(100),
            "held for {held:?}"
        );
        assert_eq!(budget.available_permits(), 1);
    }
}
