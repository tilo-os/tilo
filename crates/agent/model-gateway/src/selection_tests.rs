//! Configuration queries exercise framed decoding, caller authorization and
//! the real configuration/vault path, while an HTTP peer counts connections.
use super::*;
use std::collections::BTreeMap;
use std::io::Write;
use std::net::TcpListener;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct TemporaryConfig(PathBuf);

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove disposable selection config");
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.extend(files(&path));
        } else {
            result.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    result
}

/// Save this case's provider configuration (and key, unless the case has
/// none) and return the protocol it configured.
fn save_case_config(case: &str) -> ProviderProtocol {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "test-provider".into();
    profile.endpoint = std::env::var("SELECTION_TEST_ENDPOINT").unwrap();
    profile.models = vec![agent_provider_config::ModelDescriptor::new("test-model")];
    profile.credential = Some(agent_provider_config::SecretReference::provider_api_key(
        &profile.id,
    ));
    if case == "oversized-model" {
        profile.models[0].id = "x".repeat(257);
    } else if case == "control-model" {
        profile.models[0].id = "test\nmodel".into();
    }
    profile.location = if case.starts_with("local") {
        EndpointLocation::ThisDevice
    } else if case.starts_with("hosted") {
        profile.endpoint = profile.endpoint.replacen("http:", "https:", 1);
        EndpointLocation::Hosted
    } else {
        EndpointLocation::PrivateNetwork
    };
    if case.contains("anthropic") {
        profile.protocol = ProviderProtocol::Anthropic;
    }
    if case == "disabled" {
        profile.enabled = false;
    }
    if case == "invalid-url" {
        profile.endpoint = "http://[invalid".into();
    }
    if case == "invalid-config" {
        profile.location = EndpointLocation::Hosted;
    }
    let protocol = profile.protocol;
    ProviderConfig {
        profiles: vec![profile.clone()],
        default_profile: None,
        ..Default::default()
    }
    .save()
    .unwrap();
    if !case.ends_with("missing-key") && !matches!(case, "empty-key" | "invalid-url") {
        // The subprocess has a disposable, explicit test root, not a live
        // Secret Service session. Opt into local test custody deliberately.
        let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
        let (reference, authorization) = credential_binding(&profile).unwrap();
        ServiceCredentials::open_in(&root)
            .store(
                &reference,
                &authorization,
                match case {
                    "invalid-key" | "invalid-key-anthropic" => "fabricated\ninvalid-key",
                    _ => "fabricated-test-key",
                },
            )
            .unwrap();
    }
    protocol
}

fn child_selection(case: &str) {
    let protocol = save_case_config(case);
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap());
    let before = files(&root);
    let mut payload = serde_json::json!({
        "version": 3,
        "verb": "selection_configured",
        "profile_id": "test-provider",
        "model_id": "test-model",
        "protocol": protocol,
    });
    match case {
        "missing-profile" => payload["profile_id"] = "absent-provider".into(),
        "invalid-profile" => payload["profile_id"] = "../test-provider".into(),
        "missing-model" => payload["model_id"] = "absent-model".into(),
        "oversized-model" => payload["model_id"] = "x".repeat(257).into(),
        "control-model" => payload["model_id"] = "test\nmodel".into(),
        "protocol-mismatch" => payload["protocol"] = "anthropic".into(),
        "invalid-protocol" => payload["protocol"] = "unsupported".into(),
        "missing-protocol" => {
            payload.as_object_mut().unwrap().remove("protocol");
        }
        "secret-status" => payload["verb"] = "secret_status".into(),
        _ => {}
    }
    let app = match case {
        "settings" => SETTINGS_APP,
        "photos" => PHOTOS_APP,
        "unknown-app" => "so.tilo.untrusted",
        _ => AGENT_APP,
    };
    let (mut caller, gateway) = UnixStream::pair().unwrap();
    write_frame(&mut caller, &serde_json::json!({"protocol":1,"app_id":app})).unwrap();
    write_frame(&mut caller, &payload).unwrap();
    gateway.set_nonblocking(true).unwrap();
    let http = provider_client().unwrap();
    let response = tilo_rt::block_on(async {
        let mut gateway = tokio::net::UnixStream::from_std(gateway).unwrap();
        let Ok(handoff) = read_handoff(&mut gateway).await else {
            assert_eq!(case, "unknown-app");
            return None;
        };
        let frame = read_request_frame(&mut gateway).await.unwrap();
        match serde_json::from_slice::<Frame<Request>>(&frame) {
            Ok(frame) => {
                assert_eq!(frame.version, RPC_VERSION);
                Some(handle(&handoff.app_id, frame.value, &http).await)
            }
            Err(error) => {
                assert!(
                    matches!(case, "invalid-protocol" | "missing-protocol"),
                    "configuration request should decode: {error}"
                );
                None
            }
        }
    })
    .unwrap();
    assert_eq!(
        files(&root),
        before,
        "query must not change config/vault files"
    );
    fs::write(
        root.join("response.json"),
        serde_json::to_vec(&response).unwrap(),
    )
    .unwrap();
}

