//! The door end to end, in process: an agent discovers how to authorize,
//! registers, sends the person through sign-in and consent, redeems the code
//! with its PKCE verifier, and acts in workspaces served by real
//! `tilo-runtime` HTTP MCP endpoints on loopback.

use super::*;
use directory::{BoxFuture, DirectoryError, Host, Route, Workspace};
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_actions::remote::{Caller, TestHost};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, SignInProof};

const ISSUER: &str = "https://door.test";
const WEB_ENTRY: &str = "https://tilo.test";
const REDIRECT: &str = "http://127.0.0.1:33418/callback";
const VERIFIER: &str = "a-verifier-of-at-least-forty-three-characters-0123456789";

/// One workspace's `tilo-runtime` endpoint, served on loopback.
struct Runtime {
    url: String,
    host: Arc<TestHost>,
}

async fn runtime(workspace: &str, token: &str) -> Runtime {
    let host = Arc::new(TestHost::new());
    let endpoint = tilo_runtime::mcp_http::Endpoint::new(
        Arc::clone(&host),
        workspace.to_owned(),
        "owner".to_owned(),
        Some(token.to_owned()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let endpoint = Arc::clone(&endpoint);
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let endpoint = Arc::clone(&endpoint);
                    async move { Ok::<_, std::convert::Infallible>(endpoint.respond(request).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Runtime { url, host }
}

/// One workspace as the tests set it up: (person, workspace, route, its
/// runtime's host).
type HostedWorkspace = (String, Workspace, Route, Arc<TestHost>);

/// A host's workspaces as the tests set them up: each with its runtime.
#[derive(Default)]
struct Hosted {
    workspaces: Mutex<Vec<HostedWorkspace>>,
    revoked: Mutex<RevocationList>,
    /// Workspaces the host may still make before it refuses (a quota).
    quota: AtomicUsize,
}

impl Hosted {
    async fn add(&self, person: &str, name: &str) -> Workspace {
        let id = uuid::Uuid::new_v4().to_string();
        let token = format!("proxy-{id}");
        let served = runtime(&id, &token).await;
        let workspace = Workspace {
            id,
            name: name.to_owned(),
            host: "eu1.tilo.test".to_owned(),
        };
        lock(&self.workspaces).push((
            person.to_owned(),
            workspace.clone(),
            Route {
                url: served.url,
                token,
            },
            served.host,
        ));
        workspace
    }

    fn host_of(&self, workspace: &str) -> Arc<TestHost> {
        lock(&self.workspaces)
            .iter()
            .find(|(_, candidate, _, _)| candidate.id == workspace)
            .map(|(_, _, _, host)| Arc::clone(host))
            .unwrap()
    }
}

impl Directory for Hosted {
    fn workspaces<'a>(
        &'a self,
        person: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Workspace>, DirectoryError>> {
        Box::pin(async move {
            Ok(lock(&self.workspaces)
                .iter()
                .filter(|(owner, ..)| owner == person)
                .map(|(_, workspace, ..)| workspace.clone())
                .collect())
        })
    }

    fn hosts<'a>(&'a self, _person: &'a str) -> BoxFuture<'a, Result<Vec<Host>, DirectoryError>> {
        Box::pin(async {
            Ok(vec![Host {
                name: "eu1.tilo.test".into(),
                default: true,
            }])
        })
    }

    fn create<'a>(
        &'a self,
        person: &'a str,
        name: &'a str,
        _host: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Workspace, DirectoryError>> {
        Box::pin(async move {
            if self
                .quota
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_err()
            {
                return Err(DirectoryError::Refused(
                    "the workspace quota is used up".into(),
                ));
            }
            Ok(self.add(person, name).await)
        })
    }

    fn revocations<'a>(
        &'a self,
        _person: &'a [u8; 32],
    ) -> BoxFuture<'a, Result<RevocationList, DirectoryError>> {
        Box::pin(async move { Ok(lock(&self.revoked).clone()) })
    }

    fn route<'a>(
        &'a self,
        person: &'a str,
        workspace: &'a str,
    ) -> BoxFuture<'a, Result<Route, DirectoryError>> {
        Box::pin(async move {
            lock(&self.workspaces)
                .iter()
                .find(|(owner, candidate, ..)| owner == person && candidate.id == workspace)
                .map(|(_, _, route, _)| route.clone())
                .ok_or(DirectoryError::NotFound)
        })
    }
}

