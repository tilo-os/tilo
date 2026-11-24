#![allow(
    clippy::significant_drop_tightening,
    reason = "a test keeps what it serves until it ends, as the daemon does"
)]

//! A ticket: `tilo device add` on one computer serves one, and `tilo login` on another becomes
//! one of the person's devices with it. Served on the issuer's one endpoint, and called as the
//! daemon's `devices#logIn` action by the computer that logs in.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_actions::remote::asked::AskedRecords as _;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    APPROVE, CERTIFY, CertifyInput, ISSUE, LOG_IN, PROVIDER, RESOURCE, SCOPE, Ticket,
};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::account::Account;
use super::first_workspace::FirstWorkspace;
use super::ticket::{RootSource, Tickets};

/// A device `root` certified, and its key.
pub(super) fn enrol(
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

/// An endpoint keyed by `key`, reached on this machine alone: the daemon's endpoint, in a test.
pub(super) async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// The computer's login, naming `server`'s address: the device whose certificate is `laptop`, in
/// `workspace`.
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
    ticket: &Ticket,
    key: &SigningKey,
    scope: &str,
    action: &str,
) -> Option<Result<InvocationUpdate, ActionCallError>> {
    let dialed = tilo_server_client::ServerAddress::at(
        tilo_api::daemon::enter::address_of(&ticket.issuer).unwrap(),
    );
    let dialed_endpoint = dialed.bind(Some(key.to_bytes())).await.unwrap();
    let service = ActionService::new(key.clone(), Arc::new(NoActions));
    let device = Arc::new(service.remote(&dialed_endpoint, &dialed.addr(), scope));
    let input = CertifyInput {
        secret: ticket.secret_hex(),
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        agent: "Codex".into(),
        label: None,
        tilo_os: false,
    };
    let serde_json::Value::Object(argument) = serde_json::to_value(&input).unwrap() else {
        panic!("the input is an object");
    };
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
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
    dialed_endpoint.close().await;
    settled
}

/// The daemon's device action `action` with `args`, as its provider answers it: its output, or
/// why it failed.
pub(super) async fn ask_device(
    tickets: &Tickets,
    action: &str,
    args: Value,
) -> Result<Value, String> {
    ask_on(tickets, (RESOURCE, action), args).await
}

