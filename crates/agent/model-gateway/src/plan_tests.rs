//! Real Plan dispatch tests. Each subprocess owns disposable configuration and
//! a fabricated vault key; the HTTP peer counts fully received requests.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_model_gateway::{EVENT_TOOL_CALL_DELTA, MAX_DELTA_EVENTS};

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
    events: Vec<Response>,
    stderr: String,
}

const STREAM_REPLY: &str = concat!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    "data: {\"id\":\"c1\",\"created\":1,\"model\":\"test-model-2026\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"list_operations\",\"arguments\":\"\"}}]}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"search\\\":\\\"x\\\"}\"}}]}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"plan\",\"arguments\":\"{}\"}}]}}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7}}\n\n",
    "data: [DONE]\n\n",
);

const ERROR_BODY: &str = r#"{"error":{"message":"test instruction echoed back","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#;

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

fn child_profile(case: &str) -> ProviderProfile {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "test-provider".into();
    profile.endpoint = std::env::var("PLAN_TEST_ENDPOINT").unwrap();
    profile.models = vec![agent_provider_config::ModelDescriptor::new("test-model")];
    if case.starts_with("limits-") {
        profile.token_limit_param =
            Some(agent_provider_config::TokenLimitParam::MaxCompletionTokens);
        profile.models[0].request_limits = Some(agent_provider_config::ModelRequestLimits {
            max_output_tokens: 512,
            runtime_context_tokens: Some(4096),
            plan_timeout_ms: match case {
                "limits-owner-long" => Some(120_000),
                "limits-owner-short" => Some(200),
                "limits-owner-caller" => Some(900_000),
                "limits-owner-invalid" => Some(900_001),
                _ => None,
            },
        });
        profile.omit_params = vec!["max_completion_tokens".into(), "max_tokens".into()];
    }
    if case == "limits-other-owner" {
        let mut other = agent_provider_config::ModelDescriptor::new("other-model");
        other.request_limits = Some(agent_provider_config::ModelRequestLimits {
            max_output_tokens: 512,
            runtime_context_tokens: None,
            plan_timeout_ms: Some(900_000),
        });
        profile.models.push(other);
    }
    if case != "local-unauthenticated" && case != "local-nonloopback" && case != "local-dns-name" {
        profile.credential = Some(agent_provider_config::SecretReference::provider_api_key(
            &profile.id,
        ));
    }
    if case == "local-nonloopback" {
        profile.endpoint = "http://192.0.2.1/v1".into();
    }
    if case == "local-dns-name" {
        profile.endpoint = profile.endpoint.replace("127.0.0.1", "localhost");
    }

    if case == "anthropic" {
        profile.protocol = ProviderProtocol::Anthropic;
        profile.endpoint = profile.endpoint.trim_end_matches("/v1").to_owned();
    }
    if case == "disabled" {
        profile.enabled = false;
    }
    if case == "effort" {
        profile.token_limit_param = Some(agent_provider_config::TokenLimitParam::MaxTokens);
        profile.reasoning = Some(agent_provider_config::ReasoningSetting {
            param: agent_provider_config::ReasoningParam::ReasoningObject,
            effort: "low".into(),
        });
    }
    profile
}

