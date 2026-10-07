//! Real TLS handshakes exercise the production verifier and cancellation path.
//! Keys are generated per test in memory, never committed or device-issued.
use super::super::certificates;
use super::*;
use pki::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
// In-memory P-256 transport credentials exercise both TLS versions without
// depending on executables or writing private keys to a shared filesystem.
struct Fixture {
    certificate: CertificateDer<'static>,
    receiver: Arc<dyn rustls::sign::SigningKey>,
    attacker: Arc<dyn rustls::sign::SigningKey>,
}
impl Fixture {
    fn new() -> Self {
        let receiver = certificates::KeyPair::generate().expect("ephemeral receiver key");
        let attacker = certificates::KeyPair::generate().expect("ephemeral attacker key");
        let params = certificates::CertificateParams::new(vec!["isolated-cast-fixture".into()])
            .expect("fixture certificate parameters");
        let certificate = params
            .self_signed(&receiver)
            .expect("self-signed fixture leaf")
            .der()
            .clone();
        Self {
            certificate,
            receiver: signing_key(&receiver),
            attacker: signing_key(&attacker),
        }
    }
    fn certified_key(&self, wrong_signer: bool) -> CertifiedKey {
        let key = if wrong_signer {
            &self.attacker
        } else {
            &self.receiver
        };
        // Deliberately bypass with_single_cert's key-match validation: a malicious
        // receiver can send any leaf and sign CertificateVerify with another key.
        CertifiedKey::new(vec![self.certificate.clone()], key.clone())
    }
}
fn signing_key(key: &certificates::KeyPair) -> Arc<dyn rustls::sign::SigningKey> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    rustls::crypto::ring::sign::any_supported_type(&key).expect("fixture signing key")
}
#[derive(Debug)]
struct Resolver(Arc<CertifiedKey>);
impl ResolvesServerCert for Resolver {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}
fn name() -> ServerName<'static> {
    ServerName::try_from("isolated-cast-fixture").expect("fixture hostname")
}

async fn handshake(version: &'static rustls::SupportedProtocolVersion, wrong_signer: bool) {
    let fixture = Fixture::new();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[version])
    .expect("fixture TLS version")
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(Resolver(Arc::new(
        fixture.certified_key(wrong_signer),
    ))));
    let acceptor = tls_runtime::TlsAcceptor::from(Arc::new(config));
    let (client, server) = tokio::io::duplex(16 * 1024);
    let server = tokio::spawn(async move {
        let mut stream = acceptor.accept(server).await?;
        let mut byte = [0];
        stream.read_exact(&mut byte).await?;
        Ok::<_, io::Error>(byte)
    });
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let result = connect(
        &connector().expect("fixture connector"),
        name(),
        client,
        async {
            let _ = rx.recv().await;
        },
        Duration::from_secs(5),
    )
    .await;
    if wrong_signer {
        let error = result.expect_err("wrong CertificateVerify signature must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("BadSignature"),
            "unexpected error: {error}"
        );
        assert!(
            server.await.expect("server task").is_err(),
            "no application bytes after bad signature"
        );
    } else {
        let mut stream = result.expect("valid TLS handshake").expect("not cancelled");
        assert_eq!(stream.get_ref().1.protocol_version(), Some(version.version));
        stream
            .write_all(&[42])
            .await
            .expect("authenticated TLS transport write");
        assert_eq!(
            server
                .await
                .expect("server task")
                .expect("server handshake"),
            [42]
        );
    }
}

#[tokio::test]
async fn tls12_valid_signature() {
    handshake(&rustls::version::TLS12, false).await;
}
#[tokio::test]
async fn tls13_valid_signature() {
    handshake(&rustls::version::TLS13, false).await;
}
#[tokio::test]
async fn tls12_wrong_certificate_signer_rejected() {
    handshake(&rustls::version::TLS12, true).await;
}
#[tokio::test]
async fn tls13_wrong_certificate_signer_rejected() {
    handshake(&rustls::version::TLS13, true).await;
}

async fn cancel(close_commands: bool) {
    let (client, mut peer) = tokio::io::duplex(16 * 1024);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let task = tokio::spawn(async move {
        connect(
            &connector().expect("fixture connector"),
            name(),
            client,
            async {
                let _ = rx.recv().await;
            },
            Duration::from_secs(5),
        )
        .await
    });
    let mut hello = [0];
    tokio::time::timeout(Duration::from_secs(5), peer.read_exact(&mut hello))
        .await
        .expect("ClientHello deadline")
        .expect("handshake started");
    if close_commands {
        drop(tx);
    } else {
        tx.send(()).expect("cancel handshake");
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("prompt cancellation")
            .expect("task")
            .expect("cancellation succeeds")
            .is_none()
    );
    let mut remaining = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut remaining))
        .await
        .expect("peer EOF deadline")
        .expect("cancel closes TLS transport");
}
#[tokio::test]
async fn quit_during_handshake_closes_transport() {
    cancel(false).await;
}
#[tokio::test]
async fn command_channel_close_cancels_handshake() {
    cancel(true).await;
}
#[tokio::test]
async fn stalled_handshake_expires() {
    let (client, _peer) = tokio::io::duplex(16 * 1024);
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let error = connect(
        &connector().expect("fixture connector"),
        name(),
        client,
        async {
            let _ = rx.recv().await;
        },
        Duration::from_millis(10),
    )
    .await
    .expect_err("stalled peer must time out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}
#[tokio::test]
async fn receiver_disconnect_fails_handshake() {
    let (client, peer) = tokio::io::duplex(16 * 1024);
    drop(peer);
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    assert!(
        connect(
            &connector().expect("fixture connector"),
            name(),
            client,
            async {
                let _ = rx.recv().await;
            },
            Duration::from_secs(1)
        )
        .await
        .is_err()
    );
}
#[test]
fn advertised_signature_schemes_match_crypto_provider() {
    use rustls::client::danger::ServerCertVerifier;
    assert_eq!(
        CastTlsVerifier.supported_verify_schemes(),
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    );
    assert!(
        !CastTlsVerifier
            .supported_verify_schemes()
            .contains(&rustls::SignatureScheme::RSA_PKCS1_SHA1)
    );
}
