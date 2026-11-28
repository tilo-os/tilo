//! The server's own flows in process, as a person and their agents drive them:
//! an agent discovers how to authorize, registers, sends the person to their
//! Tilo (which these tests play: it signs the challenge with a device of the
//! person and certifies the grant's key with their root), redeems the code with
//! its PKCE verifier, and then reads and uses the key it receives through `/v1`.
//! What a grant then does in workspaces is the server's, which
//! `tilo-agent-session`'s `api` test drives through the server.

use super::*;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, SignInProof};

use tilo_peer_state::fixture::{labelled_with, member_cert};
pub(crate) const ISSUER: &str = "https://api.test";
pub(crate) const WEB_ENTRY: &str = "https://tilo.test";
pub(crate) const REDIRECT: &str = "http://127.0.0.1:33418/callback";
pub(crate) const VERIFIER: &str = "a-verifier-of-at-least-forty-three-characters-0123456789";

/// How a test's stand-in for the person's Tilo certifies a grant's key.
type Certify<'a> = Box<dyn Fn(&Asked) -> DeviceCert + 'a>;

/// A person: their root, and a device it certified (the page of their Tilo
/// on the web, which answers the server).
pub(crate) struct Person {
    pub root: ed25519_dalek::SigningKey,
    pub device: ed25519_dalek::SigningKey,
    pub cert: DeviceCert,
}

/// What the server asked the person's Tilo, from its sign-in link.
pub(crate) struct Asked {
    pub pending: String,
    pub challenge: [u8; 32],
    pub key: [u8; 32],
    pub scope: AgentScope,
    /// `None` for a server key that lasts until revoked.
    pub expires_ms: Option<u64>,
    pub server_key: bool,
    /// The link lets the key operate web pages in Browser.
    pub browser_pages: bool,
    pub client: String,
}

impl Asked {
    pub(crate) fn of(link: &str) -> Self {
        assert!(link.starts_with(&format!("{WEB_ENTRY}/sign-in?")), "{link}");
        let asked = query_of(link);
        assert_eq!(asked["audience"], ISSUER);
        assert_eq!(asked["return"], format!("{ISSUER}/sign-in"));
        Self {
            pending: asked["pending"].clone(),
            challenge: hex::decode(&asked["challenge"])
                .unwrap()
                .try_into()
                .unwrap(),
            key: hex::decode(&asked["grant_key"])
                .unwrap()
                .try_into()
                .unwrap(),
            scope: AgentScope::parse(&asked["grant_scope"]).unwrap(),
            expires_ms: match asked["grant_expires"].as_str() {
                "never" => None,
                ms => Some(ms.parse().unwrap()),
            },
            server_key: match asked.get("grant_kind").map(String::as_str) {
                None => false,
                Some("server_key") => true,
                Some(other) => panic!("no grant kind {other}"),
            },
            browser_pages: asked.contains_key("grant_pages"),
            client: asked["client"].clone(),
        }
    }
}

impl Person {
    pub(crate) fn new(seed: u8) -> Self {
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(100); 32]);
        let cert = member_cert(&root, &device, "Laptop");
        Self { root, device, cert }
    }

    pub(crate) fn id(&self) -> String {
        hex::encode(self.root.verifying_key().to_bytes())
    }

    /// The person's Tilo certifying the grant's key as asked, but reaching
    /// `scope` and expiring at `expires_ms`.
    pub(crate) fn certify(
        &self,
        asked: &Asked,
        scope: AgentScope,
        expires_ms: Option<u64>,
    ) -> DeviceCert {
        // A key the person makes is their own, and reaches what they were
        // asked for: all their workspaces or the one the link names.
        let capabilities = if asked.server_key {
            DeviceCapabilities::own_key_grant(scope)
        } else {
            DeviceCapabilities::agent_grant(scope)
        }
        .with_browser_pages(asked.browser_pages);
        self.certify_as(asked, capabilities, expires_ms)
    }

    /// The person's Tilo certifying the grant's key with `capabilities`,
    /// expiring at `expires_ms`.
    pub(crate) fn certify_as(
        &self,
        asked: &Asked,
        capabilities: DeviceCapabilities,
        expires_ms: Option<u64>,
    ) -> DeviceCert {
        let now = tilo_clock::unix_now_ms();
        let mut cert = DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: asked.key,
            device_label: format!("{} via api.test", asked.client),
            capabilities,
            issued_at_ns: now.saturating_sub(1_000).saturating_mul(1_000_000),
            expires_at_ns: expires_ms.map(|ms| ms.saturating_mul(1_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &self.root).unwrap();
        cert
    }

    /// The form the person's Tilo posts back once they allow `asked`.
    pub(crate) fn allow(&self, asked: &Asked) -> String {
        let proof =
            SignInProof::sign(self.cert.clone(), &self.device, ISSUER, asked.challenge).unwrap();
        let cert = self.certify(asked, asked.scope.clone(), asked.expires_ms);
        answer_form(asked, &proof, &cert)
    }
}

pub(crate) fn answer_form(asked: &Asked, proof: &SignInProof, cert: &DeviceCert) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("pending", &asked.pending)
        .append_pair("proof", &sign_in::encode_proof(proof).unwrap())
        .append_pair("grant", &sign_in::encode_cert(cert).unwrap())
        .finish()
}

