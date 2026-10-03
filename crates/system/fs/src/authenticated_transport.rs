//! Bounded mutual certified authentication for collaborative-FS byte streams.
//!
//! The transport (currently Iroh in production and `tokio::io::DuplexStream`
//! in tests) already authenticates endpoint keys. This handshake binds those
//! observed endpoint keys to root-certified human/device identities before any
//! workspace protocol bytes are accepted.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_user_identity::RevocationList;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use wire_frame::{FrameError, Framing};

use crate::identity::PeerCredentials;
use crate::peer_auth::{
    AuthenticatedFsPeer, DEFAULT_CHALLENGE_TTL_NS, PeerAuthChallenge, PeerAuthError, PeerAuthProof,
    PendingPeerChallenges,
};

/// Hard protocol ceiling for one authentication frame.
///
/// Certificates and proofs are normally well below 2 KiB. Keeping a fixed
/// ceiling prevents an unauthenticated peer from choosing our allocation size.
pub const MAX_AUTH_FRAME_BYTES: usize = 64 * 1024;

/// Default wall-clock deadline for the complete two-phase handshake.
pub const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_secs(10);

/// Runtime limits for [`authenticate_fs_peer_at`].
#[derive(Clone, Copy, Debug)]
pub struct AuthenticatedTransportConfig {
    /// Maximum accepted frame payload, additionally capped by
    /// [`MAX_AUTH_FRAME_BYTES`].
    pub max_frame_bytes: usize,
    /// Wall-clock deadline covering both challenge and proof exchange.
    pub timeout: Duration,
    /// Logical lifetime recorded in the locally issued challenge.
    pub challenge_ttl_ns: u64,
}

impl Default for AuthenticatedTransportConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_AUTH_FRAME_BYTES,
            timeout: DEFAULT_AUTH_TIMEOUT,
            challenge_ttl_ns: DEFAULT_CHALLENGE_TTL_NS,
        }
    }
}

