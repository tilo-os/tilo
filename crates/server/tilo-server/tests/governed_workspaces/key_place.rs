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

use tilo_actions::remote::{CatalogueQuery, RemoteActionFrame, RemoteActionMessage};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_server::{Envelope, Server, ServerConfig, WorkspaceId};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::own_device;

use super::*;

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6b65_795f_6772_6f75);

/// The longest the group's change counter may sit still for it to count as settled.
const SETTLE: Duration = Duration::from_millis(1500);
/// How long the group is watched after the server's actions member joined.
const OBSERVE: Duration = Duration::from_secs(6);
/// The most commits a join may rightly bring: the group was settled before it, and joining
/// the device that already holds a place in it changes no keys.
const AT_MOST: u64 = 2;

/// Start a host that serves its workspaces' records to pages, as a person's server does: the
/// app action server that answers what a device asks of its workspace is the server's own
/// workspace-actions member.
async fn start_serving(state: &std::path::Path, alice: &TestUser, host: &TestDevice) -> Server {
    let catalogue: tilo_agent_session::CatalogueFactory =
        Arc::new(tilo_agent_session::AppCatalogue::native);
    let mut config = ServerConfig {
        state_root: state.to_path_buf(),
        packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
        ..ServerConfig::default()
    };
    config.services.app_serving = true;
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
        None,
    )
    .await
    .unwrap()
    .0
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
        false,
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
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            None,
            &changes,
            founding,
            founding,
            Some(&sealed),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_keeps_one_place_in_a_workspaces_key_group_when_its_actions_member_joins() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = alice.enrol("alice-tablet", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_serving(state.path(), &alice, &host).await;
    let workspaces = server.workspaces().expect("the server holds workspaces");
    crate::common::hold_workspace(&server, WORKSPACE, &[&laptop.cert, &tablet.cert]).await;
    let workspace = WORKSPACE.to_string();

    // The owner's laptop holds the workspace as its own service does: a key holder beside the
    // server, and the group the server's actions member is about to join.
    let hint = workspaces
        .hosts(&workspace)
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == host.device_pubkey())
        .expect("the server names its own address");
    let (laptop_service, laptop_keys) = laptop_service(&state.path().join("laptop"), &laptop).await;
    let copies = laptop_service.workspaces().clone();
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

    let keys = server
        .workspace_keys()
        .expect("the server governs its workspaces and holds their keys");
    let quiet = settled(&keys, "before the server's actions member joined it").await;

    // A device of the owner's asks the workspace about one record: the server answers as a
    // member of it, joining the workspace and its key group as the server's own device.
    let client = super::governance_actions::subscribed(&server, &tablet, WORKSPACE).await;
    let query = CatalogueQuery {
        provider: Some("record:so.tilo.notes:key-group-probe".into()),
        ..CatalogueQuery::default()
    };
    client
        .send(Envelope::RemoteAction {
            workspace_id: WORKSPACE,
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
    crate::common::wait_until_subscribed(&server, WORKSPACE, &host.device_pubkey()).await;

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

    drop(laptop_keys);
    laptop_service.shutdown().await;
    server.shutdown().await;
}
