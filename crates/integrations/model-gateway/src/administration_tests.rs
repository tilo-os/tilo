use super::*;
use crate::transport::{AdministrationClient, FdTransport};
use agent_provider_config::management::ProviderAdmin;
use std::os::fd::AsFd as _;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_test_env::within;

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
    let empty = admin.get_config().await.unwrap();
    let config = ProviderConfig {
        profiles: vec![profile.clone()],
        default_profile: Some(profile.id.clone()),
        ..Default::default()
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
    first
}

/// Settings' model list asks a server on this computer as itself: no key
/// goes with it, as this computer keeps none.
async fn lists_models_of_a_local_server(
    admin: &dyn ProviderAdmin,
    listener: &tokio::net::TcpListener,
    profile: &ProviderProfile,
) {
    let listing = async {
        use tokio::io::AsyncWriteExt as _;
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            bytes.push(socket.read_u8().await.unwrap());
        }
        let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
        assert!(headers.starts_with("get /v1/models "), "{headers}");
        assert!(!headers.contains("authorization:"), "{headers}");
        let reply =
            r#"{"object":"list","data":[{"id":"second"},{"id":"first","context_length":8192}]}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
    };
    let (listed, ()) = tokio::join!(admin.list_models(profile.id.clone()), listing);
    let listed = listed.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(listed[0].context_window, Some(8192));
}

async fn lifecycle(admin: &dyn ProviderAdmin) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = within(
        "configuring the provider",
        configure(
            admin,
            format!("http://{}/v1", listener.local_addr().unwrap()),
        ),
    )
    .await;
    let profile = first.config.profiles[0].clone();
    within(
        "listing the provider's models",
        lists_models_of_a_local_server(admin, &listener, &profile),
    )
    .await;
    let mut next = first.config.clone();
    next.profiles[0].default_model = Some("second".into());
    let updated = within(
        "an edit of the default model",
        admin.set_config(first.revision, next),
    )
    .await
    .unwrap();
    within("refused edits", cleanup(admin, updated)).await;
    within(
        "signed metadata staying the provisioner's",
        signed_metadata_stays_owned_by_provisioner(admin),
    )
    .await;
}

async fn cleanup(
    admin: &dyn ProviderAdmin,
    updated: agent_provider_config::management::ProviderSnapshot,
) {
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
    admin
        .set_config(updated.revision, ProviderConfig::default())
        .await
        .unwrap();
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
        ..Default::default()
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

async fn child_fd() {
    // The descriptions are the settings this process reads, which the gateway
    // in it reads too.
    settings_store::install_source(Some(Arc::new(settings_store::HostDocument::new(
        serde_json::Map::new(),
        |_| Ok(()),
    ))));
    for app in [AGENT_APP, CHAT_APP] {
        within(format!("{app} being refused administration"), async {
            let (transport, server) = fd(app);
            let denied = AdministrationClient::new(transport);
            assert_eq!(
                denied
                    .list_models("test-provider".into())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::PermissionDenied
            );
            server.abort();
        })
        .await;
    }
    let (management, server) = fd(SETTINGS_APP);
    lifecycle(&AdministrationClient::new(management)).await;
    server.abort();
}

/// Run the end-to-end administration test `name` over `kind` (`fd`) in a
/// process of its own, with disposable configuration. Each step of it is
/// bounded as a hang on its own ([`within`]).
fn administration_end_to_end(name: &str, kind: &str) {
    const MARKER: &str = "PROVIDER_ADMIN_TEST_CHILD";
    if std::env::var_os(MARKER).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(child_fd());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("service::administration_tests::{name}"),
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env(MARKER, kind)
        .output_locked()
        .unwrap();
    assert!(
        output.status.success(),
        "{kind}: {}: {} {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn provider_administration_over_fd_end_to_end() {
    administration_end_to_end("provider_administration_over_fd_end_to_end", "fd");
}
