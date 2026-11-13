//! Where a host answers, as the workspaces it governs name it: the relay its
//! endpoint is reached through when it has one, and the address it has when
//! it has none.

use super::*;

/// A host answers through a relay from the moment it opens: it waited for its
/// endpoint to name one, so the address its first workspace names for it is
/// the one it had as it opened, relay included.
#[tokio::test]
async fn a_host_reached_through_a_relay_names_it_from_the_start() {
    use tilo_delivery_protocol::workspace_join::{WorkspaceDeviceHintV1, reachable};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let addr = relay.node_addr();
    assert_eq!(
        addr.relay_urls().count(),
        1,
        "a host that opened names the relay it is reached through: {addr:?}"
    );
    let workspaces = relay.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    // What the workspace's governance took down of its host as it was created.
    let hosts = workspaces
        .workspace(&created.workspace)
        .await
        .unwrap()
        .lock()
        .await
        .peer()
        .governance()
        .workspace_hosts();
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    let hint = WorkspaceDeviceHintV1::from_transport_hint(&hosts[0]).unwrap();
    // A workspace says of its host what a stranger can dial of the address
    // the host opened with.
    let named = reachable(&addr);
    assert_eq!(hint.endpoint_addr, named);
    assert_eq!(
        hint.packet_relay_url,
        named
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string)
    );
    relay.shutdown().await;
}

/// A host that comes back answering somewhere else names its new address in
/// the workspace it kept before it serves it: once, in place of the address
/// it named before, and what it answers a joiner is what the workspace says.
#[tokio::test]
async fn a_host_that_comes_back_elsewhere_names_its_new_address_in_the_workspace_it_kept() {
    use tilo_delivery_protocol::workspace_join::{WorkspaceDeviceHintV1, reachable};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let first = relay.node_addr();
    let workspace = relay.workspaces().create("Demo").await.unwrap().workspace;
    relay.shutdown().await;

    // The same host on the same state, behind another relay and another port.
    let relay = test_host(dir.path(), &key, &cert).await;
    let second = relay.node_addr();
    assert_ne!(first, second, "the host came back where it left");
    let workspaces = relay.workspaces();
    let hosts = workspaces
        .workspace(&workspace)
        .await
        .unwrap()
        .lock()
        .await
        .peer()
        .governance()
        .workspace_hosts();
    assert_eq!(hosts.len(), 1, "the host is named once: {hosts:?}");
    assert_eq!(hosts[0].host.device_public_key, cert.device_public_key);
    let hint = WorkspaceDeviceHintV1::from_transport_hint(&hosts[0]).unwrap();
    assert_eq!(hint.endpoint_addr, reachable(&second));
    assert_ne!(hint.endpoint_addr, reachable(&first));
    let served = workspaces.hosts(&workspace).await.unwrap();
    assert_eq!(
        served, hosts,
        "a joiner is answered what the workspace names"
    );
    relay.shutdown().await;
}

/// A host whose endpoint has no relay to reach waits no longer for one than a
/// net report takes, then opens all the same and names the address it has: a
/// desktop on a network with no relay still serves its workspaces.
#[tokio::test]
async fn a_host_whose_endpoint_reaches_no_relay_opens_and_names_the_address_it_has() {
    use tilo_delivery_protocol::workspace_join::{WorkspaceDeviceHintV1, reachable};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .expect("a loopback endpoint binds");
    let host = WorkspaceHost::open(
        HostConfig::new(
            dir.path().to_owned(),
            tilo_relay_protocol::WorkSetting::own_person(),
        ),
        HostIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
    )
    .await
    .expect("a host with no relay to reach opens");
    let addr = host.node_addr();
    assert_eq!(addr.relay_urls().count(), 0, "{addr:?}");
    assert!(addr.ip_addrs().next().is_some(), "{addr:?}");
    let workspaces = host.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let hosts = workspaces.hosts(&created.workspace).await.unwrap();
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    let hint = WorkspaceDeviceHintV1::from_transport_hint(&hosts[0]).unwrap();
    assert_eq!(hint.endpoint_addr, reachable(&addr));
    assert_eq!(hint.packet_relay_url, "iroh:direct");
    host.shutdown().await;
    endpoint.close().await;
}
