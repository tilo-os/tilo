//! The relay handshake's final frame, signed by a [`DeviceSigner`].

pub use collab_types::signer::{DeviceSigner, MaybeSendSync, SignError, SignFuture};

/// The final handshake frame, signed by `signer`.
///
/// # Errors
///
/// The signer's refusal or failure, as its message.
pub async fn signed_ack2(
    signer: &dyn DeviceSigner,
    server_nonce: &[u8; 32],
) -> Result<tilo_relay_protocol::auth::HelloAck2, String> {
    let sig_over_server_nonce = signer
        .sign(tilo_relay_protocol::auth::client_challenge_digest(server_nonce).to_vec())
        .await
        .map_err(|error| error.0)?;
    Ok(tilo_relay_protocol::auth::HelloAck2 {
        sig_over_server_nonce,
    })
}
