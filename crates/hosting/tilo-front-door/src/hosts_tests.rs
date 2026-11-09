//! The door end to end on the real paths: a person's host (a real
//! `WorkspaceHost`) and runtime (a real `tilo-runtime` HTTP MCP endpoint),
//! the door's own HTTP routes, and the person's Tilo played by the test
//! with real keys: discovery, registration, sign-in with a `SignInProof` of
//! one of the person's devices, the grant's certificate, a workspace made
//! through the host's own operation as the grant, an action in it carried
//! by the runtime as the agent's run, and revocation.

use super::*;
use crate::tests::{
    ISSUER, Person, REDIRECT, VERIFIER, WEB_ENTRY, answer, authorize, json_body, location, redeem,
    register, returned, runtime,
};
use crate::{Config, Door, lock, oauth, session, store::Store};
use hyper::{Method, StatusCode, header};
use std::sync::Arc;
use tilo_room_host::host::{HostConfig, HostIdentity, HostMode, WorkspaceHost};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

struct Device {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

fn enrol(root: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Device {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 200,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    Device { key, cert }
}

/// The person's host, serving on a fresh endpoint until shut down.
struct Served {
    host: WorkspaceHost,
    router: iroh::protocol::Router,
    device: Device,
    _state: tempfile::TempDir,
}

async fn serve(person: &Person) -> Served {
    let device = enrol(&person.root, 11, "Home node");
    let state = tempfile::tempdir().unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let host = WorkspaceHost::open(
        HostConfig::new(state.path().join("host"), HostMode::Device),
        HostIdentity {
            user_public_key: person.root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
    )
    .await
    .unwrap();
    let mut router = iroh::protocol::Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    Served {
        host,
        router: router.spawn(),
        device,
        _state: state,
    }
}

impl Served {
    async fn shutdown(self) {
        self.router.shutdown().await.unwrap();
        self.host.shutdown().await;
    }
}

/// The registry naming `person`'s host and runtime, written under `dir`.
fn registry(
    dir: &std::path::Path,
    person: &Person,
    node: &Served,
    runtime: &crate::tests::Runtime,
) -> Hosts {
    std::fs::write(dir.join("agents.token"), format!("{}\n", runtime.token)).unwrap();
    let registry = json!({"people": {person.id(): {
        "hosts": [{
            "name": "eu1.tilo.test",
            "default": true,
            "device": hex::encode(node.device.cert.device_public_key),
            "addr": node.host.node_addr(),
            "agents": runtime.base,
            "agents_token_file": dir.join("agents.token"),
        }],
    }}});
    let path = dir.join("hosts.json");
    std::fs::write(&path, registry.to_string()).unwrap();
    Hosts::new(path).unwrap()
}

fn headers(pairs: &[(&'static str, &str)]) -> hyper::HeaderMap {
    let mut map = hyper::HeaderMap::new();
    for (name, value) in pairs {
        map.append(*name, hyper::header::HeaderValue::from_str(value).unwrap());
    }
    map
}

async fn mcp(door: &Arc<Door>, token: &str, session: Option<&str>, message: Value) -> Reply {
    let bearer = format!("Bearer {token}");
    let mut pairs = vec![
        ("authorization", bearer.as_str()),
        ("accept", "application/json, text/event-stream"),
    ];
    if let Some(session) = session {
        pairs.push((session::SESSION_HEADER, session));
    }
    door.dispatch(
        &Method::POST,
        "/",
        "",
        &headers(&pairs),
        message.to_string().as_bytes(),
    )
    .await
}

type Reply = hyper::Response<http_body_util::Full<hyper::body::Bytes>>;

/// The last JSON-RPC message of a reply, a plain body or an event stream.
async fn last_message(reply: Reply) -> Value {
    use http_body_util::BodyExt as _;
    let events = reply
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value == "text/event-stream");
    let bytes = reply.into_body().collect().await.unwrap().to_bytes();
    if events {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let last = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .next_back()
            .unwrap();
        return serde_json::from_str(last).unwrap();
    }
    serde_json::from_slice(&bytes).unwrap()
}

async fn call(door: &Arc<Door>, token: &str, session: &str, name: &str, arguments: Value) -> Value {
    let reply = mcp(
        door,
        token,
        Some(session),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    last_message(reply).await
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one story, from the agent's first request to the person revoking it"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host lives until its `shutdown`, which consumes it; the lint misses the move"
)]
async fn an_agent_connects_signs_its_person_in_makes_a_workspace_acts_and_is_revoked() {
    let alice = Person::new(1);
    let node = serve(&alice).await;
    let runtime = runtime(alice.root.verifying_key().to_bytes()).await;
    let files = tempfile::tempdir().unwrap();
    let hosts = registry(files.path(), &alice, &node, &runtime);
    let door = Door::new(
        &Config {
            issuer: ISSUER.into(),
            web_entry: WEB_ENTRY.into(),
        },
        Store::in_memory(),
        Arc::new(hosts),
    )
    .unwrap();

    // Discovery: the first request is refused with where to learn more.
    let refused = door
        .dispatch(&Method::POST, "/", "", &hyper::HeaderMap::new(), b"{}")
        .await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    let challenge = refused.headers()[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        challenge.contains(oauth::PROTECTED_RESOURCE_PATH),
        "{challenge}"
    );
    let resource = json_body(
        door.dispatch(
            &Method::GET,
            oauth::PROTECTED_RESOURCE_PATH,
            "",
            &hyper::HeaderMap::new(),
            b"",
        )
        .await,
    )
    .await;
    assert_eq!(resource["authorization_servers"], json!([ISSUER]));
    let server = json_body(
        door.dispatch(
            &Method::GET,
            oauth::AUTHORIZATION_SERVER_PATH,
            "",
            &hyper::HeaderMap::new(),
            b"",
        )
        .await,
    )
    .await;
    assert_eq!(
        server["registration_endpoint"],
        format!("{ISSUER}/register")
    );

    // Registration, then the person's Tilo: it signs in with Alice's laptop
    // and certifies the key the door made for this grant, as asked.
    let client = register(&door).await;
    let asked = authorize(&door, &client, ISSUER).await;
    assert_eq!(asked.scope, AgentScope::AllWorkspaces);
    let reply = answer(&door, &alice.allow(&asked)).await;
    let cookie = reply.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let code = returned(&reply)["code"].clone();
    let tokens = json_body(redeem(&door, &client, &code, VERIFIER).await).await;
    let token = tokens["access_token"].as_str().unwrap().to_owned();
    let grant = door.store.grants_of(&alice.id())[0].clone();
    let key = grant.key().unwrap();
    assert_eq!(key.cert().device_public_key, asked.key);
    assert!(key.cert().device_label.starts_with("Claude Code"));

    // An agent run, which makes a workspace on Alice's host through the
    // host's own operation, as the grant, and acts in it at once.
    let reply = mcp(
        &door,
        &token,
        None,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"claude-code","version":"2"}}}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    let session = reply.headers()[session::SESSION_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    let created = call(
        &door,
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
    // The host made it and lists it to the grant; the runtime joined it
    // with the invitation the host issued for Alice herself.
    let listed = Hosts::new(files.path().join("hosts.json"))
        .unwrap()
        .workspaces(&key)
        .await
        .unwrap();
    assert!(
        listed.iter().any(|workspace| workspace.id == studio),
        "{listed:?}"
    );
    let joined = lock(&runtime.joined).clone();
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].0, studio);

    let acted = call(
        &door,
        &token,
        &session,
        crate::runtime_support::ACTION_TOOL,
        crate::runtime_support::action_arguments(&studio),
    )
    .await;
    assert!(acted.get("result").is_some(), "{acted}");
    // History names what was done for the run that did it.
    let asked_runtime = runtime.host.asked();
    let run = asked_runtime
        .iter()
        .find_map(|request| request.context.as_ref()?.group.clone())
        .unwrap_or_else(|| panic!("no attributed invocation: {asked_runtime:?}"));
    assert_eq!(run.kind, "agent-run");
    assert_eq!(run.label, "Claude Code #1");
    assert!(
        runtime
            .host
            .callers()
            .iter()
            .any(|caller| caller.workspace() == Some(studio.as_str()))
    );

    // Alice revokes the grant on her grants page: the token and the session
    // fail at once, and the door no longer holds the key.
    let browser = headers(&[("cookie", &cookie)]);
    let page = door
        .dispatch(&Method::GET, "/grants", "", &browser, b"")
        .await;
    assert_eq!(page.status(), StatusCode::OK);
    let page = {
        use http_body_util::BodyExt as _;
        String::from_utf8(
            page.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    };
    let csrf = page
        .split_once("name=\"csrf\" value=\"")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0
        .to_owned();
    let revoked = door
        .dispatch(
            &Method::POST,
            &format!("/grants/{}/revoke", grant.id),
            "",
            &browser,
            format!("csrf={csrf}").as_bytes(),
        )
        .await;
    assert_eq!(revoked.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&revoked), "/grants");
    assert_eq!(door.session_count(), 0);
    let after = mcp(
        &door,
        &token,
        Some(&session),
        json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}),
    )
    .await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
    let again = mcp(
        &door,
        &token,
        None,
        json!({"jsonrpc":"2.0","id":10,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
    )
    .await;
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED);
    assert!(door.store.grants_of(&alice.id()).is_empty());
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
    let _ = REDIRECT;

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host lives until its `shutdown`, which consumes it; the lint misses the move"
)]
async fn a_one_workspace_grant_sees_and_makes_nothing_else_on_the_host() {
    let alice = Person::new(1);
    let node = serve(&alice).await;
    let runtime = runtime(alice.root.verifying_key().to_bytes()).await;
    let files = tempfile::tempdir().unwrap();
    let hosts = registry(files.path(), &alice, &node, &runtime);
    let key = |scope: AgentScope| {
        let signing = crate::grant_key::GrantKey::generate().unwrap();
        let asked = crate::tests::Asked {
            pending: String::new(),
            challenge: [0; 32],
            key: signing.verifying_key().to_bytes(),
            scope,
            expires_ms: tilo_clock::unix_now_ms() + 60_000,
            client: "Claude Code".into(),
        };
        let cert = alice.certify(&asked, scope, asked.expires_ms);
        crate::grant_key::GrantKey::new(signing, cert, scope, tilo_clock::unix_now_ns()).unwrap()
    };
    let all = key(AgentScope::AllWorkspaces);
    let studio = hosts.create(&all, "Studio", None).await.unwrap().id;
    let garden = hosts.create(&all, "Garden", None).await.unwrap().id;
    let one = key(AgentScope::parse(&studio).unwrap());

    let seen = |listed: Vec<crate::directory::Workspace>| {
        let mut ids: Vec<String> = listed.into_iter().map(|workspace| workspace.id).collect();
        ids.sort();
        ids
    };
    let mut both = vec![studio.clone(), garden.clone()];
    both.sort();
    assert_eq!(seen(hosts.workspaces(&all).await.unwrap()), both);
    assert_eq!(
        seen(hosts.workspaces(&one).await.unwrap()),
        std::slice::from_ref(&studio)
    );
    assert_eq!(
        hosts.route(&one, &garden).await,
        Err(DirectoryError::NotFound)
    );
    assert!(hosts.route(&one, &studio).await.is_ok());
    // The host itself refuses to make a workspace for a one-workspace grant.
    assert!(matches!(
        hosts.create(&one, "Elsewhere", None).await,
        Err(DirectoryError::Refused(_))
    ));
    // Both read the person's revocations from the host.
    assert!(
        hosts
            .revocations(&one)
            .await
            .unwrap()
            .iter()
            .next()
            .is_none()
    );

    node.shutdown().await;
}
