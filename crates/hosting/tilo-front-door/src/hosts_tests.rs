//! The directory as a real host answers it: a person's device hosts their
//! workspaces, and the door, as another device of theirs, lists and creates
//! them through the host's own operations.

use super::*;
use tilo_room_host::host::{HostConfig, HostIdentity, HostMode, WorkspaceHost};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

struct Device {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

fn enrol(root: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Device {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 200,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    Device { key, cert }
}

async fn endpoint(device: &Device) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host lives until its `shutdown`, which consumes it; the lint misses the move"
)]
async fn the_door_creates_and_lists_workspaces_through_the_hosts_own_operations() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let person = hex::encode(root.verifying_key().to_bytes());
    let laptop = enrol(&root, 11, "Laptop");
    let door = enrol(&root, 12, "Tilo agents");

    let state = tempfile::tempdir().unwrap();
    let served = endpoint(&laptop).await;
    let host = WorkspaceHost::open(
        HostConfig::new(state.path().join("host"), HostMode::Device),
        HostIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: laptop.key.clone(),
            device_cert: laptop.cert.clone(),
            revocations: RevocationList::new(),
        },
        served.clone(),
        None,
    )
    .await
    .unwrap();
    let mut router = iroh::protocol::Router::builder(served);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    let router = router.spawn();

    let files = state.path();
    std::fs::write(files.join("door.key"), door.key.to_bytes()).unwrap();
    std::fs::write(
        files.join("door.cert"),
        tilo_user_identity::canonical::canonical_bytes(&door.cert).unwrap(),
    )
    .unwrap();
    std::fs::write(files.join("agents.token"), "proxy-secret\n").unwrap();
    let registry = json!({"people": {person.clone(): {
        "device": {"key_file": files.join("door.key"), "cert_file": files.join("door.cert")},
        "hosts": [{
            "name": "eu1.tilo.test",
            "default": true,
            "device": hex::encode(laptop.cert.device_public_key),
            "addr": host.node_addr(),
            "agents": "http://10.0.0.5:7391/w/{workspace}/mcp",
            "agents_token_file": files.join("agents.token"),
        }],
    }}});
    let path = files.join("hosts.json");
    std::fs::write(&path, registry.to_string()).unwrap();
    let hosts = Hosts::new(path);

    assert_eq!(hosts.workspaces(&person).await.unwrap(), []);
    let created = hosts.create(&person, "Studio", None).await.unwrap();
    assert_eq!(created.name, "Studio");
    assert_eq!(created.host, "eu1.tilo.test");
    assert_eq!(
        hosts.workspaces(&person).await.unwrap(),
        std::slice::from_ref(&created)
    );
    let route = hosts.route(&person, &created.id).await.unwrap();
    assert_eq!(
        route.url,
        format!("http://10.0.0.5:7391/w/{}/mcp", created.id)
    );
    assert_eq!(route.token, "proxy-secret");

    // The host refuses a name it does not take, and says so.
    let refused = hosts.create(&person, "", None).await.unwrap_err();
    assert!(matches!(refused, DirectoryError::Refused(_)), "{refused:?}");
    // A host the person does not have is refused before anything is asked.
    assert!(matches!(
        hosts.create(&person, "Garden", Some("us1.tilo.test")).await,
        Err(DirectoryError::Refused(_))
    ));

    // Someone the door serves no host for has nothing, and can make nothing.
    let stranger = hex::encode([9u8; 32]);
    assert_eq!(hosts.workspaces(&stranger).await.unwrap(), []);
    assert!(matches!(
        hosts.create(&stranger, "Mine", None).await,
        Err(DirectoryError::Refused(_))
    ));
    assert_eq!(
        hosts.route(&stranger, &created.id).await,
        Err(DirectoryError::NotFound)
    );

    router.shutdown().await.unwrap();
    host.shutdown().await;
}
