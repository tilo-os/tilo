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
        .expect("the server has workspaces")
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

    // The device's one copy of the workspace is its service's: the session keeps none.
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;
    assert!(
        own.copies().is_member(&workspace).await.unwrap(),
        "the workspace the login entered is a member copy here"
    );
    let copy = own
        .peer(id)
        .expect("the service holds the workspace it entered");
    assert!(
        copy.is_member(&laptop_cert.user_public_key),
        "the copy names the person a member"
    );
    assert!(own.holds(id), "held as a member copy");
    assert!(
        own.workspaces()
            .iter()
            .any(|held| held.id().to_string() == workspace),
        "the workspaces this device holds name it"
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

/// A provider registered on a device answers what another device of the workspace asks over
/// `tilo-actions/1` only as this device's own copy of the workspace says: the asking device is
/// refused while its role does not let it run the action, and once the person's server writes it
/// a role that does, the change reaches the copy and the same call is answered. Nobody signs in on
/// this device for any of it: its key and certificate are all it opens the workspace with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one grant, from the server to this device's copy to a provided call, read top to bottom"
)]
async fn a_grant_written_on_the_server_reaches_this_devices_copy_and_admits_a_provided_call() {
    use std::sync::Arc;
    use tilo_actions::remote::{
        ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget, TestProvider,
    };
    use tilo_server_protocol::workspaces::Role;

    const PROVIDER: &str = "so.tilo.devices";
    // The person the workspace fixtures sign pre-admissions as, so a guest is let in as the
    // server's owner lets people in.
    let root = SigningKey::from_bytes(&[1; 32]);
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
        .expect("the server has workspaces")
        .clone();
    let workspace = hosted.create("Home").await.unwrap().workspace;
    hosted
        .enter(&workspace, &laptop_cert)
        .await
        .expect("a device of the person enters the workspace");
    // Another person's device, in the workspace as a viewer.
    let (guest_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let (guest_key, guest_cert) = enrol(&guest_root, "Bo's laptop", DeviceCapabilities::member());
    tilo_workspace_service::workspaces::fixtures::entry_join_as(
        &hosted,
        &server_cert,
        &workspace,
        (&guest_key, &guest_cert),
        81,
        Role::Viewer,
    )
    .await;

    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.clone(),
        workspace_name: Some("Home".into()),
        agent: "tilo".into(),
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
        "the device enters its workspace",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the device enters");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;

    let provided = Arc::new(TestProvider::new());
    own.copies().provide(PROVIDER, Arc::clone(&provided) as _);
    let router = iroh::protocol::Router::builder(own.endpoint())
        .accept(
            tilo_peer_state::actions::ACTIONS_ALPN,
            own.copies().action_service(),
        )
        .spawn();

    let guest_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&guest_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let asking = tilo_peer_state::actions::ActionService::new(
        guest_key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    let laptop = Arc::new(asking.remote(&guest_endpoint, &own.endpoint().addr(), &workspace));
    let ask = || {
        let laptop = Arc::clone(&laptop);
        async move {
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new("devices", "restart"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            tilo_test_env::within(
                "the laptop answers",
                laptop.invoke_until_settled(request, Duration::from_secs(20)),
            )
            .await
            .expect("the laptop answered")
        }
    };

    let refused = tilo_test_env::within("the guest is in the laptop's copy", async {
        loop {
            let answer = ask().await;
            match &answer {
                Err(ActionCallError::Refused { reason }) if reason.contains("do not let") => {
                    return answer;
                }
                // The guest's join has not reached the laptop's copy yet.
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await;
    assert!(refused.is_err(), "a viewer may not restart the device");
    assert!(
        provided.asked().is_empty(),
        "a refused call never reaches the provider"
    );

    hosted
        .set_member_role(
            &workspace,
            &server_cert,
            tilo_server_session::workspaces::SetMemberRoleRequest {
                request_id: [82; 32],
                user_public_key: guest_root.verifying_key().to_bytes(),
                role: Role::Editor,
            },
        )
        .await
        .expect("the server writes the guest a new role");
    tilo_test_env::within("the new role reaches the laptop's copy", async {
        loop {
            if ask().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let caller = provided.callers().pop().expect("the provider was asked");
    assert_eq!(
        caller.device_public_key(),
        Some(&guest_cert.device_public_key),
        "the provider sees the device that asked"
    );

    guest_endpoint.close().await;
    let _ = router.shutdown().await;
    entered.close();
    server.shutdown().await;
}
