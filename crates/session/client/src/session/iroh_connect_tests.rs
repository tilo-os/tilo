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

/// A session beside its device's own dials its host from the endpoint the device already runs
/// and binds none of its own: the device's key names one endpoint, at its relay and everywhere.
#[tokio::test]
async fn a_session_beside_the_devices_own_dials_from_the_devices_endpoint() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[2; 32]);
    let host = SigningKey::from_bytes(&[3; 32]);
    let server = endpoint(b"dummy-alpn/1").await.unwrap();
    let config = PeerConfig {
        host: Host::at(server.addr()),
        cert: cert(&root, &device).unwrap(),
        host_cert: cert(&root, &host).unwrap(),
        revocations: RevocationList::new(),
        entering: None,
    };
    let own = Arc::new(std::sync::OnceLock::new());
    let mut dial = Dial::of(&[0; 32], Some(Arc::clone(&own)));

    // Before the device's endpoint is bound the host is not reached, and nothing is bound in the
    // endpoint's place.
    let early = connect(&config, &device, &mut dial).await;
    assert!(
        matches!(early, Err(LinkError::Unavailable(_))),
        "a dial before the device's endpoint is bound is one to make again"
    );
    assert!(
        dial.plane()
            .is_some_and(|(dialer, _)| dialer.bound().is_none()),
        "no endpoint is bound in place of the device's"
    );

    let bound = Endpoint::builder(Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .secret_key(iroh::SecretKey::from_bytes(&device.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let sockets = bound.bound_sockets();
    assert!(own.set(bound.clone()).is_ok());
    let hearing = async {
        let incoming = server.accept().await.unwrap();
        let from = incoming.remote_addr();
        assert!(
            incoming.await.is_err(),
            "the dummy ALPN must reject the dial"
        );
        from
    };
    let (from, dialled) = tokio::join!(
        tilo_test_env::within("the host hearing the dial", hearing),
        tilo_test_env::within(
            "the dial from the device's endpoint",
            connect(&config, &device, &mut dial)
        ),
    );
    assert!(
        dialled.is_err(),
        "a host that speaks another protocol is no session"
    );
    let from = match from {
        iroh::endpoint::IncomingAddr::Ip(from) => from,
        _ => panic!("a dial on loopback arrives from an address of its own"),
    };
    assert!(
        sockets.iter().any(|socket| socket.port() == from.port()),
        "the host hears the dial from the device's own endpoint: {from} is none of {sockets:?}"
    );
    let (_, delegation) = dial.plane().expect("the dialer is kept");
    assert!(
        delegation.is_empty(),
        "an endpoint of the device's key speaks for itself"
    );

    // The session ending leaves the device's endpoint open: it is the device's.
    dial.shutdown().await;
    assert!(
        !bound.is_closed(),
        "the device's endpoint outlives a session beside its own"
    );
    bound.close().await;
    server.close().await;
}

/// A session beside its device's own holds no keys and asks for none. The device's member key in
/// each workspace's group is its engine's: a second one, asked for as the same device, had the
/// group's participants commit one in and the other out for as long as both sessions ran.
#[tokio::test]
async fn a_session_beside_the_devices_own_holds_no_keys() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[2; 32]);
    let host = SigningKey::from_bytes(&[3; 32]);
    let config = PeerConfig {
        host: Host::at(iroh::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&host.to_bytes()).public(),
        )),
        cert: cert(&root, &device).unwrap(),
        host_cert: cert(&root, &host).unwrap(),
        revocations: RevocationList::new(),
        entering: None,
    };
    let session = |beside| {
        let signer: Arc<dyn DeviceSigner> = Arc::new(device.clone());
        let store: Arc<dyn PeerStore> = Arc::new(crate::MemoryPeerStore::default());
        session_task(config.clone(), signer, store, beside).0
    };
    let workspace = tilo_desktop_model::WorkspaceId::new();
    let beside = session(Some(Arc::new(std::sync::OnceLock::new())));
    assert!(
        matches!(
            beside.engine.seal_keys(workspace),
            Ok(crate::engine::SealKeys::NoSecretStore)
        ),
        "a session beside its device's own keeps no key service of the device's"
    );
    // The device's own session is the one that asks for its workspaces' keys.
    let own = session(None);
    assert!(
        !matches!(
            own.engine.seal_keys(workspace),
            Ok(crate::engine::SealKeys::NoSecretStore)
        ),
        "a device's own session keeps the device's key service"
    );
}
