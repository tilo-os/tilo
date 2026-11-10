//! The MCP front door against the relay, as tilo.sh runs them: the relay's
//! own person is its operator, the site's home workspace lets people in
//! through its door, and each person keeps one workspace of their own there.
//! A person's key works through the door's HTTP routes alone: it makes the
//! person's workspace, is answered there by the relay's app workers with no
//! tab of anyone's open, mints a key that reaches that one workspace and
//! nothing else, and revoking it ends both.
use super::hosted_documents::{application_pdf, content_in, json_in};
use super::*;
use base64::Engine as _;
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

    /// What `tilo_do` of `path` with `args` answered, which it ran.
    async fn did(&mut self, path: &str, args: Value) -> Value {
        let reply = self.tilo_do(json!({"path": path, "args": args})).await;
        let answer = &reply["result"]["structuredContent"];
        assert_eq!(answer["status"], "ok", "{path} ran: {reply}");
        answer["result"].clone()
    }

    /// The bytes of `content` the session gets back with `tilo_get`.
    async fn get(&mut self, content: &str) -> Vec<u8> {
        let (status, reply) = self
            .post(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                         "params": {"name": "tilo_get", "arguments": {"content": content}}}))
            .await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let piece = json_in(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default(),
        );
        base64::engine::general_purpose::STANDARD
            .decode(piece["bytes"].as_str().expect("the piece's bytes"))
            .expect("base64")
    }
}

/// `PUT /w/<workspace>/content` of `body` in `mcp`'s session: the status
/// and the reply.
async fn upload(mcp: &Mcp<'_>, workspace: &str, body: &[u8]) -> (u16, Value) {
    let bearer = format!("Bearer {}", mcp.token);
    let mut pairs = vec![
        ("authorization", bearer.as_str()),
        ("content-type", "application/pdf"),
    ];
    if let Some(session) = &mcp.session {
        pairs.push((tilo_front_door::session::SESSION_HEADER, session.as_str()));
    }
    json_of(
        mcp.door
            .dispatch(
                &hyper::Method::PUT,
                &format!("/w/{workspace}/content"),
                "",
                &headers(&pairs),
                body,
            )
            .await,
    )
    .await
}

/// The relay as its operator runs it, the site whose home workspace lets
/// anyone in through its door, and the front door before both.
struct OnRelay {
    door: Arc<Door>,
    /// The site's home workspace.
    lobby: String,
    _site: test_http::Responder,
    _hosted: Hosted,
    _state: tempfile::TempDir,
}

