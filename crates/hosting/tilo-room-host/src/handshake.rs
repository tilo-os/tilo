//! The relay's server side of the certified-device handshake
//! ([`crate::auth`]), once for every endpoint.
//!
//! Each endpoint names who it admits with an [`EndpointTrust`] and runs
//! [`LiveIdentity::authenticate`] over its stream, or, split across two HTTPS
//! requests, [`LiveIdentity::begin`] and [`PendingServer::finish`]. Anything an
//! endpoint checks beyond that (workspace membership, the owner gate on compute
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
use crate::frame::{read_frame, write_frame};
use crate::policy::TrustPolicy;
use tilo_lock::RwLockRecover as _;
use tilo_relay_protocol::entering::{
    ENTERING_DEADLINE, EnterProof, EnterRefusal, EnterStep, Entered, Entering, MAX_ENTERING,
};

/// The most bytes a pre-authentication frame may declare. The session's
/// 64 MiB limit would let a few hundred idle peers pin the host's memory.
const MAX_HANDSHAKE_FRAME_BYTES: usize = 64 * 1024;
/// One deadline covers reading the hello, writing the ack and verifying ack2.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// One shared budget across endpoints, not one allowance per endpoint.
const MAX_PENDING_HANDSHAKES: usize = 256;
static PENDING_HANDSHAKES: Semaphore = Semaphore::const_new(MAX_PENDING_HANDSHAKES);
/// Entries running at once, beside the pending-handshake budget they also
/// hold a place in ([`MAX_ENTERING`]).
static ENTERING: Semaphore = Semaphore::const_new(MAX_ENTERING);

/// A future an entry awaits.
pub type EntryFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Where an entering device comes in: the host's workspaces, as their door
/// and join answer it.
pub trait Doorway: Send + Sync {
    /// Let `cert`'s person in through the door `entering` names and
    /// challenge the device for the invitation the door issued.
    fn ask<'a>(
        &'a self,
        cert: &'a DeviceCert,
        entering: &'a Entering,
    ) -> EntryFuture<'a, Result<Asked, EnterRefusal>>;
    /// Admit the device that signed `challenge` with `signature` under the
    /// door's `token`, shown to the workspace's people as `name`.
    fn prove<'a>(
        &'a self,
        challenge: Vec<u8>,
        token: String,
        signature: [u8; 64],
        name: &'a str,
    ) -> EntryFuture<'a, Entered>;
}

/// What a door answered an entering device.
pub enum Asked {
    /// The workspace's challenge, which goes to the device, and the door's
    /// one-time token its admission is redeemed with, which stays here.
    Challenge {
        /// The join challenge the device signs.
        challenge: Vec<u8>,
        /// The door's one-time token for the invitation it issued.
        token: String,
    },
    /// The workspace already admitted the device under this entry: its
    /// answer was lost with an earlier connection.
    In(Entered),
}