/// [`ask_device`], of `action` on `resource`.
pub(super) async fn ask_on(
    tickets: &Tickets,
    (resource, action): (&str, &str),
    args: Value,
) -> Result<Value, String> {
    let Value::Object(argument) = args else {
        panic!("the arguments are an object");
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    tickets
        .invoke(&request, &tilo_actions::remote::Caller::local_system())
        .await
        .expect("the action is answered")
        .map_err(|error| format!("{error:?}"))?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .unwrap_or_else(Instant::now);
    loop {
        match tickets.observe(&invocation) {
            Some(state) => match state.update {
                InvocationUpdate::Succeeded { output, .. } => {
                    return Ok(output.value.unwrap_or_default());
                }
                InvocationUpdate::Failed { error, .. } => return Err(error.message),
                _ => {}
            },
            None => return Err("the daemon kept no such action".into()),
        }
        if Instant::now() >= deadline {
            return Err("the device action did not end".into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The person's own server on this machine's loopback, keeping their workspace "Studio" that
/// their laptop entered, and the laptop's key, certificate and login to it.
pub(super) struct Studio {
    pub(super) server: tilo_server::daemon::Server,
    pub(super) hosted: tilo_workspace_service::workspaces::Workspaces,
    pub(super) workspace: String,
    pub(super) laptop_key: SigningKey,
    pub(super) laptop_cert: DeviceCert,
    pub(super) login: tilo_device_identity::login::Device,
}

/// [`Studio`], for the person whose root is `root`, the server keeping its state in `state`.
pub(super) async fn studio(root: &SigningKey, state: &std::path::Path) -> Studio {
    let (server_key, server_cert) = enrol(root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(root, "laptop", DeviceCapabilities::primary());
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.join("server")),
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
    let login = login_to(&server, &workspace, (&laptop_cert, &server_cert));
    Studio {
        server,
        hosted,
        workspace,
        laptop_key,
        laptop_cert,
        login,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one person adding a computer, from the ticket to its entry, read top to bottom"
)]
async fn a_computer_logged_in_with_a_ticket_enters_as_the_persons_device() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        hosted,
        workspace,
        laptop_key,
        laptop_cert,
        login: laptop_login,
    } = studio(&root, state.path()).await;
    // The server keeps the person's own workspace too, where their list of workspaces is.
    let own = tilo_workspace_service::workspaces::person_workspace::found(&hosted)
        .await
        .expect("the person's own workspace is founded on their server");

    // `tilo device add` on the laptop: its daemon serves a ticket on its endpoint, its root
    // approving the computer that comes with it.
    let laptop_endpoint = endpoint(&laptop_key).await;
    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let service = ActionService::new(laptop_key.clone(), Arc::new(NoActions));
    // The laptop's person's part runs as the device its own ticket made, entering through the
    // server that ticket named.
    let laptop_copies = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop-service"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&laptop_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the laptop's service opens");
    let laptop = Tickets::new(
        (laptop_key.clone(), laptop_cert.clone()),
        laptop_endpoint.clone(),
        Some(Arc::new(crate::device::home::Home::new(
            laptop_copies,
            laptop_cert.clone(),
            Some(laptop_login.clone()),
        ))),
        root_in(identity.path().to_path_buf()),
        None,
        None,
        tilo_device_identity::login::Place::at(
            state.path().join("laptop"),
            tilo_device_identity::IdentityPaths::under(state.path().join("laptop-keys")),
        ),
        service.clone(),
    );
    let router = iroh::protocol::Router::builder(laptop_endpoint.clone())
        .accept(ACTIONS_ALPN, service)
        .spawn();
    let printed = ask_device(&laptop, ISSUE, json!({}))
        .await
        .expect("the laptop holding the person's root issues a ticket");
    let text = printed["ticket"].as_str().unwrap().to_owned();
    let ticket = Ticket::parse(&text).unwrap();
    let (desk_key, desk_cert) = enrol(&root, "the desk", DeviceCapabilities::member());

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

    // `tilo login <ticket>` on the desk: its daemon's `devices#logIn` asks the laptop.
    let desk_endpoint = endpoint(&desk_key).await;
    let keys = tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys"));
    tilo_device_identity::persist::adopt_device_key(&keys, &desk_key.to_bytes()).unwrap();
    let place = tilo_device_identity::login::Place::at(state.path().join("desk"), keys);
    let desk = Tickets::new(
        (desk_key.clone(), desk_cert),
        desk_endpoint.clone(),
        None,
        no_root(),
        None,
        None,
        place.clone(),
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );

    // A wrong secret is refused.
    let mut wrong = ticket.clone();
    wrong.secret = [7; 32];
    let refused = ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": wrong.encode(), "agent": "Codex", "label": "Codex on desk"}),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("didn't take this ticket"), "{refused}");

    let answer = ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "Codex on desk"}),
    )
    .await
    .unwrap_or_else(|why| panic!("the laptop certifies the desk: {why}"));
    assert_eq!(answer["workspace"], workspace);
    // The desk's certificate is filed, its daemon starts again as the device it became, and
    // the ticket is spent. Nothing of the login is written beside the certificate.
    let desk_login = desk
        .logged_in()
        .borrow()
        .clone()
        .expect("the desk's daemon starts again as the device it became");
    assert_eq!(
        desk_login.cert.device_public_key,
        desk_key.verifying_key().to_bytes()
    );
    assert_eq!(
        tilo_device_identity::login::served_cert(place.keys()),
        Some(desk_login.cert.clone()),
        "the certificate is filed"
    );
    assert!(!place.dir().join("login.json").exists());
    let (spent_key, _) = enrol(&root, "another", DeviceCapabilities::member());
    let spent = ask_issuer(&ticket, &spent_key, SCOPE, CERTIFY).await;
    assert!(
        matches!(spent, Some(Err(ActionCallError::Refused { .. }))),
        "the ticket is spent: {spent:?}"
    );

    // The desk's daemon starts again as that device: its workspace service, on the desk's
    // endpoint, holds what the login enters, and its session reads through it.
    desk_endpoint.close().await;
    let desk_service = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("desk-service"),
        &desk_key,
        &desk_login.cert,
        &tilo_api::daemon::enter::server_of(&desk_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the desk's service opens");
    let desk_session = tilo_test_env::within(
        "the desk enters",
        tilo_api::daemon::enter::connect(&desk_key, &desk_login, Some(desk_service.lent())),
    )
    .await
    .expect("the desk enters as the person's device");
    tilo_test_env::within(
        "the desk holds what it entered",
        desk_service.hold(&desk_session),
    )
    .await;
    // The desk takes the person's own workspace in by its id at the device its ticket names,
    // without anything listing it there.
    let own = tilo_server_protocol::WorkspaceId::parse(&own).unwrap();
    assert!(
        desk_session.admitted(own).await,
        "the desk enters the person's own workspace at the server its ticket names"
    );
    let listed = desk_session
        .person_workspaces(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
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

    desk_session.close();
    let _ = router.shutdown().await;
    server.shutdown().await;
}

/// A computer a ticket logged in lists the person's workspaces while it holds none of them yet,
/// and enters them from the login entry alone: neither needs their list of where each is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one computer logging in by ticket, listing and entering, read top to bottom"
)]
async fn a_ticket_logged_in_device_lists_the_persons_workspaces_before_it_holds_one() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        hosted,
        workspace,
        laptop_key,
        laptop_cert,
        login: laptop_login,
    } = studio(&root, state.path()).await;
    // The server keeps the person's own workspace too, where their list of workspaces is.
    let own = tilo_workspace_service::workspaces::person_workspace::found(&hosted)
        .await
        .expect("the person's own workspace is founded on their server");

    // `tilo device add` on the laptop serves a ticket; the desk logs in with it.
    let laptop_endpoint = endpoint(&laptop_key).await;
    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let service = ActionService::new(laptop_key.clone(), Arc::new(NoActions));
    let laptop_copies = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop-service"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&laptop_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the laptop's service opens");
    let laptop = Tickets::new(
        (laptop_key.clone(), laptop_cert.clone()),
        laptop_endpoint.clone(),
        Some(Arc::new(crate::device::home::Home::new(
            laptop_copies,
            laptop_cert.clone(),
            Some(laptop_login.clone()),
        ))),
        root_in(identity.path().to_path_buf()),
        None,
        None,
        tilo_device_identity::login::Place::at(
            state.path().join("laptop"),
            tilo_device_identity::IdentityPaths::under(state.path().join("laptop-keys")),
        ),
        service.clone(),
    );
    let router = iroh::protocol::Router::builder(laptop_endpoint.clone())
        .accept(ACTIONS_ALPN, service)
        .spawn();
    let printed = ask_device(&laptop, ISSUE, json!({}))
        .await
        .expect("the laptop issues a ticket");
    let text = printed["ticket"].as_str().unwrap().to_owned();
    let (desk_key, desk_cert) = enrol(&root, "the desk", DeviceCapabilities::member());
    let desk_endpoint = endpoint(&desk_key).await;
    let keys = tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys"));
    tilo_device_identity::persist::adopt_device_key(&keys, &desk_key.to_bytes()).unwrap();
    let desk = Tickets::new(
        (desk_key.clone(), desk_cert),
        desk_endpoint.clone(),
        None,
        no_root(),
        None,
        None,
        tilo_device_identity::login::Place::at(state.path().join("desk"), keys),
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );
    ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "Codex on desk"}),
    )
    .await
    .unwrap_or_else(|why| panic!("the laptop certifies the desk: {why}"));
    let desk_login = desk
        .logged_in()
        .borrow()
        .clone()
        .expect("the desk starts again as the device it became");
    desk_endpoint.close().await;

    // The desk's own list, asked as the account alone while it holds no workspace of the
    // person's: what a plain connection reaches before it entered one, never admitted into one.
    let listing = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("desk-listing"),
        &desk_key,
        &desk_login.cert,
        &tilo_api::daemon::enter::server_of(&desk_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the desk's service opens");
    // The desk enters at its server, which lists it as a device of the person's, and holds the
    // workspace its login names: the person's own workspace, where their list of workspaces is,
    // is not held here yet, so nothing connects into it.
    let session = tilo_test_env::within(
        "the desk enters",
        tilo_api::daemon::enter::connect(&desk_key, &desk_login, Some(listing.lent())),
    )
    .await
    .expect("the desk enters as the person's device");
    tilo_test_env::within("the desk holds what it entered", listing.hold(&session)).await;
    assert!(
        listing
            .copies()
            .is_member(&workspace)
            .await
            .expect("the desk's copies answer"),
        "the workspace its login names is held"
    );
    assert!(
        !listing
            .copies()
            .is_member(&own)
            .await
            .expect("the desk's copies answer"),
        "the person's own workspace is not held here yet"
    );
    let account = Arc::new(Account::at(
        &listing,
        &desk_login.server_address.as_ref().unwrap().endpoint_addr,
        &desk_login.workspace,
    ));
    struct DeskHere(Arc<Account>);
    impl tilo_api::session::Here for DeskHere {
        fn workspaces(&self, _: &DeviceCert) -> Vec<tilo_api::session::Workspace> {
            Vec::new()
        }
        fn enter(
            &self,
            workspace: &str,
            _: &tilo_api::grant_key::GrantKey,
        ) -> Result<tilo_api::session::Reached, String> {
            Err(format!("the desk holds no {workspace}"))
        }
        fn account(&self) -> Option<Arc<dyn tilo_api::session::AccountServer>> {
            Some(self.0.clone())
        }
    }
    let direct = tilo_api::direct::Direct::here(
        Arc::new(DeskHere(account)) as Arc<dyn tilo_api::session::Here>,
        desk_key.clone(),
        desk_login.cert.clone(),
        "the desk",
    )
    .expect("the desk's API starts");
    let tree = tilo_test_env::within("the desk asks its account alone", direct.account_tree())
        .await
        .expect("the account's own tree answers");
    let caller = tilo_actions::remote::Caller::authenticated(
        desk_login.cert.user_public_key,
        desk_login.cert.device_public_key,
    );
    let receipt = tilo_test_env::within(
        "the desk lists the person's workspaces",
        tree.invoke(
            &caller,
            tilo_actions::remote::InvokeRequest::plain(
                RemoteActionTarget {
                    provider: tilo_api::account::PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_api::account::WORKSPACES,
                        "list",
                    ),
                },
                serde_json::Map::new(),
            ),
        ),
    )
    .await
    .expect("the account's own tree answers the list")
    .expect("the account lists the person's workspaces");
    let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the list is answered: {:?}", receipt.status);
    };
    let listed = output.value.unwrap_or_default();
    assert!(
        listed["workspaces"]
            .as_array()
            .is_some_and(|known| known.iter().any(|known| known["id"] == workspace)),
        "the person's workspace is listed: {listed}"
    );
    session.close();
    listing.shutdown().await;

    // Entering with no hint from the person's list: the login entry alone is enough.
    let entering = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("desk-entering"),
        &desk_key,
        &desk_login.cert,
        &tilo_api::daemon::enter::server_of(&desk_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the desk's service opens");
    tilo_test_env::within(
        "the desk enters from its login",
        super::take_work::enter_from_login(&entering, &desk_key, &desk_login),
    )
    .await
    .expect("the desk enters from the login entry alone");
    assert!(
        entering
            .copies()
            .is_member(&workspace)
            .await
            .expect("the desk's copies answer"),
        "the desk holds the person's workspace, entered from the login entry"
    );
    entering.shutdown().await;
    let _ = router.shutdown().await;
    server.shutdown().await;
}

