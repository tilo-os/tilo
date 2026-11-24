//! A computer that holds its person's root logs in at a server with nothing but the server's own
//! line (what it keeps in `web-host.json`): the root certified the computer's key already, so no
//! ticket is asked for. The login is live, it starts in the workspace the server keeps for the
//! person, and the daemon's entry is let in there, where the server's asks for the content its
//! members hold reach this computer.
//!
//! This is how a person's laptop that keeps a workspace's files, and holds their root, hands those
//! files to a server that has taken the workspace in.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use tilo_server_protocol::web::{PageHost, PageHostAddress};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_service::service::{ServiceConfig, ServiceIdentity, WorkspaceService};

fn certified(root: &SigningKey, key: &SigningKey, label: &str, primary: bool) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

/// A server of a site that keeps people's own workspaces, serving on its endpoint: the host,
/// its certificate and endpoint, and what must live as long as it does.
async fn site_server() -> (
    Arc<WorkspaceService>,
    DeviceCert,
    Endpoint,
    (Router, tempfile::TempDir),
) {
    let site = SigningKey::from_bytes(&[41; 32]);
    let server_key = SigningKey::from_bytes(&[42; 32]);
    let server_cert = certified(&site, &server_key, "laptop stack", false);
    let server_endpoint = endpoint(&server_key).await;
    let state = tempfile::tempdir().unwrap();
    let host = Arc::new(
        WorkspaceService::open(
            ServiceConfig {
                reaching: tilo_workspace_service::Reaching::OnItsOwn,
                ..ServiceConfig::new(
                    state.path().join("server"),
                    tilo_server_protocol::WorkSetting::own_person(),
                )
            },
            ServiceIdentity {
                user_public_key: site.verifying_key().to_bytes(),
                device_signing_key: server_key.clone(),
                device_cert: server_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            server_endpoint.clone(),
            None,
        )
        .await
        .unwrap(),
    );
    let router = host
        .protocols()
        .into_iter()
        .fold(
            Router::builder(server_endpoint.clone()),
            |router, (alpn, handler)| router.accept(alpn, handler),
        )
        .spawn();
    (host, server_cert, server_endpoint, (router, state))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_holding_the_root_logs_in_at_a_server_by_its_line_and_is_asked_for_content() {
    let (host, server_cert, server_endpoint, _serving) = site_server().await;
    // A workspace on it whose owner is Alice.
    let alice = SigningKey::from_bytes(&[7; 32]);
    let workspaces = host.workspaces();
    // It keeps people's own workspaces, as a site's server does: what lets it answer a person's
    // list of theirs.
    assert!(
        workspaces
            .offer_own_workspaces(tilo_workspace_service::workspaces::OwnWorkspaceOffer::free(
                "site.test"
            ))
            .unwrap()
    );
    let company = workspaces.create("Company").await.unwrap();
    workspaces
        .declare_owner(&company.workspace, alice.verifying_key().to_bytes(), None)
        .await
        .unwrap();

    // Alice's laptop, which holds her root: its own device, certified by it.
    let laptop_key = SigningKey::from_bytes(&[8; 32]);
    let laptop_cert = certified(&alice, &laptop_key, "Alice's laptop", true);
    // The server's line, as its `web-host.json` keeps it.
    let line = serde_json::to_string(&PageHost {
        server_cert: server_cert.clone(),
        address: Some(PageHostAddress {
            endpoint_addr: addr(&server_endpoint),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .unwrap();

    let login = tilo_test_env::within(
        "the laptop logs in at the server",
        tilo_api::daemon::enter::log_in_at(
            &laptop_key,
            &laptop_cert,
            serde_json::from_str(&line).unwrap(),
            None,
        ),
    )
    .await
    .expect("the root's own device logs in with the server's line alone");
    assert!(login.reaches_a_server(), "the login is live: {login:?}");
    assert_eq!(login.workspace, company.workspace);
    assert_eq!(login.workspace_name.as_deref(), Some("Company"));
    assert_eq!(
        login.cert, laptop_cert,
        "the root's own certificate, no other"
    );
    // The workspace's roster names the laptop: the server's sync with its copy admits it.
    assert!(
        workspaces
            .state(&company.workspace)
            .await
            .unwrap()
            .certificates()
            .contains(&laptop_cert),
        "the login put the laptop in the workspace's roster"
    );

    // The daemon enters as it enters every login's workspace, through the device's own service,
    // which keeps the workspace's member copy, and is let in.
    let state = tempfile::tempdir().unwrap();
    let own = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the daemon's entry",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the server lets the root's own device in");
    tilo_test_env::within("the device's service keeps the workspace", own.hold(&entered)).await;
    assert!(entered.admitted(company.workspace_id).await);

    // The server's ask for content it lacks reaches the laptop now.
    tilo_test_env::within("the server's ask reaches the laptop", async {
        // Each ask names content of its own: one asked lately is not asked again.
        let mut asked = 0_u64;
        loop {
            asked += 1;
            let hash = format!("{asked:064x}");
            if workspaces
                .ask_surface(&company.workspace, "so.tilo.notes", &hash)
                .await
                > 0
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    entered.close();
}