/// A host's info as the host writes it (`web-host.json`): the fixture the image check hands the
/// server, with `origin` where it names its HTTPS origin.
fn host_info(origin: &str) -> tilo_server_protocol::web::ServerInfo {
    let json = include_str!("../../../../os/containers/tilo-api/host-info.json")
        .replace("https://server.example.test", origin);
    serde_json::from_str::<tilo_server_protocol::web::PageHost>(&json)
        .unwrap()
        .into()
}

/// A host is named by its HTTPS origin, as its grants' devices and its links are, even where its
/// WebTransport listener answers at another name and port (tilo.sh's `link-eu1.tilo.sh:4443`
/// beside `eu1.tilo.sh`): naming it by the listener would move every grant to another device.
/// `TILO_API_HOST` takes the bytes the host writes in `web-host.json`, as they are.
#[test]
fn a_host_is_named_by_its_https_origin_not_its_link() {
    let mut info = host_info("https://eu1.tilo.sh");
    if let Some(web) = info
        .address
        .as_mut()
        .and_then(|address| address.web.as_mut())
    {
        web.origin = "https://link-eu1.tilo.sh:4443".into();
    }
    let written = String::from_utf8(info.web_host_json().unwrap()).unwrap();
    let read = crate::hosts::read(&written).unwrap();
    assert_eq!(read, vec![info.clone()]);
    let server = crate::server::Server::new(read.into_iter().next().unwrap());
    assert_eq!(server.name(), "eu1.tilo.sh");
    assert_eq!(server.origin(), "https://eu1.tilo.sh");

    // A host's info that names no https origin of its own names no host.
    info.origin = None;
    let unnamed = String::from_utf8(info.web_host_json().unwrap()).unwrap();
    assert!(crate::hosts::read(&unnamed).is_err());
    info.origin = Some("http://eu1.tilo.sh".into());
    let plain = String::from_utf8(info.web_host_json().unwrap()).unwrap();
    assert!(crate::hosts::read(&plain).is_err());
    // A link a page pins is renewed while the file stays, so the file names none.
    let address = info.address.as_mut().unwrap();
    address.web.as_mut().unwrap().certificate_sha256 = Some([1; 32]);
    let pinned = info.web_host_json().unwrap();
    assert!(!String::from_utf8_lossy(&pinned).contains("link-eu1"));
}

fn server() -> Arc<Server> {
    Server::new(
        &Config {
            issuer: ISSUER.into(),
            web_entry: WEB_ENTRY.into(),
            // Nothing here reaches a workspace, so nothing dials it.
            hosts: vec![host_info("https://server.tilo.test")],
        },
        Store::in_memory(),
    )
    .unwrap()
}

