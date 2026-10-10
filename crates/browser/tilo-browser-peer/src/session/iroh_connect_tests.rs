//! Exercise dial classification through the native browser-peer connect path.

use super::*;
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets::Minimal};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn cert(root: &SigningKey, device: &SigningKey) -> TestResult<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "dial classification test".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root)?;
    Ok(cert)
}

async fn endpoint(alpn: &[u8]) -> TestResult<Endpoint> {
    Ok(Endpoint::builder(Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![alpn.to_vec()])
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))?
        .bind()
        .await?)
}

async fn dial_error(server: &Endpoint) -> TestResult<LinkError> {
    let root = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[2; 32]);
    let host = SigningKey::from_bytes(&[3; 32]);
    let config = PeerConfig {
        host: Host::Iroh {
            addr: server.addr(),
            relay_url: None,
        },
        cert: cert(&root, &device)?,
        host_cert: cert(&root, &host)?,
        revocations: RevocationList::new(),
    };
    match tilo_test_env::within("native browser-peer dial", connect(&config, &device)).await {
        Err(error) => Ok(error),
        Ok(link) => {
            link.close().await;
            Err("a rejecting endpoint must not authenticate a session".into())
        }
    }
}

#[tokio::test]
async fn unsupported_alpn_identifies_tilo_relay_v1() {
    let server = endpoint(b"dummy-alpn/1").await.unwrap();
    let accept = async {
        let incoming = server.accept().await.unwrap();
        assert!(
            incoming.await.is_err(),
            "the dummy ALPN must reject the dial"
        );
    };
    let ((), error) = tokio::join!(
        tilo_test_env::within("dummy-ALPN rejection", accept),
        dial_error(&server),
    );
    server.close().await;
    assert_eq!(
        error.unwrap(),
        LinkError::Unavailable("host does not speak tilo-relay/1".into()),
    );
}

#[tokio::test]
async fn transport_refusal_remains_generic_unavailable() {
    let server = endpoint(b"tilo-relay/1").await.unwrap();
    let refuse = async {
        server.accept().await.unwrap().refuse();
    };
    let ((), error) = tokio::join!(
        tilo_test_env::within("QUIC transport refusal", refuse),
        dial_error(&server),
    );
    server.close().await;
    let error = error.unwrap();
    let LinkError::Unavailable(reason) = error else {
        panic!("ordinary transport failures must remain unavailable: {error}");
    };
    assert!(!reason.is_empty());
    assert_ne!(reason, "host does not speak tilo-relay/1");
}
