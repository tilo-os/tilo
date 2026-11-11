//! The client half of the relay handshake, over any byte stream pair.

use tilo_relay_client::DeviceSigner;
use tilo_relay_client::frame::write_frame;
use tilo_user_identity::{DeviceCert, RevocationList};

use super::{ClientError, ExpectedServer, handshake_frame};
use crate::auth::{self, AuthenticatedPeer, Hello, HelloAck};
use crate::transport::MAX_FRAME_BYTES;
use tilo_relay_protocol::entering::{EnterProof, EnterStep};

/// The client half of the mutual handshake over `send`/`recv`: hello, the
/// server's proof checked against `server`, and this device's own proof.
/// With `entering`, this device enters a workspace through its door on the
/// same exchange ([`tilo_relay_protocol::entering`]): the host's step follows
/// its ack, and this device's proof of the workspace's challenge follows its
/// own.
pub(super) async fn authenticate_server<R, W>(
    send: &mut W,
    recv: &mut R,
    own_cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    revocations: &RevocationList,
    server: &ExpectedServer,
    entering: Option<super::Entry>,
) -> Result<AuthenticatedPeer, ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut client_nonce = [0u8; 32];
    {
        use rand::Rng;
        rand::rng().fill_bytes(&mut client_nonce);
    }
    let (hello, access) = match entering {
        Some(entry) => (
            Hello::entering(own_cert.clone(), client_nonce, entry.entering),
            Some(entry.access),
        ),
        None => (Hello::new(own_cert.clone(), client_nonce), None),
    };
    let entered = access.is_some();
    let hello_bytes =
        postcard::to_allocvec(&hello).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &hello_bytes).await?;

    // Nothing at all back is told apart from a broken answer: it is how
    // a refusal, and a host from before versioned handshakes, look.
    let ack_bytes = match tilo_relay_client::frame::read_frame_or_eof(recv, MAX_FRAME_BYTES).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(ClientError::NoHandshakeAnswer("the stream ended".into()));
        }
        Err(error) => return Err(ClientError::NoHandshakeAnswer(error.to_string())),
    };
    let ack: HelloAck = handshake_frame(&ack_bytes)?;
    let now_ns = tilo_clock::unix_now_ns();
    let relay_peer = if let Some(scope) = server.interactive_scope.as_ref() {
        auth::verify_interactive_owner_ack(
            &ack,
            &scope.authority,
            revocations,
            now_ns,
            &client_nonce,
        )?
    } else {
        match (server.user.as_ref(), server.device.as_ref()) {
            (Some(user), Some(device)) if server.own_device => {
                auth::verify_own_device_ack(&ack, user, device, revocations, now_ns, &client_nonce)?
            }
            (Some(expected_user), expected_device) => {
                let relay_peer = auth::verify_hello_ack(
                    &ack,
                    expected_user,
                    revocations,
                    now_ns,
                    &client_nonce,
                )?;
                if expected_device.is_some_and(|expected_device| {
                    relay_peer.cert.device_public_key != *expected_device
                }) {
                    return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
                }
                relay_peer
            }
            (None, _) if !server.own_device => {
                auth::verify_pinned_host_ack(&ack, revocations, now_ns, &client_nonce)?
            }
            (None, _) => return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject)),
        }
    };

    // The door's step comes right behind the ack: read before this device
    // proves anything, so a refusal ends the exchange here.
    let challenge = if entered {
        let step = tilo_relay_client::frame::read_frame(recv, MAX_FRAME_BYTES).await?;
        match postcard::from_bytes(&step).map_err(|e| ClientError::Codec(e.to_string()))? {
            EnterStep::Challenge { challenge } => Some(challenge),
            EnterStep::In => None,
            EnterStep::Refused(refusal) => return Err(ClientError::Refused(refusal)),
        }
    } else {
        None
    };
    let ack2 = tilo_relay_client::signer::signed_ack2(signer, &ack.server_nonce)
        .await
        .map_err(ClientError::Sign)?;
    let own_ack2_bytes =
        postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &own_ack2_bytes).await?;
    if let Some(challenge) = challenge {
        let signature = signer
            .sign(tilo_relay_protocol::workspaces::join_proof_digest(&challenge).to_vec())
            .await
            .map_err(|error| ClientError::Sign(error.0))?;
        let proof = postcard::to_allocvec(&EnterProof {
            signature,
            access: access.flatten(),
        })
        .map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(send, &proof).await?;
    }
    Ok(relay_peer)
}
