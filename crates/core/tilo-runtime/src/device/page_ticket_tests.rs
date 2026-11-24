//! A page's tickets, end to end: the person's server serves them through the device's one set of
//! tickets, and a tab calls them over the link it reaches the server's actions on, as a browser
//! does. A pairing link logs a tab in that the page which showed it approves; a tab signs in as a
//! person, approved by a device of theirs or by a page holding their root; and an agent's key is
//! approved with `tilo device add <ticket>` on a daemon.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use tilo_device_identity::login::{
    APPROVE, AgentAsked, Approved, Asked, CERTIFY, CertifyAnswer, FOLLOW, ISSUE, KeyFor, PENDING,
    Tab, Ticket,
};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions};
use tilo_session_client::session::TicketLink;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::ticket::{OnThisDevice, Tickets};
use super::ticket_tests::{Studio, ask_device, endpoint, enrol, no_root, root_in, serve, studio};

/// How long a call that waits for the person is followed.
const FOLLOW_FOR: Duration = Duration::from_secs(20);

/// The kind of a workspace's list of its agents, which Assistant keeps.
const AGENTS: &str = "tilo.workspace.agent";

/// A tab, or a page, calling the server's ticket actions as `key` over `link`.
async fn ask(
    link: &TicketLink,
    key: &SigningKey,
    host: [u8; 32],
    action: &str,
    argument: Value,
) -> Result<Value, String> {
    let Value::Object(argument) = argument else {
        unreachable!("an argument is an object")
    };
    link.ask(
        Arc::new(key.clone()),
        key.verifying_key().to_bytes(),
        host,
        action,
        argument,
        FOLLOW_FOR,
    )
    .await
    .expect("the server answers over the page's link")
}

/// A new key, as a tab makes one.
fn new_key() -> SigningKey {
    SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng))
}

