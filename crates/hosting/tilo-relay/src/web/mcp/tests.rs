//! Real HTTP requests through the relay listener and certified handshake.

use super::*;
use crate::web::tests::{browser, device};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{CatalogueProvider, CatalogueResource, CatalogueRow, ReceiptStatus};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const ROOM: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_ROOM: &str = "22222222-2222-4222-8222-222222222222";

mod record {
    use tilo_resource::{Model, resource};

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub title: String,
    }

    #[resource(kind = "test.note", label = "Note")]
    impl Note {
        /// Rename the note.
        #[action]
        pub fn rename(&mut self, #[param("New title")] title: String) {
            self.title = title;
        }
    }
}

#[derive(Clone, Default)]
struct RecordingHost {
    callers: Arc<Mutex<Vec<Caller>>>,
    invocations: Arc<Mutex<Vec<(Caller, InvokeRequest)>>>,
}

impl ActionHost for RecordingHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let resource =
            tilo_resource::publish::<record::Note>("test.note:1", "Note", &record::Note::default());
        let descriptor = tilo_actions::door::survey(&[resource])
            .into_iter()
            .find(|action| action.address.action == tilo_resource::APPLY)
            .unwrap();
        let row = CatalogueRow {
            provider: "record:so.tilo.test:record".into(),
            app_id: "so.tilo.test".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: "test.note:1".into(),
                kind: "test.note".into(),
                label: "Note".into(),
                presentation: None,
            },
            action: Some(descriptor),
            has_children: false,
            visibility: None,
        };
        let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
        page.providers = vec![CatalogueProvider {
            provider: "record:so.tilo.test:record".into(),
            app_id: "so.tilo.test".into(),
            room: caller.room().unwrap().into(),
            record_id: "record".into(),
            browsable: true,
        }];
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        self.invocations
            .lock()
            .unwrap()
            .push((caller.clone(), request));
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.invocations
            .lock()
            .unwrap()
            .iter()
            .any(|(owner, request)| owner == caller && request.invocation == *id)
            .then(|| {
                InvocationState::new(
                    *id,
                    tilo_actions::InvocationUpdate::Accepted { invocation: *id },
                )
            })
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        self.observe(caller, id)
            .map(|_| ())
            .ok_or(ActionHostError::UnknownInvocation)
    }
}