#[derive(Clone, Copy, Debug)]
pub enum EndpointTrust<'a> {
    /// [`HandshakeTrust::Live`], read from the relay's live policy when the
    /// hello is checked.
    Live(&'a RwLock<TrustPolicy>),
    /// A fixed, named requirement such as [`HandshakeTrust::OwnerDevicesOnly`].
    Fixed(HandshakeTrust<'static>),
}

/// Why a server handshake failed.
#[derive(Debug, Error)]
pub enum HandshakeError {
    /// The stream failed, a frame exceeded the endpoint's limit, the pending
    /// budget was exhausted (`WouldBlock`) or the deadline elapsed (`TimedOut`).
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
    /// this relay's ([`auth::version_answer`]) and nothing else was read.
    #[error(
        "peer speaks protocol {theirs}, this relay {}",
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
    /// The door or its workspace did not let the entering device in; it was
    /// told why.
    #[error("entry refused: {}", .0.reason())]
    Refused(EnterRefusal),
}

/// The relay's own identity and its live revocation list, shared by every
/// endpoint and outbound dial.
#[derive(Clone)]
pub struct LiveIdentity {
    /// The user root this relay chains to.
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

    /// The session endpoint's handshake: as [`Self::authenticate`] under
    /// `trust`, or, for a hello that asks to enter a workspace, the entry
    /// through `doorway` ([`crate::entering`]). Resolves to the peer and,
    /// for an entry, what the workspace answered it.
    ///
    /// # Errors
    ///
    /// As [`Self::authenticate`]; [`HandshakeError::Refused`] when the door
    /// refused the entering device, which was told so.
    pub async fn authenticate_or_enter<R, W>(
        &self,
        trust: EndpointTrust<'_>,
        recv: &mut R,
        send: &mut W,
        max_frame: usize,
        doorway: Option<&dyn Doorway>,
    ) -> Result<(AuthenticatedPeer, Option<Entered>), HandshakeError>
    where
        R: AsyncRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send,
    {
        let permit = PENDING_HANDSHAKES.try_acquire().map_err(|_| busy())?;
        let max_frame = max_frame.min(MAX_HANDSHAKE_FRAME_BYTES);
        let hello = n0_future::time::timeout(HANDSHAKE_DEADLINE, async {
            decode_hello(&read("read hello", recv, max_frame).await?, send).await
        })
        .await
        .map_err(|_| timed_out())??;
        let Some(entering) = hello.entering.clone() else {
            drop(permit);
            return self
                .authenticate_hello(trust, &hello, recv, send, max_frame)
                .await
                .map(|peer| (peer, None));
        };
        let doorway = doorway.ok_or(HandshakeError::NotEntering)?;
        let entry = ENTERING.try_acquire().map_err(|_| busy())?;
        let entered = n0_future::time::timeout(
            ENTERING_DEADLINE,
            enter(self, &hello, &entering, recv, send, max_frame, doorway),
        )
        .await
        .map_err(|_| timed_out())?;
        drop(entry);
        drop(permit);
        entered.map(|(peer, entered)| (peer, Some(entered)))
    }
}

fn busy() -> HandshakeError {
    HandshakeError::Io {
        stage: "handshake",
        source: std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "too many connections awaiting authentication",
        ),
    }
}

fn timed_out() -> HandshakeError {
    HandshakeError::Io {
        stage: "handshake",
        source: std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timed out"),
    }
}

