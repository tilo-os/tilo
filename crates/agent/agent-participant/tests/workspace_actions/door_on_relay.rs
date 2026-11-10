//! The MCP front door on a relay as tilo.sh runs them: the relay's own
//! person is its operator, its app workers answer every workspace with no
//! tab of anyone's open, the site's home workspace lets people in through
//! its door, and a person approves keys for their backend at the door.
//!
//! The door's tests drive it in process; [`serve`] serves it on loopback
//! for a client outside this process (an SDK's end-to-end run), with the
//! keys a person approved there.

use super::{
    Arc, Device, DeviceCapabilities, Hosted, ModelChoice, Options, SigningKey, common, enrol,
    harness, support,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tilo_front_door::{Config, Door, store::Store};
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_user_identity::{DeviceCert, SignInProof};

/// A reply's status and JSON body.
pub async fn json_of(reply: hyper::Response<http_body_util::Full<bytes::Bytes>>) -> (u16, Value) {
    use http_body_util::BodyExt as _;
    let status = reply.status().as_u16();
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

pub fn headers(pairs: &[(&'static str, &str)]) -> hyper::HeaderMap {
    let mut map = hyper::HeaderMap::new();
    for (name, value) in pairs {
        map.insert(*name, value.parse().unwrap());
    }
    map
}

/// The query of `url`, its plain values.
fn query_of(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// The relay as its operator runs it, the site whose home workspace lets
/// anyone in through its door, and the front door before both.
pub struct OnRelay {
    pub door: Arc<Door>,
    /// The door's public origin, as agents reach it.
    pub issuer: String,
    /// The site's home workspace.
    pub lobby: String,
    _site: test_http::Responder,
    _hosted: Hosted,
    _state: tempfile::TempDir,
}

/// The door at `issuer` before a relay serving its app workers.
pub async fn door_on_relay(issuer: &str) -> OnRelay {
    // The relay, as its operator runs it.
    let (operator, _) = tilo_user_identity::UserIdentity::generate("Relay operator", 1);
    let host = enrol(
        &operator,
        "relay",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = support::scratch_dir();
    let hosted = harness::hosted::start(
        &operator,
        &host,
        state.path(),
        Options {
            plain: true,
            ..Options::offering(common::catalogue, ModelChoice::Scripted)
        },
    )
    .await;
    let relay = hosted.web.plain_origin.clone().unwrap();
    let workspaces = hosted.relay.workspaces().unwrap();
    workspaces
        .offer_own_workspaces(tilo_relay::workspaces::OwnWorkspaceOffer::free(
            "relay.tilo.test",
        ))
        .unwrap();
    // The site's home workspace, whose door lets anyone in.
    let lobby = workspaces.create("Lounge").await.unwrap().workspace;
    workspaces
        .publish(
            &lobby,
            workspaces.serving_certificate(),
            Listing {
                description: "Everyone's".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    // The site names that door in its `entry.json`.
    let entry = json!({"home": {"host": {"origin": relay},
                                "admission": format!("{relay}/peer/v1/directory/{lobby}/")}});
    let site = test_http::Responder::handle(move |request| {
        (request.target == "/entry.json").then(|| {
            test_http::Reply::status(200)
                .header("content-type", "application/json")
                .body(entry.to_string())
        })
    });
    let door = Door::new(
        &Config {
            issuer: issuer.into(),
            web_entry: format!("http://127.0.0.1:{}", site.port()),
            relay: relay.clone(),
        },
        Store::in_memory(),
    )
    .unwrap();

    OnRelay {
        door,
        issuer: issuer.to_owned(),
        lobby,
        _site: site,
        _hosted: hosted,
        _state: state,
    }
}

/// A key called `name`, asked for as a terminal does and approved by the
/// person's Tilo on the web, which this plays: it signs the door's
/// challenge with a device of `root`'s and certifies the grant's key as the
/// server key the door asked for, lasting until revoked.
pub async fn approved_key(
    on_relay: &OnRelay,
    root: &SigningKey,
    device: &Device,
    name: &str,
) -> String {
    let door = &on_relay.door;
    let (status, asked) = json_of(
        door.dispatch(
            &hyper::Method::POST,
            tilo_front_door::keys::NEW_PATH,
            "",
            &hyper::HeaderMap::new(),
            json!({"name": name}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 201, "{asked}");
    let approve = asked["approve_url"].as_str().unwrap();
    let to_sign_in = door
        .dispatch(
            &hyper::Method::GET,
            approve.strip_prefix(on_relay.issuer.as_str()).unwrap(),
            "",
            &hyper::HeaderMap::new(),
            b"",
        )
        .await;
    let location = to_sign_in.headers()[hyper::header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let query = query_of(&location);
    let challenge: [u8; 32] = hex::decode(&query["challenge"])
        .unwrap()
        .try_into()
        .unwrap();
    let grant_key: [u8; 32] = hex::decode(&query["grant_key"])
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(query["grant_scope"], "all");
    assert_eq!(query["grant_kind"], "server_key");
    assert_eq!(query["grant_expires"], "never");
    let now_ns = tilo_clock::unix_now_ns();
    let mut grant = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: grant_key,
        device_label: format!("{name} via {}", on_relay.issuer),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: now_ns.saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, root).unwrap();
    let proof = SignInProof::sign(
        device.cert.clone(),
        &device.key,
        &on_relay.issuer,
        challenge,
    )
    .unwrap();
    let form = format!(
        "pending={}&proof={}&grant={}",
        query["pending"],
        tilo_front_door::sign_in::encode_proof(&proof).unwrap(),
        tilo_front_door::sign_in::encode_cert(&grant).unwrap()
    );
    let signed_in = door
        .dispatch(
            &hyper::Method::POST,
            "/sign-in",
            "",
            &headers(&[("content-type", "application/x-www-form-urlencoded")]),
            form.as_bytes(),
        )
        .await;
    assert!(
        signed_in.status().is_success() || signed_in.status().is_redirection(),
        "{}",
        signed_in.status()
    );
    let (status, polled) = json_of(
        door.dispatch(
            &hyper::Method::POST,
            tilo_front_door::keys::POLL_PATH,
            "",
            &hyper::HeaderMap::new(),
            json!({"poll": asked["poll"]}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 200, "{polled}");
    polled["key"].as_str().unwrap().to_owned()
}

/// Serve the door on a relay with its app workers on loopback, for a client
/// outside this process. A person approves two keys reaching all their
/// workspaces there: one for their backend and one their reviewer reads
/// with. It prints one JSON line naming the door and both keys, then runs
/// `command` with them in its environment (`TILO_DOOR`, `TILO_API_KEY`,
/// `TILO_REVIEWER_KEY`) and answers its exit status, or with no command
/// serves until its standard input closes.
#[cfg(not(test))]
pub async fn serve(command: &[std::ffi::OsString]) -> std::process::ExitCode {
    use std::io::Write as _;
    use tokio::io::AsyncReadExt as _;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let on_relay = door_on_relay(&issuer).await;
    let (person, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&person, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &person, &page, "Backend").await;
    let reviewer = approved_key(&on_relay, &person, &page, "Reviewer").await;
    tokio::spawn(tilo_front_door::serve(Arc::clone(&on_relay.door), listener));
    let ready = json!({
        "door": issuer,
        "key": key,
        "reviewer_key": reviewer,
        "person": hex::encode(person.verifying_key().to_bytes()),
    });
    let mut out = std::io::stdout().lock();
    writeln!(out, "{ready}").unwrap();
    out.flush().unwrap();
    drop(out);
    let Some((program, arguments)) = command.split_first() else {
        let mut rest = Vec::new();
        let _ = tokio::io::stdin().read_to_end(&mut rest).await;
        return std::process::ExitCode::SUCCESS;
    };
    let status = tokio::process::Command::new(program)
        .args(arguments)
        .env("TILO_DOOR", &issuer)
        .env("TILO_API_KEY", &key)
        .env("TILO_REVIEWER_KEY", &reviewer)
        .status()
        .await;
    drop(on_relay);
    match status.map(|status| status.code()) {
        Ok(Some(0)) => std::process::ExitCode::SUCCESS,
        Ok(Some(code)) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Ok(None) | Err(_) => std::process::ExitCode::FAILURE,
    }
}
