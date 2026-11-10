//! The MCP front door against the relay, as tilo.sh runs them: the relay's
//! own person is its operator, the site's home workspace lets people in
//! through its door, and each person keeps one workspace of their own there.
//! A person's key works through the door's HTTP routes alone: it makes the
//! person's workspace, is answered there by the relay's app workers with no
//! tab of anyone's open, mints a key that reaches that one workspace and
//! nothing else, and revoking it ends both.
use super::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tilo_front_door::{Config, Door, store::Store};
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_user_identity::{DeviceCert, MAX_AGENT_GRANT_LIFETIME_NS, SignInProof};

const ISSUER: &str = "https://mcp.tilo.test";

/// A reply's status and JSON body.
async fn json_of(reply: hyper::Response<http_body_util::Full<bytes::Bytes>>) -> (u16, Value) {
    use http_body_util::BodyExt as _;
    let status = reply.status().as_u16();
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn headers(pairs: &[(&'static str, &str)]) -> hyper::HeaderMap {
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

/// A key called `name`, asked for as a terminal does and approved by the
/// person's Tilo on the web, which this plays: it signs the door's
/// challenge with a device of `root`'s and certifies the grant's key as the
/// server key the door asked for, lasting until revoked.
async fn approved_key(door: &Arc<Door>, root: &SigningKey, device: &Device, name: &str) -> String {
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
            approve.strip_prefix(ISSUER).unwrap(),
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
        device_label: format!("{name} via mcp.tilo.test"),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: now_ns.saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, root).unwrap();
    let proof = SignInProof::sign(device.cert.clone(), &device.key, ISSUER, challenge).unwrap();
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

/// An MCP session at the door's `path` with `token`.
struct Mcp<'a> {
    door: &'a Arc<Door>,
    path: String,
    token: String,
    session: Option<String>,
}

impl Mcp<'_> {
    /// Post `message`: the HTTP status and the reply.
    async fn post(&mut self, message: Value) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let mut pairs = vec![
            ("authorization", bearer.as_str()),
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ];
        if let Some(session) = &self.session {
            pairs.push((tilo_front_door::session::SESSION_HEADER, session.as_str()));
        }
        let reply = self
            .door
            .dispatch(
                &hyper::Method::POST,
                &self.path,
                "",
                &headers(&pairs),
                message.to_string().as_bytes(),
            )
            .await;
        if let Some(session) = reply
            .headers()
            .get(tilo_front_door::session::SESSION_HEADER)
        {
            self.session = Some(session.to_str().unwrap().to_owned());
        }
        json_of(reply).await
    }

    async fn initialize(&mut self) -> (u16, Value) {
        self.post(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                         "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "visa-backend", "version": "1"}}}))
            .await
    }

    /// `tilo_do` with `arguments`, its typed answer: what the SDKs read.
    async fn tilo_do(&mut self, arguments: Value) -> Value {
        let (status, reply) = self
            .post(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "tilo_do", "arguments": arguments,
                                    "_meta": {"so.tilo.mcp": {"structured": true}}}}))
            .await;
        assert_eq!(status, 200, "{reply}");
        reply
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one key's story through the door, from its approval to its revocation"
)]
async fn a_key_works_through_the_door_on_the_relay_and_ends_when_revoked() {
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
        .offer_own_workspaces(tilo_relay::workspaces::OwnWorkspaceOffer {
            max_workspaces: 1,
            ..tilo_relay::workspaces::OwnWorkspaceOffer::free("relay.tilo.test")
        })
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
            issuer: ISSUER.into(),
            web_entry: format!("http://127.0.0.1:{}", site.port()),
            relay: relay.clone(),
        },
        Store::in_memory(),
    )
    .unwrap();

    // A person who never met the operator approves a key for their backend.
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&door, &ada, &page, "Visa backend").await;

    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");

    // It makes the person's workspace on the relay.
    let created = account
        .tilo_do(json!({"path": "account/workspaces#create",
                        "args": {"name": "Applicant: Jane Doe"}}))
        .await;
    let result = &created["result"]["structuredContent"]["result"];
    let workspace = result["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a workspace is made: {created}"))
        .to_owned();
    // The person keeps one of their own there: a second is refused, typed.
    let second = account
        .tilo_do(json!({"path": "account/workspaces#create", "args": {"name": "Another"}}))
        .await;
    assert_eq!(
        second["result"]["structuredContent"]["status"], "failed",
        "{second}"
    );
    let listed = account
        .tilo_do(json!({"path": "account/workspaces#list"}))
        .await;
    let rows = listed["result"]["structuredContent"]["result"]["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["id"], workspace.as_str());

    // The relay's app workers answer it there, with no tab of anyone's
    // open.
    let (status, listed) = account
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {"workspace": workspace}}}))
        .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["result"]["isError"], false, "{listed}");

    // It mints a key for that workspace alone.
    let minted = account
        .tilo_do(json!({"path": "account/keys#mint", "args": {"workspace": workspace}}))
        .await;
    let minted = &minted["result"]["structuredContent"]["result"];
    // A key that lasts until revoked mints keys that still expire.
    let most = tilo_clock::unix_now_ms() + MAX_AGENT_GRANT_LIFETIME_NS / 1_000_000;
    assert!(
        minted["expires_at_ms"]
            .as_u64()
            .is_some_and(|expiry| expiry <= most),
        "{minted}"
    );
    let minted_key = minted["token"]
        .as_str()
        .unwrap_or_else(|| panic!("a key is minted: {minted}"))
        .to_owned();
    // It joins that workspace and is answered there.
    let mut minted_session = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted_key.clone(),
        session: None,
    };
    let (status, init) = minted_session.initialize().await;
    assert_eq!(status, 200, "a minted key reaches its workspace: {init}");
    // Its door is that workspace's, so it names none.
    let (status, listed) = minted_session
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {}}}))
        .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    // It reaches no other workspace: not the site's home workspace.
    let mut lobby_session = Mcp {
        door: &door,
        path: format!("/w/{lobby}/mcp"),
        token: minted_key.clone(),
        session: None,
    };
    let (status, _) = lobby_session.initialize().await;
    assert_eq!(status, 403, "a minted key reaches only its workspace");

    // Revoking the key that minted it ends both, at once.
    let keys = account.tilo_do(json!({"path": "account/keys#list"})).await;
    let own = keys["result"]["structuredContent"]["result"]["keys"][0]["grant"]
        .as_str()
        .unwrap_or_else(|| panic!("the key lists itself: {keys}"))
        .to_owned();
    let revoked = account
        .tilo_do(json!({"path": "account/keys#revoke", "args": {"grant": own}}))
        .await;
    assert_eq!(
        revoked["result"]["structuredContent"]["status"], "ok",
        "{revoked}"
    );
    let (status, _) = account
        .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;
    assert_eq!(status, 401, "the key's session ended");
    let (status, _) = minted_session
        .post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}))
        .await;
    assert_eq!(status, 401, "the minted key's session ended with it");
    let mut minted_again = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted_key,
        session: None,
    };
    let (status, _) = minted_again.initialize().await;
    assert_eq!(status, 401, "the key it minted ended with it");
    assert_eq!(door.session_count(), 0);
    drop(site);
}
