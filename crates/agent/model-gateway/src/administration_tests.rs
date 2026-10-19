//! Real authenticated HTTP and descriptor administration over isolated files.
use super::*;
use crate::{
    caller::{Caller, Session},
    transport::{AdministrationClient, CallFuture, FdTransport, ModelClient, Transport},
};
use agent_provider_config::{
    management::{CredentialStatus, ProviderAdmin},
    protocol::Conversation,
};
use std::os::fd::AsFd as _;

struct Grant(Caller, bool);
impl Session for Grant {
    fn caller(&self) -> Caller {
        self.0
    }
    fn may_manage_credentials(&self) -> bool {
        self.1
    }
    fn active(&self) -> bool {
        true
    }
}
struct Auth;
impl crate::http_service::SessionAuthorizer for Auth {
    fn authorize(&self, headers: &hyper::HeaderMap) -> Option<Arc<dyn Session>> {
        match headers.get("authorization")?.to_str().ok()? {
            "Bearer manager" => Some(Arc::new(Grant(Caller::Settings, true))),
            "Bearer settings" => Some(Arc::new(Grant(Caller::Settings, false))),
            "Bearer agent" => Some(Arc::new(Grant(Caller::Agent, false))),
            "Bearer chat" => Some(Arc::new(Grant(Caller::Chat, false))),
            _ => None,
        }
    }
}
struct Http {
    url: String,
    token: &'static str,
}
impl Transport for Http {
    fn call<'a>(
        &'a self,
        request: Request,
        _: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a> {
        Box::pin(async move {
            let response = provider_client()
                .unwrap()
                .post(&self.url)
                .bearer_auth(self.token)
                .json(&Frame {
                    version: RPC_VERSION,
                    value: request,
                })
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            assert!(!body.contains("write-only-test-key"));
            if status == reqwest::StatusCode::FORBIDDEN {
                return Err(ModelError::new(ErrorKind::PermissionDenied));
            }
            assert!(status.is_success(), "{status}: {body}");
            Ok(serde_json::from_str(body.trim()).unwrap())
        })
    }
}
fn fd(app: &str) -> (FdTransport, tokio::task::JoinHandle<()>) {
    let (mut original, peer) = UnixStream::pair().unwrap();
    HANDOFF_FRAMING
        .write(
            &mut original,
            &serde_json::to_vec(&serde_json::json!({"protocol":1,"app_id":app})).unwrap(),
        )
        .unwrap();
    let transport = FdTransport::new(original.as_fd()).unwrap();
    peer.set_nonblocking(true).unwrap();
    let peer = tokio::net::UnixStream::from_std(peer).unwrap();
    let task = tokio::spawn(async move {
        serve_connection(peer, &provider_client().unwrap()).await;
    });
    (transport, task)
}

