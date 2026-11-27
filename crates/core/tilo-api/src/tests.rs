//! The server's own flows in process: an agent discovers how to authorize,
//! registers, sends the person to their Tilo (which these tests play: it
//! signs the challenge with a device of the person and certifies the
//! grant's key with their root), and redeems the code with its PKCE
//! verifier. What a grant then does in workspaces is the server's, which
//! `tilo-agent-session`'s `api` test drives through the server.

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
    assert_eq!(asked.scope.clone(), AgentScope::AllWorkspaces);
    let narrowed = AgentScope::parse(studio).unwrap();
    let cert = alice.certify(&asked, narrowed.clone(), asked.expires_ms);
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
        assert_eq!(asked.scope.clone(), AgentScope::parse(garden).unwrap());
        let cert = alice.certify(&asked, scope, asked.expires_ms);
        let reply = answer(&server, &answer_form(&asked, &proof(&asked), &cert)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN, "{why}");
    }
    let asked = authorize(&server, &client, &pinned).await;
    let cert = alice.certify(&asked, asked.scope.clone(), asked.expires_ms);
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

/// Awaiting a request costs its caller a pointer, not the request's whole
/// work: a script of dozens of requests in one function otherwise holds every
/// one of them, and in a debug build overflows its thread's stack.
#[test]
fn a_request_is_a_small_future_for_whoever_awaits_it() {
    let server = server();
    let headers = headers(&[]);
    let request = server.dispatch(&Method::GET, "/health", "", &headers, b"");
    let size = std::mem::size_of_val(&request);
    assert!(size <= 256, "a request's future is {size} bytes");
}

/// The grants page says, beside the keys, which of them ask before each
/// command in a workspace's machine: it keeps no workspace's setting itself.
#[test]
fn the_grants_page_says_which_keys_ask_before_each_command() {
    let page = pages::grants(&[], &HashMap::new(), "csrf");
    assert!(
        page.contains("unless you let that workspace's own keys run commands unattended")
            && page.contains("a key for all your workspaces always asks"),
        "{page}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one key's life read from minting to revocation"
)]
async fn a_server_key_lasts_until_revoked_unless_its_person_chose_an_expiry() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    let alice = Person::new(1);
    let server = server();
    let now = tilo_clock::unix_now_ms();

    // A key reaching all workspaces is a server key, asked for until revoked.
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
    // The device the person approved it on, by the name they know it by,
    // never a key's bytes.
    assert_eq!(grant.created_by.as_deref(), Some("Laptop"));
    let row = account::grant_row(&server.store.grants_of(&alice.id())[0]);
    // Shown to the minute.
    assert_eq!(
        row["last_used_at_ms"],
        later.saturating_sub(later % crate::store::LAST_USED_SAVE_MS)
    );
    assert_eq!(row["created_by"], "Laptop");
    assert!(row["expires_at_ms"].is_null());
    let page = pages::grants(
        &server.store.grants_of(&alice.id()),
        &HashMap::new(),
        "csrf",
    );
    assert!(
        page.contains(&format!("Last used {}", pages::date(later))),
        "{page}"
    );
    // Each fact of the row is its own item, never joined into one line.
    assert!(page.contains("<li>Approved on Laptop</li>"), "{page}");
    assert!(page.contains("<li>Lasts until revoked</li>"), "{page}");
    assert!(!page.contains('\u{b7}'), "{page}");
    // What a person reads of the scope: on the key's row, and the box the
    // next key is made with.
    assert!(
        page.contains("<li>Operates web pages in Browser</li>"),
        "{page}"
    );
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

    // A key for one workspace is the person's own, a server key, and stays
    // bounded.
    let studio = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
    let (_, asked) = ask_for_key(&server, &json!({"name": "Jane", "workspace": studio})).await;
    assert!(asked.server_key);
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

/// A terminal asking for a key that reaches one workspace: the person's Tilo
/// is asked to certify exactly that workspace, as their own server key, and
/// what it approves is the key the terminal collects.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_a_terminal_asks_for_holds_the_person_to_the_one_workspace() {
    let alice = Person::new(4);
    let server = server();
    let studio = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
    let (poll, asked) =
        ask_for_key(&server, &json!({"name": "Nightly", "workspace": studio})).await;
    assert_eq!(
        asked.scope.clone(),
        AgentScope::parse(studio).unwrap(),
        "the link names the workspace the key reaches"
    );
    assert!(asked.server_key, "a key the person makes is a server key");
    assert!(asked.expires_ms.is_some(), "and ends within a grant's life");
    assert!(
        answer(&server, &alice.allow(&asked))
            .await
            .status()
            .is_success()
    );
    let (status, key) = poll_key(&server, &poll).await;
    assert_eq!(status, StatusCode::OK);
    let token = key["key"].as_str().unwrap();
    let grant = server
        .store
        .access(token, tilo_clock::unix_now_ms())
        .expect("the key reaches the person's workspaces");
    assert_eq!(grant.scope, store::Scope::Workspace(studio.to_owned()));
}

