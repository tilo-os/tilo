//! A person has one list of workspaces, however each was made: a computer of
//! theirs that logged in (its own device key, through the client `tilo` uses,
//! in a session of its own or over one its holder keeps) and a key of
//! theirs for all their workspaces (over `/v1`) both read and write the list
//! the server keeps for the person.

use super::api::ISSUER;
use super::api_on_server::{OnServer, api_on_server, approved_key, headers, json_of};
use super::api_v1::Holder;
use super::*;
use serde_json::{Value, json};
use tilo_api::client::Client;

/// Where a key makes a workspace and lists them: the account's actions.
const CREATE: &str = "/v1/resources/account/workspaces/actions/create";
const LIST: &str = "/v1/resources/account/workspaces/actions/list";

/// The ids of the workspaces an `account/workspaces#list` answer lists.
fn ids(listed: &Value) -> Vec<String> {
    listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|workspace| workspace["id"].as_str().map(str::to_owned))
        .collect()
}

/// `method path` with `body` as the key `token`: the status and the answer.
async fn as_key(
    on_server: &OnServer,
    token: &str,
    method: hyper::Method,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let bearer = format!("Bearer {token}");
    json_of(
        on_server
            .api
            .dispatch(
                &method,
                path,
                "",
                &headers(&[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/json"),
                ]),
                body.to_string().as_bytes(),
            )
            .await,
    )
    .await
}

