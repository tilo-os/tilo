//! The client half of the server handshake, over any byte stream pair.

use tilo_server_client::DeviceSigner;
use tilo_server_client::frame::write_frame;
use tilo_user_identity::{DeviceCert, RevocationList};

use super::{ClientError, ExpectedServer, handshake_frame};
use crate::auth::{self, AuthenticatedPeer, Hello, HelloAck, HelloRefused, ServerProof};
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
    let ack = match read_answer(recv).await? {
        Answered::Ack(ack) => ack,
        Answered::Refused(refused) => {
            return Err(refusal(
                &refused,
                server,
                &own_cert.user_public_key,
                revocations,
                &client_nonce,
            ));
        }
    };
    let server_peer = verify_ack(
        &ack,
        server,
        &own_cert.user_public_key,
        revocations,
        &client_nonce,
    )?;
    let ack2 = tilo_server_client::signer::signed_ack2(signer, &ack.server_nonce)
        .await
        .map_err(ClientError::Sign)?;
    let own_ack2_bytes =
        postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
    write_frame(send, &own_ack2_bytes).await?;
    Ok(server_peer)
}

/// An entry's hello written ([`tilo_server_protocol::entering`]): the host's
/// proof is the first frame it answers with, checked before anything else of
/// the session is read ([`Self::verify`]).
pub(super) struct AwaitingAck {
    client_nonce: [u8; 32],
    server: ExpectedServer,
    /// The person whose device dialled.
    own_user: [u8; 32],
    revocations: RevocationList,
}

impl AwaitingAck {
    /// Read the host's `HelloAck` off `recv` and check it proves the host
    /// this device dialled.
    pub(super) async fn verify<R>(self, recv: &mut R) -> Result<(), ClientError>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let ack = match read_answer(recv).await? {
            Answered::Ack(ack) => ack,
            Answered::Refused(refused) => {
                return Err(refusal(
                    &refused,
                    &self.server,
                    &self.own_user,
                    &self.revocations,
                    &self.client_nonce,
                ));
            }
        };
        verify_ack(
            &ack,
            &self.server,
            &self.own_user,
            &self.revocations,
            &self.client_nonce,
        )
        .map(drop)
    }
}

/// Write the hello of a device entering a workspace
/// ([`tilo_server_protocol::entering`]): its entry, its request for the
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
        tilo_server_protocol::entering::Entering,
        Option<collab_types::MlsAccessRequestV1>,
    ),
    binding: &[u8; 32],
) -> Result<AwaitingAck, ClientError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tilo_server_protocol::entering::{Entry, entry_digest};
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
        own_user: own_cert.user_public_key,
        revocations,
    })
}

fn fresh_nonce() -> [u8; 32] {
    use rand::Rng;
    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// What a server answered a hello with.
enum Answered {
    /// Its ack, still to be checked ([`verify_ack`]).
    Ack(HelloAck),
    /// Its refusal that says why, still to be checked ([`refusal`]).
    Refused(HelloRefused),
}

/// The server's answer to a hello. Nothing at all back is told apart from a
/// broken answer: it is how a refusal with nothing said, and a server from
/// before versioned handshakes, look. A refusal that says a reason this
/// device does not know is one with nothing said too.
async fn read_answer<R>(recv: &mut R) -> Result<Answered, ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tilo_server_client::frame::Answer;
    match tilo_server_client::frame::read_answer(recv, MAX_FRAME_BYTES).await {
        Ok(Some(Answer::Frame(bytes))) => handshake_frame(&bytes).map(Answered::Ack),
        Ok(Some(Answer::Refusal(bytes))) => match auth::decode_handshake(&bytes) {
            Ok(refused) => Ok(Answered::Refused(refused)),
            Err(auth::HandshakeFrameError::Version { theirs }) => {
                Err(ClientError::PeerVersion { theirs })
            }
            Err(auth::HandshakeFrameError::Codec(error)) => Err(ClientError::NoHandshakeAnswer(
                format!("a refusal this device does not read: {error}"),
            )),
        },
        Ok(None) => Err(ClientError::NoHandshakeAnswer("the stream ended".into())),
        Err(error) => Err(ClientError::NoHandshakeAnswer(error.to_string())),
    }
}