struct Fixture {
    relay: crate::Relay,
    state: tempfile::TempDir,
    _live: tilo_moq::live::LoopbackLive,
    serving: tokio::task::JoinHandle<()>,
    addr: std::net::SocketAddr,
    token: [u8; 32],
    second_token: [u8; 32],
    device: [u8; 32],
    user: [u8; 32],
    host: RecordingHost,
    sessions: Arc<Sessions>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

impl Fixture {
    async fn new(limit: usize) -> Self {
        let root = SigningKey::from_bytes(&[90; 32]);
        let (host_key, host_cert) = device(&root, 91, DeviceCapabilities::host());
        let (key, cert) = device(&root, 92, DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let host = RecordingHost::default();
        let relay = crate::Relay::start_with_app_serving_and_relay_mode(
            crate::RelayConfig {
                state_root: state.path().into(),
                services: crate::ServiceCapabilities {
                    app_serving: true,
                    ..crate::ServiceCapabilities::relay_only()
                },
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: host_cert.user_public_key,
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
            Some(Arc::new(crate::app_serving::ActionHostPlane::new(
                host.clone(),
            ))),
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        for room in [ROOM, OTHER_ROOM] {
            relay
                .set_workspace_membership(
                    crate::room_workspace_id(room).unwrap(),
                    true,
                    vec![cert.device_public_key],
                    state.path().into(),
                )
                .await
                .unwrap();
        }
        let sessions = Sessions::new(relay.web_context().clone());
        let grants = super::super::live::LiveGrants::new();
        let live = tilo_moq::live::LoopbackLive::start(Arc::new(super::super::live::Authority(
            grants.clone(),
        )))
        .unwrap();
        let web = Arc::new(Web {
            mcp: RoomsMcp::new(limit),
            sessions: Arc::clone(&sessions),
            grants,
            legacy: super::super::legacy::LegacyWindows::new(
                live.relay.clone(),
                host_cert.device_public_key,
            ),
            live: live.relay.clone(),
            host_info: super::super::HostInfo {
                host_cert,
                webtransport_port: None,
                certificate_sha256: None,
                webtransport_origin: None,
            },
            allowed_origins: vec!["https://client.example".into()],
            features: 0,
            rooms: relay.rooms().cloned(),
            envelopes: super::super::envelopes::Envelopes::new(state.path().join("envelopes")),
            profiles: super::super::envelopes::Envelopes::of(
                super::super::envelopes::Kind::Profile,
                state.path().join("profiles"),
            ),
            here: Arc::new(super::super::here::Here::new(None)),
        });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn(super::super::plain_accept_loop(listener, web));
        let token = browser(addr, &key, &cert).await;
        let second_token = browser(addr, &key, &cert).await;
        Self {
            relay,
            state,
            _live: live,
            serving,
            addr,
            token,
            second_token,
            device: cert.device_public_key,
            user: cert.user_public_key,
            host,
            sessions,
        }
    }

    async fn request(
        &self,
        method: &str,
        room: &str,
        headers: &str,
        body: &str,
    ) -> (u16, String, Vec<u8>) {
        let mut socket = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        let request = format!(
            "{method} /peer/v1/rooms/{room}/mcp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tilo_test_env::within("MCP HTTP reply", socket.read_to_end(&mut response))
            .await
            .unwrap();
        let split = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let header = String::from_utf8(response[..split].to_vec()).unwrap();
        let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut bytes = response[split.checked_add(4).unwrap()..].to_vec();
        if header
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            let mut rest = bytes.as_slice();
            let mut decoded = Vec::new();
            loop {
                let end = rest.windows(2).position(|bytes| bytes == b"\r\n").unwrap();
                let length =
                    usize::from_str_radix(std::str::from_utf8(&rest[..end]).unwrap(), 16).unwrap();
                if length == 0 {
                    break;
                }
                rest = &rest[end.checked_add(2).unwrap()..];
                decoded.extend_from_slice(&rest[..length]);
                rest = &rest[length.checked_add(2).unwrap()..];
            }
            bytes = decoded;
        }
        (status, header, bytes)
    }

    async fn initialize(&self, token: [u8; 32], room: &str) -> String {
        let (status, headers, _) = self
            .request("POST", room, &auth(token), &initialize())
            .await;
        assert_eq!(status, 200, "{headers}");
        headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .starts_with("mcp-session-id:")
                    .then(|| line.split_once(':').unwrap().1.trim().to_owned())
            })
            .unwrap()
    }

    async fn rpc(&self, session: &str, value: Value) -> (u16, String, Vec<u8>) {
        self.request(
            "POST",
            ROOM,
            &format!("{}Mcp-Session-Id: {session}\r\n", auth(self.token)),
            &value.to_string(),
        )
        .await
    }
}

fn auth(token: [u8; 32]) -> String {
    format!("Authorization: Bearer {}\r\n", hex::encode(token))
}

fn initialize() -> String {
    json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2025-03-26", "capabilities":{}, "clientInfo":{"name":"forged identity", "version":"1"}}}).to_string()
}

#[tokio::test]
async fn http_mcp_auth_origin_and_transport_lifecycle() {
    let f = Fixture::new(8).await;
    for headers in [String::new(), auth([0; 32])] {
        assert_eq!(
            f.request("POST", ROOM, &headers, &initialize()).await.0,
            401
        );
    }
    assert_eq!(
        f.request("POST", "not-a-room", &auth(f.token), &initialize())
            .await
            .0,
        400
    );
    assert_eq!(
        f.request(
            "POST",
            ROOM,
            &format!("{}Origin: https://evil.example\r\n", auth(f.token)),
            &initialize()
        )
        .await
        .0,
        403
    );
    let (code, headers, _) = f
        .request("OPTIONS", ROOM, "Origin: https://client.example\r\n", "")
        .await;
    assert_eq!(code, 204);
    assert!(headers.contains("mcp-session-id"));
    let id = f.initialize(f.token, ROOM).await;
    let headers = format!("{}Mcp-Session-Id: {id}\r\n", auth(f.token));
    assert_eq!(f.request("GET", ROOM, &headers, "").await.0, 405);
    assert_eq!(
        f.rpc(
            &id,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .0,
        202
    );
    assert_eq!(
        f.rpc(&id, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .0,
        200
    );
    assert_eq!(f.request("POST", ROOM, &headers, "{").await.0, 400);
    assert_eq!(
        f.request("POST", ROOM, &headers, &"x".repeat(1024 * 1024 + 1))
            .await
            .0,
        413
    );
    let conflicting = format!(
        "Authorization: Bearer invalid\r\nX-Tilo-Session: {}\r\n",
        hex::encode(f.token)
    );
    assert_eq!(
        f.request("POST", ROOM, &conflicting, &initialize()).await.0,
        401
    );
    assert_eq!(f.request("DELETE", ROOM, &headers, "").await.0, 204);
    assert_eq!(
        f.rpc(&id, json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
            .await
            .0,
        404
    );
    drop(f);
}

#[tokio::test]
async fn http_mcp_pins_certified_caller_room_and_execution() {
    let f = Fixture::new(8).await;
    let id = f.initialize(f.token, ROOM).await;
    let second = f.initialize(f.token, ROOM).await;
    let listed = f.rpc(&id, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_list_apps","arguments":{}}})).await;
    let listed: Value = serde_json::from_slice(&listed.2).unwrap();
    assert_eq!(
        listed["result"]["structuredContent"]["apps"][0]["peer_id"], 1,
        "{listed}"
    );
    assert_eq!(
        listed["result"]["structuredContent"]["apps"][0]["record_id"],
        "record"
    );
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_invoke","arguments":{"peer_id":1,"revision":"1","resource":"test.note:1","action":tilo_resource::APPLY,"wait_ms":0}}});
    for session in [&id, &second] {
        let response = f.rpc(session, call.clone()).await;
        assert_eq!(response.0, 200);
    }
    let (retired, invocation) = {
        let invoked = f.host.invocations.lock().unwrap();
        assert_eq!(invoked.len(), 2);
        for (caller, request) in invoked.iter() {
            assert_eq!(request.target.provider, "record:so.tilo.test:record");
            assert_eq!(caller.user_public_key(), Some(&f.user));
            assert_eq!(caller.device_public_key(), Some(&f.device));
            assert_eq!(caller.room(), Some(ROOM));
            assert_eq!(
                caller.principal(),
                &tilo_actions::remote::CallerPrincipal::Person(hex::encode(f.user))
            );
        }
        assert_ne!(invoked[0].0.execution_id(), invoked[1].0.execution_id());
        (invoked[0].0.clone(), invoked[0].1.invocation.0.to_string())
    };
    for tool in ["tilo_status", "tilo_cancel"] {
        let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":tool,"arguments":{"invocation_id":invocation}}});
        let own = f.rpc(&id, call.clone()).await;
        assert_eq!(own.0, 200);
        let own: Value = serde_json::from_slice(&own.2).unwrap();
        assert_ne!(own["result"]["isError"], true, "{own}");
        let other = f.rpc(&second, call).await;
        let other: Value = serde_json::from_slice(&other.2).unwrap();
        assert_eq!(other["result"]["isError"], true, "{other}");
    }
    let headers = format!("{}Mcp-Session-Id: {id}\r\n", auth(f.token));
    assert_eq!(f.request("DELETE", ROOM, &headers, "").await.0, 204);
    assert!(!retired.execution_is_live());
    drop(f);
}

#[tokio::test]
async fn http_mcp_session_ids_do_not_cross_credentials_or_rooms() {
    let f = Fixture::new(8).await;
    let id = f.initialize(f.token, ROOM).await;
    let ping = json!({"jsonrpc":"2.0","id":2,"method":"ping"}).to_string();
    let other_credential = format!("{}Mcp-Session-Id: {id}\r\n", auth(f.second_token));
    assert_eq!(
        f.request("POST", ROOM, &other_credential, &ping).await.0,
        404
    );
    let other_room = format!("{}Mcp-Session-Id: {id}\r\n", auth(f.token));
    assert_eq!(
        f.request("POST", OTHER_ROOM, &other_room, &ping).await.0,
        404
    );
    assert_eq!(
        f.rpc(&id, json!({"jsonrpc":"2.0","id":2,"method":"ping"}))
            .await
            .0,
        200
    );
    drop(f);
}

#[tokio::test]
async fn http_mcp_rechecks_membership_and_refuses_agent_promotion() {
    let f = Fixture::new(8).await;
    let id = f.initialize(f.token, ROOM).await;
    f.relay
        .set_workspace_membership(
            crate::room_workspace_id(ROOM).unwrap(),
            true,
            vec![],
            f.state.path().into(),
        )
        .await
        .unwrap();
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_query","arguments":{"peer_id":1}}});
    assert_eq!(f.rpc(&id, call.clone()).await.0, 403);
    assert!(f.host.callers.lock().unwrap().is_empty());
    f.relay
        .set_workspace_membership(
            crate::room_workspace_id(ROOM).unwrap(),
            true,
            vec![f.device],
            f.state.path().into(),
        )
        .await
        .unwrap();
    assert_eq!(f.rpc(&id, call.clone()).await.0, 200);
    f.relay
        .web_context()
        .relay
        .agents
        .write()
        .unwrap()
        .insert(f.device, None);
    assert_eq!(f.rpc(&id, call).await.0, 403);
    drop(f);
}

#[tokio::test]
async fn http_mcp_revocation_blocks_an_existing_certified_session() {
    let f = Fixture::new(8).await;
    let id = f.initialize(f.token, ROOM).await;
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: f.user,
        revoked_device_public_key: f.device,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[90; 32]))
        .unwrap();
    f.relay
        .web_context()
        .relay
        .identity
        .revocations
        .write()
        .unwrap()
        .insert(entry)
        .unwrap();
    assert_eq!(
        f.rpc(&id, json!({"jsonrpc":"2.0","id":2,"method":"ping"}))
            .await
            .0,
        403
    );
    assert!(f.host.callers.lock().unwrap().is_empty());
    drop(f);
}

#[tokio::test]
async fn action_host_rechecks_admission_after_an_http_request_started() {
    let f = Fixture::new(8).await;
    let id = f.initialize(f.token, ROOM).await;
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_invoke","arguments":{"peer_id":1,"revision":"1","resource":"test.note:1","action":tilo_resource::APPLY,"wait_ms":0}}});
    assert_eq!(f.rpc(&id, call).await.0, 200);
    let (caller, mut request) = f.host.invocations.lock().unwrap()[0].clone();
    request.target.provider = "1".into();
    let mut routes = Providers::default();
    routes.publish("record:so.tilo.test:record").unwrap();
    let host = RoomHost {
        sessions: Arc::clone(&f.sessions),
        session: f.sessions.get(&f.token).unwrap(),
        workspace: crate::room_workspace_id(ROOM).unwrap(),
        routes: Arc::new(Mutex::new(routes)),
    };
    f.relay
        .set_workspace_membership(host.workspace, true, vec![], f.state.path().into())
        .await
        .unwrap();
    assert_eq!(
        host.query(&caller, CatalogueQuery::default()).await,
        Some(Err(ActionHostError::NotPermitted))
    );
    assert_eq!(
        host.invoke(&caller, request).await,
        Some(Err(ActionHostError::NotPermitted))
    );
    drop(host);
    assert_eq!(f.host.invocations.lock().unwrap().len(), 1);
    drop(f);
}

#[tokio::test]
async fn http_mcp_bounds_sessions_and_releases_capacity_on_delete() {
    let f = Fixture::new(1).await;
    let id = f.initialize(f.token, ROOM).await;
    assert_eq!(
        f.request("POST", ROOM, &auth(f.token), &initialize())
            .await
            .0,
        503
    );
    let headers = format!("{}Mcp-Session-Id: {id}\r\n", auth(f.token));
    assert_eq!(f.request("DELETE", ROOM, &headers, "").await.0, 204);
    f.initialize(f.token, ROOM).await;
    drop(f);
}
