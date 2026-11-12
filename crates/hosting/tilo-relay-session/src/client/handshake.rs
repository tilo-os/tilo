//! The client half of the relay handshake, over any byte stream pair.

use tilo_relay_client::DeviceSigner;
use tilo_relay_client::frame::write_frame;
use tilo_user_identity::{DeviceCert, RevocationList};

use super::{ClientError, ExpectedServer, handshake_frame};
use crate::auth::{self, AuthenticatedPeer, Hello, HelloAck};
use crate::transport::MAX_FRAME_BYTES;

/// The client half of the mutual handshake over `send`/`recv`: hello, the
/// server's proof checked against `server`, and this device's own proof.
pub(super) async fn authenticate_server<R, W>(
    send: &mut W,
    recv: &mut R,
    own_cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    revocations: &RevocationList,
    server: &ExpectedServer,
) -> Result<AuthenticatedPeer, ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let client_nonce = fresh_nonce();
    let hello = Hello::new(own_cert.clone(), client_nonce);
    let hello_bytes =
        postcard::to_allocvec(&hello).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &hello_bytes).await?;
    let ack = read_ack(recv).await?;
    let relay_peer = verify_ack(&ack, server, revocations, &client_nonce)?;
    let ack2 = tilo_relay_client::signer::signed_ack2(signer, &ack.server_nonce)
        .await
        .map_err(ClientError::Sign)?;
    let own_ack2_bytes =
        postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &own_ack2_bytes).await?;
    Ok(relay_peer)
}

/// An entry's hello written ([`tilo_relay_protocol::entering`]): the host's
/// proof is the first frame it answers with, checked before anything else of
/// the session is read ([`Self::verify`]).
pub(super) struct AwaitingAck {
    client_nonce: [u8; 32],
    server: ExpectedServer,
    revocations: RevocationList,
}

impl AwaitingAck {
    /// Read the host's `HelloAck` off `recv` and check it proves the host
    /// this device dialled.
    pub(super) async fn verify<R>(self, recv: &mut R) -> Result<(), ClientError>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let ack = read_ack(recv).await?;
        verify_ack(&ack, &self.server, &self.revocations, &self.client_nonce).map(drop)
    }
}

/// Write the hello of a device entering a workspace
/// ([`tilo_relay_protocol::entering`]): its entry, its request for the
/// workspace's channel keys and its proof, signed over `binding`, the keying
/// material this end exported from the connection `send` writes on. Nothing
/// is awaited: the device's first requests follow right behind, and the
/// host's proof is checked as the first thing read ([`AwaitingAck`]).
pub(super) async fn enter_server<W>(
    send: &mut W,
    own_cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    revocations: RevocationList,
    server: ExpectedServer,
    (entering, access): (
        tilo_relay_protocol::entering::Entering,
        Option<collab_types::MlsAccessRequestV1>,
    ),
    binding: &[u8; 32],
) -> Result<AwaitingAck, ClientError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tilo_relay_protocol::entering::{Entry, entry_digest};
    let client_nonce = fresh_nonce();
    let digest = entry_digest(binding, &own_cert.device_public_key, &entering);
    let proof = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| ClientError::Sign(error.0))?;
    let hello = Hello::entering(
        own_cert.clone(),
        client_nonce,
        Entry {
            entering,
            access,
            proof,
        },
    );
    let hello_bytes =
        postcard::to_allocvec(&hello).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &hello_bytes).await?;
    Ok(AwaitingAck {
        client_nonce,
        server,
        revocations,
    })
}

fn fresh_nonce() -> [u8; 32] {
    use rand::Rng;
    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// The host's answer to a hello. Nothing at all back is told apart from a
/// broken answer: it is how a refusal, and a host from before versioned
/// handshakes, look.
async fn read_ack<R>(recv: &mut R) -> Result<HelloAck, ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let ack_bytes = match tilo_relay_client::frame::read_frame_or_eof(recv, MAX_FRAME_BYTES).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(ClientError::NoHandshakeAnswer("the stream ended".into()));
        }
        Err(error) => return Err(ClientError::NoHandshakeAnswer(error.to_string())),
    };
    handshake_frame(&ack_bytes)
}

/// The server `ack` proves, checked against who this device dialled.
fn verify_ack(
    ack: &HelloAck,
    server: &ExpectedServer,
    revocations: &RevocationList,
    client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, ClientError> {
    let now_ns = tilo_clock::unix_now_ns();
    if let Some(scope) = server.interactive_scope.as_ref() {
        return Ok(auth::verify_interactive_owner_ack(
            ack,
            &scope.authority,
            revocations,
            now_ns,
            client_nonce,
        )?);
    }
    match (server.user.as_ref(), server.device.as_ref()) {
        (Some(user), Some(device)) if server.own_device => Ok(auth::verify_own_device_ack(
            ack,
            user,
            device,
            revocations,
            now_ns,
            client_nonce,
        )?),
        (Some(expected_user), expected_device) => {
            let relay_peer =
                auth::verify_hello_ack(ack, expected_user, revocations, now_ns, client_nonce)?;
            if expected_device.is_some_and(|expected_device| {
                relay_peer.cert.device_public_key != *expected_device
            }) {
                return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
            }
            Ok(relay_peer)
        }
        (None, _) if !server.own_device => Ok(auth::verify_pinned_host_ack(
            ack,
            revocations,
            now_ns,
            client_nonce,
        )?),
        (None, _) => Err(ClientError::Auth(crate::auth::AuthError::PolicyReject)),
    }
}