#[test]
fn a_key_for_one_workspace_is_listed_by_that_workspace_s_name() {
    let grant = || -> store::Grant {
        serde_json::from_value(json!({
            "id": "grant-1",
            "person": "aa",
            "client_id": "",
            "client_name": "Jane",
            "scope": {"workspace": "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21"},
            "kind": "key",
            "created_ms": 1,
        }))
        .unwrap()
    };
    let unnamed = pages::grants(&[grant()], &HashMap::new(), "csrf");
    assert!(unnamed.contains("<li>One workspace</li>"), "{unnamed}");
    let applicant = HashMap::from([("grant-1".to_owned(), "Applicant: Jane Doe".to_owned())]);
    let named = pages::grants(&[grant()], &applicant, "csrf");
    assert!(named.contains("<li>Applicant: Jane Doe</li>"), "{named}");
    assert!(!named.contains("One workspace"), "{named}");
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
        session::agent_session_label("Claude Code", &params, 1),
        "Claude Code #1"
    );
    assert_eq!(
        session::agent_session_label("MCP agent", &params, 3),
        "claude-code #3"
    );
    assert_eq!(session::agent_session_label("", &json!({}), 2), "Agent #2");
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

/// A site deployed without the sign-in page every approval lands on shows
/// on the server's health, and the next probe asks the site again: only a
/// served page is remembered.
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
        2,
        "a page the site does not serve is asked for again"
    );

    let (served, _) = site_answering("200 OK").await;
    assert_eq!(
        health(&server_in_front_of(&served)).await["sign_in"],
        "served"
    );
}