/// No root on this computer: a test whose daemon holds none.
pub(super) fn no_root() -> RootSource {
    Arc::new(|| Ok(None))
}

/// The root kept in `dir`, as a daemon reads it when a ticket is asked for.
pub(super) fn root_in(dir: std::path::PathBuf) -> RootSource {
    Arc::new(move || {
        let active = tilo_identity::store::resolve_active_identity(&dir)
            .ok_or_else(|| anyhow::anyhow!("the test's identity is gone"))?;
        Ok(Some(tilo_identity::store::load_from_path(active)?))
    })
}

/// The device actions `Tickets` answers on `workspaces`' endpoint, with the workspace service's
/// own protocols: what a daemon serves a computer that enters with a ticket.
pub(super) fn serve(
    workspaces: &super::workspaces::ServiceWorkspaces,
    actions: ActionService,
) -> iroh::protocol::Router {
    // As a daemon does: the workspaces' protocols, and the device actions once, on `actions`.
    let mut router = iroh::protocol::Router::builder(workspaces.endpoint());
    for (alpn, handler) in workspaces
        .service()
        .protocols()
        .into_iter()
        .filter(|(alpn, _)| alpn != ACTIONS_ALPN)
    {
        router = router.accept(alpn, handler);
    }
    router.accept(ACTIONS_ALPN, actions).spawn()
}

/// A daemon of the person's device with no login, governing the workspace it founded, issues a
/// ticket; a second computer logs in with it and enters that workspace. The second computer runs on
/// an operating system Tilo does not control, as it reports logging in, so the root adds it as
/// foreign, whatever the issuer's own certificate says.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_with_the_persons_root_and_no_login_adds_a_computer_into_the_workspace_it_founded()
{
    let workspace = tilo_desktop_model::WorkspaceId::new();
    let (issuer, state, issuer_key, issuer_cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_hosting_person(workspace).await;
    let place = tilo_device_identity::login::Place::at(
        state.path().join("issuer"),
        tilo_device_identity::IdentityPaths::under(state.path().join("issuer-keys")),
    );
    let root = root_in(state.path().to_path_buf());
    let mut foreign = issuer_cert.clone();
    foreign.capabilities.foreign = true;
    sign_device_cert(&mut foreign, &root().unwrap().unwrap()).unwrap();
    let actions = issuer.copies().action_service();
    let tickets = Tickets::new(
        (issuer_key.clone(), foreign),
        issuer.endpoint(),
        None,
        root,
        Some(FirstWorkspace::founded(Arc::clone(&issuer), workspace)),
        Some(Arc::clone(&issuer)),
        place,
        actions.clone(),
    );
    let router = serve(&issuer, actions);

    let printed = ask_device(&tickets, ISSUE, json!({}))
        .await
        .expect("a device with the person's root issues a ticket without a login");
    let text = printed["ticket"].as_str().unwrap();

    // A second computer, holding no identity of the person's, logs in with it.
    let (desk_key, desk_cert) = enrol(
        &SigningKey::from_bytes(&[0x73; 32]),
        "the desk",
        DeviceCapabilities::member(),
    );
    let desk_endpoint = endpoint(&desk_key).await;
    let desk_place = tilo_device_identity::login::Place::at(
        state.path().join("desk"),
        tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys")),
    );
    tilo_device_identity::persist::adopt_device_key(desk_place.keys(), &desk_key.to_bytes())
        .unwrap();
    let desk = Tickets::new(
        (desk_key.clone(), desk_cert),
        desk_endpoint.clone(),
        None,
        no_root(),
        None,
        None,
        desk_place.clone(),
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );
    let answer = ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "Codex on desk"}),
    )
    .await
    .unwrap_or_else(|why| panic!("the issuer certifies the desk: {why}"));
    assert_eq!(answer["workspace"], workspace.to_string());

    let login = desk
        .logged_in()
        .borrow()
        .clone()
        .expect("the desk's daemon starts again as the device it became");
    assert!(
        login.cert.capabilities.foreign,
        "a computer that reports no Tilo OS is added foreign"
    );
    assert_eq!(
        login.server_cert.device_public_key,
        issuer
            .service()
            .workspaces()
            .device_cert()
            .device_public_key,
        "the ticket names the certificate the issuer's session proves as the one it enters"
    );

    // The desk enters the workspace the issuer founded: a device its person's root certified is
    // that person's device, and needs no invitation.
    desk_endpoint.close().await;
    let desk_service = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("desk-service"),
        &desk_key,
        &login.cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the desk's service opens");
    let entered = tilo_test_env::within(
        "the desk enters the founded workspace",
        tilo_api::daemon::enter::connect(&desk_key, &login, Some(desk_service.lent())),
    )
    .await
    .expect("the desk enters the founded workspace");
    // The issuer let the desk in and signed where it holds the workspace: what the desk's own
    // workspace service holds it by.
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace.to_string()).unwrap();
    let hosts = entered.hosts();
    assert!(
        hosts.iter().any(|(admitted, _)| *admitted == id),
        "the issuer admitted the desk to the workspace it founded: {hosts:?}"
    );
    entered.close();
    let _ = router.shutdown().await;
}

/// A person's founding answers the tickets their first device asked for, issued as their own
/// act: the ticket lets a computer in, with nothing else needed to approve it, which is the one
/// way a fresh installation adds its first computers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_founding_issues_the_first_tickets_its_person_asked_for_as_their_own_act() {
    let workspace = tilo_desktop_model::WorkspaceId::new();
    let (issuer, state, issuer_key, issuer_cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_hosting_person(workspace).await;
    let place = tilo_device_identity::login::Place::at(
        state.path().join("issuer"),
        tilo_device_identity::IdentityPaths::under(state.path().join("issuer-keys")),
    );
    let actions = issuer.copies().action_service();
    let tickets = Tickets::new(
        (issuer_key.clone(), issuer_cert.clone()),
        issuer.endpoint(),
        None,
        root_in(state.path().to_path_buf()),
        Some(FirstWorkspace::founded(Arc::clone(&issuer), workspace)),
        Some(Arc::clone(&issuer)),
        place,
        actions.clone(),
    );
    let router = serve(&issuer, actions);

    // The person's own services run, as they do once a founding makes them: what a founding
    // call's tickets are issued through.
    let founding = super::unclaimed::Founding::default();
    founding.serving(issuer_cert.user_public_key, Arc::new(tickets));
    let issued = founding
        .issue(issuer_cert.user_public_key, &["device".to_owned()])
        .await
        .expect("the founding issues the ticket its person asked for");
    assert_eq!(issued[0]["kind"], "device", "{issued}");
    let text = issued[0]["ticket"]
        .as_str()
        .expect("the founding answers a ticket")
        .to_owned();
    let ticket = Ticket::parse(&text).expect("the founding answers a ticket");
    assert_eq!(
        ticket.issuer.endpoint,
        issuer_key.verifying_key().to_bytes(),
        "the ticket names the device that founded the person"
    );

    // A computer with no certificate logs in with it: it is one of the person's devices, in the
    // workspace this device founded.
    let (desk_key, desk_cert) = enrol(
        &SigningKey::from_bytes(&[0x74; 32]),
        "the desk",
        DeviceCapabilities::member(),
    );
    let desk_endpoint = endpoint(&desk_key).await;
    let desk_place = tilo_device_identity::login::Place::at(
        state.path().join("desk"),
        tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys")),
    );
    tilo_device_identity::persist::adopt_device_key(desk_place.keys(), &desk_key.to_bytes())
        .unwrap();
    let desk = Tickets::new(
        (desk_key.clone(), desk_cert),
        desk_endpoint.clone(),
        None,
        no_root(),
        None,
        None,
        desk_place,
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );
    let answer = ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "Codex on desk"}),
    )
    .await
    .unwrap_or_else(|why| panic!("the ticket lets a computer in: {why}"));
    assert_eq!(answer["workspace"], workspace.to_string());
    assert!(
        desk.logged_in().borrow().is_some(),
        "the computer is one of the person's devices now"
    );

    desk_endpoint.close().await;
    let _ = router.shutdown().await;
}