/// A person: their root, and a device it certified.
struct Person {
    root: ed25519_dalek::SigningKey,
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

impl Person {
    fn new(seed: u8) -> Self {
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(100); 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        Self { root, device, cert }
    }

    fn id(&self) -> String {
        hex::encode(self.root.verifying_key().to_bytes())
    }
}

fn door(hosted: &Arc<Hosted>) -> Arc<Door> {
    Door::new(
        &Config {
            issuer: ISSUER.into(),
            web_entry: WEB_ENTRY.into(),
        },
        Store::in_memory(),
        Arc::clone(hosted) as Arc<dyn Directory>,
    )
    .unwrap()
}

async fn body(reply: Reply) -> Vec<u8> {
    reply
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn json_body(reply: Reply) -> Value {
    serde_json::from_slice(&body(reply).await).unwrap()
}

fn headers(pairs: &[(&'static str, &str)]) -> hyper::HeaderMap {
    let mut map = hyper::HeaderMap::new();
    for (name, value) in pairs {
        map.append(*name, HeaderValue::from_str(value).unwrap());
    }
    map
}

fn location(reply: &Reply) -> String {
    reply.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned()
}

fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

/// The value of the hidden input `name` on `page`.
fn hidden(page: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let (_, rest) = page.split_once(&marker).unwrap();
    rest.split_once('"').unwrap().0.to_owned()
}

/// The link on a sign-in `page` that sends the person to their Tilo.
fn sign_in_link(page: &str) -> String {
    let start = page.find(WEB_ENTRY).unwrap();
    let (link, _) = page.get(start..).unwrap().split_once('"').unwrap();
    link.replace("&amp;", "&")
}

/// What the person chooses on the consent page.
enum Choice<'a> {
    Account,
    One(&'a str),
}

/// Register a client.
async fn register(door: &Arc<Door>) -> String {
    let reply = door
        .dispatch(
            &Method::POST,
            oauth::REGISTER_PATH,
            "",
            &hyper::HeaderMap::new(),
            json!({"client_name":"Claude Code","redirect_uris":[REDIRECT],
                   "grant_types":["authorization_code","refresh_token"],
                   "token_endpoint_auth_method":"none"})
            .to_string()
            .as_bytes(),
        )
        .await;
    assert_eq!(reply.status(), StatusCode::CREATED);
    json_body(reply).await["client_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Run the browser part: authorize, sign in, choose. Answers the code.
async fn consent(
    door: &Arc<Door>,
    client: &str,
    person: &Person,
    resource: &str,
    choice: Choice<'_>,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", client)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("code_challenge", &store::challenge_of(VERIFIER))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", "xyz")
        .append_pair("resource", resource)
        .finish();
    let reply = door
        .dispatch(
            &Method::GET,
            oauth::AUTHORIZE_PATH,
            &query,
            &hyper::HeaderMap::new(),
            b"",
        )
        .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let page = String::from_utf8(body(reply).await).unwrap();
    // The page sends the person to their Tilo, which signs the challenge.
    let asked = query_of(&sign_in_link(&page));
    assert_eq!(asked["audience"], ISSUER);
    let challenge: [u8; 32] = hex::decode(&asked["challenge"])
        .unwrap()
        .try_into()
        .unwrap();
    let proof = SignInProof::sign(person.cert.clone(), &person.device, ISSUER, challenge).unwrap();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("pending", &asked["pending"])
        .append_pair("proof", &sign_in::encode_proof(&proof).unwrap())
        .finish();
    let reply = door
        .dispatch(
            &Method::POST,
            "/sign-in",
            "",
            &hyper::HeaderMap::new(),
            form.as_bytes(),
        )
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let consent_path = location(&reply);
    let cookie = reply.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let browser = headers(&[("cookie", &cookie)]);
    let reply = door
        .dispatch(&Method::GET, &consent_path, "", &browser, b"")
        .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let page = String::from_utf8(body(reply).await).unwrap();
    // Finished before the request is sent: the serializer is not `Send`.
    let decision = {
        let mut decision = url::form_urlencoded::Serializer::new(String::new());
        decision
            .append_pair("csrf", &hidden(&page, "csrf"))
            .append_pair("decision", "allow");
        match choice {
            Choice::Account => decision.append_pair("scope", "account"),
            Choice::One(workspace) => decision
                .append_pair("scope", "workspace")
                .append_pair("workspace", workspace),
        };
        decision.finish()
    };
    let reply = door
        .dispatch(
            &Method::POST,
            &consent_path,
            "",
            &browser,
            decision.as_bytes(),
        )
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let back = location(&reply);
    assert!(back.starts_with(REDIRECT), "{back}");
    let answered = query_of(&back);
    assert_eq!(answered["state"], "xyz");
    answered
        .get("code")
        .cloned()
        .unwrap_or_else(|| panic!("no code: {back}"))
}

async fn redeem(door: &Arc<Door>, client: &str, code: &str, verifier: &str) -> Reply {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("client_id", client)
        .append_pair("code_verifier", verifier)
        .finish();
    door.dispatch(
        &Method::POST,
        oauth::TOKEN_PATH,
        "",
        &hyper::HeaderMap::new(),
        form.as_bytes(),
    )
    .await
}

/// The whole flow: an access token for `choice` at `resource`.
async fn connect(door: &Arc<Door>, person: &Person, resource: &str, choice: Choice<'_>) -> String {
    let client = register(door).await;
    let code = consent(door, &client, person, resource, choice).await;
    let reply = redeem(door, &client, &code, VERIFIER).await;
    assert_eq!(reply.status(), StatusCode::OK);
    json_body(reply).await["access_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// An MCP request with `token`, on `path`, in `session`.
async fn mcp(
    door: &Arc<Door>,
    path: &str,
    token: &str,
    session: Option<&str>,
    message: Value,
) -> Reply {
    let bearer = format!("Bearer {token}");
    let mut pairs = vec![
        ("authorization", bearer.as_str()),
        ("accept", "application/json, text/event-stream"),
    ];
    if let Some(session) = session {
        pairs.push((SESSION_HEADER, session));
    }
    door.dispatch(
        &Method::POST,
        path,
        "",
        &headers(&pairs),
        message.to_string().as_bytes(),
    )
    .await
}

async fn initialize(door: &Arc<Door>, path: &str, token: &str) -> (String, Value) {
    let reply = mcp(
        door,
        path,
        token,
        None,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"claude-code","version":"2"}}}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let session = reply.headers()[SESSION_HEADER].to_str().unwrap().to_owned();
    (session, json_body(reply).await)
}

async fn call(
    door: &Arc<Door>,
    path: &str,
    token: &str,
    session: &str,
    name: &str,
    arguments: Value,
) -> Value {
    let reply = mcp(
        door,
        path,
        token,
        Some(session),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let content_type = reply.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = body(reply).await;
    if content_type == "text/event-stream" {
        // The reply is the stream's last message.
        let text = String::from_utf8(bytes).unwrap();
        let last = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .next_back()
            .unwrap();
        return serde_json::from_str(last).unwrap();
    }
    serde_json::from_slice(&bytes).unwrap()
}

async fn tool_names(door: &Arc<Door>, path: &str, token: &str, session: &str) -> Vec<String> {
    let reply = mcp(
        door,
        path,
        token,
        Some(session),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
    )
    .await;
    json_body(reply).await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Whether the workspace's runtime was asked anything for its own workspace.
fn acted_in(host: &TestHost, workspace: &str) -> bool {
    let callers: Vec<Caller> = host.callers();
    !callers.is_empty()
        && callers
            .iter()
            .all(|caller| caller.workspace() == Some(workspace))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_discovers_the_authorization_server_from_its_first_refusal() {
    let door = door(&Arc::default());
    for (path, metadata) in [
        ("/", format!("{ISSUER}{}", oauth::PROTECTED_RESOURCE_PATH)),
        (
            "/mcp",
            format!("{ISSUER}{}", oauth::PROTECTED_RESOURCE_PATH),
        ),
        (
            "/w/0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa",
            format!(
                "{ISSUER}{}/w/0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa",
                oauth::PROTECTED_RESOURCE_PATH
            ),
        ),
    ] {
        let reply = door
            .dispatch(&Method::POST, path, "", &hyper::HeaderMap::new(), b"{}")
            .await;
        assert_eq!(reply.status(), StatusCode::UNAUTHORIZED, "{path}");
        let challenge = reply.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(
            challenge.contains(&format!("resource_metadata=\"{metadata}\"")),
            "{challenge}"
        );
        // The metadata names the resource and the door as its server.
        let metadata_path = metadata.strip_prefix(ISSUER).unwrap();
        let resource = json_body(
            door.dispatch(
                &Method::GET,
                metadata_path,
                "",
                &hyper::HeaderMap::new(),
                b"",
            )
            .await,
        )
        .await;
        assert_eq!(resource["authorization_servers"], json!([ISSUER]));
        let expected = if path.starts_with("/w/") {
            format!("{ISSUER}{path}")
        } else {
            ISSUER.to_owned()
        };
        assert_eq!(resource["resource"], expected);
    }
    for path in [
        oauth::AUTHORIZATION_SERVER_PATH,
        oauth::OPENID_CONFIGURATION_PATH,
    ] {
        let server = json_body(
            door.dispatch(&Method::GET, path, "", &hyper::HeaderMap::new(), b"")
                .await,
        )
        .await;
        assert_eq!(server["issuer"], ISSUER);
        assert_eq!(
            server["registration_endpoint"],
            format!("{ISSUER}/register")
        );
        assert_eq!(
            server["authorization_endpoint"],
            format!("{ISSUER}/authorize")
        );
        assert_eq!(server["token_endpoint"], format!("{ISSUER}/token"));
        assert_eq!(server["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(
            server["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn registration_admits_only_public_clients_with_safe_redirects() {
    let door = door(&Arc::default());
    for (body, ok) in [
        (json!({"redirect_uris":["http://localhost:4000/cb"]}), true),
        (
            json!({"redirect_uris":["http://127.0.0.1/cb"],"client_name":"Codex"}),
            true,
        ),
        (json!({"redirect_uris":["https://agent.example/cb"]}), true),
        (json!({"redirect_uris":["com.example.agent:/cb"]}), true),
        (json!({"redirect_uris":["http://evil.example/cb"]}), false),
        (json!({"redirect_uris":["javascript:alert(1)"]}), false),
        (
            json!({"redirect_uris":["https://agent.example/cb#frag"]}),
            false,
        ),
        (json!({"redirect_uris":[]}), false),
        (json!({}), false),
        (
            json!({"redirect_uris":["http://localhost/cb"],"token_endpoint_auth_method":"client_secret_basic"}),
            false,
        ),
        (
            json!({"redirect_uris":["http://localhost/cb"],"grant_types":["client_credentials"]}),
            false,
        ),
    ] {
        let reply = door
            .dispatch(
                &Method::POST,
                oauth::REGISTER_PATH,
                "",
                &hyper::HeaderMap::new(),
                body.to_string().as_bytes(),
            )
            .await;
        assert_eq!(reply.status() == StatusCode::CREATED, ok, "{body}");
        let answer = json_body(reply).await;
        if ok {
            assert!(
                answer["client_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty())
            );
            assert_eq!(answer["token_endpoint_auth_method"], "none");
        } else {
            assert!(answer["error"].is_string(), "{answer}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_code_needs_its_verifier_and_is_spent_once() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    hosted.add(&alice.id(), "Studio").await;
    let door = door(&hosted);
    let client = register(&door).await;
    let code = consent(&door, &client, &alice, ISSUER, Choice::Account).await;
    let wrong = "another-verifier-of-at-least-forty-three-characters-xyz";
    assert_eq!(
        json_body(redeem(&door, &client, &code, wrong).await).await["error"],
        "invalid_grant"
    );
    // A wrong verifier spent the code: the agent must ask again.
    assert_eq!(
        json_body(redeem(&door, &client, &code, VERIFIER).await).await["error"],
        "invalid_grant"
    );
    let code = consent(&door, &client, &alice, ISSUER, Choice::Account).await;
    let other = register(&door).await;
    assert_eq!(
        json_body(redeem(&door, &other, &code, VERIFIER).await).await["error"],
        "invalid_grant"
    );
    let code = consent(&door, &client, &alice, ISSUER, Choice::Account).await;
    let reply = redeem(&door, &client, &code, VERIFIER).await;
    assert_eq!(reply.headers()[header::CACHE_CONTROL], "no-store");
    let tokens = json_body(reply).await;
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(
        json_body(redeem(&door, &client, &code, VERIFIER).await).await["error"],
        "invalid_grant"
    );
    // The refresh token rotates: used once, it is spent.
    let refresh = |token: &str| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("refresh_token", token)
            .append_pair("client_id", &client)
            .finish()
    };
    let first = tokens["refresh_token"].as_str().unwrap();
    let renewed = json_body(
        door.dispatch(
            &Method::POST,
            oauth::TOKEN_PATH,
            "",
            &hyper::HeaderMap::new(),
            refresh(first).as_bytes(),
        )
        .await,
    )
    .await;
    assert!(renewed["access_token"].is_string(), "{renewed}");
    let again = json_body(
        door.dispatch(
            &Method::POST,
            oauth::TOKEN_PATH,
            "",
            &hyper::HeaderMap::new(),
            refresh(first).as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(again["error"], "invalid_grant");
    // Only the consent whose code was redeemed is a grant; the others left none.
    assert_eq!(door.store.grants_of(&alice.id()).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_signs_nobody_in() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: alice.root.verifying_key().to_bytes(),
        revoked_device_public_key: alice.device.verifying_key().to_bytes(),
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Stolen,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &alice.root).unwrap();
    lock(&hosted.revoked).insert(entry).unwrap();
    let door = door(&hosted);
    let client = register(&door).await;
    let query = format!(
        "response_type=code&client_id={client}&redirect_uri={}&code_challenge={}&code_challenge_method=S256",
        url::form_urlencoded::byte_serialize(REDIRECT.as_bytes()).collect::<String>(),
        store::challenge_of(VERIFIER)
    );
    let page = String::from_utf8(
        body(
            door.dispatch(
                &Method::GET,
                oauth::AUTHORIZE_PATH,
                &query,
                &hyper::HeaderMap::new(),
                b"",
            )
            .await,
        )
        .await,
    )
    .unwrap();
    let asked = query_of(&sign_in_link(&page));
    let challenge: [u8; 32] = hex::decode(&asked["challenge"])
        .unwrap()
        .try_into()
        .unwrap();
    let proof = SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, challenge).unwrap();
    let form = format!(
        "pending={}&proof={}",
        asked["pending"],
        sign_in::encode_proof(&proof).unwrap()
    );
    let reply = door
        .dispatch(
            &Method::POST,
            "/sign-in",
            "",
            &hyper::HeaderMap::new(),
            form.as_bytes(),
        )
        .await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert!(reply.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_session_never_reaches_another_workspace() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let garden = hosted.add(&alice.id(), "Garden").await;
    let door = door(&hosted);
    let pinned_path = format!("/w/{}", studio.id);
    let token = connect(
        &door,
        &alice,
        &format!("{ISSUER}{pinned_path}"),
        Choice::One(&studio.id),
    )
    .await;

    // At another workspace's address the grant does not hold.
    let elsewhere = format!("/w/{}", garden.id);
    let reply = mcp(&door, &elsewhere, &token, None, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert!(
        reply.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("insufficient_scope")
    );

    // At its own, it is the workspace's own endpoint, with no account tools.
    let (session, answer) = initialize(&door, &pinned_path, &token).await;
    assert!(answer["result"]["serverInfo"].is_object(), "{answer}");
    let tools = tool_names(&door, &pinned_path, &token, &session).await;
    assert!(
        tools.iter().any(|tool| tool == "tilo_list_apps"),
        "{tools:?}"
    );
    assert!(
        !tools
            .iter()
            .any(|tool| tool == session::LIST_WORKSPACES || tool == session::CREATE_WORKSPACE)
    );
    let refused = call(
        &door,
        &pinned_path,
        &token,
        &session,
        session::LIST_WORKSPACES,
        json!({}),
    )
    .await;
    assert!(
        refused["error"].is_object() || refused["result"]["isError"] == true,
        "{refused}"
    );
    let listed = call(
        &door,
        &pinned_path,
        &token,
        &session,
        "tilo_list_apps",
        json!({}),
    )
    .await;
    assert!(listed.get("result").is_some(), "{listed}");
    assert!(acted_in(&hosted.host_of(&studio.id), &studio.id));
    // Naming the other workspace reaches nothing there: the call goes to
    // its own workspace's endpoint, which knows no other.
    call(
        &door,
        &pinned_path,
        &token,
        &session,
        "tilo_list_apps",
        json!({"workspace": garden.id}),
    )
    .await;
    assert!(acted_in(&hosted.host_of(&studio.id), &studio.id));
    assert!(hosted.host_of(&garden.id).callers().is_empty());
    // The same grant at the door's root is still pinned.
    let (root_session, _) = initialize(&door, "/", &token).await;
    let tools = tool_names(&door, "/", &token, &root_session).await;
    assert!(!tools.iter().any(|tool| tool == session::LIST_WORKSPACES));
    assert!(hosted.host_of(&garden.id).callers().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_session_creates_a_workspace_and_acts_in_it() {
    let hosted = Arc::new(Hosted::default());
    hosted.quota.store(1, Ordering::SeqCst);
    let alice = Person::new(1);
    let bob = Person::new(2);
    let bobs = hosted.add(&bob.id(), "Bob's").await;
    let door = door(&hosted);
    let token = connect(&door, &alice, ISSUER, Choice::Account).await;
    let (session, answer) = initialize(&door, "/", &token).await;
    assert_eq!(
        answer["result"]["capabilities"]["tools"]["listChanged"],
        true
    );

    // No workspace yet: only the account's own tools.
    let tools = tool_names(&door, "/", &token, &session).await;
    assert_eq!(tools, [session::LIST_WORKSPACES, session::CREATE_WORKSPACE]);
    let listed = call(
        &door,
        "/",
        &token,
        &session,
        session::LIST_WORKSPACES,
        json!({}),
    )
    .await;
    assert_eq!(
        listed["result"]["structuredContent"]["workspaces"],
        json!([])
    );

    let created = call(
        &door,
        "/",
        &token,
        &session,
        session::CREATE_WORKSPACE,
        json!({"name":"Studio"}),
    )
    .await;
    assert_eq!(created["result"]["isError"], false, "{created}");
    let studio = created["result"]["structuredContent"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The host refuses past its quota, and says why.
    let refused = call(
        &door,
        "/",
        &token,
        &session,
        session::CREATE_WORKSPACE,
        json!({"name":"More"}),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true);
    assert!(
        refused["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("quota")
    );

    // Its tools changed: the workspace's own, each naming the workspace.
    let reply = mcp(
        &door,
        "/",
        &token,
        Some(&session),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
    )
    .await;
    let tools = json_body(reply).await["result"]["tools"].clone();
    let ls = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "tilo_list_apps")
        .unwrap();
    assert_eq!(
        ls["inputSchema"]["required"][0],
        session::WORKSPACE_ARGUMENT
    );

    let missing = call(&door, "/", &token, &session, "tilo_list_apps", json!({})).await;
    assert_eq!(missing["error"]["code"], -32602);
    let acted = call(
        &door,
        "/",
        &token,
        &session,
        "tilo_list_apps",
        json!({"workspace": studio}),
    )
    .await;
    assert!(acted.get("result").is_some(), "{acted}");
    assert!(acted_in(&hosted.host_of(&studio), &studio));

    // Someone else's workspace is no workspace of hers.
    let foreign = call(
        &door,
        "/",
        &token,
        &session,
        "tilo_list_apps",
        json!({"workspace": bobs.id}),
    )
    .await;
    assert_eq!(foreign["error"]["message"], "no such workspace");
    assert!(hosted.host_of(&bobs.id).callers().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn each_session_is_its_own_numbered_run_and_revoking_ends_them() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let door = door(&hosted);
    let token = connect(&door, &alice, ISSUER, Choice::One(&studio.id)).await;
    let (first, _) = initialize(&door, "/", &token).await;
    let (second, _) = initialize(&door, "/", &token).await;
    assert_ne!(first, second);
    let labels: Vec<String> = lock(&door.sessions)
        .values()
        .map(|session| session.label.clone())
        .collect();
    let mut labels = labels;
    labels.sort();
    assert_eq!(labels, ["Claude Code #1", "Claude Code #2"]);
    // Each run is a session of its own at the workspace's endpoint.
    call(&door, "/", &token, &first, "tilo_list_apps", json!({})).await;
    call(&door, "/", &token, &second, "tilo_list_apps", json!({})).await;
    assert!(acted_in(&hosted.host_of(&studio.id), &studio.id));

    let grant = door.store.grants_of(&alice.id())[0].id.clone();
    assert!(
        !door.revoke_grant(&bob_id(), &grant).await.unwrap(),
        "only its person revokes a grant"
    );
    assert!(door.revoke_grant(&alice.id(), &grant).await.unwrap());
    assert_eq!(door.session_count(), 0);
    let reply = mcp(
        &door,
        "/",
        &token,
        Some(&first),
        json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
}

fn bob_id() -> String {
    Person::new(2).id()
}

#[test]
fn a_pinned_path_names_only_a_canonical_workspace() {
    assert_eq!(resource_of("/"), Some(None));
    assert_eq!(resource_of("/mcp"), Some(None));
    let id = "0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa";
    assert_eq!(resource_of(&format!("/w/{id}")), Some(Some(id.to_owned())));
    assert_eq!(
        resource_of(&format!("/w/{id}/mcp")),
        Some(Some(id.to_owned()))
    );
    for bad in [
        "/w/0B0C8F0E-6F4A-4F2F-9D1C-2F2B0A5E11AA",
        "/w/00000000-0000-0000-0000-000000000000",
        "/w/../grants",
        "/w/",
        "/other",
    ] {
        assert_eq!(resource_of(bad), None, "{bad}");
    }
}

#[test]
fn the_run_is_named_for_the_person() {
    let params = json!({"clientInfo":{"name":"claude-code"}});
    assert_eq!(
        session::run_label("Claude Code", &params, 1),
        "Claude Code #1"
    );
    assert_eq!(
        session::run_label("MCP agent", &params, 3),
        "claude-code #3"
    );
    assert_eq!(session::run_label("", &json!({}), 2), "Agent #2");
}
