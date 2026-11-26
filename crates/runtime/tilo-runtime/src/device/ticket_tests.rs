#![allow(
    clippy::significant_drop_tightening,
    reason = "a test keeps what it serves until it ends, as the daemon does"
)]

//! A ticket: `tilo device add` on one computer serves one, and `tilo login` on another becomes
//! one of the person's devices with it. Served on the issuer's one endpoint, and called as the
//! daemon's `devices#logIn` action by the computer that logs in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    CERTIFY, CertifyInput, Entry, ISSUE, LOG_IN, PROVIDER, RESOURCE, SCOPE, Ticket,
};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions};
use tilo_peer_state::new_device::ServedTicket;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::ticket::{Founded, RootApprover, RootSource, Tickets};

#[tokio::test]
async fn a_member_device_cannot_issue_an_admin_ticket() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (key, cert) = enrol(&root, "laptop", DeviceCapabilities::member());
    let endpoint = endpoint(&key).await;
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".into(),
        workspace_name: None,
        agent: "Claude Code".into(),
        cert: cert.clone(),
        server_cert: cert.clone(),
        server_address: None,
    };
    let state = tempfile::tempdir().unwrap();
    let place = tilo_device_identity::login::Place::at(
        state.path().to_path_buf(),
        tilo_device_identity::IdentityPaths::under(state.path().join("keys")),
    );
    let service = ActionService::new(key.clone(), Arc::new(NoActions));
    let tickets = Tickets::new(
        (key.clone(), cert),
        endpoint,
        Some((key, login)),
        no_root(),
        None,
        None,
        place,
        service,
    );
    let refused = ask_device(&tickets, ISSUE, json!({"capability": "admin"}))
        .await
        .unwrap_err();
    assert!(refused.contains("isn't an administrator"), "{refused}");
}

/// A device `root` certified, and its key.
fn enrol(
    root: &impl tilo_user_identity::StatementSigner,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.public_key(),
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
    let Value::Object(argument) = args else {
        panic!("the arguments are an object");
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation,
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
    tickets
        .invoke(&request)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one person adding a computer, from the ticket to its entry, read top to bottom"
)]
async fn a_computer_logged_in_with_a_ticket_enters_as_the_persons_device() {
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
    // The server keeps the person's own workspace too, where their list of workspaces is.
    let own = tilo_workspace_service::workspaces::person_workspace::found(&hosted)
        .await
        .expect("the person's own workspace is founded on their server");

    let laptop_login = login_to(&server, &workspace, (&laptop_cert, &server_cert));

    // `tilo device add` on the laptop: its daemon serves a ticket on its endpoint, its root
    // approving the computer that comes with it.
    let laptop_endpoint = endpoint(&laptop_key).await;
    let ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&laptop_endpoint.addr()));
    let mut reached = tilo_api::daemon::enter::reached_of(&server.node_addr());
    if reached.relay.is_none() {
        reached
            .relay
            .clone_from(&laptop_login.server_address.as_ref().unwrap().relay_url);
    }
    let enter = Entry {
        workspace: laptop_login.workspace.clone(),
        workspace_name: laptop_login.workspace_name.clone(),
        server_cert: laptop_login.server_cert.clone(),
        server: reached,
        site: laptop_login.site.clone(),
        host_origin: laptop_login.host_origin.clone(),
    };
    let waiting = ServedTicket::new(
        ticket.clone(),
        root.verifying_key().to_bytes(),
        enter,
        DeviceCapabilities::member(),
        Arc::new(RootApprover {
            root: root.clone(),
            device: laptop_key.clone(),
            cert: laptop_cert.clone(),
            admission: None,
        }),
        RevocationList::new(),
    );
    let service = ActionService::new(laptop_key.clone(), Arc::new(NoActions));
    service.admit_uncertified(SCOPE, Arc::new(waiting.clone()));
    let router = iroh::protocol::Router::builder(laptop_endpoint.clone())
        .accept(ACTIONS_ALPN, service)
        .spawn();

    let text = ticket.encode();
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
    let desk_actions = ActionService::new(desk_key.clone(), Arc::new(NoActions));
    let desk = Tickets::new(
        (desk_key.clone(), desk_cert),
        desk_endpoint.clone(),
        None,
        no_root(),
        None,
        None,
        place.clone(),
        desk_actions.clone(),
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
    // The desk's login is kept, and the ticket is spent.
    let desk_login = tilo_device_identity::login::load(&place)
        .unwrap()
        .expect("the desk's login is kept")
        .1;
    assert_eq!(
        desk_login.cert.device_public_key,
        desk_key.verifying_key().to_bytes()
    );
    let (spent_key, _) = enrol(&root, "another", DeviceCapabilities::member());
    let spent = ask_issuer(&ticket, &spent_key, SCOPE, CERTIFY).await;
    assert!(
        matches!(spent, Some(Err(ActionCallError::Refused { .. }))),
        "the ticket is spent: {spent:?}"
    );

    let lent = tilo_session_client::Lent {
        endpoint: Some(Arc::new(std::sync::OnceLock::from(desk_endpoint.clone()))),
        ..Default::default()
    };
    let desk_session = tilo_test_env::within(
        "the desk enters",
        tilo_api::daemon::enter::connect(&desk_key, &desk_login, Some(lent)),
    )
    .await
    .expect("the desk enters as the person's device");
    // The desk takes the person's own workspace in by its id at the device its ticket names,
    // without anything listing it there.
    let own = tilo_server_protocol::WorkspaceId::parse(&own).unwrap();
    assert!(
        desk_session.admitted(own).await,
        "the desk enters the person's own workspace at the server its ticket names"
    );
    let listed = desk_session
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

    // Its daemon now runs under the certificate the laptop gave it. The desk holds no local
    // replica and reaches the host from the very endpoint its authenticated session uses.
    let holder = Tickets::new(
        (desk_key.clone(), desk_login.cert.clone()),
        desk_endpoint.clone(),
        Some((desk_key, desk_login)),
        no_root(),
        None,
        None,
        place,
        desk_actions,
    );
    let page_ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&server.node_addr()))
        .with_issuer_device(server_cert.device_public_key);
    assert_ne!(page_ticket.issuer.endpoint, page_ticket.issuer_device());
    report_from_the_desk(&hosted, &holder, &desk_session, &page_ticket, &ticket).await;
    desk_endpoint.close().await;
    let _ = router.shutdown().await;
    server.shutdown().await;
}