pub(crate) async fn body(reply: Reply) -> Vec<u8> {
    reply
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

pub(crate) async fn json_body(reply: Reply) -> Value {
    serde_json::from_slice(&body(reply).await).unwrap()
}

pub(crate) fn headers(pairs: &[(&'static str, &str)]) -> hyper::HeaderMap {
    let mut map = hyper::HeaderMap::new();
    for (name, value) in pairs {
        map.append(*name, HeaderValue::from_str(value).unwrap());
    }
    map
}

pub(crate) fn location(reply: &Reply) -> String {
    reply.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

/// Register a client.
pub(crate) async fn register(server: &Arc<Server>) -> String {
    let reply = server
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

/// Ask the server to authorize `client` for `resource`: where it sends the
/// person's browser.
pub(crate) async fn authorize(server: &Arc<Server>, client: &str, resource: &str) -> Asked {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", client)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("code_challenge", &store::challenge_of(VERIFIER))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", "xyz")
        .append_pair("resource", resource)
        .finish();
    let reply = server
        .dispatch(
            &Method::GET,
            oauth::AUTHORIZE_PATH,
            &query,
            &hyper::HeaderMap::new(),
            b"",
        )
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    Asked::of(&location(&reply))
}

/// Post the person's Tilo's answer `form` back to the server.
pub(crate) async fn answer(server: &Arc<Server>, form: &str) -> Reply {
    server
        .dispatch(
            &Method::POST,
            "/sign-in",
            "",
            &hyper::HeaderMap::new(),
            form.as_bytes(),
        )
        .await
}

/// Where the server sent the browser back to the agent, and what it said.
pub(crate) fn returned(reply: &Reply) -> HashMap<String, String> {
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let back = location(reply);
    assert!(back.starts_with(REDIRECT), "{back}");
    let answered = query_of(&back);
    assert_eq!(answered["state"], "xyz");
    answered
}

/// Run the browser part: authorize, and the person allows it in their Tilo.
/// Answers the code and the browser's cookie at the server.
pub(crate) async fn approve(
    server: &Arc<Server>,
    client: &str,
    person: &Person,
    resource: &str,
) -> (String, String) {
    let asked = authorize(server, client, resource).await;
    assert_eq!(asked.client, "Claude Code");
    let reply = answer(server, &person.allow(&asked)).await;
    let cookie = reply
        .headers()
        .get(header::SET_COOKIE)
        .map(|cookie| {
            cookie
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
        .unwrap_or_default();
    let answered = returned(&reply);
    let code = answered
        .get("code")
        .cloned()
        .unwrap_or_else(|| panic!("no code: {answered:?}"));
    (code, cookie)
}

pub(crate) async fn redeem(
    server: &Arc<Server>,
    client: &str,
    code: &str,
    verifier: &str,
) -> Reply {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("client_id", client)
        .append_pair("code_verifier", verifier)
        .finish();
    server
        .dispatch(
            &Method::POST,
            oauth::TOKEN_PATH,
            "",
            &hyper::HeaderMap::new(),
            form.as_bytes(),
        )
        .await
}

/// An MCP request with `token`, on `path`, in `session`.
pub(crate) async fn mcp(
    server: &Arc<Server>,
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
    server
        .dispatch(
            &Method::POST,
            path,
            "",
            &headers(&pairs),
            message.to_string().as_bytes(),
        )
        .await
}

/// A `/v1` request with `token`, as `tilo` sends it.
async fn v1(server: &Arc<Server>, method: &Method, path: &str, token: &str) -> Reply {
    let bearer = format!("Bearer {token}");
    server
        .dispatch(
            method,
            path,
            "",
            &headers(&[("authorization", bearer.as_str())]),
            b"",
        )
        .await
}

/// What a key reads of itself at `GET /v1`: what it reaches, who approved it
/// and until when, as `tilo` shows it.
async fn key_seen(server: &Arc<Server>, token: &str) -> Value {
    let reply = v1(server, &Method::GET, "/v1", token).await;
    assert_eq!(reply.status(), StatusCode::OK);
    json_body(reply).await["key"].clone()
}

/// Ask the server for a key as a terminal does, with `body`: the poll secret,
/// and what the server asked the person's Tilo.
pub(crate) async fn ask_for_key(server: &Arc<Server>, body: &Value) -> (String, Asked) {
    let reply = server
        .dispatch(
            &Method::POST,
            keys::NEW_PATH,
            "",
            &hyper::HeaderMap::new(),
            body.to_string().as_bytes(),
        )
        .await;
    assert_eq!(reply.status(), StatusCode::CREATED);
    let asked = json_body(reply).await;
    let approve = asked["approve_url"].as_str().unwrap();
    let reply = server
        .dispatch(
            &Method::GET,
            approve.strip_prefix(ISSUER).unwrap(),
            "",
            &hyper::HeaderMap::new(),
            b"",
        )
        .await;
    (
        asked["poll"].as_str().unwrap().to_owned(),
        Asked::of(&location(&reply)),
    )
}

/// What polling for a key answers: its status and body.
pub(crate) async fn poll_key(server: &Arc<Server>, poll: &str) -> (StatusCode, Value) {
    let reply = server
        .dispatch(
            &Method::POST,
            keys::POLL_PATH,
            "",
            &hyper::HeaderMap::new(),
            json!({ "poll": poll }).to_string().as_bytes(),
        )
        .await;
    (reply.status(), json_body(reply).await)
}

/// The person answers what their Tilo was asked for a key, and the terminal
/// collecting it then receives the key.
async fn key_answered(server: &Arc<Server>, person: &Person, poll: &str, asked: &Asked) -> Value {
    assert!(
        answer(server, &person.allow(asked))
            .await
            .status()
            .is_success()
    );
    let (status, polled) = poll_key(server, poll).await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    polled
}

/// A key a terminal asked for `body` and the person allowed: the key it
/// collected, the poll it asked by, and what the server asked the person's Tilo.
async fn key_allowed(
    server: &Arc<Server>,
    person: &Person,
    body: Value,
) -> (String, String, Asked) {
    let (poll, asked) = ask_for_key(server, &body).await;
    let polled = key_answered(server, person, &poll, &asked).await;
    let token = polled["key"].as_str().unwrap().to_owned();
    (token, poll, asked)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_discovers_the_authorization_server_from_its_first_refusal() {
    let server = server();
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
        let reply = server
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
        // The metadata names the resource and the server as its server.
        let metadata_path = metadata.strip_prefix(ISSUER).unwrap();
        let resource = json_body(
            server
                .dispatch(
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
            server
                .dispatch(&Method::GET, path, "", &hyper::HeaderMap::new(), b"")
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
    let server = server();
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
        let reply = server
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
    let alice = Person::new(1);
    let server = server();
    let client = register(&server).await;
    let (code, _) = approve(&server, &client, &alice, ISSUER).await;
    let wrong = "another-verifier-of-at-least-forty-three-characters-xyz";
    assert_eq!(
        json_body(redeem(&server, &client, &code, wrong).await).await["error"],
        "invalid_grant"
    );
    // A wrong verifier spent the code: the agent must ask again.
    assert_eq!(
        json_body(redeem(&server, &client, &code, VERIFIER).await).await["error"],
        "invalid_grant"
    );
    let (code, _) = approve(&server, &client, &alice, ISSUER).await;
    let other = register(&server).await;
    assert_eq!(
        json_body(redeem(&server, &other, &code, VERIFIER).await).await["error"],
        "invalid_grant"
    );
    let (code, _) = approve(&server, &client, &alice, ISSUER).await;
    let reply = redeem(&server, &client, &code, VERIFIER).await;
    assert_eq!(reply.headers()[header::CACHE_CONTROL], "no-store");
    let tokens = json_body(reply).await;
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(
        json_body(redeem(&server, &client, &code, VERIFIER).await).await["error"],
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
        server
            .dispatch(
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
        server
            .dispatch(
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
    assert_eq!(server.store.grants_of(&alice.id()).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_past_its_certificate_issues_no_token() {
    let alice = Person::new(1);
    let server = server();
    let client = register(&server).await;
    let asked = authorize(&server, &client, ISSUER).await;
    let proof =
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap();
    let soon = tilo_clock::unix_now_ms() + 1_500;
    let cert = alice.certify(&asked, asked.scope.clone(), Some(soon));
    let code =
        returned(&answer(&server, &answer_form(&asked, &proof, &cert)).await)["code"].clone();
    let tokens = json_body(redeem(&server, &client, &code, VERIFIER).await).await;
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    tokio::time::sleep(std::time::Duration::from_millis(1_600)).await;
    let reply = mcp(
        &server,
        "/",
        &access,
        None,
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
    let refresh = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", tokens["refresh_token"].as_str().unwrap())
        .append_pair("client_id", &client)
        .finish();
    let renewed = json_body(
        server
            .dispatch(
                &Method::POST,
                oauth::TOKEN_PATH,
                "",
                &hyper::HeaderMap::new(),
                refresh.as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(renewed["error"], "invalid_grant");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_grant_is_exactly_what_the_server_asked_the_person_to_certify() {
    let alice = Person::new(1);
    let mallory = Person::new(3);
    let server = server();
    let client = register(&server).await;

    // The person declines in their Tilo: the agent hears so.
    let asked = authorize(&server, &client, ISSUER).await;
    let declined = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("pending", &asked.pending)
        .append_pair("decision", "deny")
        .finish();
    assert_eq!(
        returned(&answer(&server, &declined).await)["error"],
        "access_denied"
    );
    // Answered once: the same authorization takes no second answer.
    let reply = answer(&server, &alice.allow(&asked)).await;
    assert_eq!(reply.status(), StatusCode::BAD_REQUEST);

    let proof = |asked: &Asked| {
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap()
    };
    let wrong: [(&str, Certify<'_>); 3] = [
        (
            "another key",
            Box::new(|asked| {
                let other = Asked {
                    pending: asked.pending.clone(),
                    challenge: asked.challenge,
                    key: [7; 32],
                    scope: asked.scope.clone(),
                    expires_ms: asked.expires_ms,
                    server_key: asked.server_key,
                    browser_pages: asked.browser_pages,
                    client: asked.client.clone(),
                };
                alice.certify(&other, asked.scope.clone(), asked.expires_ms)
            }),
        ),
        (
            "someone else's root",
            Box::new(|asked| mallory.certify(asked, asked.scope.clone(), asked.expires_ms)),
        ),
        (
            "already expired",
            Box::new(|asked| {
                let mut cert = alice.certify(asked, asked.scope.clone(), asked.expires_ms);
                cert.issued_at_ns = 1;
                cert.expires_at_ns = Some(2);
                tilo_user_identity::sign_device_cert(&mut cert, &alice.root).unwrap();
                cert
            }),
        ),
    ];
    for (why, certify) in &wrong {
        let asked = authorize(&server, &client, ISSUER).await;
        let reply = answer(
            &server,
            &answer_form(&asked, &proof(&asked), &certify(&asked)),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN, "{why}");
    }
    // A proof for another challenge signs nobody in.
    let asked = authorize(&server, &client, ISSUER).await;
    let stale = SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, [9; 32]).unwrap();
    let cert = alice.certify(&asked, asked.scope.clone(), asked.expires_ms);
    let reply = answer(&server, &answer_form(&asked, &stale, &cert)).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert!(server.store.grants_of(&alice.id()).is_empty());
    assert!(server.store.grants_of(&mallory.id()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_may_narrow_a_grant_to_one_workspace_and_never_widen_it() {
    // The person's Tilo takes a code out of a grant it certified, and the
    // grant the server then keeps: what the agent carries from it.
    async fn granted(
        server: &Arc<Server>,
        client: &str,
        person: &Person,
        asked: Asked,
        cert: DeviceCert,
    ) -> (store::Grant, String) {
        let proof = SignInProof::sign(person.cert.clone(), &person.device, ISSUER, asked.challenge)
            .unwrap();
        let reply = answer(server, &answer_form(&asked, &proof, &cert)).await;
        let code = returned(&reply)["code"].clone();
        let tokens = json_body(redeem(server, client, &code, VERIFIER).await).await;
        let access = tokens["access_token"].as_str().unwrap().to_owned();
        (server.store.grants_of(&person.id())[0].clone(), access)
    }

    let alice = Person::new(1);
    let studio = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
    let garden = "0b9e8f6c-2a1d-4e3b-8c7f-5d4a3b2c1e0f";
    let server = server();
    let client = register(&server).await;
    let proof = |asked: &Asked| {
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap()
    };
    // The agent asked for all of them and the person chose one: the grant is
    // that workspace's, not the agent's request.
    let asked = authorize(&server, &client, ISSUER).await;
    assert_eq!(asked.scope.clone(), AgentScope::AllWorkspaces);
    let narrowed = AgentScope::parse(studio).unwrap();
    let cert = alice.certify(&asked, narrowed.clone(), asked.expires_ms);
    let (grant, access) = granted(&server, &client, &alice, asked, cert).await;
    assert_eq!(grant.scope, store::Scope::Workspace(studio.to_owned()));
    assert_eq!(grant.key().unwrap().scope(), narrowed);
    // The key carries that one workspace to whatever asks it: another is refused.
    assert_eq!(
        mcp(
            &server,
            &format!("/w/{garden}"),
            &access,
            None,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );

    // Asked for one workspace, the server takes that one and nothing else: not
    // all of them, and not another.
    let pinned = format!("{ISSUER}/w/{garden}");
    for (why, scope) in [
        ("all of them", AgentScope::AllWorkspaces),
        ("another workspace", AgentScope::parse(studio).unwrap()),
    ] {
        let asked = authorize(&server, &client, &pinned).await;
        assert_eq!(asked.scope.clone(), AgentScope::parse(garden).unwrap());
        let cert = alice.certify(&asked, scope, asked.expires_ms);
        let reply = answer(&server, &answer_form(&asked, &proof(&asked), &cert)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN, "{why}");
    }
    let asked = authorize(&server, &client, &pinned).await;
    let cert = alice.certify(&asked, asked.scope.clone(), asked.expires_ms);
    let (grant, access) = granted(&server, &client, &alice, asked, cert).await;
    assert_eq!(grant.scope, store::Scope::Workspace(garden.to_owned()));
    assert_eq!(
        mcp(
            &server,
            &format!("/w/{studio}"),
            &access,
            None,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

/// A key the person makes for all their workspaces lasts until they revoke
/// it, or until the expiry they chose; it operates web pages in Browser
/// unless they said no, and says so wherever it is read.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_for_all_workspaces_lasts_until_revoked_or_the_expiry_its_person_chose() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    let alice = Person::new(1);
    let server = server();
    let now = tilo_clock::unix_now_ms();

    // Asked for all workspaces: a server key, lasting until revoked.
    let (poll, asked) = ask_for_key(&server, &json!({"name": "Visa backend"})).await;
    assert!(asked.server_key);
    assert_eq!(
        (asked.scope.clone(), asked.expires_ms),
        (AgentScope::AllWorkspaces, None)
    );
    assert!(
        asked.browser_pages,
        "a key made with no answer operates web pages in Browser"
    );
    let polled = key_answered(&server, &alice, &poll, &asked).await;
    assert!(polled["expires_at_ms"].is_null(), "{polled}");
    let token = polled["key"].as_str().unwrap().to_owned();
    assert!(token.starts_with("tilo_sk_"));
    // Shown once: the poll answers it no more.
    assert_eq!(poll_key(&server, &poll).await.0, StatusCode::NOT_FOUND);

    // The certificate the person signed carries web pages, and so does the key.
    let grant = server
        .store
        .access(&token, tilo_clock::unix_now_ms())
        .unwrap();
    assert!(grant.key().unwrap().cert().capabilities.browser_pages);
    assert!(grant.browser_pages);
    assert_eq!(grant.created_by.as_deref(), Some("Laptop"));
    let seen = key_seen(&server, &token).await;
    assert_eq!(seen["grant"], grant.id.as_str());
    assert_eq!(seen["name"], "Visa backend");
    assert_eq!(seen["scope"], "all");
    assert_eq!(seen["kind"], "key");
    assert_eq!(seen["created_by"], "Laptop");
    assert_eq!(seen["browser_pages"], true);
    assert!(seen["expires_at_ms"].is_null(), "{seen}");
    assert_eq!(
        seen["last_used_at_ms"].as_u64().unwrap() % store::LAST_USED_SAVE_MS,
        0,
        "shown to the minute"
    );

    // Still a key long past the bound of an agent's grant.
    let later = now + 400 * DAY_MS;
    let grant = server
        .store
        .access(&token, later)
        .expect("a server key lasts");
    assert_eq!(grant.last_used_ms, Some(later));

    // Said no: neither the certificate nor the key operates a page.
    let (token, _, asked) = key_allowed(
        &server,
        &alice,
        json!({"name": "Reader", "browser_pages": false}),
    )
    .await;
    assert!(!asked.browser_pages, "the person is not asked to allow it");
    let grant = server
        .store
        .access(&token, tilo_clock::unix_now_ms())
        .unwrap();
    assert!(!grant.key().unwrap().cert().capabilities.browser_pages);
    assert!(!grant.browser_pages);
    assert_eq!(key_seen(&server, &token).await["browser_pages"], false);

    // Revoking it ends it.
    assert!(server.revoke_grant(&alice.id(), &grant.id).await.unwrap());
    // A revoked key is told so, and when, not that it never was a key.
    assert!(matches!(
        server.store.access(&token, now),
        Err(store::Denied::Revoked { at_ms: Some(_) })
    ));
    assert!(matches!(
        server.store.access("tilo_sk_never", now),
        Err(store::Denied::Unknown)
    ));

    // The person may choose when it expires.
    let (poll, asked) = ask_for_key(
        &server,
        &json!({"name": "Nightly job", "expires_in": 3_600}),
    )
    .await;
    assert!(asked.server_key);
    let expires = asked.expires_ms.unwrap();
    assert!(
        (now + 3_600_000..now + 3_700_000).contains(&expires),
        "{expires}"
    );
    let polled = key_answered(&server, &alice, &poll, &asked).await;
    assert_eq!(polled["expires_at_ms"], expires);
    let token = polled["key"].as_str().unwrap();
    assert!(server.store.access(token, expires - 1).is_ok());
    assert!(matches!(
        server.store.access(token, expires),
        Err(store::Denied::Expired { at_ms: Some(at) }) if at == expires
    ));
}

/// A key a terminal asks for one workspace reaches that workspace and no
/// other, ends within a grant's life, and is a server key only because the
/// key flow asked for one.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_for_one_workspace_reaches_that_workspace_and_ends_within_a_grant() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    let alice = Person::new(4);
    let server = server();
    let now = tilo_clock::unix_now_ms();
    let studio = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
    let garden = "0b9e8f6c-2a1d-4e3b-8c7f-5d4a3b2c1e0f";

    // The link names the workspace, as the person's own server key, and stays
    // bounded.
    let (poll, asked) =
        ask_for_key(&server, &json!({"name": "Nightly", "workspace": studio})).await;
    assert_eq!(
        asked.scope.clone(),
        AgentScope::parse(studio).unwrap(),
        "the link names the workspace the key reaches"
    );
    assert!(asked.server_key, "a key the person makes is a server key");
    assert!(
        asked.expires_ms.unwrap() <= now + 90 * DAY_MS,
        "and ends within a grant's life"
    );
    // Longer than a grant lives, a zero expiry, and an unreadable one are refused.
    for body in [
        json!({"name": "Nightly", "workspace": studio, "expires_in": 100 * 24 * 60 * 60}),
        json!({"name": "Visa backend", "expires_in": 0}),
        json!({"name": "Visa backend", "expires_in": "soon"}),
    ] {
        let reply = server
            .dispatch(
                &Method::POST,
                keys::NEW_PATH,
                "",
                &hyper::HeaderMap::new(),
                body.to_string().as_bytes(),
            )
            .await;
        assert_eq!(reply.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    let polled = key_answered(&server, &alice, &poll, &asked).await;
    let token = polled["key"].as_str().unwrap();
    let grant = server
        .store
        .access(token, tilo_clock::unix_now_ms())
        .expect("the key reaches the person's workspaces");
    assert_eq!(grant.scope, store::Scope::Workspace(studio.to_owned()));
    let seen = key_seen(&server, token).await;
    assert_eq!(seen["scope"], studio);
    assert_eq!(seen["kind"], "key");
    assert!(seen["expires_at_ms"].is_number(), "{seen}");
    // Another workspace is outside what it reaches.
    let reply = v1(
        &server,
        &Method::GET,
        &format!("/v1/workspaces/{garden}/resources"),
        token,
    )
    .await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(reply).await["error"]["code"], "beyond_scope");

    // A server key is what the key flow asked for, and nothing else is.
    let (_, asked) = ask_for_key(&server, &json!({"name": "Visa backend"})).await;
    let plain = alice.certify_as(
        &asked,
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        Some(now + DAY_MS),
    );
    let proof =
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap();
    let reply = answer(&server, &answer_form(&asked, &proof, &plain)).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    let client = register(&server).await;
    let asked = authorize(&server, &client, ISSUER).await;
    assert!(!asked.server_key);
    let lasting = alice.certify_as(&asked, DeviceCapabilities::server_key_grant(), None);
    let proof =
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap();
    let reply = answer(&server, &answer_form(&asked, &proof, &lasting)).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
}

/// A key is the grant the person approved, carried in the key itself: an API
/// server that never saw the approval accepts the same key, reads the same
/// grant, and revokes that one grant.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_carries_its_grant_to_every_api_server() {
    let alice = Person::new(1);
    let first = server();
    let (token, _, _) = key_allowed(&first, &alice, json!({"name": "Visa backend"})).await;

    let now = tilo_clock::unix_now_ms();
    let second = server();
    let carried = second.store.access(&token, now).unwrap();
    assert_eq!(carried.client_name, "Visa backend");
    assert_eq!(
        carried.person,
        hex::encode(alice.root.verifying_key().to_bytes())
    );
    assert_eq!(carried.scope, store::Scope::Account);
    assert!(carried.key().is_some(), "the key travels with its grant");
    assert_eq!(carried.id, first.store.access(&token, now).unwrap().id);

    // Read from its token alone, nothing of it is kept. The API server
    // keeps it once a server it is for has let its person in.
    assert!(second.store.grants_of(&carried.person).is_empty());
    second.store.keep_carried(&carried).unwrap();
    assert_eq!(second.store.grants_of(&carried.person).len(), 1);

    // Revoked at one API server, that grant is revoked at that one for good.
    assert!(
        !second
            .store
            .revoke_grant(&carried.person, &carried.id, now)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        second.store.access(&token, now).unwrap_err(),
        store::Denied::Revoked { at_ms: Some(now) }
    );
    // A token that carries no grant reaches nothing, as before.
    assert_eq!(
        second.store.access("tilo_sk_not-a-grant", now).unwrap_err(),
        store::Denied::Unknown
    );
}

/// A key `person`'s root certified for a service of theirs, made from
/// `seed`, as whoever has it brings it.
fn key_of(person: &Person, seed: u8) -> GrantKey {
    let now = tilo_clock::unix_now_ms();
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let cert = labelled_with(
        &person.root,
        &key,
        "anything",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        now.saturating_sub(1_000).saturating_mul(1_000_000),
        Some(now.saturating_add(86_400_000).saturating_mul(1_000_000)),
    );
    GrantKey::held(key, cert, now.saturating_mul(1_000_000)).unwrap()
}

/// A key this server never issued is read from its token alone, and nothing
/// but the token vouches for it: a root anybody makes certifies a key
/// anybody makes. Reading one keeps nothing of it; once kept, a person's
/// carried keys are bounded, and what was revoked or minted keeps reading as
/// it should.
#[test]
fn a_carried_key_is_read_from_its_token_and_kept_within_its_bound() {
    let store = Store::in_memory();
    let now = tilo_clock::unix_now_ms();
    let mut kept = 0;
    for seed in 1..=20_u8 {
        // Nobody: a root made for this one request, known to no server.
        let stranger = Person::new(seed);
        let token = store::key_token("anything", &key_of(&stranger, seed + 200)).unwrap();
        let read = store.access(&token, now).unwrap();
        assert_eq!(read.person, stranger.id());
        assert!(read.key().is_some(), "the key is read from its token");
        kept += store.grants_of(&stranger.id()).len();
    }
    assert_eq!(
        kept, 0,
        "{kept} of 20 keys that nobody approved here, each certified by a root made for the \
         request, were kept as grants with their secrets"
    );
    let stranger = Person::new(1).id();
    for _ in 0..2 {
        let counted = store.next_run(&stranger).unwrap();
        assert_eq!(counted, 1, "a person nothing is kept of was counted");
    }

    // Once a server its key is for has let a person in, the key is kept, and
    // a person's keys kept that way are bounded.
    let ada = Person::new(9);
    let most = store::MAX_CARRIED_PER_PERSON;
    let tokens: Vec<String> = (0..most + 6)
        .map(|at| {
            let key = key_of(&ada, 120 + u8::try_from(at).unwrap());
            store::key_token("anything", &key).unwrap()
        })
        .collect();
    let mut ids = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let used = now + u64::try_from(at).unwrap();
        let read = store.access(token, used).unwrap();
        store.keep_carried(&read).unwrap();
        match at {
            // The first mints a key, which reads as it from then on.
            0 => {
                let minted = store::GrantKind::Minted {
                    parent: read.id.clone(),
                };
                store
                    .resource_key(&key_of(&ada, 119), "minted", minted, None, used)
                    .unwrap();
            }
            // The second is revoked.
            1 => assert!(
                !store
                    .revoke_grant(&ada.id(), &read.id, used)
                    .unwrap()
                    .is_empty()
            ),
            _ => {}
        }
        ids.push(read.id);
    }
    let kept = store
        .grants_of(&ada.id())
        .into_iter()
        .filter(|grant| grant.kind == store::GrantKind::Key)
        .count();
    assert_eq!(kept, most, "a person's keys kept this way are bounded");
    assert!(store.grant(&ids[0]).is_some(), "the one that minted stays");
    assert!(store.grant(&ids[2]).is_none(), "the oldest gave way");
    assert!(store.grant(&ids[most + 5]).is_some(), "the newest is kept");
    // What gave way is read from its token again, and what was revoked is
    // refused as revoked.
    assert_eq!(store.access(&tokens[2], now).unwrap().id, ids[2]);
    assert!(matches!(
        store.access(&tokens[1], now),
        Err(store::Denied::Revoked { .. })
    ));
}

/// A site on loopback whose every page answers `status`, counting asks.
async fn site_answering(status: &'static str) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    serve_answering(listener, status, Arc::clone(&asked));
    (origin, asked)
}

/// Answer every request on `listener` with `status`, counting the asks.
fn serve_answering(
    listener: tokio::net::TcpListener,
    status: &'static str,
    asked: Arc<std::sync::atomic::AtomicUsize>,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let reply =
                format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
}

fn server_in_front_of(site: &str) -> Arc<Server> {
    Server::new(
        &Config {
            issuer: ISSUER.into(),
            web_entry: site.into(),
            hosts: vec![host_info("https://server.tilo.test")],
        },
        Store::in_memory(),
    )
    .unwrap()
}

async fn health(server: &Arc<Server>) -> Value {
    let reply = server
        .dispatch(&Method::GET, "/health", "", &hyper::HeaderMap::new(), b"")
        .await;
    assert_eq!(
        reply.status(),
        StatusCode::OK,
        "the server answers for itself"
    );
    serde_json::from_slice(&body(reply).await).unwrap()
}

/// Health says whether the site serves the sign-in page every approval lands
/// on. A page it does not serve is asked for again; a site that starts after
/// the API is seen served at once, because the probe that found nothing there
/// is not kept.
#[tokio::test]
async fn health_says_whether_the_site_serves_the_sign_in_page() {
    let (missing, probes) = site_answering("404 Not Found").await;
    let api = server_in_front_of(&missing);
    let answered = health(&api).await;
    assert_eq!(answered["ok"], true);
    assert_eq!(
        answered["sign_in"],
        format!("{missing}/sign-in answers 404 Not Found")
    );
    health(&api).await;
    assert_eq!(
        probes.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "a page the site does not serve is asked for again"
    );

    let (served, _) = site_answering("200 OK").await;
    assert_eq!(
        health(&server_in_front_of(&served)).await["sign_in"],
        "served"
    );

    // The address the site will answer on: taken to know it, then given back
    // for it to listen on.
    let port = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let api = server_in_front_of(&format!("http://127.0.0.1:{port}"));
    assert_ne!(
        health(&api).await["sign_in"],
        "served",
        "nothing answers yet"
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    serve_answering(listener, "200 OK", Arc::clone(&probes));
    assert_eq!(health(&api).await["sign_in"], "served");
    assert_eq!(probes.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// A refusal reads as what the caller can act on: a workspace operation is
/// read by its case (one that is not the person's own is not theirs, and a
/// branch that could not take everything names the part that did not come),
/// and a person the server does not know reads why and what makes them known.
#[test]
fn a_refusal_reads_as_what_the_caller_can_do_about_it() {
    use tilo_server_protocol::workspaces::AdminRefusal;
    let refused = |refusal: AdminRefusal| {
        let error = tilo_actions::remote::ActionCallError::Refused {
            reason: refusal.to_reason(),
        };
        server::ServerError::from(tilo_session_client::device::DeviceRefusal::of(&error))
    };
    assert_eq!(
        refused(AdminRefusal::NotYourOwn),
        server::ServerError::RefusedWorkspace(AdminRefusal::NotYourOwn)
    );
    let part = "the record plan of so.tilo.notes";
    let not_made = refused(AdminRefusal::BranchNotMade { part: part.into() });
    assert_eq!(
        not_made,
        server::ServerError::RefusedWorkspace(AdminRefusal::BranchNotMade { part: part.into() })
    );
    let Some(account::Outcome::Failed { code, message, .. }) = account::not_branched(&not_made)
    else {
        panic!("a branch that was not made");
    };
    assert_eq!(code, account::BRANCH_INCOMPLETE);
    assert!(
        message.contains(part),
        "the caller reads which part did not come: {message}"
    );

    let person = [7_u8; 32];
    let refusal = tilo_session_client::device::DeviceRefusal::of(
        &tilo_actions::remote::not_on_this_server(&person),
    );
    let error = server::ServerError::from(refusal);
    let account::Outcome::Failed { code, message, .. } = account::refused(&error) else {
        panic!("a refusal fails");
    };
    assert_eq!(code, "not_on_this_server");
    assert!(
        message.contains(&hex::encode(person)) && message.contains("owner"),
        "{message}"
    );
}

/// A pinned path names a workspace only when it is canonical: a canonical one
/// is the agent's endpoint (and asks an unauthenticated caller for a key),
/// while anything else is the server's not-found page, never a workspace.
#[tokio::test]
async fn a_pinned_path_names_only_a_canonical_workspace() {
    let server = server();
    let id = "0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa";
    for path in [format!("/w/{id}"), format!("/w/{id}/mcp")] {
        let reply = server
            .dispatch(&Method::POST, &path, "", &headers(&[]), b"{}")
            .await;
        assert_eq!(reply.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    for bad in [
        "/w/0B0C8F0E-6F4A-4F2F-9D1C-2F2B0A5E11AA",
        "/w/00000000-0000-0000-0000-000000000000",
        "/w/../grants",
        "/w/",
        "/other",
    ] {
        let reply = server
            .dispatch(&Method::POST, bad, "", &headers(&[]), b"{}")
            .await;
        assert_eq!(reply.status(), StatusCode::NOT_FOUND, "{bad}");
    }
}

/// A person who opens the grants page without a session is sent to their own
/// Tilo for a sign-in that asks for no grant, and comes back signed in. A form
/// the server forgot (its page tokens live in memory, and a restart forgets
/// them) or one posted after the sign-in expired goes back to the page to ask
/// again, never to a dead end.
#[tokio::test]
async fn the_grants_page_signs_a_person_in_and_asks_again_when_its_form_is_forgotten() {
    let server = server();
    let alice = Person::new(3);
    let reply = server
        .dispatch(&Method::GET, "/grants", "", &headers(&[]), b"")
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let link = location(&reply);
    assert!(link.starts_with(&format!("{WEB_ENTRY}/sign-in?")), "{link}");
    let asked = query_of(&link);
    assert_eq!(asked["audience"], ISSUER);
    assert_eq!(asked["return"], format!("{ISSUER}/sign-in"));
    // Signing in, and nothing more: no grant is asked for.
    for field in ["grant_key", "grant_scope", "grant_expires", "grant_kind"] {
        assert!(!asked.contains_key(field), "{field} in {link}");
    }
    let challenge: [u8; 32] = hex::decode(&asked["challenge"])
        .unwrap()
        .try_into()
        .unwrap();
    let proof = SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, challenge).unwrap();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("pending", &asked["pending"])
        .append_pair("proof", &sign_in::encode_proof(&proof).unwrap())
        .finish();
    let reply = answer(&server, &form).await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&reply), "/grants");
    let cookie = reply
        .headers()
        .get(header::SET_COOKIE)
        .expect("the answer signs the browser in")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let reply = server
        .dispatch(
            &Method::GET,
            "/grants",
            "",
            &headers(&[("cookie", cookie.as_str())]),
            b"",
        )
        .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let page = String::from_utf8(body(reply).await).unwrap();
    assert!(page.contains("Connected agents"), "{page}");
    assert!(page.contains("action=\"/grants/keys\""), "{page}");

    // A form the page no longer knows goes back to it, signed in or not.
    let reply = server
        .dispatch(
            &Method::POST,
            "/grants/keys",
            "",
            &headers(&[("cookie", cookie.as_str())]),
            b"csrf=stale&name=Bot",
        )
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&reply), "/grants");
    let path = "/grants/3ee4a797-1ec0-474b-a3d0-62895d5e15af/revoke";
    let reply = server
        .dispatch(
            &Method::POST,
            path,
            "",
            &headers(&[("cookie", cookie.as_str())]),
            b"csrf=stale",
        )
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&reply), "/grants");
    // Signed out: the page starts the sign-in again.
    let reply = server
        .dispatch(&Method::POST, path, "", &headers(&[]), b"csrf=stale")
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&reply), "/grants");
}

/// What a difference between the page's clock and the server's does to an
/// approval.
mod sign_in_clock;
