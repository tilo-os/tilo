//! The place the server's device holds in a governed workspace's key group.
//!
//! The workspace-actions member the server runs joins each workspace it answers for as the
//! server's own device. A device holds one place in a workspace's key group, so a member that
//! joined as a second participant under that device key would take the device's place from the
//! server's own key service and back, once per ask: the group would commit that device in and
//! out for as long as the workspace lives. Here the group is watched while the member joins a
//! workspace the server shares with the owner's laptop, whose own key service takes part in the
//! group beside the server's.
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ReadDoc as _};
use tilo_actions::remote::{CatalogueQuery, RemoteActionFrame, RemoteActionMessage};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_server::{Envelope, Server, ServerClient, ServerConfig, WorkspaceId};
use tilo_session_client::device::Device;
use tilo_session_client::{Cmd, MemoryPeerStore, Outbox as _, PeerConfig, PeerStore};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::own_device;

use super::*;

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6b65_795f_6772_6f75);

/// The longest the group's change counter may sit still for it to count as settled.
const SETTLE: Duration = Duration::from_millis(1500);
/// How long the group is watched after the server's actions member joined.
const OBSERVE: Duration = Duration::from_secs(6);
/// How long the group is watched after the member joins a server that restarted: long enough
/// for the laptop's member copy to dial the restarted server again (it redials five seconds
/// after its link ends) and for what that brings to settle.
const OBSERVE_RESTARTED: Duration = Duration::from_secs(12);
/// The most commits a join may rightly bring: the group was settled before it, and joining
/// the device that already holds a place in it changes no keys.
const AT_MOST: u64 = 2;
/// The most epochs the workspace's key group may move across a server's restart and its
/// member joining again.
const EPOCH_MOVE: u64 = 1;

/// Start a host that serves its workspaces' records to pages, as a person's server does, with
/// `config`: the app action server that answers what a device asks of its workspace is the
/// server's own workspace-actions member.
async fn start_serving_with(
    state: &std::path::Path,
    alice: &TestUser,
    host: &TestDevice,
    mut config: ServerConfig,
) -> Server {
    config.state_root = state.to_path_buf();
    config.services.app_serving = true;
    let catalogue: tilo_agent_session::CatalogueFactory =
        Arc::new(tilo_agent_session::AppCatalogue::native);
    tilo_server::agents::start_serving_documents(
        config,
        tilo_server::daemon::ServerIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        Some(catalogue),
        None,
        None,
    )
    .await
    .unwrap()
    .0
}

/// [`start_serving_with`], reached through the packet relay this host runs itself.
async fn start_serving(state: &std::path::Path, alice: &TestUser, host: &TestDevice) -> Server {
    start_serving_with(
        state,
        alice,
        host,
        ServerConfig {
            packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
            ..ServerConfig::default()
        },
    )
    .await
}

/// [`start_serving_with`], reached at `listen` through this machine's loopback packet relay: a
/// restarted host that keeps the address, and so the governance, of the one it replaces.
async fn start_serving_at(
    state: &std::path::Path,
    alice: &TestUser,
    host: &TestDevice,
    listen: std::net::SocketAddr,
) -> Server {
    if listen.port() != 0 {
        // The stopped server's connections let go of its socket shortly after its shutdown returns.
        tilo_test_env::wait_for("the stopped server's address to be free", || {
            std::net::UdpSocket::bind(listen).ok().map(drop)
        });
    }
    start_serving_with(
        state,
        alice,
        host,
        ServerConfig {
            iroh_listen: listen,
            packet_relay_url: Some(test_server::loopback_url()),
            ..ServerConfig::default()
        },
    )
    .await
}

/// The owner's laptop, as a device holds a workspace itself: its own service, holding a
/// member copy of the workspace, and the key service that takes part in the workspace's key
/// group beside the server's.
async fn laptop_service(
    state: &std::path::Path,
    laptop: &TestDevice,
) -> (
    tilo_server::service::WorkspaceService,
    own_device::KeyService,
) {
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(laptop.sk.clone());
    let keys = own_device::KeyService::open(
        "laptop-groups",
        &state.join("groups"),
        key_group::VaultKey::new([5; 32]),
        Arc::clone(&signer),
        Arc::new(tilo_workspace_keys::NoSealChecks),
    )
    .expect("the laptop's key service opens");
    let service = own_device::open(
        state.join("service"),
        own_device::Device {
            cert: &laptop.cert,
            signer: &signer,
        },
        device_endpoint(laptop).await,
        keys.groups(),
        |config| config,
    )
    .await
    .expect("the laptop's own service opens");
    keys.attach(service.workspaces());
    (service, keys)
}

/// The workspace's layout, a record every member reads and the workspace's group seals.
fn layout_route() -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: WORKSPACE.to_string(),
        document: DESKTOP_DOCUMENT.into(),
    }
}