/// A report from the admitted desk reaches the page's host with its certified identity,
/// workspace and original arguments. Closing the session ends that authority.
async fn report_from_the_desk(
    hosted: &tilo_workspace_service::workspaces::Workspaces,
    holder: &Tickets,
    entered: &tilo_session_client::device::Device,
    page_ticket: &Ticket,
    laptop_ticket: &Ticket,
) {
    use tilo_actions::remote::{Caller, InvocationReceipt, Pending, ReceiptStatus, TestProvider};
    use tilo_device_identity::login::{CertifiedAction, REPORTED};

    let cert = entered.cert();
    let workspace = entered.workspace_id().to_string();
    let person = Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let arguments = |ticket: &Ticket| {
        json!({
            "ticket": ticket.encode(), "step": "failed", "agent": "Codex", "detail": "setup interrupted"
        })
    };
    let Value::Object(object) = arguments(page_ticket) else {
        panic!("the report arguments are an object");
    };
    let caller_at_host = person.clone().in_workspace(&workspace);
    let argument_at_host = InvokeArgument::Json(object);
    let reports = Arc::new(TestProvider::new().answering(move |caller, request| {
        if caller != &caller_at_host
            || request.target.provider != PROVIDER
            || request.target.address != tilo_actions::ActionAddress::new(RESOURCE, REPORTED)
            || request.argument != argument_at_host
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        }))
    }));
    hosted.provide(PROVIDER, reports.clone());
    let report = |workspace: &str, caller: &Caller, ticket: &Ticket| {
        holder.from_entered(
            entered,
            workspace,
            caller,
            CertifiedAction::Reported,
            arguments(ticket),
        )
    };
    let denied = Some(Err(ActionCallError::NotPermitted));
    let other_workspace = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
    assert_eq!(report(&other_workspace, &person, page_ticket).await, denied);
    let laptop = Caller::authenticated(cert.user_public_key, laptop_ticket.issuer_device());
    assert_eq!(report(&workspace, &laptop, page_ticket).await, denied);
    let wrong_signer = page_ticket
        .clone()
        .with_issuer_device(laptop_ticket.issuer_device());
    assert_eq!(report(&workspace, &person, &wrong_signer).await, denied);
    let wrong_transport = laptop_ticket
        .clone()
        .with_issuer_device(page_ticket.issuer_device());
    assert_eq!(report(&workspace, &person, &wrong_transport).await, denied);
    assert!(reports.asked().is_empty());

    assert_eq!(
        report(&workspace, &person, page_ticket).await,
        Some(Ok(json!({})))
    );
    assert_eq!(reports.asked().len(), 1);
    entered.close();
    entered
        .session()
        .until(Duration::from_secs(10), |status| {
            (status.phase == tilo_session_client::TransportPhase::Closed).then_some(())
        })
        .await
        .expect("the desk's session closes");
    assert_eq!(
        report(&workspace, &person, page_ticket).await,
        Some(Err(ActionCallError::NotReady))
    );
    assert_eq!(reports.asked().len(), 1);
}

