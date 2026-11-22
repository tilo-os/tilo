//! A logged-in computer's own workspace service holds the workspace its login entered as a member
//! copy synced with the server that hosts it: the session dials from the device's endpoint and
//! reads with its key service, and the copy opens what the server sealed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the server and the device stay up until it ends"
)]

use std::time::Duration;

use automerge::ReadDoc as _;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::Workspaces;

use super::ServiceWorkspaces;

/// A device `root` certified, and its key.
fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// The workspace's layout, a record every member reads and its group seals.
fn layout_route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: workspace.to_owned(),
        document: DESKTOP_DOCUMENT.into(),
    }
}

/// The layout's front as `workspaces` reads it now, when it reads.
fn front(workspaces: &Workspaces, workspace: &str) -> Option<String> {
    let read = ServiceDocuments::of(workspaces.clone())
        .read(workspace, DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    let layout = automerge::AutoCommit::load(&bytes).ok()?;
    match layout.get(automerge::ROOT, "front").ok()? {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Found the layout on `workspaces` with `want` as its front, sealed with the keys its group holds.
async fn found_front(workspaces: &Workspaces, workspace: &str, want: &str) {
    let sealed = tilo_test_env::within("the server holds the layout's keys", async {
        loop {
            if let Some(Ok(keys)) = workspaces
                .own_route_keys(&layout_route(workspace), true)
                .await
            {
                return keys;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let mut document = automerge::AutoCommit::new();
    automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "front", want)
        .unwrap();
    ServiceWrites::of(workspaces.clone())
        .write(
            workspace,
            &document.save(),
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding: true,
                snapshot: true,
                sealed: Some(&sealed),
            },
        )
        .await
        .unwrap_or_else(|refusal| panic!("the server seals the layout: {refusal:?}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_devices_service_holds_its_workspace_and_opens_what_the_server_sealed() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server holds workspaces")
        .clone();
    let workspace = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .enter(&workspace, &laptop_cert)
        .await
        .expect("a device of the person enters the workspace");
    found_front(&hosted, &workspace, "Main").await;

    // The computer's login, as `tilo login` left it, naming the server's address.
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.clone(),
        workspace_name: Some("Studio".into()),
        agent: "Claude Code".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let own = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the login enters its workspace from the device's endpoint",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the login enters");

    let engine = entered.session().engine();
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    assert!(
        engine.workspaces().contains(&id),
        "the session holds the workspace it entered: {:?}",
        engine.workspaces()
    );
    assert!(
        engine.member_of(id).is_some(),
        "the session names where the workspace is served"
    );
    tilo_test_env::within(
        "the device's service holds the workspace",
        own.hold(&entered),
    )
    .await;
    assert!(
        own.copies().is_member(&workspace).await.unwrap(),
        "the workspace the login entered is a member copy here"
    );
    tilo_test_env::within("the member copy opens the server's sealed layout", async {
        loop {
            let (copies, named) = (own.copies().clone(), workspace.clone());
            let read = tokio::task::spawn_blocking(move || front(&copies, &named))
                .await
                .unwrap();
            if read.as_deref() == Some("Main") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    entered.close();
    server.shutdown().await;
}
