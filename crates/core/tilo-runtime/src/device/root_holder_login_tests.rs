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

/// The server of a site, its endpoint bound before it serves: its key, certificate and endpoint.
async fn site_endpoint() -> (SigningKey, DeviceCert, Endpoint) {
    let site = SigningKey::from_bytes(&[41; 32]);
    let server_key = SigningKey::from_bytes(&[42; 32]);
    let server_cert = certified(&site, &server_key, "laptop stack", false);
    let server_endpoint = endpoint(&server_key).await;
    (server_key, server_cert, server_endpoint)
}

/// The site's server serving on `server_endpoint`, taking the work `work` names from others: the
/// host, and what must live as long as it does.
async fn site_server(
    (server_key, server_cert, server_endpoint): (SigningKey, DeviceCert, Endpoint),
    work: tilo_server_protocol::WorkSetting,
) -> (Arc<WorkspaceService>, (Router, tempfile::TempDir)) {
    let state = tempfile::tempdir().unwrap();
    let host = Arc::new(
        WorkspaceService::open(
            ServiceConfig {
                reaching: tilo_workspace_service::Reaching::OnItsOwn,
                founding_registries: tilo_workspace_service::service::RegistriesFile::default(),
                ..ServiceConfig::new(state.path().join("server"), work)
            },
            ServiceIdentity {
                user_public_key: server_cert.user_public_key,
                device_signing_key: server_key,
                device_cert: server_cert,
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
            Router::builder(server_endpoint),
            |router, (alpn, handler)| router.accept(alpn, handler),
        )
        .spawn();
    (host, (router, state))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_holding_the_root_logs_in_at_a_server_by_its_line_and_is_asked_for_content() {
    let (server_key, server_cert, server_endpoint) = site_endpoint().await;
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
    let address = tilo_server_client::ServerAddress::at(addr(&server_endpoint));

    // Alice founds "Company" on her desktop, a device her root certified, under her own root.
    let alice = SigningKey::from_bytes(&[7; 32]);
    let desktop_key = SigningKey::from_bytes(&[9; 32]);
    let desktop_cert = certified(&alice, &desktop_key, "Alice's desktop", true);
    let desktop_state = tempfile::tempdir().unwrap();
    let desktop = super::workspaces::ServiceWorkspaces::open_for_test(
        &desktop_state.path().join("desktop"),
        &desktop_key,
        &desktop_cert,
        &address,
    )
    .await
    .expect("the desktop's own service opens");
    let company = desktop
        .service()
        .workspaces()
        .create("Company")
        .await
        .expect("Alice's device founds her workspace");

    // The site's operator has its server keep Alice's workspace, and it keeps people's own
    // workspaces, as a site's server does: what lets it answer a person's list of theirs.
    let (host, _serving) = site_server(
        (server_key, server_cert.clone(), server_endpoint.clone()),
        tilo_server_protocol::WorkSetting {
            from: tilo_server_protocol::WorkFrom::Workspaces([company.workspace_id].into()),
            ..tilo_server_protocol::WorkSetting::own_person()
        },
    )
    .await;
    let workspaces = host.workspaces();
    assert!(
        workspaces
            .offer_own_workspaces(tilo_workspace_service::workspaces::OwnWorkspaceOffer::free(
                "site.test"
            ))
            .unwrap()
    );
    // Her desktop hands the workspace to the site's server by its line, as `servers add` does,
    // and then stops: what the server asks for later, only the laptop answers.
    desktop
        .service()
        .workspaces()
        .add_server(
            &company.workspace,
            &line,
            tilo_peer_state::CopyRole::KeyHolder,
        )
        .await
        .expect("the desktop hands the workspace to the site's server");
    desktop.shutdown().await;

    // Alice's laptop, which holds her root: its own device, certified by it.
    let laptop_key = SigningKey::from_bytes(&[8; 32]);
    let laptop_cert = certified(&alice, &laptop_key, "Alice's laptop", true);

    let login = the_laptop_logs_in(
        (&laptop_key, &laptop_cert),
        (&line, &company.workspace),
        workspaces,
    )
    .await;

    the_daemon_enters_and_the_servers_asks_reach_the_laptop(
        (&laptop_key, &laptop_cert),
        &login,
        (&company.workspace, company.workspace_id),
        workspaces,
    )
    .await;
}

/// Alice's laptop logs in at the site's server with nothing but the server's own line: her root
/// certified the laptop already, the login is live, and the workspace's roster names it as the
/// server's sync with its copy admits it.
async fn the_laptop_logs_in(
    (laptop_key, laptop_cert): (&SigningKey, &DeviceCert),
    (line, workspace): (&str, &str),
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
) -> tilo_device_identity::login::Device {
    let login = tilo_test_env::within(
        "the laptop logs in at the server",
        tilo_api::daemon::enter::log_in_at(
            laptop_key,
            laptop_cert,
            serde_json::from_str(line).unwrap(),
            None,
        ),
    )
    .await
    .expect("the root's own device logs in with the server's line alone");
    assert!(login.reaches_a_server(), "the login is live: {login:?}");
    assert_eq!(login.workspace, workspace);
    assert_eq!(login.workspace_name.as_deref(), Some("Company"));
    assert_eq!(
        login.cert, *laptop_cert,
        "the root's own certificate, no other"
    );
    assert!(
        workspaces
            .state(workspace)
            .await
            .unwrap()
            .certificates()
            .contains(laptop_cert),
        "the login put the laptop in the workspace's roster"
    );
    login
}

/// The daemon enters the login's workspace as it enters every one, through the laptop's own
/// service, which keeps the workspace's member copy, and then the server's asks for content it
/// lacks reach the laptop.
async fn the_daemon_enters_and_the_servers_asks_reach_the_laptop(
    (laptop_key, laptop_cert): (&SigningKey, &DeviceCert),
    login: &tilo_device_identity::login::Device,
    (workspace, workspace_id): (&str, tilo_server_protocol::WorkspaceId),
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
) {
    let state = tempfile::tempdir().unwrap();
    let own = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        laptop_key,
        laptop_cert,
        &tilo_api::daemon::enter::server_of(login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the daemon's entry",
        tilo_api::daemon::enter::connect(laptop_key, login, Some(own.lent())),
    )
    .await
    .expect("the server lets the root's own device in");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;
    assert!(entered.admitted(workspace_id).await);

    tilo_test_env::within("the server's ask reaches the laptop", async {
        // Each ask names content of its own: one asked lately is not asked again.
        let mut asked = 0_u64;
        loop {
            asked = asked.saturating_add(1);
            let hash = format!("{asked:064x}");
            if workspaces
                .ask_surface(workspace, "so.tilo.notes", &hash)
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