/// The computer (`computer`, called `who`) and the key `token` each make a
/// workspace; both list both, and each reaches the other's by its id.
async fn one_list(on_server: &OnServer, token: &str, who: &str, computer: &Client) {
    let (status, made_by_key) = as_key(
        on_server,
        token,
        hyper::Method::POST,
        CREATE,
        &json!({"name": format!("Made by the backend beside {who}")}),
    )
    .await;
    assert_eq!(status, 200, "{made_by_key}");
    let by_key = made_by_key["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let listed = computer.workspaces().await;
    assert!(
        listed
            .as_ref()
            .is_ok_and(|listed| ids(listed).contains(&by_key)),
        "{who} lists the key's {by_key}: {listed:?}"
    );
    let read = computer
        .get(&format!("/v1/workspaces/{by_key}/resources"))
        .await;
    assert!(read.is_ok(), "{who} reads the key's workspace: {read:?}");

    let made_here = computer
        .account(
            "account/workspaces",
            "create",
            &json!({"name": format!("Made by {who}")}),
        )
        .await;
    let here = made_here
        .as_ref()
        .ok()
        .and_then(|made| made["workspace"]["id"].as_str())
        .unwrap_or_else(|| panic!("{who} makes a workspace: {made_here:?}"))
        .to_owned();
    let listed = computer.workspaces().await;
    assert!(
        listed
            .as_ref()
            .is_ok_and(|listed| ids(listed).contains(&here)),
        "{who} lists its own {here}: {listed:?}"
    );
    let (status, key_lists) = as_key(on_server, token, hyper::Method::POST, LIST, &json!({})).await;
    assert_eq!(status, 200, "{key_lists}");
    assert!(
        ids(&key_lists["result"]).contains(&here),
        "the key lists {who}'s {here}: {key_lists}"
    );
    let (status, read) = as_key(
        on_server,
        token,
        hyper::Method::GET,
        &format!("/v1/workspaces/{here}/resources"),
        &json!({}),
    )
    .await;
    assert_eq!(status, 200, "the key reads {who}'s workspace: {read}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_and_a_key_share_the_persons_workspace_list() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &page, "Visa backend").await;

    // A computer `tilo login` made a device of Ada's, acting in a session of its own.
    let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
    let file = tilo_api::direct::KeyFile::new(
        &on_server.server,
        Some(&on_server.web_entry),
        "Ada's laptop",
        &laptop.key,
        &laptop.cert,
    )
    .reaching_at(on_server.hosted.web.host_info().clone());
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("laptop.json");
    std::fs::write(&key_file, serde_json::to_vec(&file).unwrap()).unwrap();
    let own_session = Client::open(tilo_api::client::Reach {
        api: None,
        key_file: Some(&key_file),
        json: false,
    })
    .unwrap();
    one_list(&on_server, &token, "the laptop", &own_session).await;

    // Another computer of hers, whose commands go over the one session its holder keeps.
    let tower = enrol(&ada, "Ada's tower", DeviceCapabilities::member());
    let file = tilo_api::direct::KeyFile::new(
        &on_server.server,
        Some(&on_server.web_entry),
        "Ada's tower",
        &tower.key,
        &tower.cert,
    )
    .reaching_at(on_server.hosted.web.host_info().clone());
    let holder = Arc::new(tilo_api::server::Server::new(
        on_server.hosted.web.host_info().clone(),
    ));
    let held = holder
        .connect(
            &tilo_api::grant_key::GrantKey::held(
                tower.key.clone(),
                tower.cert.clone(),
                tilo_clock::unix_now_ms().saturating_mul(1_000_000),
            )
            .unwrap(),
        )
        .await
        .expect("the tower reaches its server");
    let direct = Arc::new(tilo_api::direct::Direct::over(&file, held.device.clone()).unwrap());
    let socket = keys.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(tilo_api::direct::serve_current(listener, move || {
        std::future::ready(Ok(Arc::clone(&direct)))
    }));
    one_list(&on_server, &token, "the tower", &Client::session(&socket)).await;

    // A computer `tilo login` set up: its holder entered the workspace the login named, as the
    // person's agent there, and its commands go over that session. Making a workspace
    // is what the person's grants ask them first about; this computer holds Ada's root, which
    // answers right here, so the command completes in one go.
    let logged_into = first_workspace(&on_server, &token).await;
    let mac = Holder::start(&on_server, &ada, &logged_into, "Claude Code").await;
    let approving = approve_as_root(&on_server, &ada, &logged_into, &agent_of(&mac)).await;
    let started = std::time::Instant::now();
    one_list(&on_server, &token, "the mac", &mac.command()).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "approved here, the commands went on at once: {:?}",
        started.elapsed()
    );
    // A people action asks first too, and is approved here the same way: in a workspace of
    // Ada's own.
    let (status, made) = as_key(
        &on_server,
        &token,
        hyper::Method::POST,
        CREATE,
        &json!({"name": "Ada's letters"}),
    )
    .await;
    assert_eq!(status, 200, "{made}");
    let letters = made["result"]["workspace"]["id"].as_str().unwrap();
    let invited = mac
        .command()
        .post(
            &format!("/v1/workspaces/{letters}/resources/workspace/invitations/actions/invite"),
            &json!({"role": "editor"}),
        )
        .await
        .expect("approved here, the computer's agent invites");
    assert!(invited.to_string().contains("http"), "{invited}");
    let answered = approving.stop().await;
    assert!(
        answered >= 2,
        "Ada's root answered the agent's asks: {answered}"
    );
    assert!(mac.device.is_live());
}

/// The first workspace the key `token` lists.
async fn first_workspace(on_server: &OnServer, token: &str) -> String {
    let (status, listed) = as_key(on_server, token, hyper::Method::POST, LIST, &json!({})).await;
    assert_eq!(status, 200, "{listed}");
    ids(&listed["result"])
        .first()
        .cloned()
        .expect("a workspace of Ada's")
}

/// The person's root, held on the computer, answering what `agent` asks them, allowing each, as
/// a computer holding their root does while its command is under way; until stopped.
struct RootApproving {
    stop: Arc<tokio::sync::Notify>,
    answered: tokio::task::JoinHandle<usize>,
}

impl RootApproving {
    /// Stop answering: how many questions it allowed.
    async fn stop(self) -> usize {
        self.stop.notify_one();
        self.answered.await.unwrap()
    }
}

/// The agent the computer `holder` is, as its person's questions name it.
fn agent_of(holder: &Holder) -> String {
    tilo_actions::remote::device_agent_id(&holder.laptop.cert.user_public_key, "Claude Code")
}

async fn approve_as_root(
    on_server: &OnServer,
    root: &SigningKey,
    workspace: &str,
    agent: &str,
) -> RootApproving {
    use tilo_actions::remote::asked::AskedBy;
    let stop = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::clone(&stop);
    let mut page = super::api_unattended::page_in(on_server, root, workspace).await;
    let agent = agent.to_owned();
    let answered = tokio::spawn(async move {
        let mut answered = 0;
        loop {
            for asked in page.asks().await {
                let AskedBy::Agent(who) = &asked.facts.by else {
                    continue;
                };
                if who.id == agent && page.resolve(&asked, "approve", None).await.is_ok() {
                    answered += 1;
                }
            }
            tokio::select! {
                () = stopped.notified() => break,
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }
        }
        answered
    });
    RootApproving { stop, answered }
}

/// On a computer that does not hold the person's root, a command its agent's grants ask the
/// person about answers that it waits for their approval, never that the workspace was out of
/// reach, and what it asked is done once they allow it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_without_the_root_says_it_waits_for_the_persons_approval() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &page, "Visa backend").await;
    let logged_into = first_workspace(&on_server, &token).await;
    let mac = Holder::start(&on_server, &ada, &logged_into, "Claude Code").await;

    let waiting = mac
        .command()
        .account(
            "account/workspaces",
            "create",
            &json!({"name": "Made while Ada was away"}),
        )
        .await
        .expect_err("nobody approved it yet");
    let said = format!("{waiting:#}");
    assert_eq!(
        tilo_api::client::Refusal::of(&waiting).map(tilo_api::client::Refusal::code),
        Some("awaiting_approval"),
        "{said}"
    );
    assert!(said.contains("Waiting for your approval"), "{said}");
    assert!(!said.contains("couldn't reach"), "{said}");

    // Ada allows it on a device holding her root: the workspace is made.
    let approving = approve_as_root(&on_server, &ada, &logged_into, &agent_of(&mac)).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (_, listed) = as_key(&on_server, &token, hyper::Method::POST, LIST, &json!({})).await;
        if listed.to_string().contains("Made while Ada was away") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "allowed, the workspace is made: {listed}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(approving.stop().await, 1, "Ada was asked once");
    assert!(mac.device.is_live());
}
