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
    let mut client_nonce = [0u8; 32];
    {
        use rand::Rng;
        rand::rng().fill_bytes(&mut client_nonce);
    }
    let hello = Hello::new(own_cert.clone(), client_nonce);
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

    let ack2 = tilo_relay_client::signer::signed_ack2(signer, &ack.server_nonce)
        .await
        .map_err(ClientError::Sign)?;
    let own_ack2_bytes =
        postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &own_ack2_bytes).await?;
    Ok(relay_peer)
}
