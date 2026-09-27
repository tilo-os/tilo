use super::*;
use std::os::unix::fs::PermissionsExt;
pub(in crate::https_owner) async fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::net::TcpListener,
    std::net::UdpSocket,
) {
    let (transport_root, prepared) = crate::https_owner::prepared_transport_test().await;
    drop(prepared);
    // Quinn releases its prepared UDP endpoint asynchronously. This fixture
    // prepares a new listener, so choose a fresh simultaneously free TCP/UDP
    // address instead of racing the retired endpoint's destructor. Keep both
    // sockets: the service takes them via the bound entry point.
    let (tcp, udp) = bind_free_origin(&transport_root.path().join("operator.json"));
    let store_root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(store_root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = store_root.path().join("https-owner.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "state_root": store_root.path(), "machine_id": "https-owner",
            "transport_config": transport_root.path().join("operator.json"),
            "initial_owner_root": hex::encode([7; 32]),
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let ready = store_root.path().join("ready.json");
    (transport_root, store_root, config, ready, tcp, udp)
}

/// Points the transport config at a TCP/UDP address that is free right now and
/// returns the bound sockets. Callers hand them to the service so the origin
/// is never released between reservation and listen.
pub(in crate::https_owner) fn bind_free_origin(
    transport_config: &std::path::Path,
) -> (std::net::TcpListener, std::net::UdpSocket) {
    // The kernel picks a free UDP port, but its TCP twin may still be held:
    // a just dropped transport's listener winds down asynchronously, and the
    // kernel may hand that just-freed UDP port straight back. Keep every
    // refused candidate bound until a pair is found, so each retry gets a
    // different port.
    let mut refused = Vec::new();
    let (udp, address, tcp) = loop {
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = udp.local_addr().unwrap();
        match std::net::TcpListener::bind(address) {
            Ok(tcp) => break (udp, address, tcp),
            Err(error) => {
                assert!(refused.len() < 64, "no free TCP/UDP loopback pair: {error}");
                refused.push(udp);
            }
        }
    };
    drop(refused);
    let mut transport: serde_json::Value =
        serde_json::from_slice(&std::fs::read(transport_config).unwrap()).unwrap();
    transport["origin"] = serde_json::json!(format!("https://{address}"));
    std::fs::write(transport_config, serde_json::to_vec(&transport).unwrap()).unwrap();
    (tcp, udp)
}

#[tokio::test]
async fn cloud_https_startup_prepares_original_transport_without_owner_or_ready() {
    let (_transport_root, root, config, ready, tcp, udp) = fixture().await;
    let prepared = PreparedCloud::prepare_bound(&config, &ready, tcp, udp)
        .await
        .unwrap();
    let pool = prepared.transport.host_bytes().unwrap();
    assert!(prepared.transport.host_work().unwrap().same_pool(&pool));
    assert_eq!(prepared.config.machine_id, "https-owner");
    assert_eq!(prepared.ready_name, "ready.json");
    prepared
        .ready_directory
        .validate_privacy(atomic_file::Privacy::OwnerOnly)
        .unwrap();
    assert!(!root.path().join("state.json").exists());
    assert!(!root.path().join("state.lock").exists());
    assert!(!ready.exists());
}

#[tokio::test]
async fn cloud_https_startup_unqualified_owner_never_provisions_or_publishes_ready() {
    let (_transport_root, root, config, ready, tcp, udp) = fixture().await;
    let result = serve_until_bound(&config, &ready, Some((tcp, udp)), std::future::pending())
        .await
        .unwrap_err();
    assert!(
        result
            .to_string()
            .contains("existing enrolled Cloud Store required")
    );
    assert!(!root.path().join("state.json").exists());
    assert!(!root.path().join("state.lock").exists());
    assert!(!ready.exists());
}

#[tokio::test]
async fn cloud_https_startup_ready_destination_cannot_replace_another_owner_record() {
    let (_transport_root, _root, config, ready, tcp, udp) = fixture().await;
    std::fs::write(&ready, b"retained other boot").unwrap();
    assert!(
        PreparedCloud::prepare_bound(&config, &ready, tcp, udp)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&ready).unwrap(), b"retained other boot");
}

/// A held origin cannot be bound from config alone. The service must take the
/// pre-bound sockets; releasing and retrying another port is not allowed.
#[tokio::test]
async fn cloud_https_prebound_sockets_required_when_origin_is_held() {
    let (_transport_root, _root, config, ready, tcp, udp) = fixture().await;
    let reserved = {
        let transport: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(&config).unwrap())
                    .unwrap()["transport_config"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        transport["origin"].as_str().unwrap().to_owned()
    };
    let address = tcp.local_addr().unwrap();
    assert!(
        std::net::TcpListener::bind(address).is_err(),
        "reservation must keep the TCP origin held"
    );
    assert!(
        std::net::UdpSocket::bind(address).is_err(),
        "reservation must keep the UDP origin held"
    );
    let error = match PreparedCloud::prepare(&config, &ready).await {
        Ok(_) => panic!("expected bind failure while sockets are held"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("Address already in use"),
        "binding from config must fail while sockets are held, got {error}"
    );
    let prepared = PreparedCloud::prepare_bound(&config, &ready, tcp, udp)
        .await
        .unwrap();
    assert_eq!(prepared.transport.origin(), reserved);
}