/// Fail-closed errors produced before collaborative-FS protocol admission.
#[derive(Debug, Error)]
pub enum AuthenticatedTransportError {
    /// The local endpoint was not created from the credential's certified key.
    #[error("local certified device does not match the local transport endpoint")]
    LocalEndpointMismatch,
    /// The remote challenge names an endpoint other than the observed peer.
    #[error("remote challenge is not bound to the observed transport endpoint")]
    RemoteVerifierEndpointMismatch,
    /// The configured frame limit is unusable or exceeds the hard protocol cap.
    #[error("authentication frame limit must be between 1 and {MAX_AUTH_FRAME_BYTES} bytes")]
    InvalidFrameLimit,
    /// The peer announced or we encoded a frame beyond the configured bound.
    #[error("authentication frame is too large: {announced} bytes (maximum {maximum})")]
    FrameTooLarge {
        /// Encoded or announced payload length.
        announced: usize,
        /// Effective configured maximum.
        maximum: usize,
    },
    /// A phase received a valid frame of the wrong kind.
    #[error("unexpected authentication frame; expected {expected}")]
    UnexpectedFrame {
        /// Human-readable expected phase.
        expected: &'static str,
    },
    /// Postcard could not encode or decode the bounded frame.
    #[error("invalid authentication frame: {0}")]
    Codec(#[from] postcard::Error),
    /// The underlying stream closed or failed.
    #[error("authentication transport failed: {0}")]
    Io(#[from] io::Error),
    /// Certificate, revocation, endpoint, freshness, or signature verification
    /// rejected the peer.
    #[error("peer authentication failed: {0}")]
    Authentication(#[from] PeerAuthError),
    /// The peer did not finish the complete handshake by the wall-clock limit.
    #[error("peer authentication timed out")]
    Timeout,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum AuthFrame {
    Challenge(PeerAuthChallenge),
    Proof(Box<PeerAuthProof>),
}

/// Mutually authenticate one already-established endpoint-bound byte stream.
///
/// Both sides run the same function. Each phase writes and reads concurrently,
/// so the protocol remains live even when the transport's send buffer is
/// smaller than a frame. `observed_remote_endpoint` must come from the secure
/// transport connection itself, never from peer-controlled protocol data.
///
/// `now_ns` is Unix time used for certificate, revocation, and challenge
/// validation. The caller should sample it immediately before this call.
///
/// # Errors
///
/// Returns an error when endpoint binding, frame bounds, transport I/O,
/// certificate validation, revocation checks, or the handshake deadline fails.
pub async fn authenticate_fs_peer_at<S>(
    stream: &mut S,
    credentials: &PeerCredentials,
    local_endpoint: [u8; 32],
    observed_remote_endpoint: [u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
    config: AuthenticatedTransportConfig,
) -> Result<AuthenticatedFsPeer, AuthenticatedTransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(&mut *stream);
    authenticate_fs_peer_parts_at(
        &mut reader,
        &mut writer,
        credentials,
        local_endpoint,
        observed_remote_endpoint,
        revocations,
        now_ns,
        config,
    )
    .await
}

/// Split-stream form of [`authenticate_fs_peer_at`].
///
/// This maps directly to Iroh/QUIC's `(RecvStream, SendStream)` bidirectional
/// stream halves while retaining the same concurrent, deadlock-free exchange.
///
/// # Errors
///
/// Returns an error under the same fail-closed conditions as
/// [`authenticate_fs_peer_at`].
// The split-stream entry point keeps the two I/O halves and the six
// independently authenticated handshake inputs explicit at this trust boundary.
pub async fn authenticate_fs_peer_parts_at<R, W>(
    reader: &mut R,
    writer: &mut W,
    credentials: &PeerCredentials,
    local_endpoint: [u8; 32],
    observed_remote_endpoint: [u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
    config: AuthenticatedTransportConfig,
) -> Result<AuthenticatedFsPeer, AuthenticatedTransportError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if credentials.device_public_key() != local_endpoint {
        return Err(AuthenticatedTransportError::LocalEndpointMismatch);
    }
    if config.max_frame_bytes == 0 || config.max_frame_bytes > MAX_AUTH_FRAME_BYTES {
        return Err(AuthenticatedTransportError::InvalidFrameLimit);
    }

    let handshake = async {
        // The store is scoped to this connection: dropping the handshake also
        // drops an unconsumed challenge, so timeouts cannot exhaust a shared
        // pending-challenge registry.
        let mut pending = PendingPeerChallenges::new(local_endpoint, 1, config.challenge_ttl_ns);
        let local_challenge = pending.issue_with_ttl(now_ns, config.challenge_ttl_ns)?;

        let remote_frame = exchange_frame(
            reader,
            writer,
            &AuthFrame::Challenge(local_challenge),
            config.max_frame_bytes,
        )
        .await?;
        let remote_challenge = match remote_frame {
            AuthFrame::Challenge(challenge) => challenge,
            AuthFrame::Proof(_) => {
                return Err(AuthenticatedTransportError::UnexpectedFrame {
                    expected: "challenge",
                });
            }
        };
        if remote_challenge.verifier_endpoint != observed_remote_endpoint {
            return Err(AuthenticatedTransportError::RemoteVerifierEndpointMismatch);
        }

        let local_proof = PeerAuthProof::respond(
            credentials.cert().clone(),
            local_endpoint,
            remote_challenge,
            credentials.signing_key(),
        )?;
        let remote_frame = exchange_frame(
            reader,
            writer,
            &AuthFrame::Proof(Box::new(local_proof)),
            config.max_frame_bytes,
        )
        .await?;
        let remote_proof = match remote_frame {
            AuthFrame::Proof(proof) => *proof,
            AuthFrame::Challenge(_) => {
                return Err(AuthenticatedTransportError::UnexpectedFrame { expected: "proof" });
            }
        };

        pending
            .verify(&remote_proof, observed_remote_endpoint, revocations, now_ns)
            .map_err(AuthenticatedTransportError::from)
    };

    tokio::time::timeout(config.timeout, handshake)
        .await
        .map_err(|_| AuthenticatedTransportError::Timeout)?
}

async fn exchange_frame<R, W>(
    reader: &mut R,
    writer: &mut W,
    outgoing: &AuthFrame,
    maximum: usize,
) -> Result<AuthFrame, AuthenticatedTransportError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let ((), incoming) = tokio::try_join!(
        write_frame(writer, outgoing, maximum),
        read_frame(reader, maximum)
    )?;
    Ok(incoming)
}

/// Each handshake frame: a big-endian `u32` length, then the postcard-encoded
/// [`AuthFrame`], bounded by the configured frame limit.
const AUTH_FRAMING: Framing = Framing::big_endian(MAX_AUTH_FRAME_BYTES);

fn frame_error(error: FrameError, maximum: usize) -> AuthenticatedTransportError {
    match error {
        FrameError::TooLarge(announced) | FrameError::PayloadTooLarge(announced) => {
            AuthenticatedTransportError::FrameTooLarge { announced, maximum }
        }
        FrameError::Encode(error) | FrameError::Decode(error) => {
            AuthenticatedTransportError::Codec(error)
        }
        other => AuthenticatedTransportError::Io(other.into()),
    }
}

async fn write_frame<W>(
    writer: &mut W,
    frame: &AuthFrame,
    maximum: usize,
) -> Result<(), AuthenticatedTransportError>
where
    W: AsyncWrite + Unpin,
{
    AUTH_FRAMING
        .with_max_len(maximum)
        .write_value_async(writer, frame)
        .await
        .map_err(|error| frame_error(error, maximum))?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R>(
    reader: &mut R,
    maximum: usize,
) -> Result<AuthFrame, AuthenticatedTransportError>
where
    R: AsyncRead + Unpin,
{
    AUTH_FRAMING
        .with_max_len(maximum)
        .read_value_async(reader)
        .await
        .and_then(|frame| frame.ok_or(FrameError::Closed))
        .map_err(|error| frame_error(error, maximum))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, IdentityPaths, RevocationEntry, RevocationReason,
        adopt_device_key, persist::write_device_cert, sign_device_cert, sign_revocation_entry,
    };

    const NOW: u64 = 5_000_000;

    #[tokio::test]
    async fn auth_frames_are_big_endian_length_then_postcard() {
        let challenge = AuthFrame::Challenge(PeerAuthChallenge {
            verifier_endpoint: [1; 32],
            nonce: [2; 32],
            expires_at_ns: 300,
        });
        let mut expected = vec![0, 0, 0, 67, 0];
        expected.extend_from_slice(&[1; 32]);
        expected.extend_from_slice(&[2; 32]);
        expected.extend_from_slice(&[0xac, 0x02]);

        let mut wire = Vec::new();
        write_frame(&mut wire, &challenge, MAX_AUTH_FRAME_BYTES)
            .await
            .unwrap();
        assert_eq!(wire, expected);

        let AuthFrame::Challenge(decoded) = read_frame(&mut expected.as_slice(), 67).await.unwrap()
        else {
            panic!("expected a challenge");
        };
        assert_eq!(decoded.nonce, [2; 32]);
        assert_eq!(decoded.expires_at_ns, 300);

        // The configured limit bounds both directions.
        assert!(matches!(
            write_frame(&mut Vec::new(), &challenge, 66).await,
            Err(AuthenticatedTransportError::FrameTooLarge {
                announced: 67,
                maximum: 66
            })
        ));
        assert!(matches!(
            read_frame(&mut expected.as_slice(), 66).await,
            Err(AuthenticatedTransportError::FrameTooLarge {
                announced: 67,
                maximum: 66
            })
        ));
    }

    #[tokio::test]
    async fn closed_and_truncated_auth_streams_are_io_errors() {
        let mut truncated = vec![0, 0, 0, 67, 0];
        truncated.extend_from_slice(&[1; 10]);
        for wire in [&[][..], &[0, 0][..], &truncated[..]] {
            assert!(
                matches!(
                    read_frame(&mut &wire[..], MAX_AUTH_FRAME_BYTES).await,
                    Err(AuthenticatedTransportError::Io(ref error)) if error.kind() == io::ErrorKind::UnexpectedEof
                ),
                "{wire:?}"
            );
        }
        assert!(matches!(
            read_frame(&mut &[0, 0, 0, 1, 9][..], MAX_AUTH_FRAME_BYTES).await,
            Err(AuthenticatedTransportError::Codec(_))
        ));
    }

    struct Party {
        root: SigningKey,
        credentials: PeerCredentials,
        endpoint: [u8; 32],
    }

    fn party(root_seed: u8, device_seed: u8, label: &str) -> Party {
        let root = SigningKey::from_bytes(&[root_seed; 32]);
        let device = SigningKey::from_bytes(&[device_seed; 32]);
        let endpoint = device.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: endpoint,
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: NOW - 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(temp.path());
        adopt_device_key(&paths, &device.to_bytes()).unwrap();
        write_device_cert(&paths.device_cert, &cert).unwrap();
        let certified = crate::identity::load_certified_mount_identity(&paths, NOW).unwrap();
        Party {
            root,
            credentials: certified.credentials,
            endpoint,
        }
    }

    fn fast_config() -> AuthenticatedTransportConfig {
        AuthenticatedTransportConfig {
            timeout: Duration::from_millis(250),
            ..AuthenticatedTransportConfig::default()
        }
    }

    #[tokio::test]
    async fn duplex_handshake_certifies_identity_within_bounds_and_deadlines() {
        // Phase `duplex_peers_mutually_establish_certified_identity`.
        {
            // Capacity 1 proves neither phase relies on both full frames being
            // buffered before either side begins reading.
            let (mut a_stream, mut b_stream) = tokio::io::duplex(1);
            let a = party(1, 2, "Alice laptop");
            let b = party(3, 4, "Bob phone");
            let empty_a = RevocationList::new();
            let empty_b = RevocationList::new();

            let (a_result, b_result) = tokio::join!(
                authenticate_fs_peer_at(
                    &mut a_stream,
                    &a.credentials,
                    a.endpoint,
                    b.endpoint,
                    &empty_a,
                    NOW,
                    fast_config(),
                ),
                authenticate_fs_peer_at(
                    &mut b_stream,
                    &b.credentials,
                    b.endpoint,
                    a.endpoint,
                    &empty_b,
                    NOW,
                    fast_config(),
                )
            );

            let seen_b = a_result.unwrap();
            let seen_a = b_result.unwrap();
            assert_eq!(seen_b.device_public_key, b.endpoint);
            assert_eq!(seen_b.user_public_key, b.credentials.user_public_key());
            assert_eq!(seen_a.device_public_key, a.endpoint);
            assert_eq!(seen_a.user_public_key, a.credentials.user_public_key());
        }
        // Phase `oversized_frame_is_rejected_before_payload_allocation`.
        {
            let (mut local_stream, mut attacker_stream) = tokio::io::duplex(128);
            let local = party(5, 6, "Local");
            let remote = party(7, 8, "Remote");
            let config = AuthenticatedTransportConfig {
                max_frame_bytes: 1024,
                ..fast_config()
            };

            let attacker = tokio::spawn(async move {
                // Drain the local frame concurrently so even a tiny duplex buffer
                // cannot obscure the actual oversize-header rejection.
                let _ = read_frame(&mut attacker_stream, MAX_AUTH_FRAME_BYTES).await;
                attacker_stream
                    .write_all(&(1025u32).to_be_bytes())
                    .await
                    .unwrap();
            });
            let result = authenticate_fs_peer_at(
                &mut local_stream,
                &local.credentials,
                local.endpoint,
                remote.endpoint,
                &RevocationList::new(),
                NOW,
                config,
            )
            .await;
            attacker.await.unwrap();
            assert!(matches!(
                result,
                Err(AuthenticatedTransportError::FrameTooLarge {
                    announced: 1025,
                    maximum: 1024
                })
            ));
        }
        // Phase `silent_peer_hits_whole_handshake_timeout`.
        {
            let (mut local_stream, _silent_stream) = tokio::io::duplex(64);
            let local = party(9, 10, "Local");
            let config = AuthenticatedTransportConfig {
                timeout: Duration::from_millis(20),
                ..AuthenticatedTransportConfig::default()
            };
            let result = authenticate_fs_peer_at(
                &mut local_stream,
                &local.credentials,
                local.endpoint,
                [77; 32],
                &RevocationList::new(),
                NOW,
                config,
            )
            .await;
            assert!(matches!(result, Err(AuthenticatedTransportError::Timeout)));
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn handshake_rejects_misdirected_revoked_and_tampered_proofs() {
        // Phase `challenge_for_another_endpoint_is_not_signed`.
        {
            let (mut a_stream, mut b_stream) = tokio::io::duplex(256);
            let a = party(11, 12, "A");
            let b = party(13, 14, "B");
            let wrong_observed_b = [99; 32];
            let empty_a = RevocationList::new();
            let empty_b = RevocationList::new();

            let (a_result, _b_result) = tokio::join!(
                authenticate_fs_peer_at(
                    &mut a_stream,
                    &a.credentials,
                    a.endpoint,
                    wrong_observed_b,
                    &empty_a,
                    NOW,
                    fast_config(),
                ),
                authenticate_fs_peer_at(
                    &mut b_stream,
                    &b.credentials,
                    b.endpoint,
                    a.endpoint,
                    &empty_b,
                    NOW,
                    fast_config(),
                )
            );
            assert!(matches!(
                a_result,
                Err(AuthenticatedTransportError::RemoteVerifierEndpointMismatch)
            ));
        }
        // Phase `effective_revocation_rejects_otherwise_valid_peer`.
        {
            let (mut a_stream, mut b_stream) = tokio::io::duplex(256);
            let a = party(15, 16, "A");
            let b = party(17, 18, "B");
            let mut revocations = RevocationList::new();
            let mut entry = RevocationEntry {
                user_public_key: b.credentials.user_public_key(),
                revoked_device_public_key: b.endpoint,
                revoked_at_ns: NOW - 1,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &b.root).unwrap();
            revocations.insert(entry).unwrap();
            let empty = RevocationList::new();

            let (a_result, b_result) = tokio::join!(
                authenticate_fs_peer_at(
                    &mut a_stream,
                    &a.credentials,
                    a.endpoint,
                    b.endpoint,
                    &revocations,
                    NOW,
                    fast_config(),
                ),
                authenticate_fs_peer_at(
                    &mut b_stream,
                    &b.credentials,
                    b.endpoint,
                    a.endpoint,
                    &empty,
                    NOW,
                    fast_config(),
                )
            );
            assert!(matches!(
                a_result,
                Err(AuthenticatedTransportError::Authentication(
                    PeerAuthError::Revoked
                ))
            ));
            assert!(
                b_result.is_ok(),
                "revocation is evaluated by the admitting side"
            );
        }
        // Phase `tampered_proof_is_rejected`.
        {
            let (mut local_stream, mut attacker_stream) = tokio::io::duplex(128);
            let local = party(19, 20, "Local");
            let attacker = party(21, 22, "Attacker");
            let attacker_endpoint = attacker.endpoint;

            let malicious = tokio::spawn(async move {
                let AuthFrame::Challenge(local_challenge) =
                    read_frame(&mut attacker_stream, MAX_AUTH_FRAME_BYTES)
                        .await
                        .unwrap()
                else {
                    panic!("expected challenge");
                };
                let mut verifier = PendingPeerChallenges::new(attacker_endpoint, 1, 1_000);
                let attacker_challenge = verifier.register([42; 32], NOW, 1_000).unwrap();
                write_frame(
                    &mut attacker_stream,
                    &AuthFrame::Challenge(attacker_challenge),
                    MAX_AUTH_FRAME_BYTES,
                )
                .await
                .unwrap();
                let _local_proof = read_frame(&mut attacker_stream, MAX_AUTH_FRAME_BYTES)
                    .await
                    .unwrap();
                let mut proof = PeerAuthProof::respond(
                    attacker.credentials.cert().clone(),
                    attacker.endpoint,
                    local_challenge,
                    attacker.credentials.signing_key(),
                )
                .unwrap();
                proof.signature[0] ^= 1;
                write_frame(
                    &mut attacker_stream,
                    &AuthFrame::Proof(Box::new(proof)),
                    MAX_AUTH_FRAME_BYTES,
                )
                .await
                .unwrap();
            });

            let result = authenticate_fs_peer_at(
                &mut local_stream,
                &local.credentials,
                local.endpoint,
                attacker_endpoint,
                &RevocationList::new(),
                NOW,
                fast_config(),
            )
            .await;
            malicious.await.unwrap();
            assert!(matches!(
                result,
                Err(AuthenticatedTransportError::Authentication(
                    PeerAuthError::BadProofSignature
                ))
            ));
        }
    }
}
