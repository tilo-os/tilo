//! The member proof on a workspace link's first stream: the device that dials
//! proves which device it is, and the device it dialled proves itself back,
//! before the dial names its workspace ([`crate::direct`]).
//!
//! Every host of a workspace link reads the proof, the workspace service's and
//! a device's own alike, so one dial reaches either: a host that read the
//! dial's first frame as the workspace would refuse a proving device, and one
//! that expected the proof would refuse a dial without it.

use collab_types::signer::DeviceSigner;
use tilo_server_client::frame::{read_frame, write_frame, write_refusal};
use tilo_server_client::link::{ExpectedHost, prove_stream};
use tilo_server_protocol::auth::{
    self, HandshakeFrameError, Hello, HelloAck, HelloAck2, HelloRefusal, HelloRefused,
    PEER_PROTOCOL_VERSION,
};
use tilo_server_protocol::refusal::Refusal;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::io::{AsyncRead, AsyncWrite};

/// The largest handshake frame either side reads.
const MAX_PROOF_BYTES: usize = 64 * 1024;

/// Prove this device, `cert` signed by `signer`, to `device`, the device dialled for `workspace`,
/// on `send`/`recv`: the first stream of the link. The host of the person's own workspace must be
/// one of the person's devices; the host of any other workspace is the device its signed address
/// names, of whichever person shares it.
///
/// # Errors
///
/// The host refused the device, proved another device, or answered nothing: a device its
/// person's root retired is told with the entry the root signed ([`Refusal::Revoked`]).
pub(crate) async fn prove_dialled<R, W>(
    send: &mut W,
    recv: &mut R,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    revocations: &RevocationList,
    workspace: &str,
    device: [u8; 32],
) -> Result<(), Refusal>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let own = cert.user_public_key;
    let expected = if workspace == tilo_server_protocol::servers::person_workspace_id(&own) {
        ExpectedHost::Person {
            user: own,
            device: Some(device),
        }
    } else {
        ExpectedHost::Device(device)
    };
    prove_stream(send, recv, cert, signer, revocations, expected).await?;
    Ok(())
}

/// The host's half of the proof on `send`/`recv`, the first stream of a link
/// this device accepted: the dialling device's certificate, valid and
/// unrevoked under `revocations`, and its proof of the certificate's key, with
/// this device's own, `cert` signed by `signer`. Answers the device proved;
/// which workspaces it reaches is the admission's to decide, after the dial
/// names one.
///
/// # Errors
///
/// The device spoke another protocol version, offered a certificate that does
/// not verify or is revoked, or did not prove its key.
pub(crate) async fn proved_dialler<R, W>(
    send: &mut W,
    recv: &mut R,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    revocations: &RevocationList,
) -> Result<[u8; 32], String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let read =
        |error: std::io::Error| format!("the dialling device's proof did not arrive: {error}");
    let hello: Hello = match auth::decode_handshake(
        &read_frame(recv, MAX_PROOF_BYTES).await.map_err(read)?,
    ) {
        Ok(hello) => hello,
        Err(HandshakeFrameError::Version { theirs }) => {
            // The device learns this one's version; there is nothing more to tell it.
            let told = match auth::version_answer() {
                Ok(answer) => write_frame(send, &answer)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let untold = match told {
                Ok(()) => String::new(),
                Err(error) => format!(", and it could not be told so: {error}"),
            };
            return Err(format!(
                "the dialling device speaks protocol {theirs}, this one {PEER_PROTOCOL_VERSION}{untold}"
            ));
        }
        Err(HandshakeFrameError::Codec(error)) => {
            return Err(format!(
                "the dialling device's hello does not read: {error}"
            ));
        }
    };
    let now = tilo_clock::unix_now_ns();
    tilo_user_identity::verify_device_cert_at(&hello.cert, now)
        .map_err(|error| format!("the dialling device's certificate: {error}"))?;
    if revocations.refuses_at(&hello.cert, now) {
        // A device its person's root retired is told so with the entry the root signed, as a
        // refused hello is everywhere: a server that was away learns it was retired from the
        // first device that refuses it, and lets go of what it holds. A device revoked for any
        // other reason is told nothing.
        if let Some(entry) =
            revocations.retired(&hello.cert.user_public_key, &hello.cert.device_public_key)
        {
            let refusal = HelloRefusal::DeviceRevoked;
            let sig_over_refusal = signer
                .sign(auth::refusal_digest(&hello.client_nonce, refusal).to_vec())
                .await
                .map_err(|error| error.0)?;
            let refused = HelloRefused {
                protocol: PEER_PROTOCOL_VERSION,
                cert: cert.clone(),
                refusal,
                sig_over_refusal,
                revocation: Some(entry.clone()),
            };
            let refused = postcard::to_allocvec(&refused).map_err(|error| error.to_string())?;
            write_refusal(send, &refused)
                .await
                .map_err(|error| error.to_string())?;
        }
        return Err("the dialling device is revoked".into());
    }
    let mut server_nonce = [0; 32];
    rand_core::Rng::fill_bytes(
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
        &mut server_nonce,
    );
    let sig_over_client_nonce = signer
        .sign(auth::server_challenge_digest(&hello.client_nonce).to_vec())
        .await
        .map_err(|error| error.0)?;
    let ack = HelloAck {
        protocol: PEER_PROTOCOL_VERSION,
        cert: cert.clone(),
        server_nonce,
        sig_over_client_nonce,
    };
    let ack = postcard::to_allocvec(&ack).map_err(|error| error.to_string())?;
    write_frame(send, &ack)
        .await
        .map_err(|error| error.to_string())?;
    let ack2: HelloAck2 =
        postcard::from_bytes(&read_frame(recv, MAX_PROOF_BYTES).await.map_err(read)?)
            .map_err(|error| format!("the dialling device's proof does not read: {error}"))?;
    let device = hello.cert.device_public_key;
    auth::verify_hello_ack2(&ack2, &device, &server_nonce).map_err(|error| error.to_string())?;
    Ok(device)
}