/// The person's own server, holding their Servers workspace: where a role's entry is written.
struct ServersHost {
    server: tilo_server::daemon::Server,
    hosted: tilo_workspace_service::workspaces::Workspaces,
    issuer_key: SigningKey,
    issuer_cert: DeviceCert,
    server_cert: DeviceCert,
    _state: tempfile::TempDir,
}

/// The person's own server, holding their Servers workspace, and one of their devices entered
/// into it: where a role's entry is written.
async fn servers_host(root: &SigningKey) -> ServersHost {
    let (server_key, server_cert) = enrol(root, "home-server", DeviceCapabilities::member());
    let (issuer_key, issuer_cert) = enrol(root, "laptop", DeviceCapabilities::primary());
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
    let admin = tilo_workspace_service::workspaces::person_workspace::found(&hosted)
        .await
        .expect("the person's Servers workspace is founded");
    hosted
        .enter(&admin, &issuer_cert)
        .await
        .expect("a device of the person enters the Servers workspace");
    ServersHost {
        server,
        hosted,
        issuer_key,
        issuer_cert,
        server_cert,
        _state: state,
    }
}

/// A ticket that names a plan fills that plan's entry with the certified machine's key, as the
/// approval that took a token-enrolled machine in did: the entry becomes the machine's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ticket_that_names_a_plan_fills_its_plan() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = servers_host(&root).await;
    let hosted = &host.hosted;
    let plan_id = "0123456789abcdef0123456789abcdef";
    let plan = json!({
        "schema": 1,
        "plan": plan_id,
        "revision": 1,
        "destination": "tilo-server",
        "name": "cupboard",
        "network": { "addressing": "dhcp", "addresses": "", "gateway": "", "dns": "" },
        "disk": { "kind": "empty", "identity": "" },
        "unlock": "tpm",
        "updates": "stable",
        "hosting": [],
        "open": [],
        "token": { "nonce": "00112233445566778899aabbccddeeff", "expires_at_ns": u64::MAX / 2 },
    })
    .to_string();
    tilo_workspace_service::workspaces::servers::apply(
        hosted,
        tilo_workspace_service::workspaces::servers::Servers::PLAN_MACHINE
            .with((plan, "cupboard".into())),
    )
    .await
    .expect("the person plans a machine in their Servers workspace");

    let (desk_key, desk_cert) = enrol(&root, "the cupboard", DeviceCapabilities::member());
    super::ticket::plan_entry(hosted, None, plan_id, &desk_cert)
        .await
        .expect("the certified machine's key is taken into its plan");
    let device = tilo_workspace_service::workspaces::servers::device_id(
        &desk_key.verifying_key().to_bytes(),
    );
    let entry = tilo_workspace_service::workspaces::servers::read(hosted)
        .await
        .expect("the person's servers read")
        .servers
        .get(&device)
        .cloned();
    let entry = entry.expect("the plan's entry is the certified machine's");
    assert_eq!(
        entry.plan.plan, plan_id,
        "the entry keeps the plan it fills"
    );
    host.server.shutdown().await;
}

/// A ticket that names a server leaves that server's entry in the person's Servers document, as
/// the server's own start would have made it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ticket_that_names_a_server_leaves_its_entry() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = servers_host(&root).await;
    let hosted = &host.hosted;

    let (desk_key, desk_cert) = enrol(&root, "home", DeviceCapabilities::primary());
    super::ticket::server_entry(hosted, (None, &desk_cert), None, false)
        .await
        .expect("the certified server is taken in among the person's servers");
    let device = tilo_workspace_service::workspaces::servers::device_id(
        &desk_key.verifying_key().to_bytes(),
    );
    let entry = tilo_workspace_service::workspaces::servers::read(hosted)
        .await
        .expect("the person's servers read")
        .servers
        .get(&device)
        .cloned();
    assert_eq!(
        entry.expect("the server's entry is written").label,
        "home",
        "the entry is named as its certificate names it"
    );
    host.server.shutdown().await;
}