fn child_plan(case: &str) {
    let profile = child_profile(case);
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
    }
    .save()
    .unwrap();
    if !matches!(
        case,
        "missing-key" | "local-unauthenticated" | "local-nonloopback" | "local-dns-name"
    ) {
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
    if case == "local-unauthenticated" {
        assert!(
            selection_configured(
                "test-provider",
                "test-model",
                ProviderProtocol::OpenAiCompatible
            )
            .is_ok()
        );
        assert!(
            SecretVault::open()
                .get(&vault_id("test-provider").unwrap())
                .is_none()
        );
    }
    let body = match case {
        "limits-cap" => serde_json::json!({"max_completion_tokens":4096}),
        "limits-short" => serde_json::json!({"max_tokens":64}),
        "limits-malformed" => serde_json::json!({"max_tokens":false}),
        "limits-conflicting" => serde_json::json!({"max_tokens":64,"max_completion_tokens":4096}),
        // The caller names another model and asks for a raw stream; the
        // gateway sends the checked model and a plain completion request.
        "override" => serde_json::json!({
            "model": "unlisted-model", "stream": true, "max_tokens": 64,
            "messages": [{"role":"user", "content":"test instruction"}]
        }),
        _ => {
            serde_json::json!({"model":"test-model", "messages":[{"role":"user", "content":"test instruction"}]})
        }
    };
    let mut events = Vec::new();
    let http = provider_client().unwrap();
    let response = tilo_rt::block_on(handle_streaming(
        if case == "unauthorized" {
            PHOTOS_APP
        } else {
            AGENT_APP
        },
        child_request(case, body),
        &http,
        &mut |event| {
            events.push(serde_json::to_value(event).unwrap());
            true
        },
    ))
    .unwrap();
    let root = std::env::var("XDG_CONFIG_HOME").unwrap();
    fs::write(
        Path::new(&root).join("response.json"),
        serde_json::to_vec(&response).unwrap(),
    )
    .unwrap();
    fs::write(
        Path::new(&root).join("events.json"),
        serde_json::to_vec(&events).unwrap(),
    )
    .unwrap();
}

fn child_request(case: &str, body: serde_json::Value) -> Request {
    if case.ends_with("transform") {
        Request::Transform {
            profile_id: "test-provider".into(),
            model_id: "test-model".into(),
            instructions: "Return JSON.".into(),
            input: "bounded fixture".into(),
            max_output_tokens: 2048,
            timeout_ms: 10_000,
        }
    } else {
        Request::Plan {
            profile_id: "test-provider".into(),
            model_id: if case == "missing-model" {
                "absent-model"
            } else {
                "test-model"
            }
            .into(),
            body,
            timeout_ms: match case {
                "timeout" => 200,
                "expired" => 0,
                "limits-deadline" | "limits-owner-long" | "limits-other-owner" => 120_000,
                "limits-owner-caller" => 100,
                _ => 10_000,
            },
            stream: case == "stream" || case == "stream-deltas",
            deltas: case == "stream-deltas",
            trace: Some(PlanTrace { id: 7, turn: 1 }),
        }
    }
}

#[test]
fn plan_dispatch_enforces_selected_model_allowance_after_omission() {
    for case in [
        "limits-cap",
        "limits-short",
        "limits-deadline",
        "limits-malformed",
        "limits-conflicting",
    ] {
        let Some(observed) = run_case(
            "plan_dispatch_enforces_selected_model_allowance_after_omission",
            case,
        ) else {
            return;
        };
        if matches!(case, "limits-malformed" | "limits-conflicting") {
            assert!(!observed.response.ok);
            assert!(observed.requests.is_empty());
        } else {
            assert!(observed.response.ok);
            assert_eq!(observed.requests.len(), 1);
            let body = request_body(&observed.requests[0]);
            assert_eq!(
                body["max_completion_tokens"],
                if case == "limits-short" { 64 } else { 512 }
            );
            assert!(body.get("max_tokens").is_none());
            assert_eq!(body["model"], "test-model");
            assert!(
                observed
                    .stderr
                    .contains("source_context=- runtime_context=4096")
            );
            assert!(observed.stderr.contains(if case == "limits-deadline" {
                "timeout_ms=60000"
            } else {
                "timeout_ms=10000"
            }));
            assert!(observed.stderr.contains(if case == "limits-short" {
                "output_allowance=64"
            } else {
                "output_allowance=512"
            }));
            assert!(
                observed
                    .stderr
                    .contains("prompt_tokens=- completion_tokens=-")
            );
        }
    }
}