/// The layout as `workspaces` reads it now, when it reads.
fn layout(workspaces: &tilo_server::workspaces::Workspaces) -> Option<automerge::AutoCommit> {
    let read = ServiceDocuments::of(workspaces.clone())
        .read(&WORKSPACE.to_string(), DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    automerge::AutoCommit::load(&bytes).ok()
}

/// Write the layout's `front` on `workspaces`, sealed with the keys its device's key service
/// holds for the layout's group: what has that key service take part in the workspace's group.
async fn write_front(workspaces: &tilo_server::workspaces::Workspaces, what: &str, want: &str) {
    let sealed = tilo_test_env::within(what, async {
        loop {
            if let Some(Ok(keys)) = workspaces.own_route_keys(&layout_route(), true).await {
                return keys;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let read = workspaces.clone();
    let held = tokio::task::spawn_blocking(move || layout(&read))
        .await
        .unwrap();
    let founding = held.is_none();
    let mut document = held.unwrap_or_default();
    let heads = document.get_heads();
    automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "front", want)
        .unwrap();
    let changes = if founding {
        document.save()
    } else {
        document.save_after(&heads)
    };
    ServiceWrites::of(workspaces.clone())
        .write(
            &WORKSPACE.to_string(),
            &changes,
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding,
                snapshot: founding,
                sealed: Some(&sealed),
            },
        )
        .await
        .unwrap_or_else(|refusal| panic!("{what}: {refusal:?}"));
}

/// Wait until the server's key service has not changed `workspace`'s group for [`SETTLE`]:
/// everything the group had to commit, it committed. Answers the change count it settled at.
async fn settled(keys: &tilo_workspace_keys::SharedKeys, what: &str) -> u64 {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("the test's deadline fits");
    let mut last = keys.workspace_change(WORKSPACE);
    let mut since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let change = keys.workspace_change(WORKSPACE);
        if change != last {
            last = change;
            since = Instant::now();
        } else if since.elapsed() >= SETTLE {
            return change;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace's key group never settled {what}: it kept committing"
        );
    }
}

/// Let `laptop` hold `WORKSPACE` as a device holds a workspace itself: its own service keeps a
/// member copy of the server's, and its key service takes part in the workspace's key group
/// beside the server's, sealing the layout with it.
async fn laptop_holds(
    state: &std::path::Path,
    server: &Server,
    laptop: &TestDevice,
) -> (
    tilo_server::service::WorkspaceService,
    own_device::KeyService,
) {
    let workspaces = server.workspaces().expect("the server holds workspaces");
    let own_device = workspaces.host_device();
    let hint = workspaces
        .hosts(&WORKSPACE.to_string())
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == own_device)
        .expect("the server names its own address");
    let (service, keys) = laptop_service(state, laptop).await;
    let copies = service.workspaces().clone();
    assert_eq!(
        tilo_test_env::within(
            "the laptop holds the workspace as a member copy",
            copies.hold_as_member(WORKSPACE, &hint),
        )
        .await
        .expect("the server admits the laptop's membership"),
        WORKSPACE
    );
    write_front(&copies, "the laptop seals the layout", "Main").await;
    (service, keys)
}

/// Make the server's workspace-actions member join `workspace` as the server's own device: a
/// device of the workspace asks it, through the server's action API, about one record.
async fn member_joins(
    server: &Server,
    asking: &TestDevice,
    host: &TestDevice,
    workspace: WorkspaceId,
) -> ServerClient {
    let client = super::governance_actions::subscribed(server, asking, workspace).await;
    let query = CatalogueQuery {
        provider: Some("record:so.tilo.notes:key-group-probe".into()),
        ..CatalogueQuery::default()
    };
    client
        .send(Envelope::RemoteAction {
            workspace_id: workspace,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query,
            })
            .encode()
            .unwrap(),
        })
        .unwrap();
    crate::common::wait_until_subscribed(server, workspace, &host.device_pubkey()).await;
    client
}

/// The epoch `keys` holds `workspace`'s key group at now, if it is in it.
fn epoch_held(keys: &tilo_workspace_keys::SharedKeys) -> Option<u64> {
    match keys.access(WORKSPACE).0 {
        tilo_workspace_keys::KeyAccess::Ready(held) => Some(held.epoch()),
        _ => None,
    }
}

