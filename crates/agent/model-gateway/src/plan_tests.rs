//! Real Plan dispatch tests. Each subprocess owns disposable configuration and
//! a fabricated vault key; the HTTP peer counts fully received requests.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const OUTPUT: &str = r#"{"choices":[{"message":{"content":"nothing-fits"}}]}"#;
const ANTHROPIC_OUTPUT: &str = r#"{"content":[{"type":"text","text":"nothing-fits"}]}"#;

struct TemporaryConfig(PathBuf);

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove this test's disposable config");
    }
}

struct Observed {
    response: Response,
    requests: Vec<String>,
    elapsed: Duration,
}

fn read_request(stream: &mut TcpStream) -> String {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
        headers.push_str(&line);
    }
    let mut body = vec![0; length.expect("Plan POST has a JSON body")];
    reader.read_exact(&mut body).unwrap();
    headers.push_str(&String::from_utf8(body).unwrap());
    headers
}

fn child_plan(case: &str) {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "test-provider".into();
    profile.endpoint = std::env::var("PLAN_TEST_ENDPOINT").unwrap();
    profile.models = vec![agent_provider_config::ModelDescriptor::new("test-model")];
    if case == "anthropic" {
        profile.protocol = ProviderProtocol::Anthropic;
        profile.endpoint = profile.endpoint.trim_end_matches("/v1").to_owned();
    }
    if case == "disabled" {
        profile.enabled = false;
    }
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
    }
    .save()
    .unwrap();
    if case != "missing-key" {
        let mut vault = SecretVault::open();
        vault.put(
            &vault_id("test-provider").unwrap(),
            &Secret::password(if case == "invalid-key" {
                "fabricated\ninvalid-key"
            } else {
                "fabricated-test-key"
            }),
        );
        vault.save();
        assert!(
            SecretVault::open()
                .get(&vault_id("test-provider").unwrap())
                .is_some()
        );
    }
    let response = handle(
        if case == "unauthorized" {
            PHOTOS_APP
        } else {
            AGENT_APP
        },
        Request::Plan {
            profile_id: "test-provider".into(),
            model_id: if case == "missing-model" {
                "absent-model"
            } else {
                "test-model"
            }
            .into(),
            body: serde_json::json!({"model":"test-model", "messages":[{"role":"user", "content":"test instruction"}]}),
            timeout_ms: match case {
                "timeout" => 200,
                "expired" => 0,
                _ => 10_000,
            },
        },
    );
    fs::write(
        Path::new(&std::env::var("XDG_CONFIG_HOME").unwrap()).join("response.json"),
        serde_json::to_vec(&response).unwrap(),
    )
    .unwrap();
}

