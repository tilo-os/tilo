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
        issuer: None,
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
        host: Host::at(server.addr()),
        cert: cert(&root, &device)?,
        host_cert: cert(&root, &host)?,
        revocations: RevocationList::new(),
        entering: None,
    };
    match tilo_test_env::within(
        "native browser-peer dial",
        connect(&config, &device, &mut Dial::default()),
    )
    .await
    {
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

#[test]
fn only_an_unanswered_dial_is_tried_again() {
    // The host was not reached, or closed a handshake it may take next time.
    assert!(transient(&LinkError::Unavailable("timed out".into())));
    assert!(transient(&LinkError::Refused("the stream ended".into())));
    // Answers another attempt cannot change.
    assert!(!transient(&LinkError::Sign("locked".into())));
    assert!(!transient(&LinkError::Malformed("bad ack".into())));
    assert!(!transient(&LinkError::HostChanged));
    assert!(!transient(&LinkError::PeerVersion { theirs: 0 }));
}

#[test]
fn retry_waits_double_with_jitter_and_stay_capped() {
    // The base wait of each attempt, before jitter: doubling, then capped.
    for (index, expected_ms) in [500u64, 1000, 2000, 4000, 5000, 5000].iter().enumerate() {
        let attempt = u32::try_from(index).unwrap().saturating_add(1);
        let base = std::time::Duration::from_millis(*expected_ms);
        for _ in 0..32 {
            let delay = connect_backoff(attempt);
            assert!(
                delay >= base.mul_f64(0.75) && delay <= base.mul_f64(1.25),
                "attempt {attempt} waited {delay:?}, expected around {base:?}",
            );
            assert!(delay <= std::time::Duration::from_secs(5));
        }
    }
}

#[test]
fn every_client_error_has_a_deliberate_link_class() {
    use tilo_relay_protocol::auth::AuthError;
    use tilo_relay_session::ClientError;

    // Reachability: retried with backoff, never counted as a refusal.
    assert_eq!(
        link_error(ClientError::UnsupportedProtocol),
        LinkError::Unavailable("host does not speak tilo-relay/1".into()),
    );
    assert_eq!(
        link_error(ClientError::Transport("timed out".into())),
        LinkError::Unavailable("timed out".into()),
    );
    // The device's own signer.
    assert_eq!(
        link_error(ClientError::Sign("locked".into())),
        LinkError::Sign("locked".into()),
    );
    // An answer that does not decode says nothing about this device.
    assert_eq!(
        link_error(ClientError::Codec("bad ack".into())),
        LinkError::Malformed("bad ack".into()),
    );
    // The host turned this device away without a reason.
    assert_eq!(
        link_error(ClientError::Auth(AuthError::Revoked)),
        LinkError::Refused("auth: peer device is revoked".into()),
    );
    assert_eq!(
        link_error(ClientError::Io(std::io::ErrorKind::UnexpectedEof.into())),
        LinkError::Refused("io: unexpected end of file".into()),
    );
}
