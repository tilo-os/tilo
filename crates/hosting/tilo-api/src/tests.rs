//! The server's own flows in process: an agent discovers how to authorize,
//! registers, sends the person to their Tilo (which these tests play: it
//! signs the challenge with a device of the person and certifies the
//! grant's key with their root), and redeems the code with its PKCE
//! verifier. What a grant then does in workspaces is the relay's, which
//! `tilo-agent-participant`'s `api` test drives through the server.

use super::*;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, SignInProof};

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
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
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
        let capabilities = if asked.server_key {
            DeviceCapabilities::server_key_grant()
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
        let cert = self.certify(asked, asked.scope, asked.expires_ms);
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

fn server() -> Arc<Server> {
    Server::new(
        &Config {
            issuer: ISSUER.into(),
            web_entry: WEB_ENTRY.into(),
            // Nothing here reaches a workspace, so nothing dials it.
            relay: "https://relay.tilo.test".into(),
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
                    scope: asked.scope,
                    expires_ms: asked.expires_ms,
                    server_key: asked.server_key,
                    browser_pages: asked.browser_pages,
                    client: asked.client.clone(),
                };
                alice.certify(&other, asked.scope, asked.expires_ms)
            }),
        ),
        (
            "someone else's root",
            Box::new(|asked| mallory.certify(asked, asked.scope, asked.expires_ms)),
        ),
        (
            "already expired",
            Box::new(|asked| {
                let mut cert = alice.certify(asked, asked.scope, asked.expires_ms);
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
    let cert = alice.certify(&asked, asked.scope, asked.expires_ms);
    let reply = answer(&server, &answer_form(&asked, &stale, &cert)).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert!(server.store.grants_of(&alice.id()).is_empty());
    assert!(server.store.grants_of(&mallory.id()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_may_narrow_a_grant_to_one_workspace_and_never_widen_it() {
    // The person's Tilo takes a code out of a grant it certified, and the
    // grant the server then holds.
    async fn granted(
        server: &Arc<Server>,
        client: &str,
        person: &Person,
        asked: Asked,
        cert: DeviceCert,
    ) -> store::Grant {
        let proof = SignInProof::sign(person.cert.clone(), &person.device, ISSUER, asked.challenge)
            .unwrap();
        let reply = answer(server, &answer_form(&asked, &proof, &cert)).await;
        let code = returned(&reply)["code"].clone();
        json_body(redeem(server, client, &code, VERIFIER).await).await;
        server.store.grants_of(&person.id())[0].clone()
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
    assert_eq!(asked.scope, AgentScope::AllWorkspaces);
    let narrowed = AgentScope::parse(studio).unwrap();
    let cert = alice.certify(&asked, narrowed, asked.expires_ms);
    let grant = granted(&server, &client, &alice, asked, cert).await;
    assert_eq!(grant.scope, store::Scope::Workspace(studio.to_owned()));
    assert_eq!(grant.key().unwrap().scope(), narrowed);

    // Asked for one workspace, the server takes that one and nothing else: not
    // all of them, and not another.
    let pinned = format!("{ISSUER}/w/{garden}");
    for (why, scope) in [
        ("all of them", AgentScope::AllWorkspaces),
        ("another workspace", AgentScope::parse(studio).unwrap()),
    ] {
        let asked = authorize(&server, &client, &pinned).await;
        assert_eq!(asked.scope, AgentScope::parse(garden).unwrap());
        let cert = alice.certify(&asked, scope, asked.expires_ms);
        let reply = answer(&server, &answer_form(&asked, &proof(&asked), &cert)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN, "{why}");
    }
    let asked = authorize(&server, &client, &pinned).await;
    let cert = alice.certify(&asked, asked.scope, asked.expires_ms);
    let grant = granted(&server, &client, &alice, asked, cert).await;
    assert_eq!(grant.scope, store::Scope::Workspace(garden.to_owned()));
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
    let cert = alice.certify(&asked, asked.scope, Some(soon));
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

/// Ask the server for a key as a terminal does, with `body`: the poll secret,
/// and what the server asked the person's Tilo.
async fn ask_for_key(server: &Arc<Server>, body: &Value) -> (String, Asked) {
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
async fn poll_key(server: &Arc<Server>, poll: &str) -> (StatusCode, Value) {
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

#[tokio::test(flavor = "multi_thread")]
async fn a_key_operates_web_pages_in_browser_unless_its_person_says_no() {
    let alice = Person::new(1);
    let server = server();

    // Made with no answer: the certificate its person approves lets it
    // operate web pages, and the key the server keeps says so.
    let (poll, asked) = ask_for_key(&server, &json!({"name": "Visa backend"})).await;
    assert!(asked.browser_pages);
    assert!(
        answer(&server, &alice.allow(&asked))
            .await
            .status()
            .is_success()
    );
    let (status, polled) = poll_key(&server, &poll).await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    let token = polled["key"].as_str().unwrap().to_owned();
    let grant = server
        .store
        .access(&token, tilo_clock::unix_now_ms())
        .unwrap();
    assert!(
        grant.key().unwrap().cert().capabilities.browser_pages,
        "the certificate his person signed carries it"
    );
    assert!(grant.browser_pages);
    assert_eq!(
        account::grant_row(&grant)["browser_pages"],
        serde_json::json!(true)
    );

    // Said no: neither the certificate nor the key operates a page.
    let (poll, asked) =
        ask_for_key(&server, &json!({"name": "Reader", "browser_pages": false})).await;
    assert!(!asked.browser_pages, "the person is not asked to allow it");
    assert!(
        answer(&server, &alice.allow(&asked))
            .await
            .status()
            .is_success()
    );
    let (status, polled) = poll_key(&server, &poll).await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    let token = polled["key"].as_str().unwrap().to_owned();
    let grant = server
        .store
        .access(&token, tilo_clock::unix_now_ms())
        .unwrap();
    assert!(!grant.key().unwrap().cert().capabilities.browser_pages);
    assert!(!grant.browser_pages);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_key_lasts_until_revoked_unless_its_person_chose_an_expiry() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    let alice = Person::new(1);
    let server = server();
    let now = tilo_clock::unix_now_ms();

    // A key reaching all workspaces is a server key, asked for until revoked.
    let (poll, asked) = ask_for_key(&server, &json!({"name": "Visa backend"})).await;
    assert!(asked.server_key);
    assert_eq!(
        (asked.scope, asked.expires_ms),
        (AgentScope::AllWorkspaces, None)
    );
    assert!(
        asked.browser_pages,
        "a key made with no answer operates web pages in Browser"
    );
    assert!(
        answer(&server, &alice.allow(&asked))
            .await
            .status()
            .is_success()
    );
    let (status, polled) = poll_key(&server, &poll).await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    assert!(polled["expires_at_ms"].is_null(), "{polled}");
    let token = polled["key"].as_str().unwrap().to_owned();
    assert!(token.starts_with("tilo_sk_"));
    // Shown once: the poll answers it no more.
    assert_eq!(poll_key(&server, &poll).await.0, StatusCode::NOT_FOUND);

    // Still a key long past the bound of an agent's grant.
    let later = now + 400 * DAY_MS;
    let grant = server
        .store
        .access(&token, later)
        .expect("a server key lasts");
    assert_eq!(grant.last_used_ms, Some(later));
    let device = hex::encode(alice.device.verifying_key().to_bytes());
    let approved_on = format!("Laptop ({})", device.get(..8).unwrap());
    assert_eq!(grant.created_by.as_deref(), Some(approved_on.as_str()));
    let row = account::grant_row(&server.store.grants_of(&alice.id())[0]);
    assert_eq!(row["last_used_at_ms"], later);
    assert_eq!(row["created_by"], approved_on.as_str());
    assert!(row["expires_at_ms"].is_null());
    let page = pages::grants(&server.store.grants_of(&alice.id()), "csrf");
    assert!(
        page.contains(&format!("Last used {}", pages::date(later))),
        "{page}"
    );
    assert!(page.contains("approved on Laptop"), "{page}");
    assert!(page.contains("lasts until revoked"), "{page}");
    // What a person reads of the scope: on the key's row, and the box the
    // next key is made with.
    assert!(page.contains("operates web pages in Browser"), "{page}");
    assert!(page.contains("name=\"browser_pages\""), "{page}");
    assert!(
        page.contains("The key opens, fills in and clicks the page without asking you"),
        "{page}"
    );
    assert!(
        pages::key_created(&grant, "tilo_sk_secret")
            .contains("It operates web pages in Browser; submitting a page still asks you."),
        "the key shown once says what it may do"
    );
    assert!(page.contains(&format!("action=\"/grants/{}/revoke\"", grant.id)));
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
    assert!(
        answer(&server, &alice.allow(&asked))
            .await
            .status()
            .is_success()
    );
    let token = poll_key(&server, &poll).await.1["key"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(server.store.access(&token, expires - 1).is_ok());
    assert!(matches!(
        server.store.access(&token, expires),
        Err(store::Denied::Expired { at_ms: Some(at) }) if at == expires
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_key_reaching_all_workspaces_lasts_and_only_the_key_flow_makes_one() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    let alice = Person::new(1);
    let server = server();
    let now = tilo_clock::unix_now_ms();

    // A key for one workspace is no server key and stays bounded.
    let studio = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
    let (_, asked) = ask_for_key(&server, &json!({"name": "Jane", "workspace": studio})).await;
    assert!(!asked.server_key);
    assert!(asked.expires_ms.unwrap() <= now + 90 * DAY_MS);
    for body in [
        json!({"name": "Jane", "workspace": studio, "expires_in": 100 * 24 * 60 * 60}),
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

#[test]
fn a_date_reads_as_its_utc_day_and_minute() {
    assert_eq!(pages::date(0), "1970-01-01 00:00 UTC");
    assert_eq!(pages::date(1_762_696_980_000), "2025-11-09 14:03 UTC");
    assert_eq!(pages::date(951_782_400_000), "2000-02-29 00:00 UTC");
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

/// A site on loopback whose every page answers `status`, counting asks.
async fn site_answering(status: &'static str) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let reply =
                format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (origin, asked)
}

fn server_in_front_of(site: &str) -> Arc<Server> {
    Server::new(
        &Config {
            issuer: ISSUER.into(),
            web_entry: site.into(),
            relay: "https://relay.tilo.test".into(),
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

/// A site deployed without the sign-in page every approval lands on shows
/// on the server's health, and the server asks the site again only after a
/// while, however often it is probed.
#[tokio::test]
async fn health_says_when_the_site_serves_no_sign_in_page() {
    let (missing, asked) = site_answering("404 Not Found").await;
    let api = server_in_front_of(&missing);
    let answered = health(&api).await;
    assert_eq!(answered["ok"], true);
    assert_eq!(
        answered["sign_in"],
        format!("{missing}/sign-in answers 404 Not Found")
    );
    health(&api).await;
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "probes within a minute reuse what the site answered"
    );

    let (served, _) = site_answering("200 OK").await;
    assert_eq!(
        health(&server_in_front_of(&served)).await["sign_in"],
        "served"
    );
}

/// A relay busy with the person's changes ran nothing: the server answers it
/// typed as `rate_limited`, with the wait the relay said, never as a refusal
/// the caller must act on.
#[test]
fn a_busy_relay_answers_rate_limited_with_its_wait() {
    let busy = tilo_actions::remote::ActionHostError::Unavailable {
        reason: format!(
            "{}: {}",
            tilo_relay_protocol::refusal::BACKPRESSURE,
            tilo_relay_protocol::refusal::too_many_changes(std::time::Duration::from_millis(480))
        ),
    };
    let relay = relay::RelayError::from(tilo_session_client::device::HostRefusal::of(&busy));
    assert_eq!(
        relay,
        relay::RelayError::RateLimited {
            retry_after_ms: 480
        }
    );
    let answered = account::answer(true, "account/workspaces#create", account::refused(&relay));
    let error = &answered["structuredContent"]["error"];
    assert_eq!(error["code"], "rate_limited", "{answered}");
    assert_eq!(error["retry_after_ms"], 480, "{answered}");
    assert_eq!(answered["isError"], true);
}

/// Where the site keeps no invitation, the link carries the invitation's
/// text after a route that names the relay, as the site's own workspace
/// links do, so following it reaches the workspace's host.
#[tokio::test]
async fn an_invitation_the_site_keeps_not_names_the_relay_in_its_link() {
    let (site, _) = site_answering("404 Not Found").await;
    let server = server_in_front_of(&site);
    let workspace = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";
    let link = server.invitation_link(workspace, b"signed", u64::MAX).await;
    assert!(
        link.starts_with(&format!("{site}/#/@relay.tilo.test/w/{workspace}?invite=")),
        "{link}"
    );
}