#[test]
fn shared_transport_limits_transform_without_changing_legacy_requests() {
    for case in ["limits-transform", "legacy-transform"] {
        let Some(observed) = run_case(
            "shared_transport_limits_transform_without_changing_legacy_requests",
            case,
        ) else {
            return;
        };
        assert!(observed.response.ok);
        assert_eq!(observed.requests.len(), 1);
        let body = request_body(&observed.requests[0]);
        if case == "limits-transform" {
            assert_eq!(body["max_completion_tokens"], 512);
            assert!(body.get("max_tokens").is_none());
        } else {
            assert_eq!(body["max_tokens"], 2048);
            assert!(body.get("max_completion_tokens").is_none());
        }
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][1]["content"], "bounded fixture");
    }
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
                "timeout" | "limits-owner-short" | "limits-owner-caller" => {
                    held.push(stream);
                    continue;
                }
                "truncated" => {
                    "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{partial"
                        .into()
                }
                "stream" | "stream-deltas" => STREAM_REPLY.into(),
                "400detail" => format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ERROR_BODY}",
                    ERROR_BODY.len()
                ),
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
        // Held connections stay open, unanswered, until the peer stops.
        drop(held);
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
    let events = serde_json::from_slice(&fs::read(temp.0.join("events.json")).unwrap()).unwrap();
    Some(Observed {
        response,
        requests,
        elapsed,
        events,
        stderr: String::from_utf8(child.stderr).unwrap(),
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
}

#[test]
fn plan_dispatch_lost_response_is_unknown() {
    if let Some(observed) = run_case("plan_dispatch_lost_response_is_unknown", "lost") {
        assert_unknown(observed);
    }
}

#[test]
fn plan_dispatch_owner_ceiling_preserves_shorter_caller_and_transport_bounds() {
    let name = "plan_dispatch_owner_ceiling_preserves_shorter_caller_and_transport_bounds";
    for (case, applied, succeeds) in [
        ("limits-owner-long", 120_000, true),
        ("limits-other-owner", 60_000, true),
        ("limits-owner-short", 200, false),
        ("limits-owner-caller", 100, false),
    ] {
        let Some(observed) = run_case(name, case) else {
            return;
        };
        assert!(observed.stderr.contains(&format!("timeout_ms={applied}")));
        if succeeds {
            assert!(observed.response.ok);
            assert_eq!(observed.requests.len(), 1);
        } else {
            assert!(observed.elapsed < Duration::from_secs(5));
            assert_unknown(observed);
        }
    }
    let Some(observed) = run_case(name, "limits-owner-invalid") else {
        return;
    };
    assert!(!observed.response.ok);
    assert!(observed.requests.is_empty());
}

#[test]
fn plan_dispatch_timeout_is_unknown() {
    if let Some(observed) = run_case("plan_dispatch_timeout_is_unknown", "timeout") {
        // The peer holds the connection: only the 200 ms Plan timeout, not the
        // 10 s one the other cases pass, ends the child before this bound.
        assert!(observed.elapsed < Duration::from_secs(10));
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
fn plan_dispatch_unauthenticated_policy_keeps_other_profiles_in_the_vault() {
    let mut profile = ProviderProfile::local_vllm();
    assert!(unauthenticated_local_plan(&profile).unwrap());
    profile.endpoint = "http://[::1]:8000/v1".into();
    assert!(unauthenticated_local_plan(&profile).unwrap());
    profile.endpoint = "http://user@127.0.0.1:8000/v1".into();
    assert!(unauthenticated_local_plan(&profile).is_err());
    profile.endpoint = "http://127.0.0.1:8000/v1".into();
    profile.credential = Some(agent_provider_config::SecretReference::provider_api_key(
        &profile.id,
    ));
    assert!(!unauthenticated_local_plan(&profile).unwrap());
    profile.credential = None;
    profile.location = EndpointLocation::PrivateNetwork;
    assert!(!unauthenticated_local_plan(&profile).unwrap());
    profile.location = EndpointLocation::Hosted;
    profile.endpoint = "https://provider.example/v1".into();
    assert!(!unauthenticated_local_plan(&profile).unwrap());
}

#[test]
fn plan_dispatch_allows_only_explicit_unauthenticated_literal_loopback() {
    for case in [
        "local-unauthenticated",
        "local-nonloopback",
        "local-dns-name",
    ] {
        let Some(observed) = run_case(
            "plan_dispatch_allows_only_explicit_unauthenticated_literal_loopback",
            case,
        ) else {
            return;
        };
        if case == "local-unauthenticated" {
            assert!(observed.response.ok);
            assert_eq!(observed.requests.len(), 1);
            assert!(
                !observed.requests[0]
                    .to_ascii_lowercase()
                    .contains("authorization:")
            );
            assert_eq!(observed.response.output.as_deref(), Some(OUTPUT));
        } else {
            assert!(!observed.response.ok);
            assert!(observed.requests.is_empty());
        }
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
    stream.set_nonblocking(true).unwrap();
    tilo_rt::block_on(async {
        let stream = tokio::net::UnixStream::from_std(stream).unwrap();
        assert!(!same_user(&stream));
    })
    .unwrap();
}

fn request_body(request: &str) -> serde_json::Value {
    // `read_request` keeps header lines, then the JSON body.
    serde_json::from_str(request.get(request.find('{').unwrap()..).unwrap()).unwrap()
}

#[test]
fn plan_dispatch_sends_the_checked_model_and_no_caller_stream() {
    let Some(observed) = run_case(
        "plan_dispatch_sends_the_checked_model_and_no_caller_stream",
        "override",
    ) else {
        return;
    };
    assert!(observed.response.ok);
    let body = request_body(&observed.requests[0]);
    assert_eq!(body["model"], "test-model");
    assert!(body.get("stream").is_none());
    // A local (this-device) profile keeps the caller's token-limit name and
    // gets no reasoning effort it did not configure.
    assert_eq!(body["max_tokens"], 64);
    assert!(body.get("reasoning_effort").is_none());
    assert!(observed.events.is_empty());
}

#[test]
fn plan_dispatch_applies_the_configured_reasoning_effort() {
    let Some(observed) = run_case(
        "plan_dispatch_applies_the_configured_reasoning_effort",
        "effort",
    ) else {
        return;
    };
    assert!(observed.response.ok);
    let body = request_body(&observed.requests[0]);
    assert_eq!(body["reasoning"], serde_json::json!({"effort": "low"}));
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn plan_dispatch_streams_completed_tool_calls_then_the_assembled_body() {
    let Some(observed) = run_case(
        "plan_dispatch_streams_completed_tool_calls_then_the_assembled_body",
        "stream",
    ) else {
        return;
    };
    assert_eq!(observed.requests.len(), 1);
    let body = request_body(&observed.requests[0]);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert!(observed.response.ok, "{:?}", observed.response.error);
    assert!(observed.response.event.is_none());
    let events = observed
        .events
        .iter()
        .map(|event| {
            assert_eq!(event.event.as_deref(), Some(EVENT_TOOL_CALL));
            serde_json::from_str::<serde_json::Value>(event.output.as_deref().unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["id"], "call_a");
    assert_eq!(events[0]["name"], "list_operations");
    assert_eq!(events[0]["arguments"], r#"{"search":"x"}"#);
    assert_eq!(events[1]["name"], "plan");
    let assembled: serde_json::Value =
        serde_json::from_str(observed.response.output.as_deref().unwrap()).unwrap();
    assert_eq!(assembled["object"], "chat.completion");
    assert_eq!(assembled["model"], "test-model-2026");
    assert_eq!(assembled["choices"][0]["finish_reason"], "tool_calls");
    let calls = assembled["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["id"], "call_b");
    assert_eq!(calls[1]["function"]["arguments"], "{}");
    assert_eq!(assembled["usage"]["completion_tokens"], 7);
}

#[test]
fn plan_dispatch_reports_bounded_provider_error_identifiers_only() {
    let Some(observed) = run_case(
        "plan_dispatch_reports_bounded_provider_error_identifiers_only",
        "400detail",
    ) else {
        return;
    };
    assert_eq!(observed.requests.len(), 1);
    let error = observed.response.error.unwrap();
    assert!(error.contains("provider returned HTTP 400"), "{error}");
    assert!(
        error.contains("type=invalid_request_error code=unsupported_parameter param=max_tokens"),
        "{error}"
    );
    assert!(!error.contains("test instruction"), "{error}");
    assert!(!error.contains("fabricated-test-key"), "{error}");
}

#[test]
fn plan_dispatch_streams_argument_deltas_only_when_asked() {
    let Some(observed) = run_case(
        "plan_dispatch_streams_argument_deltas_only_when_asked",
        "stream-deltas",
    ) else {
        return;
    };
    assert!(observed.response.ok, "{:?}", observed.response.error);
    let events = observed
        .events
        .iter()
        .map(|event| {
            (
                event.event.clone().unwrap(),
                serde_json::from_str::<serde_json::Value>(event.output.as_deref().unwrap())
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let kinds = events
        .iter()
        .map(|(kind, _)| kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            EVENT_TOOL_CALL_DELTA,
            EVENT_TOOL_CALL,
            EVENT_TOOL_CALL_DELTA,
            EVENT_TOOL_CALL
        ]
    );
    assert_eq!(
        events[0].1,
        serde_json::json!({"index": 0, "id": "call_a", "name": "list_operations", "delta": r#"{"search":"x"}"#})
    );
    assert_eq!(events[2].1["index"], 1);
    assert_eq!(events[2].1["name"], "plan");
    assert_eq!(events[2].1["delta"], "{}");
    // The final body is unchanged by deltas.
    let assembled: serde_json::Value =
        serde_json::from_str(observed.response.output.as_deref().unwrap()).unwrap();
    assert_eq!(
        assembled["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        r#"{"search":"x"}"#
    );
}

/// SSE text for a list of `chat.completion.chunk` values.
fn sse(chunks: &[serde_json::Value]) -> String {
    let mut text = String::new();
    for chunk in chunks {
        text.push_str("data: ");
        text.push_str(&chunk.to_string());
        text.push_str("\n\n");
    }
    text.push_str("data: [DONE]\n\n");
    text
}

fn args_chunk(
    index: usize,
    id: Option<&str>,
    name: Option<&str>,
    arguments: &str,
) -> serde_json::Value {
    let mut call = serde_json::json!({"index": index, "function": {"arguments": arguments}});
    if let Some(id) = id {
        call["id"] = id.into();
    }
    if let Some(name) = name {
        call["function"]["name"] = name.into();
    }
    serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [call]}}]})
}

fn finish_chunk() -> serde_json::Value {
    serde_json::json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
}

/// Run the SSE core over `chunks`; the clock reads `times_ms` (one per data
/// line) from a fixed base. Returns every emitted frame as (event, output).
fn feed(
    chunks: &[serde_json::Value],
    times_ms: &[u64],
    deltas: bool,
) -> (Vec<(String, serde_json::Value)>, String) {
    let text = sse(chunks);
    let mut reader = std::io::Cursor::new(text.into_bytes());
    let base = Instant::now();
    let mut times = times_ms.iter().copied();
    let mut clock = move || {
        base.checked_add(Duration::from_millis(times.next().unwrap_or(u64::MAX / 4)))
            .unwrap()
    };
    let mut frames = Vec::new();
    let mut timing = PlanTiming::start();
    let body = read_sse(
        &mut reader,
        &mut |event| {
            frames.push((
                event.event.clone().unwrap(),
                serde_json::from_str(event.output.as_deref().unwrap()).unwrap(),
            ));
            true
        },
        &mut timing,
        deltas,
        &mut clock,
    )
    .unwrap_or_else(|failure| panic!("{}", failure.why));
    (frames, body)
}

#[test]
fn streamed_reasoning_is_preserved_for_history_without_visible_delta_frames() {
    let chunks = [
        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":"provider "}}]}),
        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":"thought"}}]}),
        args_chunk(0, Some("call_1"), Some("list_operations"), "{}"),
        finish_chunk(),
    ];
    let (frames, body) = feed(&chunks, &[0, 10, 20, 30], true);
    let completion: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        completion["choices"][0]["message"]["reasoning_content"],
        "provider thought"
    );
    assert_eq!(
        completion["choices"][0]["message"]["content"],
        serde_json::Value::Null
    );
    assert!(
        frames
            .iter()
            .all(|(_, value)| !value.to_string().contains("provider thought"))
    );
    let replay = tilo_model_gateway::openai::read_turn(&completion).unwrap();
    assert_eq!(replay.message["reasoning_content"], "provider thought");
    assert_eq!(replay.calls[0].name, "list_operations");
    assert!(replay.text.is_none());
}

#[test]
fn malformed_streamed_reasoning_is_a_provider_failure() {
    let text = sse(&[
        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":{"wrong":"type"}}}]}),
        finish_chunk(),
    ]);
    let mut reader = std::io::Cursor::new(text.into_bytes());
    let mut timing = PlanTiming::start();
    let mut emitted = false;
    let result = read_sse(
        &mut reader,
        &mut |_event| {
            emitted = true;
            true
        },
        &mut timing,
        false,
        &mut Instant::now,
    );
    assert!(result.is_err());
    assert!(!emitted);
}

#[test]
fn deltas_are_coalesced_by_window_and_size() {
    let big = "y".repeat(300);
    let chunks = [
        args_chunk(0, Some("call_p"), Some("plan"), "{\"re"),
        args_chunk(0, None, None, "ply"),
        args_chunk(0, None, None, "\":\"h"),
        args_chunk(0, None, None, "i"),
        args_chunk(0, None, None, &big),
        args_chunk(0, None, None, "\"}"),
        finish_chunk(),
    ];
    // 0: first frame; 10, 20: inside the window; 45: window over;
    // 50: 300 buffered bytes force a frame; 60: buffered until the call ends.
    let (frames, body) = feed(&chunks, &[0, 10, 20, 45, 50, 60, 70], true);
    let deltas = frames
        .iter()
        .filter(|(kind, _)| kind == EVENT_TOOL_CALL_DELTA)
        .map(|(_, output)| output["delta"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        deltas,
        ["{\"re", "ply\":\"hi", big.as_str(), "\"}"],
        "coalesced at 40 ms / 256 bytes, flushed before the call"
    );
    let (kind, call) = frames.last().unwrap();
    assert_eq!(kind, EVENT_TOOL_CALL);
    assert_eq!(call["arguments"].as_str().unwrap(), deltas.concat());
    for (_, output) in &frames[..frames.len() - 1] {
        assert_eq!(output["index"], 0);
        assert_eq!(output["id"], "call_p");
        assert_eq!(output["name"], "plan");
    }
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        deltas.concat()
    );
}

#[test]
fn every_delta_of_a_call_precedes_its_tool_call() {
    let chunks = [
        args_chunk(0, Some("a"), Some("list_operations"), "{\"search\""),
        args_chunk(0, None, None, ":\"x\"}"),
        args_chunk(1, Some("b"), Some("plan"), "{\"why\""),
        args_chunk(1, None, None, ":\"z\"}"),
        finish_chunk(),
    ];
    // Everything lands inside one window after the first frame of each call.
    let (frames, _) = feed(&chunks, &[0, 1, 2, 3, 4], true);
    let summary = frames
        .iter()
        .map(|(kind, output)| {
            let text = if kind == EVENT_TOOL_CALL {
                output["arguments"].as_str().unwrap()
            } else {
                output["delta"].as_str().unwrap()
            };
            (kind.as_str(), text.to_owned())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (EVENT_TOOL_CALL_DELTA, "{\"search\"".to_owned()),
            (EVENT_TOOL_CALL_DELTA, ":\"x\"}".to_owned()),
            (EVENT_TOOL_CALL, "{\"search\":\"x\"}".to_owned()),
            (EVENT_TOOL_CALL_DELTA, "{\"why\"".to_owned()),
            (EVENT_TOOL_CALL_DELTA, ":\"z\"}".to_owned()),
            (EVENT_TOOL_CALL, "{\"why\":\"z\"}".to_owned()),
        ]
    );
    // A delta frame for a call without an id carries null.
    let (frames, _) = feed(
        &[args_chunk(0, None, Some("plan"), "{}"), finish_chunk()],
        &[0, 1],
        true,
    );
    assert_eq!(frames[0].1["id"], serde_json::Value::Null);
}

#[test]
fn delta_frames_have_their_own_bound() {
    let mut chunks = (0..MAX_DELTA_EVENTS + 100)
        .map(|_| args_chunk(0, Some("a"), Some("plan"), "x"))
        .collect::<Vec<_>>();
    chunks.push(finish_chunk());
    // Each chunk arrives a full window after the previous one.
    let times = (0..chunks.len() as u64).map(|i| i * 50).collect::<Vec<_>>();
    let (frames, _) = feed(&chunks, &times, true);
    let deltas = frames
        .iter()
        .filter(|(kind, _)| kind == EVENT_TOOL_CALL_DELTA)
        .count();
    assert_eq!(deltas, MAX_DELTA_EVENTS);
    let (kind, call) = frames.last().unwrap();
    assert_eq!(kind, EVENT_TOOL_CALL, "the tool call still follows");
    assert_eq!(
        call["arguments"].as_str().unwrap().len(),
        MAX_DELTA_EVENTS + 100
    );
}

#[test]
fn no_delta_frames_without_the_flag() {
    let chunks = [
        args_chunk(0, Some("a"), Some("plan"), "{\"reply\""),
        args_chunk(0, None, None, ":\"hi\"}"),
        finish_chunk(),
    ];
    let (frames, _) = feed(&chunks, &[0, 100, 200], false);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, EVENT_TOOL_CALL);
    assert_eq!(frames[0].1["arguments"], "{\"reply\":\"hi\"}");
}

#[test]
fn old_plan_requests_decode_without_deltas_and_never_serialize_it() {
    let old = serde_json::json!({
        "verb": "plan", "profile_id": "p", "model_id": "m", "body": {},
        "timeout_ms": 5, "stream": true
    });
    let Request::Plan { deltas, stream, .. } = serde_json::from_value(old).unwrap() else {
        panic!("a plan request");
    };
    assert!(stream && !deltas);
    let encoded = serde_json::to_value(Request::Plan {
        profile_id: "p".into(),
        model_id: "m".into(),
        body: serde_json::json!({}),
        timeout_ms: 5,
        stream: true,
        deltas: false,
        trace: None,
    })
    .unwrap();
    assert!(encoded.get("deltas").is_none());
}

#[test]
fn content_free_stream_progress_survives_unfinished_reasoning_response() {
    let mut reader = SseReader::new(false);
    let mut timing = PlanTiming::start();
    let mut emit = |_event: &Response| true;
    let mut clock = Instant::now;
    for chunk in [
        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":"private"}}]}),
        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":"💡"}}]}),
    ] {
        let line = format!("data: {chunk}\n");
        assert_eq!(
            reader
                .line(line.as_bytes(), &mut emit, &mut timing, &mut clock)
                .unwrap_or_else(|_| panic!("valid SSE fixture must parse")),
            Flow::More
        );
    }
    let last_reasoning = timing.last_reasoning;
    for chunk in [
        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":""}}]}),
        serde_json::json!({"choices":[{"index":1,"delta":{"reasoning_content":"foreign"}}]}),
        serde_json::json!({"usage":{"completion_tokens":17},"choices":[]}),
    ] {
        let line = format!("data: {chunk}\n");
        reader
            .line(line.as_bytes(), &mut emit, &mut timing, &mut clock)
            .unwrap_or_else(|_| panic!("valid SSE fixture must parse"));
    }
    assert!(reader.finish(&mut emit, &mut timing).is_err());
    assert_eq!(timing.stream_chunks, 5);
    assert_eq!(timing.reasoning_chunks, 2);
    assert_eq!(timing.reasoning_bytes, 11);
    assert_eq!(timing.last_reasoning, last_reasoning);
    assert!(timing.first_reasoning.is_some());
    assert!(timing.last_stream >= timing.last_reasoning);
    // Missing terminal usage stays unknown; chunk/byte counts are not tokens.
    assert!(timing.completion_tokens.is_none());
    let fields = timing.progress_log_fields();
    assert!(fields.contains("reasoning_chunks=2 reasoning_bytes=11"));
    for private in ["private", "💡", "foreign", "reasoning_content"] {
        assert!(!fields.contains(private));
    }
}