/// Where a case ran: in the re-executed child, which only writes its
/// observation, or in the parent, with the handler's response if the request
/// reached the handler at all.
enum Outcome {
    Child,
    Parent(Option<Response>),
}

fn run_case(test: &str, case: &str) -> Outcome {
    if let Ok(case) = std::env::var("SELECTION_TEST_CASE") {
        child_selection(&case);
        return Outcome::Child;
    }
    let nonce = tilo_clock::try_unix_now_ns().unwrap();
    let temp = TemporaryConfig(std::env::temp_dir().join(format!(
        "tilo-selection-{test}-{case}-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir(&temp.0).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_stop = stop.clone();
    let server_accepted = accepted.clone();
    let peer = std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    server_accepted.fetch_add(1, Ordering::AcqRel);
                    let _ = stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if server_stop.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("local selection listener: {error}"),
            }
        }
    });
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("service::selection_tests::{test}"),
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", &temp.0)
        .env("XDG_CONFIG_HOME", &temp.0)
        .env("SELECTION_TEST_CASE", case)
        .env("SELECTION_TEST_ENDPOINT", endpoint)
        .output()
        .unwrap();
    stop.store(true, Ordering::Release);
    peer.join().unwrap();
    assert_eq!(
        accepted.load(Ordering::Acquire),
        0,
        "{case}: zero provider connections"
    );
    assert!(
        child.status.success(),
        "{case}: child configuration path failed: {} {}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    Outcome::Parent(
        serde_json::from_slice(&fs::read(temp.0.join("response.json")).unwrap()).unwrap(),
    )
}

#[test]
fn selection_configured_preserves_plan_locations_and_protocols_without_dispatch() {
    for case in [
        "local",
        "local-anthropic",
        "private",
        "private-anthropic",
        "hosted",
        "hosted-anthropic",
    ] {
        let Outcome::Parent(response) = run_case(
            "selection_configured_preserves_plan_locations_and_protocols_without_dispatch",
            case,
        ) else {
            return;
        };
        let response = response.expect("valid request response");
        assert!(response.ok, "{case}: {:?}", response.error);
        assert_eq!(response.configured, Some(true));
        assert!(response.output.is_none());
        assert!(response.error.is_none());
    }
}

#[test]
fn selection_configured_refuses_unavailable_or_mismatched_prerequisites() {
    for case in [
        "settings",
        "photos",
        "disabled",
        "missing-profile",
        "invalid-profile",
        "missing-model",
        "oversized-model",
        "control-model",
        "protocol-mismatch",
        "missing-key",
        "local-missing-key",
        "hosted-missing-key",
        "empty-key",
        "invalid-key",
        "invalid-key-anthropic",
        "invalid-url",
        "invalid-config",
        "secret-status",
    ] {
        let Outcome::Parent(response) = run_case(
            "selection_configured_refuses_unavailable_or_mismatched_prerequisites",
            case,
        ) else {
            return;
        };
        let response = response.expect("valid request response");
        assert!(!response.ok, "{case}: must refuse");
        assert!(response.configured.is_none());
        assert!(response.output.is_none());
        let error = response.error.expect("bounded refusal reason");
        assert!(error.len() <= 160);
        assert!(!error.contains("fabricated"));
    }
}

#[test]
fn selection_configured_refuses_malformed_protocol_and_unknown_caller() {
    for case in ["invalid-protocol", "missing-protocol", "unknown-app"] {
        let Outcome::Parent(response) = run_case(
            "selection_configured_refuses_malformed_protocol_and_unknown_caller",
            case,
        ) else {
            return;
        };
        assert!(response.is_none(), "{case}: reject before handler");
    }
}