/// No root on this computer: a test whose daemon holds none.
fn no_root() -> RootSource {
    Arc::new(|| Ok(None))
}

/// The root kept in `dir`, as a daemon reads it when a ticket is asked for.
pub(super) fn root_in(dir: std::path::PathBuf) -> RootSource {
    Arc::new(move || {
        let active = tilo_identity::store::resolve_active_identity(&dir)
            .ok_or_else(|| anyhow::anyhow!("the test's identity is gone"))?;
        Ok(Some(tilo_enrollment::root::PersonSigner::new(
            tilo_identity::store::load_from_path(active)?,
        )))
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
/// ticket; a second computer logs in with it and enters that workspace. The issuer runs on an
/// operating system Tilo does not control, as a `tilo login` computer does, so it adds the second
/// computer as one too.
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
        Some(Founded {
            workspace: workspace.to_string(),
            name: "Home".into(),
        }),
        Some(Arc::clone(&issuer)),
        place,
        actions.clone(),
    );
    let router = serve(&issuer, actions);

    // Its own certificate administers no devices; the person's root approves one anyway.
    let admin = json!({"capability": "admin", "server": true});
    let approved = ask_device(&tickets, ISSUE, admin).await;
    approved.expect("the person's root approves an administrator");
    let printed = ask_device(&tickets, ISSUE, json!({"capability": "member"}))
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

    let login = tilo_device_identity::login::load(&desk_place)
        .unwrap()
        .expect("the desk's login is kept")
        .1;
    assert!(
        login.cert.capabilities.foreign,
        "a foreign issuer adds a foreign computer"
    );
    assert_eq!(
        login.server_cert.device_public_key,
        issuer
            .service()
            .workspaces()
            .serving_certificate()
            .device_public_key,
        "the ticket names the certificate the issuer's session proves as the one it enters"
    );

    // The desk enters the workspace the issuer founded: a device its person's root certified is
    // that person's device, and needs no invitation.
    desk_endpoint.close().await;
    let entered = tilo_test_env::within(
        "the desk enters the founded workspace",
        tilo_api::daemon::enter::connect(&desk_key, &login, None),
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
async fn servers_host(root: &impl tilo_user_identity::StatementSigner) -> ServersHost {
    let (server_key, server_cert) = enrol(root, "home-server", DeviceCapabilities::member());
    let (issuer_key, issuer_cert) = enrol(root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig {
            state_root: state.path().join("server"),
            ..Default::default()
        },
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.public_key(),
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

/// The legacy ticket a second enrolled computer issues on its own keyed endpoint, and the
/// provider observing the signed reports it receives against the workspace's roster.
async fn peer_report_ticket(
    host: &ServersHost,
    workspace: &str,
    copy: tilo_peer_state::StatePeer,
) -> (
    Ticket,
    iroh::protocol::Router,
    Arc<tilo_actions::remote::TestProvider>,
) {
    use tilo_actions::remote::TestProvider;

    let peer_endpoint = endpoint(&host.issuer_key).await;
    let reports = Arc::new(TestProvider::new());
    let peer_actions = ActionService::new(host.issuer_key.clone(), reports.clone());
    assert!(copy.certificates().contains(&host.issuer_cert));
    peer_actions.hold(tilo_peer_state::replica::Replica::new(copy));
    let peer_login = login_to(
        &host.server,
        workspace,
        (&host.issuer_cert, &host.server_cert),
    );
    let peer_state = tempfile::tempdir().unwrap();
    let issuer = Tickets::new(
        (host.issuer_key.clone(), host.issuer_cert.clone()),
        peer_endpoint.clone(),
        Some((host.issuer_key.clone(), peer_login)),
        Arc::new(super::person::root),
        None,
        None,
        tilo_device_identity::login::Place::at(
            peer_state.path().to_path_buf(),
            tilo_device_identity::IdentityPaths::under(peer_state.path().join("keys")),
        ),
        peer_actions.clone(),
    );
    let router = iroh::protocol::Router::builder(peer_endpoint)
        .accept(ACTIONS_ALPN, peer_actions)
        .spawn();
    let issued = ask_device(&issuer, ISSUE, json!({})).await.unwrap();
    let text = issued["ticket"].as_str().unwrap();
    let ticket = Ticket::parse(text).unwrap();
    (ticket, router, reports)
}

/// Once the daemon holds its workspace, its normal native provider still reaches a ticket
/// issued by another enrolled device whose endpoint is keyed by that device's own certificate.
#[test]
fn a_workspace_on_this_device_reports_to_the_peer_that_issued_its_ticket() {
    use super::services::{Daemon, tests::isolated_root};
    use crate::RuntimeServices as _;
    use tilo_device_identity::login::REPORTED;

    let Some(profile) = isolated_root(
        "device::ticket_tests::a_workspace_on_this_device_reports_to_the_peer_that_issued_its_ticket",
    ) else {
        return;
    };
    let runtime = tilo_rt::app();
    let root = super::person::root().unwrap().unwrap();
    let (desk_key, desk_cert) = tilo_identity::broker::trusted_current_device("").unwrap();
    let host = runtime.handle().block_on(servers_host(&root));
    let workspace = tilo_server_protocol::servers::person_workspace_id(&desk_cert.user_public_key);
    let login = login_to(&host.server, &workspace, (&desk_cert, &host.server_cert));
    let place = tilo_device_identity::login::Place::here(tilo_api::client::device_keys()).unwrap();
    tilo_device_identity::login::store(&place, &login).unwrap();
    let daemon = Arc::new(Daemon::start_reporting(&profile, &mut |_| {}).unwrap());
    let api = daemon.api().unwrap();
    runtime.handle().block_on(async {
        let entered =
            tilo_api::daemon::enter::connect(&desk_key, &login, Some(daemon.serving().lent()))
                .await
                .unwrap();
        tilo_test_env::within(
            "the daemon holds the signed workspace",
            daemon.serving().hold(&entered),
        )
        .await;
        let id = tilo_desktop_model::WorkspaceId::parse(&workspace).unwrap();
        assert!(
            daemon
                .serving()
                .workspaces()
                .iter()
                .any(|held| held.id() == id)
        );

        let copy = daemon
            .serving()
            .peer(id)
            .expect("the workspace roster reached the desk");
        let (ticket, router, reports) = peer_report_ticket(&host, &workspace, copy).await;
        assert_eq!(ticket.issuer_device(), ticket.issuer.endpoint);
        assert_ne!(ticket.issuer_device(), login.server_cert.device_public_key);
        let report = json!({
            "ticket": ticket.encode(),
            "step": "entered",
            "agent": "Codex",
            "detail": "workspace open"
        });
        let answer = api
            .answer(
                "POST",
                &format!("/v1/workspaces/{workspace}/resources/devices/actions/{REPORTED}"),
                &[],
                &serde_json::to_vec(&report).unwrap(),
            )
            .await;
        let tilo_api::direct::AnswerBody::Whole(body) = answer.body else {
            panic!("the device action answers with JSON");
        };
        assert_eq!(answer.status, 200, "{}", String::from_utf8_lossy(&body));
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["status"], "ok");
        let calls = reports.asked();
        assert_eq!(calls.len(), 1);
        let called = &calls[0];
        assert_eq!(called.target.provider, PROVIDER);
        assert_eq!(
            called.target.address,
            tilo_actions::ActionAddress::new(RESOURCE, REPORTED)
        );
        assert_eq!(
            called.argument,
            InvokeArgument::Json(report.as_object().unwrap().clone())
        );
        let callers = reports.callers();
        assert_eq!(callers[0].workspace(), Some(workspace.as_str()));
        assert_eq!(
            callers[0].device_public_key(),
            Some(&desk_cert.device_public_key)
        );
        assert_eq!(
            callers[0].user_public_key(),
            Some(&desk_cert.user_public_key)
        );
        entered.close();
        router.shutdown().await.unwrap();
        host.server.shutdown().await;
    });
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
        Some((host.issuer_key.clone(), login.clone())),
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
