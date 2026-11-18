//! A person has one list of workspaces, however each was made: a computer of
//! theirs that logged in (its own device key, through the client `tilo` uses,
//! in a session of its own or over one its holder keeps) and a key of
//! theirs for all their workspaces (over `/v1`) both read and write the list
//! the server keeps for the person.

use super::api::ISSUER;
use super::api_on_server::{OnServer, api_on_server, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use tilo_api::client::Client;

/// The ids of the workspaces a `GET /v1/workspaces` answer lists.
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
        "/v1/workspaces",
        &json!({"name": format!("Made by the backend beside {who}")}),
    )
    .await;
    assert_eq!(status, 201, "{made_by_key}");
    let by_key = made_by_key["id"].as_str().unwrap().to_owned();
    let listed = computer.get("/v1/workspaces").await;
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
        .post("/v1/workspaces", &json!({"name": format!("Made by {who}")}))
        .await;
    let here = made_here
        .as_ref()
        .ok()
        .and_then(|made| made["id"].as_str())
        .unwrap_or_else(|| panic!("{who} makes a workspace: {made_here:?}"))
        .to_owned();
    let listed = computer.get("/v1/workspaces").await;
    assert!(
        listed
            .as_ref()
            .is_ok_and(|listed| ids(listed).contains(&here)),
        "{who} lists its own {here}: {listed:?}"
    );
    let (status, key_lists) = as_key(
        on_server,
        token,
        hyper::Method::GET,
        "/v1/workspaces",
        &json!({}),
    )
    .await;
    assert_eq!(status, 200, "{key_lists}");
    assert!(
        ids(&key_lists).contains(&here),
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
    );
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
    );
    let held = Arc::new(tilo_api::server::Server::new(
        &tilo_net::Client::builder().build().unwrap(),
        &on_server.server,
    ))
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
}