/// [`epoch_held`], waiting until `keys` takes part in the workspace's group and holds it, or
/// failing naming `what` with the access it found instead.
async fn held_epoch(keys: &tilo_workspace_keys::SharedKeys, what: &str) -> u64 {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .expect("the test's wait fits");
    loop {
        match keys.access(WORKSPACE).0 {
            tilo_workspace_keys::KeyAccess::Ready(held) => return held.epoch(),
            other => {
                assert!(
                    Instant::now() < deadline,
                    "the server holds no place in the workspace's key group {what}: {other:?}, \
                     its record keys {record:?}",
                    record = keys
                        .record_keys(WORKSPACE)
                        .map(|held| held.map(|held| held.epoch()))
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

/// The epoch the server's key service holds the workspace's group at, once its change counter
/// has sat still for [`SETTLE`]: everything the group had to commit, it committed.
async fn settled_epoch(server: &Server, what: &str) -> u64 {
    let keys = server
        .workspace_keys()
        .expect("the server governs its workspaces and holds their keys");
    settled(&keys, what).await;
    held_epoch(&keys, what).await
}

/// The layout's `front`, as `doc` holds it.
fn front(doc: &AutoCommit) -> Option<String> {
    doc.get(automerge::ROOT, "front")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// The layout as `device`'s session reads it: what only a session the workspace's group
/// admitted opens.
fn read_front(device: &Device) -> Option<String> {
    let workspace = WORKSPACE.to_string();
    let read = tilo_workspace_service::documents::session_page::page_holder(
        device.session().engine(),
        tilo_desktop_model::WorkspaceId::parse(&workspace)?,
    )?
    .read(&workspace, DESKTOP_APP, DESKTOP_DOCUMENT, None)
    .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    front(&AutoCommit::load(&bytes).ok()?)
}

/// A grant device's session over the store its grant keeps, as the API server opens one for a
/// run.
fn grant_session(
    server: &Server,
    host: &TestDevice,
    grant: &TestDevice,
    store: Arc<dyn PeerStore>,
) -> Device {
    Device::kept_in(
        PeerConfig {
            host: tilo_server_client::ServerAddress::at(server.node_addr()),
            cert: grant.cert.clone(),
            server_cert: host.cert.clone(),
            revocations: RevocationList::new(),
            entering: None,
        },
        Arc::new(grant.sk.clone()),
        "grant",
        store,
    )
}

/// Join the workspace and open the sealed layout, waiting until the session reads it.
async fn reads_the_layout(device: &Device, what: &str) -> String {
    device
        .join(&WORKSPACE.to_string())
        .await
        .expect("the workspace admits the device");
    device
        .session()
        .send(Cmd::OpenRoute(layout_route()))
        .expect("the session stays open");
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("the test's wait fits");
    loop {
        if let Some(front) = read_front(device) {
            return front;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: the session did not read the sealed layout"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_keeps_one_place_in_a_workspaces_key_group_when_its_actions_member_joins() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = alice.enrol("alice-tablet", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_serving(state.path(), &alice, &host).await;
    crate::common::hold_workspace(&server, WORKSPACE, &[&laptop.cert, &tablet.cert]).await;

    // The owner's laptop holds the workspace as its own service does: a key holder beside the
    // server, and the group the server's actions member is about to join.
    let (laptop_service, laptop_keys) =
        laptop_holds(&state.path().join("laptop"), &server, &laptop).await;

    let keys = server
        .workspace_keys()
        .expect("the server governs its workspaces and holds their keys");
    let quiet = settled(&keys, "before the server's actions member joined it").await;

    // A device of the owner's asks the workspace about one record: the server answers as a
    // member of it, joining the workspace and its key group as the server's own device.
    let client = member_joins(&server, &tablet, &host, WORKSPACE).await;

    // The join is the device taking the place it already holds: nothing more is committed for
    // it, and the server keeps the workspace's keys.
    let observing = Instant::now()
        .checked_add(OBSERVE)
        .expect("the test's observation fits");
    while Instant::now() < observing {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after = keys.workspace_change(WORKSPACE);
    assert!(
        after.saturating_sub(quiet) <= AT_MOST,
        "the workspace's key group committed {} times once the server's actions member joined \
         it (from {quiet} to {after}): its device was taken out of its own group and put back",
        after.saturating_sub(quiet)
    );
    assert!(
        keys.record_keys(WORKSPACE).is_some_and(|keys| keys.is_ok()),
        "the server still holds the workspace's keys after its member joined"
    );

    drop(client);
    drop(laptop_keys);
    laptop_service.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_server_keeps_its_place_in_the_key_group_when_its_actions_member_joins_again() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = alice.enrol("alice-tablet", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let listen = "127.0.0.1:0".parse().unwrap();
    let server = start_serving_at(state.path(), &alice, &host, listen).await;
    crate::common::hold_workspace(&server, WORKSPACE, &[&laptop.cert, &tablet.cert]).await;

    // The owner's laptop holds the workspace as its own service does, its key service taking
    // part in the workspace's group beside the server's, and keeps holding it across the
    // server's restart: it dials the address serving the workspace again.
    let (laptop_service, laptop_keys) =
        laptop_holds(&state.path().join("laptop"), &server, &laptop).await;

    // The server's actions member joins before the restart, as it does in any workspace a
    // person asks its host about.
    let client = member_joins(&server, &tablet, &host, WORKSPACE).await;
    drop(client);

    // The group takes up the join as it will; a server that kept the one place its device
    // holds is in it, at the epoch it was.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let keys = server
        .workspace_keys()
        .expect("the server governs its workspaces and holds their keys");
    let before = epoch_held(&keys);
    server.shutdown().await;

    // The server restarts on the same state, its key service's group state under it. Its
    // endpoint is new — the app action service it ran keeps the old one bound — and a device's
    // place in a key group is its device key, not its address.
    let server =
        start_serving_at(state.path(), &alice, &host, "127.0.0.1:0".parse().unwrap()).await;
    let keys = server
        .workspace_keys()
        .expect("the restarted server governs its workspaces and holds their keys");
    let restored = held_epoch(&keys, "as it restarted").await;

    // Its actions member joins again, and the group is watched as the laptop dials back.
    let client = member_joins(&server, &tablet, &host, WORKSPACE).await;
    let observing = Instant::now()
        .checked_add(OBSERVE_RESTARTED)
        .expect("the test's observation fits");
    while Instant::now() < observing {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after = held_epoch(&keys, "after its member joined again").await;

    // (a) The restart and the second join are the device taking the place it already held: the
    // group's epoch does not move beyond the one it was at, and never goes back.
    assert!(
        restored >= before.unwrap_or(restored),
        "the restarted server's key group went back from epoch {} to {restored}",
        before.unwrap_or(restored)
    );
    let baseline = before.unwrap_or(restored);
    let moved = after.saturating_sub(baseline);
    assert!(
        moved <= EPOCH_MOVE,
        "the workspace's key group moved {moved} epochs across the server's restart and its \
         actions member joining again (from {baseline} to {after}): its device was taken out of \
         its own group and put back"
    );
    // (b) It still holds the workspace's record keys: it did not lose its place.
    assert!(
        keys.record_keys(WORKSPACE).is_some_and(|keys| keys.is_ok()),
        "the restarted server holds no record keys for the workspace"
    );

    drop(client);
    drop(laptop_keys);
    laptop_service.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_devices_session_started_again_over_its_kept_store_keeps_its_place_in_the_key_group() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let grant = alice.enrol("api-grant", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = crate::common::start_server(
        ServerConfig::loopback(state.path().to_path_buf()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    crate::common::hold_workspace(&server, WORKSPACE, &[&laptop.cert, &grant.cert]).await;
    let (laptop_service, laptop_keys) =
        laptop_holds(&state.path().join("laptop"), &server, &laptop).await;

    // The one store a grant's sessions keep between them, as the API server keeps it per grant
    // device: a session started again over it asks with the member key the last one held.
    let store: Arc<dyn PeerStore> = Arc::new(MemoryPeerStore::for_session());

    // A run's session: it asks for the workspace's keys, is admitted, and reads the sealed
    // layout.
    let first = grant_session(&server, &host, &grant, Arc::clone(&store));
    assert_eq!(
        reads_the_layout(&first, "the first session reads the layout").await,
        "Main"
    );
    let admitted = settled_epoch(&server, "after the first session was admitted").await;

    // The run ends and the next starts over the same store: the device keeps the one place it
    // holds, so the group commits nothing, and the session reads the record too.
    first.close();
    let second = grant_session(&server, &host, &grant, Arc::clone(&store));
    assert_eq!(
        reads_the_layout(&second, "the session started again reads the layout").await,
        "Main"
    );
    let again = settled_epoch(
        &server,
        "after the session started again over its kept store",
    )
    .await;
    assert_eq!(
        again,
        admitted,
        "the workspace's key group moved {} epochs when the device's session started again over \
         its kept store (from {admitted} to {again}): it asked with a new key, which took the \
         device's place from the session before it",
        again.saturating_sub(admitted)
    );

    // And with the session before it still alive while the next one opens: one device, one
    // place, and the group commits nothing more.
    let overlapping = grant_session(&server, &host, &grant, Arc::clone(&store));
    assert_eq!(
        reads_the_layout(&overlapping, "the overlapping session reads the layout").await,
        "Main"
    );
    let together = settled_epoch(&server, "while two sessions of the device are open").await;
    assert_eq!(
        together,
        admitted,
        "the workspace's key group moved {} epochs while two sessions of the device were open \
         (from {admitted} to {together}): the second asked with a new key",
        together.saturating_sub(admitted)
    );

    overlapping.close();
    second.close();
    drop(laptop_keys);
    laptop_service.shutdown().await;
    server.shutdown().await;
}
