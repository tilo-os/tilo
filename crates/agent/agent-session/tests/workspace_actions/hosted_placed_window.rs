//! Which device of its person a turn on the server acts as, where the server has placed a
//! device of its own for the workspace's Browser window.
//!
//! The server certifies a device in its person's name for the window in a workspace's machine,
//! and that device enters the workspace as any session does. It is then the newest device the
//! person has there. A turn the server carries out for the person works on a device of the
//! person's own all the same, and where they have none it works on none.
//!
//! The server, the person and the assistant are those of the test this is a part of. The
//! window is a stand-in the test holds: the device the server placed asks to be entered and
//! joins, as the session in a workspace's machine does, and does nothing else.
use std::time::{Duration, Instant};

use super::*;
use tilo_server::Envelope;
use tilo_server::hosted_browser::{BrowserSessions, HostedBrowsers, Unmetered, Window};
use tilo_server_protocol::hosted::Material;

/// The provider the stand-in window publishes under.
const WINDOW: &str = "4711";

/// Where the server starts a workspace's Browser: nowhere, here. It keeps what the server
/// handed the session, for the stand-in to join as.
#[derive(Clone, Default)]
struct Machines(Arc<Mutex<Vec<Material>>>);

impl BrowserSessions for Machines {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.lock().unwrap().push(material.clone());
        Ok(Window {
            device: material.device.device_public_key,
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, _: &str) {}
}

/// A server that places Browser windows, with the window of its person's workspace placed:
/// the server, what it handed the window's session, and the device a turn of the person acted
/// as before the window was placed.
async fn with_a_placed_window() -> (Hosted, Material, Option<[u8; 32]>) {
    let machines = Machines::default();
    let browsers = Arc::new(HostedBrowsers::new(
        Box::new(machines.clone()),
        Box::new(Unmetered),
        "acct_ada".into(),
    ));
    let mut hosted = Box::pin(Hosted::start_placing(Some(Arc::clone(&browsers)))).await;
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    let person = hosted.root.verifying_key().to_bytes();
    let before = hosted.workspaces.person_device(workspace_id, person).await;
    // The person's own device opens Browser in the workspace.
    let opened = hosted
        .page
        .invoke_on(
            &format!("workspace:{}", hosted.workspace),
            ActionAddress::new("app.so.tilo.browser", "open"),
            InvokeArgument::None,
        )
        .await;
    let deadline = tilo_test_env::hang_deadline();
    while browsers.window(workspace_id).is_none() {
        assert!(
            Instant::now() < deadline,
            "the workspace's Browser window was never placed: {opened:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let material = machines.0.lock().unwrap()[0].clone();
    (hosted, material, before)
}

/// The device the server placed asks its server to enter it into the workspace and joins, as
/// the session in a workspace's machine does: the workspace's governance names it from then
/// on. Its session lasts as long as what this answers is kept.
async fn enters(hosted: &Hosted, material: &Material) -> tilo_server::ServerClient {
    let person = hosted.root.verifying_key().to_bytes();
    let client = tilo_server::ServerClient::connect(
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap(),
        hosted.address.clone(),
        person,
        material.device.clone(),
        material.key().unwrap(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .expect("the window's device dials the server");
    tilo_server::workspaces::enter_invitation(&client, &material.workspace, 600)
        .await
        .expect("the server enters the device it placed into its workspace");
    client
        .send(Envelope::Subscribe {
            workspace_id: material.workspace_id,
        })
        .unwrap();
    loop {
        match tilo_test_env::within("the window joins", client.recv()).await {
            Some(Envelope::Ready { .. }) => break,
            Some(_) => {}
            None => panic!("the window's device was let go before it joined"),
        }
    }
    // The workspace has settled who is in it by the time anyone asks.
    tokio::time::sleep(Duration::from_secs(2)).await;
    client
}

/// The window's device is the newest the person has in the workspace, and a turn of theirs
/// still acts as the device it acted as before the window was placed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_of_the_person_acts_as_their_own_device_beside_a_placed_one() {
    let (hosted, material, before) = Box::pin(with_a_placed_window()).await;
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    let person = hosted.root.verifying_key().to_bytes();
    let _window = enters(&hosted, &material).await;
    let after = hosted.workspaces.person_device(workspace_id, person).await;
    let window = material.device.device_public_key;
    hosted.page.peer.close();
    assert!(
        before.is_some(),
        "the person has a device of their own in the workspace"
    );
    assert_ne!(
        after.map(hex::encode),
        Some(hex::encode(window)),
        "a turn never acts as the device the server placed for the window"
    );
    assert_eq!(
        after.map(hex::encode),
        before.map(hex::encode),
        "a turn acts as the device it acted as before the window was placed"
    );
}

/// A person every one of whose devices in the workspace the server placed has no device of
/// their own there: a turn of theirs acts as none, and never as a placed one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_with_none_but_placed_devices_has_no_device_for_a_turn() {
    let (hosted, material, _) = Box::pin(with_a_placed_window()).await;
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    let person = hosted.root.verifying_key().to_bytes();
    let _window = enters(&hosted, &material).await;
    let window = material.device.device_public_key;
    let placed = hosted
        .server
        .downcast_ref::<tilo_server::daemon::Server>()
        .expect("the server of this test")
        .environments();
    // The server's own list of the devices it placed takes each device the person still has
    // of their own, one after the other, as it took the window's: what a turn acts as is asked
    // after each.
    let scope = tilo_server::environment::EnvironmentScope::for_resource(
        workspace_id,
        "so.tilo.browser",
        &material.record,
    );
    let until_ns = material
        .device
        .expires_at_ns
        .expect("the window's certificate ends");
    let mut own: Vec<[u8; 32]> = Vec::new();
    let mut acts_as = hosted.workspaces.person_device(workspace_id, person).await;
    while let Some(device) = acts_as {
        assert_ne!(
            hex::encode(device),
            hex::encode(window),
            "a turn never acts as the device the server placed for the window"
        );
        assert!(
            own.len() < 8 && !own.contains(&device),
            "a device the server placed is answered no more: {}",
            hex::encode(device)
        );
        own.push(device);
        placed
            .place(device, scope, until_ns)
            .expect("the server keeps what it placed");
        acts_as = hosted.workspaces.person_device(workspace_id, person).await;
    }
    hosted.page.peer.close();
    assert!(
        !own.is_empty(),
        "the person had a device of their own to begin with"
    );
}