async fn door_on_relay() -> OnRelay {
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
            issuer: ISSUER.into(),
            web_entry: format!("http://127.0.0.1:{}", site.port()),
            relay: relay.clone(),
        },
        Store::in_memory(),
    )
    .unwrap();

    OnRelay {
        door,
        lobby,
        _site: site,
        _hosted: hosted,
        _state: state,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one key's story through the door, from its approval to its revocation"
)]
async fn a_key_works_through_the_door_on_the_relay_and_ends_when_revoked() {
    let on_relay = door_on_relay().await;
    let door = Arc::clone(&on_relay.door);
    let lobby = on_relay.lobby.clone();
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
    // A person has as many workspaces of their own there as they ask for: a
    // second, named, is another one.
    let second = account
        .tilo_do(json!({"path": "account/workspaces#create", "args": {"name": "Another"}}))
        .await;
    assert_eq!(
        second["result"]["structuredContent"]["status"], "ok",
        "{second}"
    );
    let another = second["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("another workspace is made: {second}"))
        .to_owned();
    assert_ne!(another, workspace);
    let listed = account
        .tilo_do(json!({"path": "account/workspaces#list"}))
        .await;
    let rows = listed["result"]["structuredContent"]["result"]["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 2, "{listed}");
    let ids: Vec<&str> = rows.iter().filter_map(|row| row["id"].as_str()).collect();
    assert!(
        ids.contains(&workspace.as_str()) && ids.contains(&another.as_str()),
        "{listed}"
    );

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
    // The person's own documents, through the door alone with the key for
    // that workspace, run by the relay's app workers though the relay is not
    // theirs: a form goes in over HTTP, PDF Viewer fills it, Writer writes a
    // letter and exports it, and the letter comes back out.
    let (status, put) = upload(&minted_session, &workspace, &application_pdf()).await;
    assert_eq!(status, 201, "the form goes into the workspace: {put}");
    let form = put["content"].as_str().unwrap().to_owned();
    let work = &mut minted_session;
    work.did("pdf-viewer#open", json!({})).await;
    let imported = work
        .did(
            "pdf-viewer#import-pdf-content",
            json!({"content": form, "name": "application.pdf"}),
        )
        .await;
    assert!(
        imported.to_string().contains("application.pdf"),
        "{imported}"
    );
    let fields = work.did("pdf-viewer#read-form-fields", json!({})).await;
    let surname = fields["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|field| field["name"] == "surname")
        .unwrap_or_else(|| panic!("the form has a surname: {fields}"))
        .clone();
    let edit = json!({"path": surname["path"], "op": "set",
                      "value": {"kind": "text", "value": "Okafor"}});
    work.did(
        "pdf-viewer/application.pdf#field-edit",
        json!({"edit": edit}),
    )
    .await;
    let filled = work.did("pdf-viewer#read-form-fields", json!({})).await;
    assert!(
        filled.to_string().contains("Okafor"),
        "the form is filled: {filled}"
    );
    work.did("word-processor#open", json!({})).await;
    work.did(
        "word-processor#append-paragraph",
        json!({"text": "Dear Visa Officer,"}),
    )
    .await;
    let exported = work
        .tilo_do(json!({"path": "word-processor#export-content", "args": {"format": "docx"}}))
        .await;
    let letter = work.get(&content_in(&exported.to_string())).await;
    assert!(letter.starts_with(b"PK"), "the letter is a Word file");
    // History names the key's agent, acting for the person, for the edit.
    let history = work
        .did("word-processor#history", json!({"last": 50}))
        .await;
    let ada_hex = hex::encode(ada.verifying_key().to_bytes());
    let edits: Vec<&Value> = history["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|version| version.get("actor").is_some())
        .collect();
    assert!(
        !edits.is_empty()
            && edits.iter().all(|version| {
                version["actor"]["kind"] == "agent" && version["actor"]["for"] == ada_hex.as_str()
            }),
        "{history}"
    );
    // The key puts nothing into another of the person's workspaces.
    let (status, _) = upload(work, &another, &application_pdf()).await;
    assert_eq!(
        status, 403,
        "a workspace's key puts only into its workspace"
    );

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
}

/// Workspaces a backend makes for its customers with one key, here.
const CUSTOMERS: usize = 200;

/// Of those, the ones it works in at once.
const AT_ONCE: usize = 20;

/// The `quantile` of `durations`.
fn quantile(durations: &[Duration], quantile: f64) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short list"
    )]
    let index = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted[index]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_key_makes_and_works_in_hundreds_of_workspaces_at_once() {
    let on_relay = door_on_relay().await;
    let door = Arc::clone(&on_relay.door);
    // A startup's backend, whose person is not the relay's operator, makes a
    // workspace per customer with its one key.
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&door, &ada, &page, "Customer backend").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");

    let mut made = Vec::with_capacity(CUSTOMERS);
    let mut creating = Vec::with_capacity(CUSTOMERS);
    for customer in 0..CUSTOMERS {
        let began = Instant::now();
        let created = account
            .did(
                "account/workspaces#create",
                json!({"name": format!("Customer {customer}")}),
            )
            .await;
        creating.push(began.elapsed());
        made.push(
            created["workspace"]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("a workspace is made: {created}"))
                .to_owned(),
        );
    }
    let began = Instant::now();
    let listed = account.did("account/workspaces#list", json!({})).await;
    let listing = began.elapsed();
    let ids: std::collections::HashSet<&str> = listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert_eq!(ids.len(), CUSTOMERS, "every workspace is listed");
    assert!(made.iter().all(|id| ids.contains(id.as_str())));
    let (create_p50, create_p95) = (quantile(&creating, 0.5), quantile(&creating, 0.95));
    eprintln!(
        "{CUSTOMERS} workspaces: create p50 {create_p50:?}, p95 {create_p95:?}, max {:?}; list {listing:?}",
        quantile(&creating, 1.0)
    );
    assert!(
        create_p95 < Duration::from_millis(500),
        "making a workspace stays fast: p95 {create_p95:?}"
    );
    assert!(
        listing < Duration::from_millis(500),
        "listing them stays fast: {listing:?}"
    );

    // It works in some of them at once, each a session of the same key, and
    // puts a file into one without naming a session while they are open:
    // none cuts another off.
    let began = Instant::now();
    let mut working = tokio::task::JoinSet::new();
    for workspace in made.iter().take(AT_ONCE).cloned() {
        let door = Arc::clone(&door);
        let token = key.clone();
        working.spawn(async move {
            let mut session = Mcp {
                door: &door,
                path: format!("/w/{workspace}/mcp"),
                token,
                session: None,
            };
            let (status, init) = session.initialize().await;
            assert_eq!(status, 200, "{workspace}: {init}");
            // A workspace entered for the first time starts its app workers
            // on the relay; with many starting at once its first catalogue
            // may outlast the runtime's wait, which a session asks again.
            // Being cut off would not pass on a second ask.
            let mut slow = 0;
            loop {
                let (status, listed) = session
                    .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                                 "params": {"name": "tilo_ls", "arguments": {}}}))
                    .await;
                assert_eq!(status, 200, "{workspace}: {listed}");
                if listed["result"]["isError"] == false {
                    break;
                }
                assert!(
                    slow < 3 && listed.to_string().contains("did not answer within"),
                    "{workspace}: {listed}"
                );
                slow += 1;
            }
            (workspace, session.session, slow)
        });
    }
    let uploader = Mcp {
        door: &door,
        path: String::new(),
        token: key.clone(),
        session: None,
    };
    let (status, put) = upload(&uploader, &made[0], &application_pdf()).await;
    assert_eq!(
        status, 201,
        "a file goes in beside the open sessions: {put}"
    );
    let mut open = Vec::with_capacity(AT_ONCE);
    let mut asked_again = 0;
    while let Some(worked) = working.join_next().await {
        let (workspace, id, slow) = worked.expect("a session worked");
        asked_again += slow;
        open.push((workspace, id));
    }
    eprintln!(
        "{AT_ONCE} sessions opened and answered at once in {:?} ({asked_again} first catalogues asked again)",
        began.elapsed()
    );

    // Every one of them, and the account's own, still answers.
    for (workspace, id) in open {
        let mut session = Mcp {
            door: &door,
            path: format!("/w/{workspace}/mcp"),
            token: key.clone(),
            session: id,
        };
        let (status, listed) = session
            .post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                         "params": {"name": "tilo_ls", "arguments": {}}}))
            .await;
        assert_eq!(status, 200, "{workspace} still answers: {listed}");
        assert_eq!(listed["result"]["isError"], false, "{workspace}: {listed}");
    }
    let listed = account.did("account/workspaces#list", json!({})).await;
    assert_eq!(
        listed["workspaces"].as_array().map(Vec::len),
        Some(CUSTOMERS),
        "the account's own session still answers"
    );
}