/// A logged-in issuer asks its host to write the entry: `tilo device add --server` on one of the
/// person's laptops leaves the server's entry in the Servers document their server holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "a laptop issuing a server ticket, from its own entry to the server's, read top to bottom"
)]
async fn a_logged_in_issuers_server_ticket_leaves_the_entry_on_the_host() {
    use tilo_workspace_service::workspaces::servers;

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = servers_host(&root).await;
    let admin =
        tilo_server_protocol::servers::person_workspace_id(&root.verifying_key().to_bytes());
    let login = login_to(&host.server, &admin, (&host.issuer_cert, &host.server_cert));
    let state = tempfile::tempdir().unwrap();
    let address = tilo_api::daemon::enter::server_of(&login).await.unwrap();
    let service = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &host.issuer_key,
        &host.issuer_cert,
        &address,
    )
    .await
    .expect("the issuer's service opens");
    let entered = tilo_test_env::within(
        "the issuer enters its Servers workspace",
        tilo_api::daemon::enter::connect(&host.issuer_key, &login, Some(service.lent())),
    )
    .await
    .expect("the issuer enters");
    tilo_test_env::within("the issuer holds it", service.hold(&entered)).await;
    assert!(
        service.session().is_some(),
        "the issuer's session is there to ask its host through"
    );

    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let place = tilo_device_identity::login::Place::at(
        state.path().join("issuer"),
        tilo_device_identity::IdentityPaths::under(state.path().join("issuer-keys")),
    );
    let actions = service.copies().action_service();
    let tickets = Tickets::new(
        (host.issuer_key.clone(), host.issuer_cert.clone()),
        service.endpoint(),
        Some(Arc::new(crate::device::home::Home::new(
            Arc::clone(&service),
            login.cert.clone(),
            Some(login.clone()),
        ))),
        root_in(identity.path().to_path_buf()),
        None,
        Some(Arc::clone(&service)),
        place,
        actions.clone(),
    );
    let router = serve(&service, actions);
    let issued = ask_device(&tickets, ISSUE, json!({"server": true}))
        .await
        .expect("a server ticket is issued from the logged-in device");
    let text = issued["ticket"].as_str().unwrap();

    let (desk_key, desk_cert) = enrol(
        &SigningKey::from_bytes(&[0x95; 32]),
        "home",
        DeviceCapabilities::primary(),
    );
    let desk_endpoint = endpoint(&desk_key).await;
    let desk_place = tilo_device_identity::login::Place::at(
        state.path().join("desk"),
        tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys")),
    );
    tilo_device_identity::persist::adopt_device_key(desk_place.keys(), &desk_key.to_bytes())
        .unwrap();
    let desk = Tickets::new(
        (desk_key.clone(), desk_cert),
        desk_endpoint,
        None,
        no_root(),
        None,
        None,
        desk_place,
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );
    ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "home"}),
    )
    .await
    .expect("the issuer certifies the server");

    let device = servers::device_id(&desk_key.verifying_key().to_bytes());
    tilo_test_env::within("the entry reaches the host", async {
        loop {
            let listed = servers::read(&host.hosted)
                .await
                .expect("the person's servers read");
            if listed.servers.get(&device).is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    entered.close();
    let _ = router.shutdown().await;
    host.server.shutdown().await;
}

/// A computer that serves a person already takes a ticket another person's laptop issued: the
/// certificate their root signs is filed, its daemon starts again as their device, entering
/// through the server the ticket named, and takes their own workspace in. Started once more, with
/// nothing of the login written anywhere, it finds their server in that copy and serves them
/// through it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one computer claimed and started again, from the ticket to the server it finds, read top to bottom"
)]
async fn a_claimed_computer_serves_its_new_person_through_their_server_after_a_restart() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let person = root.verifying_key().to_bytes();
    let host = servers_host(&root).await;
    let admin = tilo_server_protocol::servers::person_workspace_id(&person);
    let laptop = login_to(&host.server, &admin, (&host.issuer_cert, &host.server_cert));
    let state = tempfile::tempdir().unwrap();
    let address = tilo_api::daemon::enter::server_of(&laptop).await.unwrap();
    let service = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &host.issuer_key,
        &host.issuer_cert,
        &address,
    )
    .await
    .expect("the laptop's service opens");
    let entered = tilo_test_env::within(
        "the laptop enters its person's workspace",
        tilo_api::daemon::enter::connect(&host.issuer_key, &laptop, Some(service.lent())),
    )
    .await
    .expect("the laptop enters");
    tilo_test_env::within("the laptop keeps it", service.hold(&entered)).await;
    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let actions = service.copies().action_service();
    let tickets = Tickets::new(
        (host.issuer_key.clone(), host.issuer_cert.clone()),
        service.endpoint(),
        Some(Arc::new(crate::device::home::Home::new(
            Arc::clone(&service),
            laptop.cert.clone(),
            Some(laptop.clone()),
        ))),
        root_in(identity.path().to_path_buf()),
        None,
        Some(Arc::clone(&service)),
        tilo_device_identity::login::Place::at(
            state.path().join("laptop-login"),
            tilo_device_identity::IdentityPaths::under(state.path().join("laptop-keys")),
        ),
        actions.clone(),
    );
    let router = serve(&service, actions);
    let issued = ask_device(&tickets, ISSUE, json!({}))
        .await
        .expect("the laptop issues a ticket");
    let text = issued["ticket"].as_str().unwrap();

    // The desk serves another person already: their certificate is filed for its key.
    let (desk_key, before) = enrol(
        &SigningKey::from_bytes(&[0x95; 32]),
        "desk",
        DeviceCapabilities::primary(),
    );
    let desk_place = tilo_device_identity::login::Place::at(
        state.path().join("desk"),
        tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys")),
    );
    tilo_device_identity::persist::adopt_device_key(desk_place.keys(), &desk_key.to_bytes())
        .unwrap();
    tilo_device_identity::login::file_certificate(&desk_place, &before).unwrap();
    let desk = Tickets::new(
        (desk_key.clone(), before),
        endpoint(&desk_key).await,
        None,
        no_root(),
        None,
        None,
        desk_place.clone(),
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );
    ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "Codex on desk"}),
    )
    .await
    .expect("the laptop certifies the desk");
    let became = *desk
        .logged_in()
        .borrow()
        .clone()
        .expect("the desk's daemon starts again as the device it became");
    assert_eq!(became.cert.user_public_key, person);
    assert_eq!(
        became.server_address.as_ref().map(|at| at.endpoint_addr.id),
        Some(host.server.node_addr().id),
        "the ticket names the person's server"
    );

    // Its person's part starts again as Ada's device, entering through the server the ticket
    // named, and takes her own workspace in.
    let desk_dir = state.path().join("desk-service");
    let first = super::workspaces::ServiceWorkspaces::open_for_test(
        &desk_dir,
        &desk_key,
        &became.cert,
        &tilo_api::daemon::enter::server_of(&became).await.unwrap(),
    )
    .await
    .expect("the desk's service opens");
    let home = crate::device::home::Home::new(
        Arc::clone(&first),
        became.cert.clone(),
        Some(became.clone()),
    );
    assert_eq!(
        home.get().await.as_ref(),
        Some(&became),
        "the ticket's server, at first"
    );
    let desk_entered = tilo_test_env::within(
        "the desk enters its person's workspace",
        tilo_api::daemon::enter::connect(&desk_key, &became, Some(first.lent())),
    )
    .await
    .expect("the desk enters");
    tilo_test_env::within("the desk keeps it", first.hold(&desk_entered)).await;
    tilo_test_env::within("the desk's copy names the server", async {
        loop {
            let hosts = first.copies().hosts(&admin).await.unwrap_or_default();
            if crate::device::home::server_among(&hosts, &desk_key.verifying_key().to_bytes())
                .is_some()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    desk_entered.close();
    drop(home);
    drop(first);

    // Started again: nothing of the login is written anywhere, only the certificate.
    let written: Vec<_> = walk(state.path())
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "login.json"))
        .collect();
    assert!(written.is_empty(), "no login is kept: {written:?}");
    let cert = tilo_device_identity::login::served_cert(desk_place.keys())
        .expect("the desk's certificate");
    assert_eq!(cert, became.cert, "the desk serves as Ada's device");
    let again = super::workspaces::ServiceWorkspaces::open_for_test(
        &desk_dir,
        &desk_key,
        &cert,
        &tilo_api::daemon::enter::server_of(&became).await.unwrap(),
    )
    .await
    .expect("the desk's service opens again");
    let home = crate::device::home::Home::new(Arc::clone(&again), cert, None);
    let found = home.get().await.expect("its copy names Ada's server");
    assert_eq!(found.workspace, admin);
    assert_eq!(found.agent, "Codex", "the agent is the certificate's");
    assert_eq!(
        found.server_cert.device_public_key,
        host.server_cert.device_public_key
    );
    assert_eq!(
        found.server_address.map(|at| at.endpoint_addr.id),
        Some(host.server.node_addr().id),
        "the server the copy names is the person's"
    );
    entered.close();
    let _ = router.shutdown().await;
    host.server.shutdown().await;
}

