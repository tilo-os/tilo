//! `tilo device add` on one computer and `tilo login <ticket>` on another: the ticket's
//! computer is certified by the person's root and enters their workspace as their device.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

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

/// The computer's login, as `tilo login` left it, naming `server`'s address: the device whose
/// certificate is `laptop`, in `workspace`.
fn login_to(
    server: &tilo_server::daemon::Server,
    workspace: &str,
    (laptop_cert, server_cert): (&DeviceCert, &DeviceCert),
) -> tilo_device_identity::login::Device {
    tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.to_owned(),
        workspace_name: Some("Studio".into()),
        agent: "Claude Code".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    }
}

/// Call `action` of the ticket's provider on the device that issued `ticket`, as the computer
/// with `key` and no certificate, in `scope`: how it ended.
async fn ask_issuer(
    ticket: &tilo_device_identity::login::Ticket,
    key: &SigningKey,
    scope: &str,
    action: &str,
) -> Option<Result<tilo_actions::InvocationUpdate, tilo_actions::remote::ActionCallError>> {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    use tilo_device_identity::login::{PROVIDER, RESOURCE};
    let dialed =
        tilo_server_client::ServerAddress::at(crate::login::address_of(&ticket.issuer).unwrap());
    let endpoint = dialed.bind(Some(key.to_bytes())).await.unwrap();
    let service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    let device = Arc::new(service.remote(&endpoint, &dialed.addr(), scope));
    let input = tilo_device_identity::login::CertifyInput {
        secret: ticket.secret_hex(),
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        agent: "Codex".into(),
        label: None,
    };
    let serde_json::Value::Object(argument) = serde_json::to_value(&input).unwrap() else {
        panic!("the input is an object");
    };
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let settled = device
        .invoke_until_settled(request, Duration::from_secs(10))
        .await;
    endpoint.close().await;
    settled
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one person adding a computer, from the ticket to its entry, read top to bottom"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the server answers the laptop's ticket and the desk's entry until it shuts down"
)]
async fn a_computer_logged_in_with_a_ticket_enters_as_the_persons_device() {
    use tilo_actions::remote::ActionCallError;
    use tilo_device_identity::login::{CERTIFY, SCOPE};

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig {
            state_root: state.path().join("server"),
            ..Default::default()
        },
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

    let laptop_login = login_to(&server, &workspace, (&laptop_cert, &server_cert));

    // `tilo device add` on the laptop: a ticket its root approves.
    let issued = crate::device::issue(
        &laptop_login,
        Arc::new(crate::device::RootApprover {
            root: root.clone(),
            device: laptop_key.clone(),
            cert: laptop_cert.clone(),
        }),
    )
    .await
    .unwrap();
    let text = issued.ticket.encode();
    let ticket = tilo_device_identity::login::Ticket::parse(&text).unwrap();
    let desk_key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));

    // A computer with no certificate gets nothing but the certify action.
    let other = ask_issuer(&ticket, &desk_key, SCOPE, "list").await;
    assert!(
        matches!(other, Some(Err(ActionCallError::UnknownAction))),
        "another action of the ticket's provider is refused: {other:?}"
    );
    let in_workspace = ask_issuer(&ticket, &desk_key, &workspace, CERTIFY).await;
    assert!(
        matches!(
            in_workspace,
            Some(Err(
                ActionCallError::NotPermitted | ActionCallError::Refused { .. }
            ))
        ),
        "a call in the person's workspace is refused to a device it does not know: {in_workspace:?}"
    );
    // A wrong secret is refused.
    let mut wrong = ticket.clone();
    wrong.secret = [7; 32];
    let refused = crate::login::certified_with(&wrong, &desk_key, "Codex", "Codex on desk")
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("didn't take this ticket"), "{refused}");

    // `tilo login <ticket>` on the desk.
    let answer = crate::login::certified_with(&ticket, &desk_key, "Codex", "Codex on desk")
        .await
        .expect("the laptop certifies the desk");
    assert!(answer.cert.is_root_signed_device());
    assert_eq!(answer.cert.user_public_key, root.verifying_key().to_bytes());
    assert_eq!(
        answer.cert.device_public_key,
        desk_key.verifying_key().to_bytes()
    );
    assert_eq!(answer.enter.workspace, workspace);
    let given = crate::device::certified(&issued).await.unwrap();
    assert_eq!(
        given, answer.cert,
        "the laptop says which computer it added"
    );
    // The ticket is spent.
    let spent_key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let spent = crate::login::certified_with(&ticket, &spent_key, "Codex", "Codex on desk")
        .await
        .unwrap_err()
        .to_string();
    assert!(spent.contains("didn't take this ticket"), "{spent}");
    issued.end().await;

    // The desk enters as the person's device: it lists their workspaces.
    let desk_login = crate::login::device_of(answer.clone(), "Codex");
    let desk = tilo_test_env::within(
        "the desk enters",
        tilo_api::daemon::enter::connect(&desk_key, &desk_login, None),
    )
    .await
    .expect("the desk enters as the person's device");
    let listed = desk
        .personal_list(
            &tilo_server_session::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .expect("the desk reads the person's workspaces");
    assert!(
        listed
            .workspaces
            .iter()
            .any(|known| known.workspace == workspace),
        "the desk lists the person's workspace: {:?}",
        listed.workspaces
    );

    desk.close();
    server.shutdown().await;
}
