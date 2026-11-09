//! The door end to end, in process: an agent discovers how to authorize,
//! registers, sends the person to their Tilo (which these tests play: it
//! signs the challenge with a device of the person and certifies the
//! grant's key with their root), redeems the code with its PKCE verifier,
//! and acts in workspaces served by a real `tilo-runtime` HTTP MCP endpoint
//! on loopback, which checks the grant's key itself. The directory here is
//! in memory; `hosts_tests` runs the same flow against a real host.

use super::*;
use directory::{BoxFuture, DirectoryError, Host, Route, Workspace};
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_actions::remote::Caller;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, RevocationList, SignInProof};

pub(crate) const ISSUER: &str = "https://door.test";
pub(crate) const WEB_ENTRY: &str = "https://tilo.test";
pub(crate) const REDIRECT: &str = "http://127.0.0.1:33418/callback";
pub(crate) const VERIFIER: &str = "a-verifier-of-at-least-forty-three-characters-0123456789";

pub(crate) use crate::runtime_support::{Runtime, runtime};

/// One workspace as the tests set it up: (person, workspace).
type HostedWorkspace = (String, Workspace);

/// A host's workspaces as the tests set them up, and each person's runtime.
#[derive(Default)]
struct Hosted {
    workspaces: Mutex<Vec<HostedWorkspace>>,
    runtimes: tokio::sync::Mutex<HashMap<String, Arc<Runtime>>>,
    revoked: Mutex<RevocationList>,
    /// Workspaces the host may still make before it refuses (a quota).
    quota: AtomicUsize,
}

impl Hosted {
    async fn runtime_of(&self, person: &str) -> Arc<Runtime> {
        let mut runtimes = self.runtimes.lock().await;
        if let Some(found) = runtimes.get(person) {
            return Arc::clone(found);
        }
        let root: [u8; 32] = hex::decode(person).unwrap().try_into().unwrap();
        let made = Arc::new(runtime(root).await);
        runtimes.insert(person.to_owned(), Arc::clone(&made));
        made
    }

    async fn add(&self, person: &str, name: &str) -> Workspace {
        self.runtime_of(person).await;
        let workspace = Workspace {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
            host: "eu1.tilo.test".to_owned(),
        };
        lock(&self.workspaces).push((person.to_owned(), workspace.clone()));
        workspace
    }

    /// What the person's runtime was asked, by the workspace each caller
    /// acted in.
    async fn callers_of(&self, person: &str) -> Vec<Caller> {
        self.runtime_of(person).await.host.callers()
    }
}

impl Directory for Hosted {
    fn workspaces<'a>(
        &'a self,
        grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<Vec<Workspace>, DirectoryError>> {
        Box::pin(async move {
            let person = grant.person();
            Ok(lock(&self.workspaces)
                .iter()
                .filter(|(owner, workspace)| {
                    *owner == person && grant.scope().reaches(&workspace.id)
                })
                .map(|(_, workspace)| workspace.clone())
                .collect())
        })
    }

    fn hosts<'a>(
        &'a self,
        _grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<Vec<Host>, DirectoryError>> {
        Box::pin(async {
            Ok(vec![Host {
                name: "eu1.tilo.test".into(),
                default: true,
            }])
        })
    }

    fn create<'a>(
        &'a self,
        grant: &'a GrantKey,
        name: &'a str,
        _host: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Workspace, DirectoryError>> {
        Box::pin(async move {
            if !grant.scope().makes_workspaces() {
                return Err(DirectoryError::Refused("not this grant's to make".into()));
            }
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
            Ok(self.add(&grant.person(), name).await)
        })
    }

    fn revocations<'a>(
        &'a self,
        _grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<RevocationList, DirectoryError>> {
        Box::pin(async move { Ok(lock(&self.revoked).clone()) })
    }

    fn route<'a>(
        &'a self,
        grant: &'a GrantKey,
        workspace: &'a str,
    ) -> BoxFuture<'a, Result<Route, DirectoryError>> {
        Box::pin(async move {
            let person = grant.person();
            let has = lock(&self.workspaces).iter().any(|(owner, candidate)| {
                *owner == person && candidate.id == workspace && grant.scope().reaches(workspace)
            });
            if !has {
                return Err(DirectoryError::NotFound);
            }
            Ok(self.runtime_of(&person).await.route())
        })
    }
}