/// Every file below `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A computer that holds its person's root logs in at a server by the server's line alone, through
/// its daemon's `devices#logIn`: the server is named among the hosts of the person's own
/// workspace, so the copy names it from then on, and what follows that copy hears it at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one login at a server, from the line to the copy naming it, read top to bottom"
)]
async fn a_root_holders_login_at_a_server_names_it_among_the_persons_hosts() {
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

    // The laptop keeps the person's own workspace, as a computer holding their root does.
    let shape = login_to(&server, &workspace, (&laptop_cert, &server_cert));
    let service = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&shape).await.unwrap(),
    )
    .await
    .expect("the laptop's service opens");
    let person = tilo_workspace_service::workspaces::person_workspace::found(service.copies())
        .await
        .expect("the laptop's own workspace");
    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let actions = service.copies().action_service();
    let tickets = Tickets::new(
        (laptop_key.clone(), laptop_cert.clone()),
        service.endpoint(),
        None,
        root_in(identity.path().to_path_buf()),
        None,
        Some(Arc::clone(&service)),
        tilo_device_identity::login::Place::at(
            state.path().join("laptop-login"),
            tilo_device_identity::IdentityPaths::under(state.path().join("laptop-keys")),
        ),
        actions.clone(),
    );
    let router = serve(&service, actions);
    // What follows the copy: nothing names a server yet.
    let home = Arc::new(crate::device::home::Home::new(
        Arc::clone(&service),
        laptop_cert.clone(),
        None,
    ));
    assert!(home.get().await.is_none(), "no server is named yet");
    let heard = tokio::spawn({
        let home = Arc::clone(&home);
        async move { home.moved().await }
    });

    let line = serde_json::to_string(&tilo_server_protocol::web::PageHost {
        server_cert: server_cert.clone(),
        address: Some(tilo_server_protocol::web::PageHostAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .unwrap();
    let answer = ask_device(&tickets, LOG_IN, json!({ "server": line }))
        .await
        .unwrap_or_else(|why| panic!("the root's own device logs in at the server: {why}"));
    assert_eq!(answer["workspace"], workspace);
    let became = tickets
        .logged_in()
        .borrow()
        .clone()
        .expect("the person's part starts again through the server");
    assert_eq!(
        became.server_cert.device_public_key,
        server_cert.device_public_key
    );

    // The copy names the server at once, and whoever follows it hears so.
    let moved = tilo_test_env::within("the copy's change is heard", heard)
        .await
        .unwrap();
    assert_eq!(
        moved.server_cert.device_public_key, server_cert.device_public_key,
        "the server the copy names now is the one logged in at"
    );
    assert_eq!(moved.workspace, person);
    let started_again = crate::device::home::Home::new(Arc::clone(&service), laptop_cert, None);
    assert_eq!(
        started_again
            .get()
            .await
            .and_then(|device| device.server_address)
            .map(|address| address.endpoint_addr.id),
        Some(server.node_addr().id),
        "a later start enters through it, with nothing but the copy"
    );
    let _ = router.shutdown().await;
    server.shutdown().await;
}

/// The person's recovery words and a backup of their root are handed to them once, in the answer
/// to their call: what the daemon keeps of each call says only that it was given, so following
/// the call afterwards finds no secret, and both actions say so on their surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_words_and_a_backup_are_handed_over_and_kept_nowhere() {
    use tilo_actions::remote::ReceiptStatus;

    let profile = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", profile.path())
        .set(tilo_identity::store::FILE_SECRETS_ENV, "1");
    // The person whose root this daemon keeps, as founding left it: the root is sealed with this
    // computer's own device key, where the daemon keeps it.
    let identity_dir = crate::device::services::this_accounts_root_dir().unwrap();
    let ada = tilo_identity::provisioning::create_person(
        &identity_dir,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        "Ada",
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();

    let workspace = tilo_desktop_model::WorkspaceId::new();
    let (issuer, state, key, cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_hosting_person(workspace).await;
    let place = tilo_device_identity::login::Place::at(
        state.path().join("issuer"),
        tilo_device_identity::IdentityPaths::under(state.path().join("issuer-keys")),
    );
    let tickets = Tickets::new(
        (key, cert),
        issuer.endpoint(),
        None,
        root_in(state.path().to_path_buf()),
        None,
        Some(Arc::clone(&issuer)),
        place,
        issuer.copies().action_service(),
    );

    // The device's one dispatch: its record of each invocation, and the person who asks: Ada,
    // whose root this computer keeps and whose call runs with it. Anyone else is answered no
    // secret of hers.
    let invocations = Arc::new(tilo_actions::remote::invocation_record::Invocations::default());
    let person = tilo_actions::remote::Caller::authenticated(ada, [8; 32]);
    let someone_else = tilo_actions::remote::Caller::authenticated([7; 32], [8; 32]);
    let page = super::ticket::catalogue(&tilo_actions::remote::CatalogueQuery::default());
    for (action, args, field) in [
        (
            tilo_device_identity::login::RECOVERY_WORDS,
            json!({}),
            "words",
        ),
        (
            tilo_device_identity::login::BACKUP,
            json!({"passphrase": "a passphrase of some length"}),
            "backup",
        ),
    ] {
        let listed = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|listed| listed.address.action == action)
            .expect("the action is listed");
        assert!(
            listed.contract.answers_a_secret(),
            "{action} says its answer is a secret"
        );

        let Value::Object(argument) = args else {
            unreachable!("the arguments are an object");
        };
        let request = |invocation| InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(argument.clone()),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let refused = super::services::device_action(
            (&invocations, None),
            &tickets,
            &someone_else,
            request(InvocationId::new()),
            "workspace",
        )
        .await
        .expect("the action is answered");
        assert!(
            refused.is_err(),
            "another person is handed {action}: {refused:?}"
        );
        let invocation = InvocationId::new();
        let receipt = super::services::device_action(
            (&invocations, None),
            &tickets,
            &person,
            request(invocation),
            "workspace",
        )
        .await
        .expect("the action is answered")
        .expect("the action runs");
        let ReceiptStatus::Completed { output } = receipt.status else {
            panic!("{action} was not answered at once: {:?}", receipt.status);
        };
        let secret = output.value.unwrap()[field]
            .as_str()
            .expect("the answer carries the secret")
            .to_owned();
        assert!(secret.len() > 32, "{action} answered {secret}");

        // Following the call afterwards finds it given, and nothing of the secret: neither in
        // the provider's own cache nor in the dispatch's record of the invocation, which keeps
        // nothing of what it was asked with either.
        let cached = tickets.observe(&invocation).expect("the call is known");
        let cached = format!("{:?}", cached.update);
        assert!(cached.contains("given"), "{cached}");
        assert!(!cached.contains(&secret), "{action} cached its answer");
        assert_eq!(
            invocations
                .observe(&person, &invocation)
                .map(|state| state.update),
            Some(InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(json!({ "given": true })),
                    resources: Vec::new(),
                },
            }),
            "{action}'s record says it was given"
        );
        let recorded = invocations
            .get(&person, &invocation)
            .expect("the dispatch recorded the call");
        let recorded = format!("{:?} {:?}", recorded.request, recorded.state);
        assert!(!recorded.contains(&secret), "{action} recorded its answer");
        assert!(
            !recorded.contains("a passphrase of some length"),
            "{action} recorded what sealed it"
        );
    }
    drop(env);
}