/// One entry, after its hello: the device's certificate checked as any
/// device's (valid and unrevoked, of any person), the door asked and the
/// device challenged, the device's proof of its key and of the challenge
/// read, and the workspace's answer. Nothing but these frames is read.
async fn enter<R, W>(
    identity: &LiveIdentity,
    hello: &Hello,
    entering: &Entering,
    recv: &mut R,
    send: &mut W,
    max_frame: usize,
    doorway: &dyn Doorway,
) -> Result<(AuthenticatedPeer, Entered), HandshakeError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let (pending, ack) = identity.begin(
        EndpointTrust::Fixed(HandshakeTrust::AnyCertifiedDevice),
        hello,
    )?;
    let asking = crate::timing::start();
    let asked = doorway.ask(&hello.cert, entering).await;
    crate::timing::event("entry.door_ask", asking, 0);
    let step = match &asked {
        Ok(Asked::Challenge { challenge, .. }) => EnterStep::Challenge {
            challenge: challenge.clone(),
        },
        Ok(Asked::In(_)) => EnterStep::In,
        Err(refusal) => EnterStep::Refused(refusal.clone()),
    };
    for (stage, frame) in [
        ("encode ack", postcard::to_allocvec(&ack)),
        ("encode entry step", postcard::to_allocvec(&step)),
    ] {
        let frame = frame.map_err(|source| HandshakeError::Codec { stage, source })?;
        write_frame(send, &frame)
            .await
            .map_err(|source| HandshakeError::Io {
                stage: "write ack",
                source,
            })?;
    }
    let asked = asked.map_err(HandshakeError::Refused)?;
    // From the host's answer to the device's proof: a round trip, and the
    // device's signing.
    let proving = crate::timing::start();
    let ack2: HelloAck2 = decode("decode ack2", &read("read ack2", recv, max_frame).await?)?;
    let peer = pending.finish(&ack2)?;
    let (challenge, token) = match asked {
        Asked::Challenge { challenge, token } => (challenge, token),
        Asked::In(entered) => return Ok((peer, entered)),
    };
    let proof: EnterProof = decode(
        "decode entry proof",
        &read("read entry proof", recv, max_frame).await?,
    )?;
    crate::timing::event("entry.proof_wait", proving, 0);
    let entered = doorway
        .prove(challenge, token, proof.signature, &entering.name)
        .await;
    Ok((peer, entered))
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
    let permit = budget.try_acquire().map_err(|_| HandshakeError::Io {
        stage: "handshake",
        source: std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "too many connections awaiting authentication",
        ),
    })?;
    let max_frame = max_frame.min(MAX_HANDSHAKE_FRAME_BYTES);
    let result = n0_future::time::timeout(HANDSHAKE_DEADLINE, async {
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
        let (pending, ack) = identity.begin(trust, hello)?;
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
    .await
    .map_err(|_| HandshakeError::Io {
        stage: "handshake",
        source: std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timed out"),
    })?;
    // Authenticated sessions no longer occupy the pre-authentication budget.
    // Errors, timeouts and cancellation release it too through RAII.
    drop(permit);
    result
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

/// The client's hello; at another version, this relay's version is the
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
            // The peer learns this relay's version even if the write fails;
            // there is nothing more to tell it either way.
            let _ = write_frame(send, &answer).await;
            tracing::info!(
                theirs,
                ours = auth::PEER_PROTOCOL_VERSION,
                "a peer at another protocol version was told this relay's"
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
    async fn pending_budget_is_shared_and_released_on_cancellation_and_success() {
        let identity = identity("host").unwrap();
        let hello = hello(&identity);
        // Local budgets isolate this test from unrelated relay tests.
        let budget = Semaphore::new(MAX_PENDING_HANDSHAKES);
        let mut streams: Vec<_> = (0..MAX_PENDING_HANDSHAKES)
            .map(|_| {
                let (server, client) = tokio::io::duplex(4096);
                let (recv, send) = split(server);
                (recv, send, client)
            })
            .collect();
        let mut pending: Vec<_> = streams
            .iter_mut()
            .enumerate()
            .map(|(index, (recv, send, _))| {
                Box::pin(authenticate_bounded(
                    &identity,
                    trust(),
                    (index % 2 == 0).then_some(&hello),
                    recv,
                    send,
                    MAX_FRAME_BYTES,
                    &budget,
                ))
            })
            .collect();
        for handshake in &mut pending {
            poll_pending(handshake.as_mut()).await;
        }
        assert_eq!(budget.available_permits(), 0);

        let (server, mut client) = tokio::io::duplex(4096);
        let (mut recv, mut send) = split(server);
        for supplied in [false, true] {
            assert_io(
                &authenticate_bounded(
                    &identity,
                    trust(),
                    supplied.then_some(&hello),
                    &mut recv,
                    &mut send,
                    MAX_FRAME_BYTES,
                    &budget,
                )
                .await
                .unwrap_err(),
                "handshake",
                std::io::ErrorKind::WouldBlock,
            );
        }
        // Cancelling one waiting exchange makes workspace immediately.
        drop(pending.pop().unwrap());
        assert_eq!(budget.available_permits(), 1);
        for supplied in [false, true] {
            let (result, ()) = tokio::join!(
                authenticate_bounded(
                    &identity,
                    trust(),
                    supplied.then_some(&hello),
                    &mut recv,
                    &mut send,
                    MAX_FRAME_BYTES,
                    &budget,
                ),
                async {
                    if !supplied {
                        write_frame(&mut client, &postcard::to_allocvec(&hello).unwrap())
                            .await
                            .unwrap();
                    }
                    answer_challenge(&identity, &mut client).await.unwrap();
                },
            );
            assert_eq!(
                result.unwrap().device_public_key,
                identity.cert.device_public_key
            );
            assert_eq!(budget.available_permits(), 1);
        }
        drop(pending);
        assert_eq!(budget.available_permits(), MAX_PENDING_HANDSHAKES);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hello_asking_to_enter_is_refused_where_no_one_enters() {
        let identity = identity("host").unwrap();
        let budget = Semaphore::new(1);
        let entering = Hello::entering(
            identity.cert.clone(),
            [1; 32],
            Entering {
                workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".to_owned(),
                name: "Stranger".to_owned(),
                request_id: [2; 32],
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
}