/// What `refused`, a refusal of this device's hello, comes to: its reason
/// where the server this device dialled signed it for this very hello, which
/// is checked as that server's ack is ([`verify_ack`]); a refusal with
/// nothing said where it did not. Nobody but its server tells a device that
/// it was removed.
fn refusal(
    refused: &HelloRefused,
    server: &ExpectedServer,
    own_user: &[u8; 32],
    revocations: &RevocationList,
    client_nonce: &[u8; 32],
) -> ClientError {
    match verify_ack(refused, server, own_user, revocations, client_nonce) {
        Ok(_) => match &refused.revocation {
            Some(entry)
                if refused.refusal == auth::HelloRefusal::DeviceRevoked
                    && entry.user_public_key == *own_user
                    && tilo_user_identity::verify_revocation_entry(entry).is_ok() =>
            {
                ClientError::Retired(Box::new(entry.clone()))
            }
            _ => ClientError::Refused(refused.refusal),
        },
        Err(error) => ClientError::NoHandshakeAnswer(format!(
            "a refusal that does not prove its server: {error}"
        )),
    }
}

/// The server `ack` proves, checked against who this device dialled. A
/// host pinned as a device of the dialling device's own person (`own_user`)
/// proves that exact device under their root and needs no host capability:
/// whichever host of theirs it is, the person's server or their own computer,
/// it is theirs.
fn verify_ack(
    ack: &impl ServerProof,
    server: &ExpectedServer,
    own_user: &[u8; 32],
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
        (Some(user), Some(device)) if user == own_user => Ok(auth::verify_own_device_ack(
            ack,
            user,
            device,
            revocations,
            now_ns,
            client_nonce,
        )?),
        (Some(expected_user), expected_device) => {
            let server_peer =
                auth::verify_hello_ack(ack, expected_user, revocations, now_ns, client_nonce)?;
            if expected_device.is_some_and(|expected_device| {
                server_peer.cert.device_public_key != *expected_device
            }) {
                return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
            }
            Ok(server_peer)
        }
        (None, _) => Ok(auth::verify_pinned_host_ack(
            ack,
            revocations,
            now_ns,
            client_nonce,
        )?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_server_client::frame::{frame, read_frame, write_refusal};
    use tilo_user_identity::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};

    type Device = (SigningKey, DeviceCert);

    /// A device of the person `root` is, named `label`: its key and certificate.
    fn device_of(root: &SigningKey, label: &str) -> Device {
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

    /// `by`'s refusal of the hello that carried `nonce`, as its frame's bytes.
    fn refused(by: &Device, nonce: &[u8; 32]) -> Vec<u8> {
        postcard::to_allocvec(&auth::build_hello_refused(
            by.1.clone(),
            &by.0,
            nonce,
            auth::HelloRefusal::DeviceRevoked,
            None,
        ))
        .unwrap()
    }

    /// `payload` as a server writes a refusal that says why: behind an empty frame.
    fn behind_the_mark(payload: &[u8]) -> Vec<u8> {
        let mut wire = vec![0; 4];
        wire.extend_from_slice(&frame(payload).unwrap());
        wire
    }

    /// A person's browser, their server as the browser pinned it, and their laptop.
    struct Ada {
        browser: Device,
        server: Device,
        laptop: Device,
    }

    impl Ada {
        fn new() -> Self {
            let (root, _) = UserIdentity::generate("Ada", 1);
            Self {
                browser: device_of(&root, "browser"),
                server: device_of(&root, "server"),
                laptop: device_of(&root, "laptop"),
            }
        }

        fn pinned(&self) -> ExpectedServer {
            ExpectedServer {
                user: Some(self.server.1.user_public_key),
                device: Some(self.server.1.device_public_key),
                interactive_scope: None,
            }
        }

        /// What the browser's hello comes to when its server writes what `answer` makes of
        /// that hello, and closes the stream.
        async fn told(&self, answer: impl FnOnce(&Hello) -> Vec<u8>) -> ClientError {
            let (device, mut server) = tokio::io::duplex(4096);
            let (mut recv, mut send) = tokio::io::split(device);
            let pinned = self.pinned();
            let none_removed = RevocationList::new();
            let asking = authenticate_server(
                &mut send,
                &mut recv,
                &self.browser.1,
                &self.browser.0,
                &none_removed,
                &pinned,
            );
            let answering = async move {
                let hello = read_frame(&mut server, MAX_FRAME_BYTES).await.unwrap();
                let wire = answer(&auth::decode_handshake(&hello).unwrap());
                tokio::io::AsyncWriteExt::write_all(&mut server, &wire)
                    .await
                    .unwrap();
            };
            let (asked, ()) = tokio::join!(asking, answering);
            asked.unwrap_err()
        }
    }

    #[tokio::test]
    async fn a_device_hears_why_it_was_refused_from_its_server_alone() {
        let ada = Ada::new();
        // The bytes these answers stand behind are the ones a server writes.
        let mut written = Vec::new();
        write_refusal(&mut written, b"why").await.unwrap();
        assert_eq!(written, behind_the_mark(b"why"));

        // Its server's refusal of this very hello: the device knows why.
        let told = ada
            .told(|hello| behind_the_mark(&refused(&ada.server, &hello.client_nonce)))
            .await;
        assert!(
            matches!(
                told,
                ClientError::Refused(auth::HelloRefusal::DeviceRevoked)
            ),
            "{told}"
        );

        // Signed by another device, the person's own though it is: not its server's word.
        let told = ada
            .told(|hello| behind_the_mark(&refused(&ada.laptop, &hello.client_nonce)))
            .await;
        assert!(matches!(told, ClientError::NoHandshakeAnswer(_)), "{told}");

        // Its server's refusal of another hello, sent again: none of this one.
        let told = ada
            .told(|_| behind_the_mark(&refused(&ada.server, &[9; 32])))
            .await;
        assert!(matches!(told, ClientError::NoHandshakeAnswer(_)), "{told}");

        // A refusal that does not read whole.
        let told = ada
            .told(|hello| {
                let whole = refused(&ada.server, &hello.client_nonce);
                behind_the_mark(&whole[..whole.len() / 2])
            })
            .await;
        assert!(matches!(told, ClientError::NoHandshakeAnswer(_)), "{told}");

        // Nothing said, as every server closed a refused hello before: read as it was.
        let told = ada.told(|_| Vec::new()).await;
        assert!(matches!(told, ClientError::NoHandshakeAnswer(_)), "{told}");
    }

    #[tokio::test]
    async fn a_device_entering_hears_why_it_was_refused() {
        let ada = Ada::new();
        let nonce = [5; 32];
        let awaiting = || AwaitingAck {
            client_nonce: nonce,
            server: ada.pinned(),
            own_user: ada.browser.1.user_public_key,
            revocations: RevocationList::new(),
        };
        let mut said = std::io::Cursor::new(behind_the_mark(&refused(&ada.server, &nonce)));
        let told = awaiting().verify(&mut said).await.unwrap_err();
        assert!(
            matches!(
                told,
                ClientError::Refused(auth::HelloRefusal::DeviceRevoked)
            ),
            "{told}"
        );
        let mut forged = std::io::Cursor::new(behind_the_mark(&refused(&ada.laptop, &nonce)));
        let told = awaiting().verify(&mut forged).await.unwrap_err();
        assert!(matches!(told, ClientError::NoHandshakeAnswer(_)), "{told}");
    }
}