/// A device the person's devices record does not mark as keeping their root is asked to revoke a
/// device: its cap keeps the call as its asked record in the workspace, under way there, and asks
/// nothing. A device the record marks comes online, takes the record up, the person answers it
/// there once, and it runs under that root; the device that asked shows how it ended, and the
/// record is gone from the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoke_on_a_device_without_the_root_is_answered_and_run_at_one_that_keeps_it() {
    use tilo_actions::remote::ActionProvider as _;
    use tilo_actions::remote::asked::{AskedKeeping, AskedStore, Offer, Resolve};

    // The person is the one this account founded here: their root is kept for this account
    // alone, where only a call arriving as it signs with it.
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let account = rustix::process::geteuid().as_raw();
    let workspace = tilo_desktop_model::WorkspaceId::new();
    let (issuer, state, key, cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_of_account_person(account).await;
    issuer.found(workspace, "Home").await.unwrap();
    let account_dir = super::services::account_identity_dir(account).unwrap();
    // The device without the root is another computer, whose identity store keeps no root of
    // theirs: this one's store, until the device that keeps it comes online below.
    let keeping_store = home.config().to_owned();
    let rootless_store_dir = home.root().join("rootless-config");
    std::fs::create_dir_all(&rootless_store_dir).unwrap();
    home.env().set("XDG_CONFIG_HOME", &rootless_store_dir);
    let copies = issuer.copies();
    // The person's own workspace, where a call that only their root signs waits for a device
    // that keeps it.
    tilo_workspace_service::workspaces::person_workspace::found(copies)
        .await
        .expect("the person's own workspace is founded here");
    let tickets = |root: RootSource, at: &str, cert: &DeviceCert| {
        Tickets::new(
            (key.clone(), cert.clone()),
            issuer.endpoint(),
            None,
            root,
            None,
            Some(Arc::clone(&issuer)),
            tilo_device_identity::login::Place::at(
                state.path().join(at),
                tilo_device_identity::IdentityPaths::under(state.path().join(format!("{at}-keys"))),
            ),
            copies.action_service(),
        )
    };
    // Each device's one dispatch, keeping what waits in the person's workspace.
    let device = |key: [u8; 32]| {
        let keeping: Arc<dyn AskedKeeping> = copies.asked_keeping();
        let store = AskedStore::opened(None, Some(keeping), Vec::new()).unwrap();
        store.asks_on(&key);
        let invocations: Arc<tilo_actions::remote::invocation_record::Invocations> = Arc::default();
        let asked = super::asked::DeviceAsked::new(
            Arc::clone(&store),
            Arc::clone(&invocations),
            Box::new(|_| Err("nothing waits here to be sent again".to_owned())),
        );
        (store, invocations, asked)
    };
    let rootless_key = [8; 32];
    // Another device of the person's, which their devices record does not mark.
    let rootless_cert = DeviceCert {
        device_public_key: rootless_key,
        ..cert.clone()
    };
    let rootless = tickets(no_root(), "rootless", &rootless_cert);
    let (rootless_store, invocations, rootless_asked) = device(rootless_key);
    let follows = Arc::downgrade(&rootless_asked);
    copies.follow_asked_at_root(move |asks| {
        if let Some(asked) = follows.upgrade() {
            asked.kept_changed(asks);
        }
    });

    // The person, on the device without their root, revokes a key.
    let person = tilo_actions::remote::Caller::authenticated(cert.user_public_key, rootless_key);
    let invocation = InvocationId::new();
    let revoked = hex::encode([9; 32]);
    let revoke = InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                RESOURCE,
                tilo_device_identity::login::REVOKE,
            ),
        },
        argument: InvokeArgument::Json(json!({"device": revoked}).as_object().unwrap().clone()),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = super::services::device_action(
        (&invocations, Some(&rootless_asked)),
        &rootless,
        &person,
        revoke.clone(),
        &workspace.to_string(),
    )
    .await
    .expect("the call is taken")
    .expect("the call is admitted");
    assert!(
        !matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "nothing runs on a device without the root: {:?}",
        receipt.status
    );
    let at = rootless_store
        .record(&invocation.0.to_string())
        .expect("the call is kept as its asked record");
    // The record is written to the workspace by a task of its own, after the call is answered.
    let deadline = tilo_test_env::hang_deadline();
    let mut kept = copies.asked_kept().await;
    while !kept.iter().any(|ask| ask.at == at) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        kept = copies.asked_kept().await;
    }
    let ask = kept
        .iter()
        .find(|ask| ask.at == at)
        .expect("the record is in the person's workspace");
    assert!(
        ask.dispatch == tilo_actions::remote::asked::AT_ROOT
            && ask.taken.is_none()
            && ask.outcome.is_none()
    );
    assert!(
        rootless_asked
            .store()
            .asked_of(&cert.user_public_key)
            .is_empty(),
        "a device without the root asks nothing of it"
    );
    let refused = rootless_store
        .resolve(
            &at,
            &person,
            &Resolve {
                resolution: Offer::Approve,
                ..Resolve::default()
            },
        )
        .await
        .expect("the answer is decided at once");
    assert!(
        refused.is_err(),
        "a device without the root does not run what only it signs"
    );
    assert!(
        rootless_asked
            .observe(&person, &invocation)
            .is_some_and(|state| !state.update.is_terminal()),
        "the call waits under way"
    );

    // A device that keeps the root comes online and takes the record up.
    home.env().set("XDG_CONFIG_HOME", &keeping_store);
    let holder_key = cert.device_public_key;
    let holder = tickets(root_in(account_dir.clone()), "holder", &cert);
    let root = root_in(account_dir)().unwrap().unwrap();
    // The person's own workspace keeps their devices record. A computer that keeps their root
    // founds it, at the id their key names, the first time they need it, as it does now.
    issuer
        .found(
            super::first_workspace::own_workspace(&cert.user_public_key),
            super::first_workspace::FOUNDED_NAME,
        )
        .await
        .expect("the person's workspace is founded");
    tilo_workspace_service::workspaces::person_workspace::devices::mark_root_holder(
        copies, &root, &cert,
    )
    .await
    .expect("the root marks the device that keeps it");
    let (holder_store, _, holder_asked) = device(holder_key);
    holder_asked.serve_at_root(holder.clone());
    let (taken, took) = tokio::sync::oneshot::channel();
    let taken = std::sync::Mutex::new(Some(taken));
    let follows = Arc::downgrade(&holder_asked);
    copies.follow_asked_at_root(move |asks| {
        let here = asks.iter().any(|ask| ask.request.invocation == invocation);
        if let Some(asked) = follows.upgrade() {
            asked.kept_changed(asks);
        }
        if here && let Some(taken) = taken.lock().unwrap().take() {
            let _ = taken.send(());
        }
    });
    tokio::time::timeout(Duration::from_secs(30), took)
        .await
        .expect("the device that keeps the root reads the record")
        .unwrap();
    let at = holder_store
        .record(&invocation.0.to_string())
        .expect("the record waits for the person there");
    assert_eq!(
        holder_asked.store().asked_of(&cert.user_public_key).len(),
        1,
        "the person is asked there"
    );

    // The device that asked starts again and sends the call again under its id: it follows the
    // record the workspace keeps, which stays as it was, and the person is still asked once.
    let (_, invocations, restarted) = device(rootless_key);
    let again = super::services::device_action(
        (&invocations, Some(&restarted)),
        &rootless,
        &person,
        revoke,
        &workspace.to_string(),
    )
    .await
    .expect("the call is followed")
    .expect("the call is admitted again");
    assert!(!matches!(
        again.status,
        tilo_actions::remote::ReceiptStatus::Completed { .. }
    ));
    let kept_again = copies.asked_kept().await;
    assert_eq!(
        kept_again.iter().find(|again| again.at == at),
        Some(ask),
        "the record is not written again"
    );
    assert_eq!(
        holder_asked.store().asked_of(&cert.user_public_key).len(),
        1
    );

    // The person answers once, there.
    let answering = tilo_actions::remote::Caller::authenticated(cert.user_public_key, holder_key);
    holder_store
        .resolve(
            &at,
            &answering,
            &Resolve {
                resolution: Offer::Approve,
                ..Resolve::default()
            },
        )
        .await
        .expect("the answer is decided at once")
        .expect("the person's answer is taken at a device that keeps the root");

    // The device that asked shows how the call ended where it ran.
    let ends = Instant::now() + Duration::from_secs(30);
    let ended = loop {
        let state = rootless_asked
            .observe(&person, &invocation)
            .expect("the device that asked follows its call");
        if state.update.is_terminal() {
            break state.update;
        }
        let change = rootless_asked
            .next_change(&person, &invocation)
            .expect("the call is followed while it waits");
        assert!(
            tokio::time::timeout_at(tokio::time::Instant::from_std(ends), change)
                .await
                .is_ok(),
            "the call ends on the device that asked"
        );
    };
    let InvocationUpdate::Failed { error, .. } = &ended else {
        panic!("the revoke of a key that is no device ends failed: {ended:?}");
    };
    assert!(
        error.message.contains("no device of yours"),
        "it ran under the root, which found no such device: {}",
        error.message
    );
    assert!(
        rootless_store.record(&invocation.0.to_string()).is_none(),
        "the device that asked forgets the record once it shows how it ended"
    );
}

const FIRST_APP_TEST: &str = "device::ticket_tests::the_first_app_launched_in_the_persons_workspace_founds_it_with_the_device_listed";
/// The socket the helper process of [`FIRST_APP_TEST`] connects to as an app.
const FIRST_APP_SOCKET: &str = "TILO_FIRST_APP_SOCKET";