/// How a test's stand-in for the person's Tilo certifies a grant's key.
type Certify<'a> = Box<dyn Fn(&Asked) -> DeviceCert + 'a>;

/// A person: their root, and a device it certified (the page of their Tilo
/// on the web, which answers the door).
pub(crate) struct Person {
    pub root: ed25519_dalek::SigningKey,
    pub device: ed25519_dalek::SigningKey,
    pub cert: DeviceCert,
}

/// What the door asked the person's Tilo, from its sign-in link.
pub(crate) struct Asked {
    pub pending: String,
    pub challenge: [u8; 32],
    pub key: [u8; 32],
    pub scope: AgentScope,
    pub expires_ms: u64,
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
            expires_ms: asked["grant_expires"].parse().unwrap(),
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

    /// The person's Tilo certifying the grant's key as asked, but expiring
    /// at `expires_ms`.
    pub(crate) fn certify(&self, asked: &Asked, scope: AgentScope, expires_ms: u64) -> DeviceCert {
        let now = tilo_clock::unix_now_ms();
        let mut cert = DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: asked.key,
            device_label: format!("{} via door.test", asked.client),
            capabilities: DeviceCapabilities::agent_grant(scope),
            issued_at_ns: now.saturating_sub(1_000).saturating_mul(1_000_000),
            expires_at_ns: Some(expires_ms.saturating_mul(1_000_000)),
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
pub(crate) async fn register(door: &Arc<Door>) -> String {
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

/// Ask the door to authorize `client` for `resource`: where it sends the
/// person's browser.
pub(crate) async fn authorize(door: &Arc<Door>, client: &str, resource: &str) -> Asked {
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
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    Asked::of(&location(&reply))
}

/// Post the person's Tilo's answer `form` back to the door.
pub(crate) async fn answer(door: &Arc<Door>, form: &str) -> Reply {
    door.dispatch(
        &Method::POST,
        "/sign-in",
        "",
        &hyper::HeaderMap::new(),
        form.as_bytes(),
    )
    .await
}

/// Where the door sent the browser back to the agent, and what it said.
pub(crate) fn returned(reply: &Reply) -> HashMap<String, String> {
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let back = location(reply);
    assert!(back.starts_with(REDIRECT), "{back}");
    let answered = query_of(&back);
    assert_eq!(answered["state"], "xyz");
    answered
}

/// Run the browser part: authorize, and the person allows it in their Tilo.
/// Answers the code and the browser's cookie at the door.
pub(crate) async fn approve(
    door: &Arc<Door>,
    client: &str,
    person: &Person,
    resource: &str,
) -> (String, String) {
    let asked = authorize(door, client, resource).await;
    assert_eq!(asked.client, "Claude Code");
    let reply = answer(door, &person.allow(&asked)).await;
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

pub(crate) async fn redeem(door: &Arc<Door>, client: &str, code: &str, verifier: &str) -> Reply {
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

/// The whole flow: an access token for `resource`.
pub(crate) async fn connect(door: &Arc<Door>, person: &Person, resource: &str) -> String {
    let client = register(door).await;
    let (code, _) = approve(door, &client, person, resource).await;
    let reply = redeem(door, &client, &code, VERIFIER).await;
    assert_eq!(reply.status(), StatusCode::OK);
    json_body(reply).await["access_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// An MCP request with `token`, on `path`, in `session`.
pub(crate) async fn mcp(
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

pub(crate) async fn initialize(door: &Arc<Door>, path: &str, token: &str) -> (String, Value) {
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

pub(crate) async fn call(
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

/// [`call`], asking for the typed answer as the SDK does.
pub(crate) async fn call_typed(
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
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
            "name":name,"arguments":arguments,"_meta":{"so.tilo.mcp":{"structured":true}}}}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let text = String::from_utf8(body(reply).await).unwrap();
    let last = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .unwrap_or(&text);
    serde_json::from_str(last).unwrap()
}

pub(crate) async fn tool_names(
    door: &Arc<Door>,
    path: &str,
    token: &str,
    session: &str,
) -> Vec<String> {
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

/// Whether any of `callers` acted in `workspace`.
fn acted_in(callers: &[Caller], workspace: &str) -> bool {
    callers
        .iter()
        .any(|caller| caller.workspace() == Some(workspace))
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
    let (code, _) = approve(&door, &client, &alice, ISSUER).await;
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
    let (code, _) = approve(&door, &client, &alice, ISSUER).await;
    let other = register(&door).await;
    assert_eq!(
        json_body(redeem(&door, &other, &code, VERIFIER).await).await["error"],
        "invalid_grant"
    );
    let (code, _) = approve(&door, &client, &alice, ISSUER).await;
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
    let asked = authorize(&door, &client, ISSUER).await;
    let reply = answer(&door, &alice.allow(&asked)).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert!(reply.headers().get(header::SET_COOKIE).is_none());
    assert!(door.store.grants_of(&alice.id()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_grant_is_exactly_what_the_door_asked_the_person_to_certify() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let mallory = Person::new(3);
    let door = door(&hosted);
    let client = register(&door).await;

    // The person declines in their Tilo: the agent hears so.
    let asked = authorize(&door, &client, ISSUER).await;
    let declined = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("pending", &asked.pending)
        .append_pair("decision", "deny")
        .finish();
    assert_eq!(
        returned(&answer(&door, &declined).await)["error"],
        "access_denied"
    );
    // Answered once: the same authorization takes no second answer.
    let reply = answer(&door, &alice.allow(&asked)).await;
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
        let asked = authorize(&door, &client, ISSUER).await;
        let reply = answer(
            &door,
            &answer_form(&asked, &proof(&asked), &certify(&asked)),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN, "{why}");
    }
    // A proof for another challenge signs nobody in.
    let asked = authorize(&door, &client, ISSUER).await;
    let stale = SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, [9; 32]).unwrap();
    let cert = alice.certify(&asked, asked.scope, asked.expires_ms);
    let reply = answer(&door, &answer_form(&asked, &stale, &cert)).await;
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert!(door.store.grants_of(&alice.id()).is_empty());
    assert!(door.store.grants_of(&mallory.id()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_may_narrow_a_grant_to_one_workspace_and_never_widen_it() {
    // The person's Tilo takes a code out of a grant it certified, and the
    // grant the door then holds.
    async fn granted(
        door: &Arc<Door>,
        client: &str,
        person: &Person,
        asked: Asked,
        cert: DeviceCert,
    ) -> store::Grant {
        let proof = SignInProof::sign(person.cert.clone(), &person.device, ISSUER, asked.challenge)
            .unwrap();
        let reply = answer(door, &answer_form(&asked, &proof, &cert)).await;
        let code = returned(&reply)["code"].clone();
        json_body(redeem(door, client, &code, VERIFIER).await).await;
        door.store.grants_of(&person.id())[0].clone()
    }

    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let garden = hosted.add(&alice.id(), "Garden").await;
    let door = door(&hosted);
    let client = register(&door).await;
    let proof = |asked: &Asked| {
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap()
    };
    // The agent asked for all of them and the person chose one: the grant is
    // that workspace's, not the agent's request.
    let asked = authorize(&door, &client, ISSUER).await;
    assert_eq!(asked.scope, AgentScope::AllWorkspaces);
    let narrowed = AgentScope::parse(&studio.id).unwrap();
    let cert = alice.certify(&asked, narrowed, asked.expires_ms);
    let grant = granted(&door, &client, &alice, asked, cert).await;
    assert_eq!(grant.scope, store::Scope::Workspace(studio.id.clone()));
    assert_eq!(grant.key().unwrap().scope(), narrowed);

    // A workspace of the person's own that the host does not hold is not one
    // a grant may be narrowed to.
    let asked = authorize(&door, &client, ISSUER).await;
    let nowhere = AgentScope::parse("0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa").unwrap();
    let cert = alice.certify(&asked, nowhere, asked.expires_ms);
    let reply = answer(&door, &answer_form(&asked, &proof(&asked), &cert)).await;
    assert_eq!(returned(&reply)["error"], "invalid_target");
    assert_eq!(door.store.grants_of(&alice.id()).len(), 1);

    // Asked for one workspace, the door takes that one and nothing else: not
    // all of them, and not another.
    let pinned = format!("{ISSUER}/w/{}", garden.id);
    for (why, scope) in [
        ("all of them", AgentScope::AllWorkspaces),
        ("another workspace", AgentScope::parse(&studio.id).unwrap()),
    ] {
        let asked = authorize(&door, &client, &pinned).await;
        assert_eq!(asked.scope, AgentScope::parse(&garden.id).unwrap());
        let cert = alice.certify(&asked, scope, asked.expires_ms);
        let reply = answer(&door, &answer_form(&asked, &proof(&asked), &cert)).await;
        assert_eq!(reply.status(), StatusCode::FORBIDDEN, "{why}");
    }
    let asked = authorize(&door, &client, &pinned).await;
    let cert = alice.certify(&asked, asked.scope, asked.expires_ms);
    let grant = granted(&door, &client, &alice, asked, cert).await;
    assert_eq!(grant.scope, store::Scope::Workspace(garden.id));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_past_its_certificate_issues_no_token() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    hosted.add(&alice.id(), "Studio").await;
    let door = door(&hosted);
    let client = register(&door).await;
    let asked = authorize(&door, &client, ISSUER).await;
    let proof =
        SignInProof::sign(alice.cert.clone(), &alice.device, ISSUER, asked.challenge).unwrap();
    let soon = tilo_clock::unix_now_ms() + 1_500;
    let cert = alice.certify(&asked, asked.scope, soon);
    let code = returned(&answer(&door, &answer_form(&asked, &proof, &cert)).await)["code"].clone();
    let tokens = json_body(redeem(&door, &client, &code, VERIFIER).await).await;
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let (session, _) = initialize(&door, "/", &access).await;
    tokio::time::sleep(std::time::Duration::from_millis(1_600)).await;
    let reply = mcp(
        &door,
        "/",
        &access,
        Some(&session),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
    let refresh = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", tokens["refresh_token"].as_str().unwrap())
        .append_pair("client_id", &client)
        .finish();
    let renewed = json_body(
        door.dispatch(
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
async fn a_pinned_session_never_reaches_another_workspace() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let garden = hosted.add(&alice.id(), "Garden").await;
    let door = door(&hosted);
    let pinned_path = format!("/w/{}", studio.id);
    let token = connect(&door, &alice, &format!("{ISSUER}{pinned_path}")).await;

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
    // The account is not reached from one workspace.
    let refused = call(
        &door,
        &pinned_path,
        &token,
        &session,
        "tilo_do",
        json!({"path": "account/workspaces#list"}),
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
    assert!(acted_in(&hosted.callers_of(&alice.id()).await, &studio.id));
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
    assert!(!acted_in(&hosted.callers_of(&alice.id()).await, &garden.id));
    // The same grant at the door's root is still pinned.
    let (root_session, _) = initialize(&door, "/", &token).await;
    let tools = tool_names(&door, "/", &token, &root_session).await;
    assert!(
        tools.iter().any(|tool| tool == "tilo_list_apps"),
        "{tools:?}"
    );
    assert!(!acted_in(&hosted.callers_of(&alice.id()).await, &garden.id));
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one session's story, from no workspace to acting in the one it made"
)]
async fn an_account_session_creates_a_workspace_and_acts_in_it() {
    let hosted = Arc::new(Hosted::default());
    hosted.quota.store(1, Ordering::SeqCst);
    let alice = Person::new(1);
    let bob = Person::new(2);
    let bobs = hosted.add(&bob.id(), "Bob's").await;
    let door = door(&hosted);
    let token = connect(&door, &alice, ISSUER).await;
    let (session, answer) = initialize(&door, "/", &token).await;
    assert_eq!(
        answer["result"]["capabilities"]["tools"]["listChanged"],
        true
    );

    // No workspace yet: only the tools the account answers.
    let tools = tool_names(&door, "/", &token, &session).await;
    assert_eq!(tools, ["tilo_ls", "tilo_read", "tilo_do", "tilo_status"]);
    let listed = call_typed(
        &door,
        "/",
        &token,
        &session,
        "tilo_do",
        json!({"path": "account/workspaces#list"}),
    )
    .await;
    assert_eq!(
        listed["result"]["structuredContent"]["result"]["workspaces"],
        json!([])
    );
    let ls = call(
        &door,
        "/",
        &token,
        &session,
        "tilo_ls",
        json!({"path": "account"}),
    )
    .await;
    assert!(
        ls["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("#mint(workspace, name?, expires_in?)"),
        "{ls}"
    );

    let created = call_typed(
        &door,
        "/",
        &token,
        &session,
        "tilo_do",
        json!({"path": "account/workspaces#create", "args": {"name": "Studio"}}),
    )
    .await;
    assert_eq!(created["result"]["isError"], false, "{created}");
    let studio = created["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The host refuses past its quota, and says why.
    let refused = call(
        &door,
        "/",
        &token,
        &session,
        "tilo_do",
        json!({"path": "account/workspaces#create", "args": {"name": "More"}}),
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
    assert!(ls["inputSchema"]["properties"][session::WORKSPACE_ARGUMENT].is_object());

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
    assert!(acted_in(&hosted.callers_of(&alice.id()).await, &studio));

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
    assert!(hosted.callers_of(&bob.id()).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn each_session_is_its_own_numbered_run_and_revoking_ends_them() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let door = door(&hosted);
    let token = connect(&door, &alice, &format!("{ISSUER}/w/{}", studio.id)).await;
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
    assert!(acted_in(&hosted.callers_of(&alice.id()).await, &studio.id));

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

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_refuses_a_grant_its_person_revoked() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let door = door(&hosted);
    let path = format!("/w/{}", studio.id);
    let token = connect(&door, &alice, &format!("{ISSUER}{path}")).await;
    let (session, _) = initialize(&door, &path, &token).await;
    let listed = call(&door, &path, &token, &session, "tilo_list_apps", json!({})).await;
    assert!(listed.get("result").is_some(), "{listed}");

    // Alice removes the grant's key from her devices, and her runtime hears
    // of it as it hears of any revocation of hers: it drops the session and
    // opens no other for that key.
    let key = door.store.grants_of(&alice.id())[0].key().unwrap();
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: alice.root.verifying_key().to_bytes(),
        revoked_device_public_key: key.cert().device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &alice.root).unwrap();
    let runtime = hosted.runtime_of(&alice.id()).await;
    lock(&runtime.revoked).insert(entry).unwrap();
    let refused = call(&door, &path, &token, &session, "tilo_list_apps", json!({})).await;
    assert!(refused["error"].is_object(), "{refused}");
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

/// A key asked for from a terminal, approved by the person, collected by
/// polling: then it mints a key for one workspace, which reaches that one
/// alone and ends when the key that minted it is revoked.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one key's story, from the terminal asking to its minted key ending"
)]
async fn a_key_mints_a_key_for_one_workspace_that_ends_with_it() {
    let hosted = Arc::new(Hosted::default());
    let alice = Person::new(1);
    let studio = hosted.add(&alice.id(), "Studio").await;
    let garden = hosted.add(&alice.id(), "Garden").await;
    let door = door(&hosted);
    let post = |path: &'static str, body: Value| {
        let door = Arc::clone(&door);
        async move {
            door.dispatch(
                &Method::POST,
                path,
                "",
                &hyper::HeaderMap::new(),
                body.to_string().as_bytes(),
            )
            .await
        }
    };

    // The terminal asks; nothing is there to collect until the person answers.
    let asked = post(keys::NEW_PATH, json!({"name": "Visa backend"})).await;
    assert_eq!(asked.status(), StatusCode::CREATED);
    let asked = json_body(asked).await;
    let poll = asked["poll"].as_str().unwrap().to_owned();
    let waiting = post(keys::POLL_PATH, json!({"poll": poll})).await;
    assert_eq!(waiting.status(), StatusCode::ACCEPTED);

    // The link sends the person to their Tilo, which approves the key.
    let approve = asked["approve_url"].as_str().unwrap();
    let path = approve.strip_prefix(ISSUER).unwrap();
    let reply = door
        .dispatch(&Method::GET, path, "", &hyper::HeaderMap::new(), b"")
        .await;
    assert_eq!(reply.status(), StatusCode::SEE_OTHER);
    let sign_in = Asked::of(&location(&reply));
    assert_eq!(sign_in.scope, AgentScope::AllWorkspaces);
    assert!(
        sign_in.client.contains("Visa backend"),
        "{}",
        sign_in.client
    );
    let page = answer(&door, &alice.allow(&sign_in)).await;
    assert_eq!(page.status(), StatusCode::OK);
    let page = String::from_utf8(body(page).await).unwrap();
    assert!(
        !page.contains("tilo_sk_"),
        "the browser never sees a key it did not ask for"
    );

    // Collected once.
    let collected = post(keys::POLL_PATH, json!({"poll": poll})).await;
    assert_eq!(collected.status(), StatusCode::OK);
    let collected = json_body(collected).await;
    let key = collected["key"].as_str().unwrap().to_owned();
    let key_grant = collected["grant"].as_str().unwrap().to_owned();
    assert!(key.starts_with("tilo_sk_"));
    assert_eq!(collected["scope"], "all");
    let again = post(keys::POLL_PATH, json!({"poll": poll})).await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);

    // The key reaches the account: it mints a key for the studio alone.
    let (session, answer) = initialize(&door, "/", &key).await;
    assert!(
        answer["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("account/keys#mint"),
        "{answer}"
    );
    let minted = call_typed(
        &door,
        "/",
        &key,
        &session,
        "tilo_do",
        json!({"path": "account/keys#mint", "args": {"workspace": studio.id, "name": "Jane Doe's agent"}}),
    )
    .await;
    let minted = &minted["result"]["structuredContent"]["result"];
    let token = minted["token"].as_str().unwrap().to_owned();
    let minted_grant = minted["grant"].as_str().unwrap().to_owned();
    assert_eq!(minted["mcp_url"], format!("{ISSUER}/w/{}/mcp", studio.id));
    assert!(
        minted["expires_at_ms"].as_u64().unwrap()
            <= door.store.grant(&key_grant).unwrap().expires_ms
    );
    let listed = call(
        &door,
        "/",
        &key,
        &session,
        "tilo_read",
        json!({"path": "account/keys"}),
    )
    .await;
    assert!(
        listed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Jane Doe's agent"),
        "{listed}"
    );

    // The minted key acts in the studio: the runtime takes its certificate,
    // which the key's own key signed.
    let studio_path = format!("/w/{}/mcp", studio.id);
    let (pinned, _) = initialize(&door, &studio_path, &token).await;
    let acted = call(
        &door,
        &studio_path,
        &token,
        &pinned,
        "tilo_list_apps",
        json!({}),
    )
    .await;
    assert!(acted.get("result").is_some(), "{acted}");
    assert!(acted_in(&hosted.callers_of(&alice.id()).await, &studio.id));
    // Nowhere else, and not the account.
    let garden_path = format!("/w/{}/mcp", garden.id);
    let elsewhere = mcp(
        &door,
        &garden_path,
        &token,
        None,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await;
    assert_eq!(elsewhere.status(), StatusCode::FORBIDDEN);
    let account = call(
        &door,
        &studio_path,
        &token,
        &pinned,
        "tilo_do",
        json!({"path": "account/keys#mint", "args": {"workspace": garden.id}}),
    )
    .await;
    assert!(
        account.get("error").is_some() || account["result"]["isError"] == true,
        "{account}"
    );
    assert!(!acted_in(&hosted.callers_of(&alice.id()).await, &garden.id));
    assert!(door.store.minted_by(&minted_grant).is_empty());

    // Revoking the key ends what it minted: its token and its session.
    assert!(door.revoke_grant(&alice.id(), &key_grant).await.unwrap());
    assert!(door.store.grant(&minted_grant).is_none());
    let ended = mcp(
        &door,
        &studio_path,
        &token,
        Some(&pinned),
        json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}),
    )
    .await;
    assert_eq!(ended.status(), StatusCode::UNAUTHORIZED);
}