/// The API starts before the site, as `stack.sh up` starts it: the probe that
/// found nothing there is not kept, so the first health check after the site
/// is up finds its sign-in page.
#[tokio::test]
async fn a_site_that_starts_after_the_api_is_seen_served_at_once() {
    // The address the site will answer on: taken to know it, then given back
    // for it to listen on.
    let port = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let api = server_in_front_of(&format!("http://127.0.0.1:{port}"));
    let answered = health(&api).await;
    assert_ne!(answered["sign_in"], "served", "nothing answers yet");

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    serve_answering(listener, "200 OK", Arc::clone(&asked));
    assert_eq!(health(&api).await["sign_in"], "served");
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// A workspace operation the server refused is read by its case, never by its
/// words: one that is not the person's own is not theirs, and a branch that
/// could not take everything names the part that did not come.
#[test]
fn a_workspace_refusal_is_read_by_its_case() {
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
}

/// A person the server does not know yet (no owner its operator declared, no invitation that
/// brought them) reads that from the API, typed, with their key and what makes them known: not
/// that the action is not theirs.
#[test]
fn a_person_the_server_does_not_know_reads_why_and_what_to_do() {
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

/// A page's form may lead to the person's sign-in card on a local site
/// named under `localhost` (the local stack's `tilo.localhost`), which is
/// plain http: a form-action that left it out blocked the grants page's
/// new key at the redirect to that card.
#[test]
fn a_page_form_may_lead_to_a_local_site_under_localhost() {
    let page = html(StatusCode::OK, "<p>grants</p>");
    let policy = page
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let form_action = policy
        .split(';')
        .map(str::trim)
        .find_map(|directive| directive.strip_prefix("form-action "))
        .unwrap_or_default();
    assert!(
        form_action
            .split(' ')
            .any(|source| source == "http://*.localhost:*"),
        "{policy}"
    );
}

/// A person who opens the grants page without a session is sent to their own
/// Tilo for a sign-in that asks for no grant, and comes back signed in.
#[tokio::test]
async fn the_grants_page_signs_a_person_in_through_their_own_tilo() {
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
}

/// A form the server no longer knows (its page tokens live in memory, and a
/// restart forgets them) or one posted after the sign-in expired goes back to
/// the grants page to ask again, never to a dead end.
#[tokio::test]
async fn a_form_the_server_forgot_returns_the_person_to_their_grants_page() {
    let server = server();
    let alice = Person::new(4);
    let client = register(&server).await;
    let (_, cookie) = approve(&server, &client, &alice, &format!("{ISSUER}/mcp")).await;
    assert!(!cookie.is_empty());
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

/// A key is the grant the person approved, carried in the key itself: a
/// API server that never saw the approval accepts the same key, reads the
/// same grant, and revokes that one grant.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_carries_its_grant_to_every_api_server() {
    let alice = Person::new(1);
    let first = server();
    let (poll, asked) = ask_for_key(&first, &json!({"name": "Visa backend"})).await;
    assert!(
        answer(&first, &alice.allow(&asked))
            .await
            .status()
            .is_success()
    );
    let (status, polled) = poll_key(&first, &poll).await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    let token = polled["key"].as_str().unwrap().to_owned();

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
/// `seed`, as whoever holds it brings it.
fn key_of(person: &Person, seed: u8) -> GrantKey {
    let now = tilo_clock::unix_now_ms();
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: person.root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "anything".into(),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        issued_at_ns: now.saturating_sub(1_000).saturating_mul(1_000_000),
        expires_at_ns: Some(now.saturating_add(86_400_000).saturating_mul(1_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person.root).unwrap();
    GrantKey::held(key, cert, now.saturating_mul(1_000_000)).unwrap()
}

/// A key this server never issued is read from its token alone, and nothing
/// but the token vouches for it: a root anybody makes certifies a key
/// anybody makes. Reading one keeps nothing of it, neither the grant with
/// its secret nor a count for its person, however many are shown.
#[test]
fn a_key_read_from_its_token_alone_leaves_nothing_in_the_store() {
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
}

/// Once a server its key is for has let a person in, the key is kept, and
/// a person's keys kept that way are bounded: past the bound the one used
/// longest ago gives way, and is read from its token again when it is
/// shown. One that was revoked stays refused, and one that minted a key
/// stays for the key that reads as it.
#[test]
fn a_persons_keys_kept_without_being_issued_here_are_bounded() {
    let store = Store::in_memory();
    let ada = Person::new(9);
    let now = tilo_clock::unix_now_ms();
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

/// `tilo invocations` reads the workspace's invocations as their record: what the daemon's record
/// read answers for it is the resource the command pages, its invocations its fields' items.
#[tokio::test]
async fn the_workspaces_invocations_read_as_the_resource_the_cli_pages() {
    use tilo_action_calls::answered;
    use tilo_action_services::workspace_invocations::{Invocation, InvocationStatus, PROVIDER};
    use tilo_actions::remote::{
        Caller, ReadAnswer, ReadsAnswer, ResourceAddress, ResourceCopy, ResourceReadRequest,
        ResourceReads, ResourceWake,
    };
    /// The workspace's invocations, as the device's reader answers their record.
    struct Invocations(Value);
    impl ResourceReads for Invocations {
        fn answer_read(
            self: std::sync::Arc<Self>,
            caller: &Caller,
            correlation: u64,
            read: tilo_actions::remote::ResourceReadRequest,
        ) -> tilo_actions::remote::ReadingAnswer
        where
            Self: 'static,
        {
            Box::pin(tilo_action_calls::answer_read(
                self,
                caller.clone(),
                correlation,
                read,
            ))
        }

        fn read<'a>(
            &'a self,
            _: &'a Caller,
            address: &'a ResourceAddress,
        ) -> ReadsAnswer<'a, ResourceCopy> {
            assert_eq!(
                (address.app.as_str(), address.record.as_str()),
                (PROVIDER, "")
            );
            Box::pin(std::future::ready(Ok(ResourceCopy::reading(
                self.0.clone(),
                None,
            ))))
        }
        fn watch<'a>(
            &'a self,
            _: &'a Caller,
            _: &'a ResourceAddress,
            _: &'a ResourceWake,
        ) -> ReadsAnswer<'a, ()> {
            Box::pin(std::future::ready(Ok(())))
        }
    }
    let waiting = Invocation {
        id: Some("0f3c".into()),
        status: InvocationStatus::Waiting,
        action: Some("notes#rename".into()),
        ..Invocation::default()
    };
    let reads = Arc::new(Invocations(
        json!({"items": [waiting], "total": 1, "waiting": 1}),
    ));
    // Read as the agent tools' `tilo_read` of `record:tilo.invocations:` reads it.
    let read = ResourceReadRequest {
        workspace: "atlas".into(),
        app: PROVIDER.into(),
        record: String::new(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: ReadAnswer::Reading,
    };
    let answer = answered(reads, &Caller::local_person("jane"), read)
        .await
        .unwrap();
    let path = format!("record:{PROVIDER}:");
    let resource = crate::calls::read_answer(&path, answer).unwrap();
    assert_eq!(resource["path"], path.as_str());
    assert_eq!(resource["fields"]["items"][0]["id"], "0f3c");
    assert_eq!(resource["fields"]["items"][0]["status"], "waiting");
    assert_eq!(resource["fields"]["total"], 1);
}

/// What a difference between the page's clock and the server's does to an
/// approval.
mod sign_in_clock;