/// On a computer with no login, the first app this daemon launched in the person's own workspace
/// founds that workspace when its service connection names it, before the app signs anything,
/// and the device is listed in the person's devices then. A process the daemon did not launch,
/// and an app it launched in another workspace, found nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_app_launched_in_the_persons_workspace_founds_it_with_the_device_listed() {
    use std::io::Read as _;
    use tilo_actions::remote::asked::{AskedKeeping, AskedStore};
    use tilo_identity::sources::PersonResourceTypes as _;

    // Helper role: a process of its own that connects as an app, and stays until told to go.
    if let Some(socket) = std::env::var_os(FIRST_APP_SOCKET) {
        let _connection = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let _ = std::io::stdin().read(&mut [0]);
        return;
    }
    const APP: &str = "dev.tilo.first";
    let (workspaces, state, key, cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_of_person().await;
    let copies = workspaces.copies().clone();
    let first = FirstWorkspace::open(
        Arc::clone(&workspaces),
        None,
        state.path(),
        &cert.user_public_key,
    )
    .unwrap();
    // What the daemon's start has its person's workspace held with, once it is here.
    let tickets = Arc::new(Tickets::new(
        (key.clone(), cert.clone()),
        workspaces.endpoint(),
        None,
        no_root(),
        None,
        Some(Arc::clone(&workspaces)),
        tilo_device_identity::login::Place::at(
            state.path().join("login"),
            tilo_device_identity::IdentityPaths::under(state.path().join("login-keys")),
        ),
        copies.action_service(),
    ));
    let keeping: Arc<dyn AskedKeeping> = copies.asked_keeping();
    let invocations: Arc<tilo_actions::remote::invocation_record::Invocations> = Arc::default();
    let asked = super::asked::DeviceAsked::new(
        AskedStore::opened(None, Some(keeping), Vec::new()).unwrap(),
        invocations,
        Box::new(|_| Err("nothing waits here to be sent again".to_owned())),
    );
    first.when_here(super::host_runs::settling(
        &workspaces,
        &asked,
        &tickets,
        &cert,
    ));
    let apps = Arc::new(crate::apps::Apps::new(
        None,
        state.path(),
        std::path::PathBuf::new(),
        Vec::new(),
    ));
    let surfaces = super::services::AttachedSurfaces {
        apps: Arc::clone(&apps),
        first: Arc::clone(&first),
        dev: false,
    };
    let own = super::first_workspace::own_workspace(&cert.user_public_key);
    let elsewhere = tilo_desktop_model::WorkspaceId::new();
    // The app's service connection, from a process of its own.
    let socket = state.path().join("identity.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut app = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", FIRST_APP_TEST, "--nocapture"])
        .env(FIRST_APP_SOCKET, &socket)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn_locked()
        .unwrap();
    let (daemon, _) = listener.accept().unwrap();
    let listed = || {
        let workspaces = Arc::clone(&workspaces);
        let device = cert.device_public_key;
        std::thread::spawn(move || {
            super::signing::devices_seen(&workspaces)
                .certified
                .contains(&device)
        })
        .join()
        .unwrap()
    };

    // A process no launch here started names the person's workspace: nothing is founded.
    surfaces.connected(APP, &own, &daemon);
    assert_eq!(
        first.get(),
        None,
        "a process the daemon did not launch founds nothing"
    );

    // A connection the daemon made itself for no launch (an agent run, an internal client) names
    // the person's workspace: nothing is founded.
    let (_internal, made_here) = std::os::unix::net::UnixStream::pair().unwrap();
    surfaces.connected(APP, &own, &made_here);
    assert_eq!(
        first.get(),
        None,
        "a connection the daemon made for no launch founds nothing"
    );

    // An app the daemon launched in another workspace names it: nothing is founded either.
    apps.record_started(app.id(), APP, &elsewhere.to_string());
    surfaces.connected(APP, &elsewhere, &daemon);
    assert_eq!(
        first.get(),
        None,
        "an app launched elsewhere founds nothing"
    );
    apps.forget_started(app.id());

    // The app the daemon launched in the person's own workspace founds it, with this device
    // listed in their devices before the app signs anything.
    apps.record_started(app.id(), APP, &own.to_string());
    let founding = tokio::task::spawn_blocking({
        let own = own;
        move || surfaces.connected(APP, &own, &daemon)
    });
    founding.await.unwrap();
    assert_eq!(first.get(), Some(own.to_string().as_str()));
    assert!(listed(), "the device is listed in the person's devices");
    drop(app.stdin.take());
    assert!(app.wait().unwrap().success());
}

/// A browser's sign-in approved by a device that keeps the person's root: the certificate that
/// root signs for the tab is kept in the person's devices record at the moment it is signed, so
/// the tab is one of theirs before anyone is told it was let in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tab_the_root_signs_in_is_listed_with_the_persons_devices() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    // The person's root, kept here: what signs the tab.
    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let record = tilo_identity::store::resolve_active_identity(identity.path())
        .expect("the imported root is the active identity");
    let _root_kept_here = super::services::root_record_for_test(record);
    // The device that keeps it: its service governs the person's own workspace here, where the
    // record of their devices is.
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let address = tilo_server_client::ServerAddress::at(iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&key.verifying_key().to_bytes())
            .expect("a device key is an endpoint id"),
    ));
    let own = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &key,
        &cert,
        &address,
    )
    .await
    .expect("the laptop's service opens");
    tilo_workspace_service::workspaces::person_workspace::found(own.copies())
        .await
        .expect("the laptop keeps the person's own workspace");
    let service = own.copies().action_service();
    let tickets = Tickets::new(
        (key.clone(), cert.clone()),
        own.endpoint(),
        None,
        no_root(),
        None,
        Some(Arc::clone(&own)),
        tilo_device_identity::login::Place::at(
            state.path().join("laptop-login"),
            tilo_device_identity::IdentityPaths::under(state.path().join("laptop-keys")),
        ),
        service,
    );

    // `tilo device add <ticket>` for a browser: the tab it names, signed by the root kept here.
    let tab_key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let tab = tilo_device_identity::login::Tab {
        device_public_key: hex::encode(tab_key.verifying_key().to_bytes()),
        label: "Firefox on Linux".into(),
        key_for: None,
    };
    let ticket = tilo_device_identity::login::Ticket {
        issuer: tilo_api::daemon::enter::reached_of(&iroh::EndpointAddr::new(
            iroh::EndpointId::from_bytes(&tab_key.verifying_key().to_bytes())
                .expect("a key is an endpoint id"),
        )),
        direction: tilo_device_identity::login::Direction::SignIn,
        secret: [9; 32],
    };
    // The approve is refused after it signs, because this computer's person is not logged in to
    // tell the browser how it went; what it signed is listed all the same.
    let refused = ask_device(
        &tickets,
        APPROVE,
        json!({"ticket": ticket.encode(), "tab": tab}),
    )
    .await
    .expect_err("a computer with no login tells no browser it signed it in");
    assert!(
        refused.contains("not logged in"),
        "it is told the browser cannot be answered from here: {refused}"
    );
    let person = cert.user_public_key;
    let listed =
        tilo_workspace_service::workspaces::person_workspace::devices::of(own.copies(), &person)
            .await
            .listed(&person);
    let signed = listed
        .iter()
        .find(|listed| listed.cert.device_public_key == tab_key.verifying_key().to_bytes())
        .expect("the tab the root signed is listed with the person's devices");
    assert_eq!(signed.cert.device_label, "Firefox on Linux");
    assert!(
        tilo_user_identity::verify_device_cert(&signed.cert).is_ok(),
        "the listed certificate is the root's own"
    );
}