fn run_case(test: &str, case: &str) -> Option<Observed> {
    if let Ok(child_case) = std::env::var("PLAN_TEST_CASE") {
        child_plan(&child_case);
        return None;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = TemporaryConfig(std::env::temp_dir().join(format!(
        "tilo-plan-{test}-{case}-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir(&temp.0).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let redirect = format!("{endpoint}/replayed");
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_case = case.to_owned();
    let peer = std::thread::spawn(move || {
        let mut requests = Vec::new();
        let mut held = Vec::new();
        while !server_stop.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("accept local Plan request: {error}"),
            };
            requests.push(read_request(&mut stream));
            let reply = match server_case.as_str() {
                "lost" => continue,
                "timeout" => {
                    held.push(stream);
                    continue;
                }
                "truncated" => {
                    "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{partial"
                        .into()
                }
                "redirect" => format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                value if value.parse::<u16>().is_ok() => format!(
                    "HTTP/1.1 {value} Provider Error\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                _ => {
                    let body = if server_case == "anthropic" {
                        ANTHROPIC_OUTPUT
                    } else {
                        OUTPUT
                    };
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
            };
            stream.write_all(reply.as_bytes()).unwrap();
        }
        requests
    });
    let began = Instant::now();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("plan_tests::{test}"), "--nocapture"])
        // No ambient credentials, proxy settings, settings path, or secret-tool.
        .env_clear()
        .env("PATH", &temp.0)
        .env("XDG_CONFIG_HOME", &temp.0)
        .env("PLAN_TEST_CASE", case)
        .env("PLAN_TEST_ENDPOINT", endpoint)
        .output()
        .unwrap();
    let elapsed = began.elapsed();
    stop.store(true, Ordering::Release);
    let requests = peer.join().unwrap();
    assert!(
        child.status.success(),
        "child Plan path failed: {} {}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let response =
        serde_json::from_slice(&fs::read(temp.0.join("response.json")).unwrap()).unwrap();
    Some(Observed {
        response,
        requests,
        elapsed,
    })
}

fn assert_unknown(observed: Observed) {
    assert_eq!(observed.requests.len(), 1, "one accepted Plan POST");
    assert!(!observed.response.ok);
    assert!(observed.response.output.is_none());
    let error = observed.response.error.unwrap();
    assert!(error.contains("provider outcome is unknown"), "{error}");
    assert!(error.contains("request was not retried"), "{error}");
    assert!(error.len() <= 160, "bounded error: {error}");
    assert!(!error.contains("fabricated-test-key"));
    assert!(!error.contains("test instruction"));
    assert!(observed.elapsed < Duration::from_secs(5));
}

#[test]
fn plan_dispatch_lost_response_is_unknown() {
    if let Some(observed) = run_case("plan_dispatch_lost_response_is_unknown", "lost") {
        assert_unknown(observed);
    }
}

#[test]
fn plan_dispatch_timeout_is_unknown() {
    if let Some(observed) = run_case("plan_dispatch_timeout_is_unknown", "timeout") {
        assert_unknown(observed);
    }
}

#[test]
fn plan_dispatch_unreadable_response_is_unknown() {
    if let Some(observed) = run_case("plan_dispatch_unreadable_response_is_unknown", "truncated") {
        assert_unknown(observed);
    }
}

#[test]
fn plan_dispatch_never_replays_provider_status() {
    for status in [503, 408, 429, 500, 502, 504, 529, 401] {
        let Some(observed) = run_case(
            "plan_dispatch_never_replays_provider_status",
            &status.to_string(),
        ) else {
            return;
        };
        assert_unknown(observed);
    }
}

#[test]
fn plan_dispatch_never_replays_redirect() {
    if let Some(observed) = run_case("plan_dispatch_never_replays_redirect", "redirect") {
        assert_unknown(observed);
    }
}

#[test]
fn plan_dispatch_preserves_success_and_protocol_credentials() {
    for case in ["openai", "anthropic"] {
        let Some(observed) = run_case(
            "plan_dispatch_preserves_success_and_protocol_credentials",
            case,
        ) else {
            return;
        };
        assert_eq!(observed.requests.len(), 1);
        assert!(observed.response.ok);
        assert!(observed.response.error.is_none());
        let request = &observed.requests[0];
        if case == "anthropic" {
            assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
            assert!(request.contains("x-api-key: fabricated-test-key\r\n"));
            assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
            assert_eq!(observed.response.output.as_deref(), Some(ANTHROPIC_OUTPUT));
        } else {
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
            assert!(request.contains("authorization: Bearer fabricated-test-key\r\n"));
            assert_eq!(observed.response.output.as_deref(), Some(OUTPUT));
        }
        assert!(request.contains("test instruction"));
    }
}

#[test]
fn plan_dispatch_validation_failures_send_nothing() {
    for (case, reason) in [
        ("disabled", "provider is not enabled"),
        ("missing-model", "model is not configured for this provider"),
        ("missing-key", "API key is not configured"),
        ("invalid-key", "provider transport failed"),
        (
            "unauthorized",
            "only tilo-agent may plan through the model gateway",
        ),
        ("expired", "provider request timed out"),
    ] {
        let Some(observed) = run_case("plan_dispatch_validation_failures_send_nothing", case)
        else {
            return;
        };
        assert!(observed.requests.is_empty(), "{case}");
        assert!(!observed.response.ok);
        assert_eq!(observed.response.error.as_deref(), Some(reason));
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_gateway_host_refuses_peer_authentication() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    assert!(!same_user(&stream));
}