async fn configure(
    admin: &dyn ProviderAdmin,
    endpoint: String,
) -> agent_provider_config::management::ProviderSnapshot {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "test-provider".into();
    profile.endpoint = endpoint;
    profile.models = vec![
        agent_provider_config::ModelDescriptor::new("first"),
        agent_provider_config::ModelDescriptor::new("second"),
    ];
    profile.default_model = Some("first".into());
    profile.credential = Some(agent_provider_config::SecretReference::provider_api_key(
        &profile.id,
    ));
    let empty = admin.get_config().await.unwrap();
    let config = ProviderConfig {
        profiles: vec![profile.clone()],
        default_profile: Some(profile.id.clone()),
    };
    let first = admin
        .set_config(empty.revision.clone(), config.clone())
        .await
        .unwrap();
    assert_eq!(
        admin
            .set_config(empty.revision, config)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
    // Establish test-only custody, never consult an ambient keyring.
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
    let (reference, authorization) = credential_binding(&profile).unwrap();
    ServiceCredentials::open_in(&root)
        .store(&reference, &authorization, "seed")
        .unwrap();
    admin
        .set_api_key(profile.id.clone(), "write-only-test-key".into())
        .await
        .unwrap();
    assert_eq!(
        admin.credential_status(profile.id.clone()).await.unwrap(),
        CredentialStatus::Configured
    );
    let view = admin.get_config().await.unwrap();
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("write-only-test-key")
    );
    let mut unsafe_edit = first.config.clone();
    unsafe_edit.profiles[0].endpoint = "http://127.0.0.1:9/v1".into();
    assert_eq!(
        admin
            .set_config(first.revision.clone(), unsafe_edit)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    assert_eq!(
        admin
            .set_config(first.revision.clone(), ProviderConfig::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );

    first
}

async fn lifecycle<T: Transport>(admin: &dyn ProviderAdmin, model: &ModelClient<T>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = configure(
        admin,
        format!("http://{}/v1", listener.local_addr().unwrap()),
    )
    .await;
    let profile = first.config.profiles[0].clone();
    let (began, mut started) = tokio::sync::mpsc::channel(1);
    let (release, released) = tokio::sync::oneshot::channel();
    let provider = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        let mut released = Some(released);
        for expected in ["first", "second"] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                bytes.push(socket.read_u8().await.unwrap());
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
            };
            let headers = String::from_utf8(bytes.clone())
                .unwrap()
                .to_ascii_lowercase();
            assert!(headers.contains("authorization: bearer write-only-test-key"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            bytes.resize(header_end.checked_add(length).unwrap(), 0);
            socket.read_exact(&mut bytes[header_end..]).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes[header_end..]).unwrap();
            assert_eq!(body["model"], expected);
            if expected == "first" {
                began.send(()).await.unwrap();
                released.take().unwrap().await.unwrap();
            }
            let reply = serde_json::json!({"id":"answer","object":"chat.completion","created":0,"model":expected,"choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        }
    });
    let request = Conversation::new("system", "hello").request(vec![], 32);
    let mut next = first.config.clone();
    next.profiles[0].default_model = Some("second".into());
    let update = async {
        started.recv().await.unwrap();
        let updated = admin.set_config(first.revision, next).await.unwrap();
        release.send(()).unwrap();
        updated
    };
    let (answer, updated) = tokio::join!(model.complete_once(&request), update);
    answer.unwrap();
    model.complete_once(&request).await.unwrap();
    provider.await.unwrap();
    admin.remove_api_key(profile.id.clone()).await.unwrap();
    assert_eq!(
        admin.credential_status(profile.id.clone()).await.unwrap(),
        CredentialStatus::Missing
    );
    assert_eq!(
        model.complete_once(&request).await.unwrap_err().kind,
        ErrorKind::Authentication
    );
    cleanup(admin, &profile, updated).await;
    signed_metadata_stays_owned_by_provisioner(admin).await;
}

async fn cleanup(
    admin: &dyn ProviderAdmin,
    profile: &ProviderProfile,
    updated: agent_provider_config::management::ProviderSnapshot,
) {
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
    let legacy = accounts::AccountId::new(format!("agent-provider-{}", profile.id));
    let mut vault = accounts::SecretVault::open_in(&root);
    vault.put(
        &legacy,
        &accounts::Secret::password("legacy-write-only-key"),
    );
    vault.try_save().unwrap();
    assert_eq!(
        admin.credential_status(profile.id.clone()).await.unwrap(),
        CredentialStatus::MigrationRequired
    );
    let mut retargeted = updated.config.clone();
    retargeted.profiles[0].endpoint = "http://127.0.0.1:9/v1".into();
    assert_eq!(
        admin
            .set_config(updated.revision.clone(), retargeted)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    assert!(
        admin
            .migrate_api_key(profile.id.clone(), "https://wrong-origin.example".into())
            .await
            .is_err()
    );
    assert_eq!(
        admin.credential_status(profile.id.clone()).await.unwrap(),
        CredentialStatus::MigrationRequired
    );
    admin
        .remove_legacy_api_key(profile.id.clone())
        .await
        .unwrap();
    let mut invalid = updated.config.clone();
    invalid.profiles[0].endpoint = "http://127.0.0.1/v1?api_key=never-store-me".into();
    assert_eq!(
        admin
            .set_config(updated.revision.clone(), invalid)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
    let mut forged = updated.config.clone();
    forged.profiles[0].models[0].revision = Some("a".repeat(40));
    assert_eq!(
        admin
            .set_config(updated.revision.clone(), forged)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    let cleared = admin
        .set_config(updated.revision, ProviderConfig::default())
        .await
        .unwrap();
    // Persistence failure must not be acknowledged as a successful edit.
    let path = settings_store::settings_path_in(&root);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        admin
            .set_config(cleared.revision, ProviderConfig::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    fs::remove_dir(path).unwrap();
}

async fn send(client: &mut tokio::net::UnixStream, value: Request) {
    FRAMING
        .write_async(
            client,
            &serde_json::to_vec(&Frame {
                version: RPC_VERSION,
                value,
            })
            .unwrap(),
        )
        .await
        .unwrap();
}

async fn receive(client: &mut tokio::net::UnixStream) -> Response {
    serde_json::from_slice(&FRAMING.read_async(client).await.unwrap().unwrap()).unwrap()
}

async fn signed_metadata_stays_owned_by_provisioner(admin: &dyn ProviderAdmin) {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "signed-profile".into();
    let mut model = agent_provider_config::ModelDescriptor::new("signed-model");
    model.revision = Some("a".repeat(40));
    profile.models.push(model);
    let installed = ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
    };
    installed.save().unwrap();
    let snapshot = admin.get_config().await.unwrap();
    let mut disabled = snapshot.config.clone();
    disabled.profiles[0].enabled = false;
    let snapshot = admin.set_config(snapshot.revision, disabled).await.unwrap();
    let mut tampered = snapshot.config.clone();
    tampered.profiles[0].models[0].revision = None;
    assert_eq!(
        admin
            .set_config(snapshot.revision.clone(), tampered)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    admin
        .set_config(snapshot.revision, ProviderConfig::default())
        .await
        .unwrap();
}

async fn mutation_stop_waits_for_commit() {
    let (mut client, peer) = tokio::net::UnixStream::pair().unwrap();
    HANDOFF_FRAMING
        .write_async(
            &mut client,
            &serde_json::to_vec(&serde_json::json!({"protocol":1,"app_id":SETTINGS_APP})).unwrap(),
        )
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        serve_connection(peer, &provider_client().unwrap()).await;
    });
    send(&mut client, Request::CancellationSupported).await;
    assert_eq!(
        receive(&mut client).await.output.as_deref(),
        Some("cancel-v1")
    );
    let (held, acquired) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _serial = management_write().unwrap();
        held.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    acquired.await.unwrap();
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "committed-profile".into();
    let config = ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
    };
    send(
        &mut client,
        Request::Cancellable {
            id: 7,
            request: Box::new(Request::SetProviderConfig {
                expected_revision: provider_snapshot(ProviderConfig::default())
                    .unwrap()
                    .revision,
                config: config.clone(),
            }),
        },
    )
    .await;
    send(&mut client, Request::Cancel { id: 7 }).await;
    // Readiness may be spurious after the negotiation. Clear that readiness
    // without starting a partial frame read; any actual byte is premature.
    let premature = async {
        loop {
            client.readable().await.unwrap();
            match client.try_read(&mut [0u8; 1]) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                result => return result,
            }
        }
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(30), premature)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    let committed = receive(&mut client).await;
    assert!(committed.ok, "{:?}", committed.error);
    assert_eq!(
        serde_json::from_str::<agent_provider_config::management::ProviderSnapshot>(
            committed.output.as_deref().unwrap()
        )
        .unwrap()
        .config,
        config
    );
    assert_eq!(
        receive(&mut client).await.output.as_deref(),
        Some("cancelled:7")
    );
    assert_eq!(ProviderConfig::load(), config);
    holder.join().unwrap();
    drop(client);
    server.await.unwrap();
}

/// How long a model turn may take. The first turn's reply waits for a
/// configuration edit, whose vault and settings writes can take seconds on a
/// loaded machine.
const MODEL_WAIT: Duration = Duration::from_secs(30);

async fn child(http: bool) {
    if http {
        let service = crate::http_service::HttpService::new(Arc::new(Auth), 4).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model/rpc", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let service = service.clone();
                tokio::spawn(async move {
                    service.serve_io(stream).await.unwrap();
                });
            }
        });
        for token in ["agent", "chat", "settings"] {
            let denied = AdministrationClient::new(Http {
                url: url.clone(),
                token,
            });
            assert_eq!(
                denied.get_config().await.unwrap_err().kind,
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                denied
                    .set_config("stale".into(), ProviderConfig::default())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                denied
                    .set_api_key("test-provider".into(), "write-only-test-key".into())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::PermissionDenied
            );
        }
        lifecycle(
            &AdministrationClient::new(Http {
                url: url.clone(),
                token: "manager",
            }),
            &ModelClient::default_model(
                Http {
                    url,
                    token: "agent",
                },
                MODEL_WAIT,
            ),
        )
        .await;
        server.abort();
    } else {
        for app in [AGENT_APP, CHAT_APP] {
            let (transport, server) = fd(app);
            let denied = AdministrationClient::new(transport);
            assert_eq!(
                denied.get_config().await.unwrap_err().kind,
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                denied
                    .set_config("stale".into(), ProviderConfig::default())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::PermissionDenied
            );
            server.abort();
        }
        let (management, server) = fd(SETTINGS_APP);
        let (inference, infer_server) = fd(AGENT_APP);
        lifecycle(
            &AdministrationClient::new(management),
            &ModelClient::default_model(inference, MODEL_WAIT),
        )
        .await;
        server.abort();
        infer_server.abort();
        mutation_stop_waits_for_commit().await;
    }
}

#[test]
fn provider_administration_http_and_fd_end_to_end() {
    const MARKER: &str = "PROVIDER_ADMIN_TEST_CHILD";
    if let Ok(kind) = std::env::var(MARKER) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(120), child(kind == "http"))
                    .await
                    .unwrap();
            });
        return;
    }
    for kind in ["http", "fd"] {
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service::administration_tests::provider_administration_http_and_fd_end_to_end",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env(MARKER, kind)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{kind}: {}: {} {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