/// The certificate the person's root signs for the tab `key`, listed as `label`, as the page
/// holding the root signs it ([`Tab::certificate`]).
fn signed_for(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let tab = Tab {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        label: label.to_owned(),
        key_for: None,
    };
    let mut cert = tab
        .certificate(root.verifying_key().to_bytes(), tilo_clock::unix_now_ns())
        .unwrap();
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// What a tab that followed its sign-in ticket by `id` reads once it is signed in: its
/// certificate and the server's, both as the text they were signed as.
async fn followed(
    link: &TicketLink,
    key: &SigningKey,
    host: [u8; 32],
    id: &Value,
) -> (Value, DeviceCert, DeviceCert) {
    let answer = ask(link, key, host, FOLLOW, json!({ "id": id }))
        .await
        .expect("the tab follows its ticket");
    let certified = serde_json::from_str(answer["certified"].as_str().expect("signed in")).unwrap();
    let host_cert = serde_json::from_str(answer["hostCert"].as_str().unwrap()).unwrap();
    (answer, certified, host_cert)
}

/// A pairing link: the page holding the person's root asks for a login ticket for a browser,
/// the new tab logs in with it over its own link, and the page, which follows the ticket, signs
/// the certificate the server built and approves. The tab is a device of the person's, kept by
/// the server, and the ticket is spent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_logs_a_tab_in_that_the_page_showing_it_approves() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        workspace,
        laptop_key,
        laptop_cert,
        login,
        ..
    } = studio(&root, state.path()).await;
    let host = login.server_cert.device_public_key;
    let reach = tilo_server_client::ServerAddress::at(server.node_addr());
    let user = hex::encode(root.verifying_key().to_bytes());

    let page = TicketLink::new(reach.clone());
    let minted = ask(
        &page,
        &laptop_key,
        host,
        ISSUE,
        json!({"user": user, "workspace": workspace, "browser": true, "name": "Studio"}),
    )
    .await
    .expect("the page asks for a pairing link's ticket");
    let ticket = Ticket::parse(minted["ticket"].as_str().unwrap()).unwrap();

    // The new tab opens the link: it logs in with the ticket, and waits for the page.
    let phone = new_key();
    let tab = Arc::new(TicketLink::new(reach.clone()));
    let logging_in = tokio::spawn({
        let tab = Arc::clone(&tab);
        let phone = phone.clone();
        let secret = ticket.secret_hex();
        async move {
            ask(
                &tab,
                &phone,
                host,
                CERTIFY,
                json!({
                    "secret": secret,
                    "device_public_key": hex::encode(phone.verifying_key().to_bytes()),
                    "agent": "Safari on iPhone",
                    "label": "Safari on iPhone",
                }),
            )
            .await
        }
    });

    // The page sees the tab that logged in, and approves it with the root it holds.
    let asked: Asked = tilo_test_env::within("the tab asks", async {
        loop {
            let status = ask(
                &page,
                &laptop_key,
                host,
                FOLLOW,
                json!({"id": minted["id"]}),
            )
            .await
            .unwrap();
            if let Some(asked) = status.get("asked") {
                return serde_json::from_value(asked.clone()).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert_eq!(asked.device_public_key, phone.verifying_key().to_bytes());
    assert_eq!(asked.label, "Safari on iPhone");
    let mut cert = asked.cert.clone();
    sign_device_cert(&mut cert, &root).unwrap();
    let approved = Approved {
        cert: cert.clone(),
        approver: laptop_cert.clone(),
        signature: laptop_key.sign(&asked.digest).to_bytes(),
    };
    ask(
        &page,
        &laptop_key,
        host,
        "answer",
        json!({"id": minted["id"], "approved": approved}),
    )
    .await
    .expect("the page's approval is taken");

    let answer: CertifyAnswer = serde_json::from_value(
        logging_in
            .await
            .unwrap()
            .expect("the tab is certified once the page approved"),
    )
    .unwrap();
    assert_eq!(
        answer.cert, cert,
        "the tab holds the certificate the root signed"
    );
    assert_eq!(answer.enter.workspace, workspace);
    assert_eq!(
        answer.enter.server_cert.device_public_key, host,
        "and pins the server it entered through"
    );

    // The ticket is spent: another tab logging in with it is refused.
    let late = new_key();
    let spent = ask(
        &tab,
        &late,
        host,
        CERTIFY,
        json!({
            "secret": ticket.secret_hex(),
            "device_public_key": hex::encode(late.verifying_key().to_bytes()),
            "agent": "Firefox",
        }),
    )
    .await;
    assert!(spent.is_err(), "a pairing link works once: {spent:?}");

    tab.close().await;
    page.close().await;
    server.shutdown().await;
}

/// A tab joining a workspace as its person (`joinViaHost`), and the last step of a tab's sign-in:
/// it asks for a sign-in ticket naming the person with its own key, a device of theirs lists it
/// with the certificate it holds and approves it, and the tab reads its certificate and the
/// server's when it follows the ticket. No pairing step is left: the key that asked is the key
/// signed in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tab_naming_its_person_is_signed_in_by_a_device_of_theirs() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        laptop_key,
        laptop_cert,
        login,
        ..
    } = studio(&root, state.path()).await;
    let host = login.server_cert.device_public_key;
    let reach = tilo_server_client::ServerAddress::at(server.node_addr());
    let user = hex::encode(root.verifying_key().to_bytes());

    let tab = TicketLink::new(reach.clone());
    let key = new_key();
    let minted = ask(
        &tab,
        &key,
        host,
        ISSUE,
        json!({"direction": "sign-in", "user": user, "label": "Firefox on Linux"}),
    )
    .await
    .expect("the tab asks to be signed in as Ada");
    let before = ask(&tab, &key, host, FOLLOW, json!({"id": minted["id"]}))
        .await
        .unwrap();
    assert!(
        before.get("certified").is_none(),
        "not signed in yet: {before}"
    );
    let stranger = new_key();
    assert!(
        ask(&tab, &stranger, host, FOLLOW, json!({"id": minted["id"]}))
            .await
            .is_err(),
        "only the key that asked reads its ticket"
    );

    let laptop = TicketLink::new(reach.clone());
    let listed = ask(
        &laptop,
        &laptop_key,
        host,
        PENDING,
        json!({"as": laptop_cert}),
    )
    .await
    .expect("Ada's laptop lists what waits for her");
    let waiting = listed["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .find(|waiting| waiting["device"] == hex::encode(key.verifying_key().to_bytes()))
        .expect("the tab waits for Ada")
        .clone();
    assert_eq!(waiting["label"], "Firefox on Linux");
    // Another person's device sees nothing of hers.
    let (other_root, _) = tilo_user_identity::UserIdentity::generate("Bea", 2);
    let (bea_key, bea_cert) = enrol(&other_root, "bea", DeviceCapabilities::primary());
    let theirs = ask(&laptop, &bea_key, host, PENDING, json!({"as": bea_cert})).await;
    assert!(
        theirs.map_or(true, |listed| listed["waiting"] == json!([])),
        "Bea lists none of Ada's sign-ins"
    );

    let cert = signed_for(&root, &key, "Firefox on Linux");
    ask(
        &laptop,
        &laptop_key,
        host,
        APPROVE,
        json!({"secret": waiting["secret"], "cert": cert, "as": laptop_cert, "label": "laptop"}),
    )
    .await
    .expect("Ada's laptop signs the tab in");

    let (answer, certified, host_cert) = followed(&tab, &key, host, &minted["id"]).await;
    assert_eq!(
        certified, cert,
        "the tab reads the certificate the root signed"
    );
    assert_eq!(host_cert.device_public_key, host, "and the server's");
    assert_eq!(answer["claimed"]["user"], user);
    assert_eq!(answer["claimed"]["by"], "laptop");

    tab.close().await;
    laptop.close().await;
    server.shutdown().await;
}

/// The person's root page approving sign-ins: a page that holds the root calls as the root,
/// lists what waits for its person, and signs it in; a key that is not the root it names is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_root_page_lists_and_approves_the_sign_ins_waiting_for_its_person() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio { server, login, .. } = studio(&root, state.path()).await;
    let host = login.server_cert.device_public_key;
    let reach = tilo_server_client::ServerAddress::at(server.node_addr());
    let user = hex::encode(root.verifying_key().to_bytes());

    let tab = TicketLink::new(reach.clone());
    let key = new_key();
    let minted = ask(
        &tab,
        &key,
        host,
        ISSUE,
        json!({"direction": "sign-in", "user": user, "label": "Safari on iPad"}),
    )
    .await
    .unwrap();

    let page = TicketLink::new(reach.clone());
    let impostor = new_key();
    assert!(
        ask(&page, &impostor, host, PENDING, json!({"user": user}))
            .await
            .is_err(),
        "a key that is not Ada's root lists nothing as her"
    );
    let listed = ask(&page, &root, host, PENDING, json!({"user": user}))
        .await
        .expect("the root page lists what waits for Ada");
    let waiting = listed["waiting"][0].clone();
    assert_eq!(
        waiting["device"],
        hex::encode(key.verifying_key().to_bytes())
    );

    let cert = signed_for(&root, &key, "Safari on iPad");
    ask(
        &page,
        &root,
        host,
        APPROVE,
        json!({"user": user, "secret": waiting["secret"], "cert": cert, "label": "Your browser"}),
    )
    .await
    .expect("the root page signs the tab in");
    let (_, certified, _) = followed(&tab, &key, host, &minted["id"]).await;
    assert_eq!(certified, cert);
    let after = ask(&page, &root, host, PENDING, json!({"user": user}))
        .await
        .unwrap();
    assert_eq!(after["waiting"], json!([]), "a signed-in tab waits no more");

    tab.close().await;
    page.close().await;
    server.shutdown().await;
}

/// An agent's key, served as a sign-in ticket by the daemon of a device that holds the person's
/// root, is approved the ordinary way: `tilo device add <ticket>` (`devices#pending`, then
/// `devices#approve` with the certificate the root signed). The agent hears its certificate at
/// once, with no restart of anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tilo_device_add_approves_an_agents_key_and_the_agent_hears_it_at_once() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        hosted,
        workspace,
        laptop_key,
        laptop_cert,
        login,
    } = studio(&root, state.path()).await;
    let identity = tempfile::tempdir().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        identity.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let laptop_endpoint = endpoint(&laptop_key).await;
    let copies = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("laptop-service"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's service opens");
    let service = ActionService::new(laptop_key.clone(), Arc::new(NoActions));
    let laptop = Tickets::new(
        (laptop_key.clone(), laptop_cert.clone()),
        laptop_endpoint.clone(),
        Some(Arc::new(crate::device::home::Home::new(
            copies,
            laptop_cert.clone(),
            Some(login.clone()),
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

    // Ada makes the agent in Studio from her laptop, through Assistant's own create-agent action
    // on the server: the agent the key is asked for is one the workspace names.
    hosted
        .author_agents(&workspace, |_| Ok(()))
        .await
        .expect("Studio's list of agents starts");
    let agents = tilo_agent_session::resource_provider(
        &tilo_peer_state::document_frame::DocumentRoute {
            app_id: tilo_app_id::apps::ASSISTANT.to_owned(),
            cluster_id: workspace.clone(),
            document: collab_types::workspace_document_id(AGENTS),
        },
    );
    let from_laptop = Arc::new(
        ActionService::new(laptop_key.clone(), Arc::new(NoActions)).remote(
            &laptop_endpoint,
            &server.node_addr(),
            &workspace,
        ),
    );
    let made = from_laptop
        .invoke_until_settled(
            tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: agents,
                    address: tilo_actions::ActionAddress::new(AGENTS, "create-agent"),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(
                    json!({"name": "Claude", "id": "claude"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
            Duration::from_secs(20),
        )
        .await;
    assert!(
        matches!(
            made,
            Some(Ok(tilo_actions::InvocationUpdate::Succeeded { .. }))
        ),
        "Ada makes the agent in Studio: {made:?}"
    );

    // The device the agent runs on, another of the person's, holds the agent's workspace and
    // asks for the agent's key on its own endpoint; the laptop, which holds their root,
    // approves it.
    let (host_key, host_cert) = enrol(&root, "agent host", DeviceCapabilities::member());
    let host_login = tilo_device_identity::login::Device {
        cert: host_cert.clone(),
        ..login.clone()
    };
    let host_copies = super::workspaces::ServiceWorkspaces::open_for_test(
        &state.path().join("host-service"),
        &host_key,
        &host_cert,
        &tilo_api::daemon::enter::server_of(&host_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the agent host's service opens");
    let host_session = tilo_test_env::within(
        "the agent host enters the agent's workspace",
        tilo_api::daemon::enter::connect(&host_key, &host_login, Some(host_copies.lent())),
    )
    .await
    .expect("the agent host enters");
    tilo_test_env::within(
        "the agent host holds the agent's workspace",
        host_copies.hold(&host_session),
    )
    .await;
    let host_actions = host_copies.copies().action_service();
    let host = Tickets::new(
        (host_key.clone(), host_cert),
        host_copies.endpoint(),
        None,
        no_root(),
        None,
        Some(Arc::clone(&host_copies)),
        tilo_device_identity::login::Place::at(
            state.path().join("host"),
            tilo_device_identity::IdentityPaths::under(state.path().join("host-keys")),
        ),
        host_actions.clone(),
    );
    // As the daemon does: what the person's other devices ask of this one.
    host_copies.copies().provide(
        tilo_device_identity::login::PROVIDER,
        Arc::new(OnThisDevice(host.clone())),
    );
    let host_router = serve(&host_copies, host_actions);

    let agent = new_key();
    let (ticket, mut certified) = host
        .serve_key(Tab {
            device_public_key: hex::encode(agent.verifying_key().to_bytes()),
            label: "Claude Code".into(),
            key_for: Some(KeyFor::Agent(AgentAsked {
                workspace: workspace.clone(),
                agent: "claude".into(),
            })),
        })
        .expect("the daemon serves the agent's key");
    let text = ticket.encode();

    // `tilo device add <ticket>`: what waits, then the root's certificate for it.
    let pending = ask_device(&laptop, PENDING, json!({ "ticket": text }))
        .await
        .expect("the daemon shows the agent's key");
    assert_eq!(
        pending["device"],
        hex::encode(agent.verifying_key().to_bytes())
    );
    let tab: Tab = serde_json::from_value(json!({
        "device_public_key": hex::encode(agent.verifying_key().to_bytes()),
        "label": "Claude Code",
        "for": {"agent": {"workspace": workspace, "agent": "claude"}},
    }))
    .unwrap();
    let mut cert = tab
        .certificate(root.verifying_key().to_bytes(), tilo_clock::unix_now_ns())
        .unwrap();
    sign_device_cert(&mut cert, &root).unwrap();
    ask_device(&laptop, APPROVE, json!({ "ticket": text, "cert": cert }))
        .await
        .expect("the root's certificate approves the agent's key");

    let heard = tilo_test_env::within("the agent hears its certificate", async {
        loop {
            if let Some(cert) = certified.borrow_and_update().clone() {
                return cert;
            }
            certified.changed().await.expect("the ticket is served");
        }
    })
    .await;
    assert_eq!(heard, cert);
    assert!(
        heard.capabilities.agent.is_some(),
        "the key is the agent's grant"
    );

    // The agent acts on it at once: with nothing restarted, the person's server admits what its
    // key signs in the workspace the grant the root just signed names.
    let at_server = tilo_server_client::ServerAddress::at(server.node_addr());
    let agent_endpoint = at_server.bind(Some(agent.to_bytes())).await.unwrap();
    let as_agent = ActionService::new(agent.clone(), Arc::new(NoActions));
    let in_workspace = as_agent.remote(&agent_endpoint, &at_server.addr(), &workspace);
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        agent.verifying_key().to_bytes(),
    );
    let listed = tilo_test_env::within(
        "the server answers the agent",
        tilo_actions::remote::ActionProvider::query(
            &in_workspace,
            &caller,
            tilo_actions::remote::CatalogueQuery::default(),
        ),
    )
    .await
    .expect("the agent's ask is answered");
    assert!(
        listed.is_ok(),
        "the server admits the agent's key in the workspace its grant names: {listed:?}"
    );
    agent_endpoint.close().await;
    host_session.close();

    let _ = host_router.shutdown().await;
    let _ = router.shutdown().await;
    server.shutdown().await;
}

/// A guest link (`redeemTicket`): the tab asks for a sign-in ticket naming the workspace's root,
/// as the host's certificate names it, and hands the ticket to the web host the link came from.
/// The host, which holds that root and spends the guest link, signs in exactly the key that asked,
/// by the ticket alone, and the tab reads its certificate when it follows its ticket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_link_signs_in_the_tab_that_handed_its_ticket_to_the_web_host() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Studio", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio { server, login, .. } = studio(&root, state.path()).await;
    let host = login.server_cert.device_public_key;
    let reach = tilo_server_client::ServerAddress::at(server.node_addr());
    let owner = hex::encode(login.server_cert.user_public_key);

    let tab = TicketLink::new(reach.clone());
    let key = new_key();
    let minted = ask(
        &tab,
        &key,
        host,
        ISSUE,
        json!({"direction": "sign-in", "user": owner, "label": "Firefox on Linux"}),
    )
    .await
    .expect("the visitor's tab asks to be signed in by the workspace's root");
    // What the tab posts to the web host: the ticket text, nothing else of its own.
    let handed = Ticket::parse(minted["ticket"].as_str().unwrap()).unwrap();

    // The web host, holding the root, spends the guest link on that ticket.
    let web_host = TicketLink::new(reach.clone());
    let cert = signed_for(&root, &key, "Firefox on Linux");
    ask(
        &web_host,
        &root,
        host,
        APPROVE,
        json!({"user": owner, "secret": handed.secret_hex(), "cert": cert, "label": "Studio"}),
    )
    .await
    .expect("the web host signs the visitor's tab in");
    // A certificate for another key is never taken for this ticket.
    let other = new_key();
    let wrong = signed_for(&root, &other, "Firefox on Linux");
    assert!(
        ask(
            &web_host,
            &root,
            host,
            APPROVE,
            json!({"user": owner, "secret": handed.secret_hex(), "cert": wrong, "label": "Studio"}),
        )
        .await
        .is_err(),
        "the ticket signs in the key that asked, once"
    );

    let (_, certified, host_cert) = followed(&tab, &key, host, &minted["id"]).await;
    assert_eq!(
        certified, cert,
        "the tab is signed in as the root signed it"
    );
    assert_eq!(certified.user_public_key, login.server_cert.user_public_key);
    assert_eq!(host_cert.device_public_key, host);

    tab.close().await;
    web_host.close().await;
    server.shutdown().await;
}
