use super::*;
use std::os::unix::fs::PermissionsExt;
pub(in crate::https_owner) async fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let (transport_root, prepared) = crate::https_owner::prepared_transport_test().await;
    drop(prepared);
    // Quinn releases its prepared UDP endpoint asynchronously. This fixture
    // prepares a new listener, so choose a fresh simultaneously free TCP/UDP
    // address instead of racing the retired endpoint's destructor.
    assign_free_origin(&transport_root.path().join("operator.json"));
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
    (transport_root, store_root, config, ready)
}
/// Points the transport config at a TCP/UDP address that is free right now.
/// The address is released before the service binds it, so another test
/// process can take it in between; a caller that sees the service fail with
/// "Address already in use" assigns another one and starts again.
pub(in crate::https_owner) fn assign_free_origin(transport_config: &std::path::Path) {
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
    drop(tcp);
    drop(udp);
}
#[tokio::test]
async fn cloud_https_startup_prepares_original_transport_without_owner_or_ready() {
    let (_transport_root, root, config, ready) = fixture().await;
    let prepared = PreparedCloud::prepare(&config, &ready).await.unwrap();
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
    let (_transport_root, root, config, ready) = fixture().await;
    let result = serve_until(&config, &ready, std::future::pending())
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
    let (_transport_root, _root, config, ready) = fixture().await;
    std::fs::write(&ready, b"retained other boot").unwrap();
    assert!(PreparedCloud::prepare(&config, &ready).await.is_err());
    assert_eq!(std::fs::read(&ready).unwrap(), b"retained other boot");
}
