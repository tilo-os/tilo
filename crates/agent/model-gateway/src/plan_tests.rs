//! Real Plan dispatch tests. Each subprocess owns disposable configuration and
//! a fabricated vault key; the HTTP peer counts fully received requests.
use super::*;
use crate::EVENT_TOOL_CALL_DELTA;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const OUTPUT: &str = r#"{"choices":[{"message":{"role":"assistant","content":"nothing-fits"},"finish_reason":"stop"}]}"#;
const ANTHROPIC_OUTPUT: &str = r#"{"type":"message","role":"assistant","content":[{"type":"text","text":"nothing-fits"}],"stop_reason":"end_turn"}"#;

struct TemporaryConfig(PathBuf);

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove this test's disposable config");
    }
}

struct Observed {
    response: Response,
    requests: Vec<String>,
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

/// An Anthropic Messages stream as the provider writes it: `event:` lines,
/// a `ping`, a tool call whose input arrives in pieces, one with no input.
const ANTHROPIC_STREAM_REPLY: &str = concat!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"test-model-2026\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":3,\"output_tokens\":1}}}\n\n",
    "event: ping\ndata: {\"type\": \"ping\"}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"list_operations\",\"input\":{}}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"search\\\":\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" \\\"x\\\"}\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"plan\",\"input\":{}}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":7}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

const ERROR_BODY: &str = r#"{"error":{"message":"test instruction echoed back","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#;
const QUOTA_BODY: &str = r#"{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}"#;

/// One request as the peer read it, or `None` when the client closed the
/// connection before a whole request arrived.
fn read_request(stream: &mut TcpStream) -> Option<String> {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            return None;
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
        headers.push_str(&line);
    }
    let mut body = vec![0; length.expect("Plan POST has a JSON body")];
    reader.read_exact(&mut body).ok()?;
    headers.push_str(&String::from_utf8(body).unwrap());
    Some(headers)
}

fn prepare_plan_credentials(case: &str, profile: &ProviderProfile) {
    if case != "unavailable-storage" {
        // The subprocess has a disposable, explicit test root, not a live
        // Secret Service session. Opt into local test custody deliberately.
        let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
        let (reference, authorization) = credential_binding(profile).unwrap();
        ServiceCredentials::open_in(&root)
            .store(
                &reference,
                &authorization,
                if case == "invalid-key" {
                    "fabricated\ninvalid-key"
                } else {
                    "fabricated-test-key"
                },
            )
            .unwrap();
        if case == "missing-key" {
            // An absent key is meaningful only after custody was established.
            // Removing the disposable entry leaves a valid empty sealed vault.
            assert!(
                ServiceCredentials::open_in(&root)
                    .remove(&reference, &authorization)
                    .unwrap()
            );
        }
        assert_eq!(
            ServiceCredentials::open()
                .status(&reference, &authorization)
                .unwrap(),
            case != "missing-key"
        );
    }
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

    if case.starts_with("anthropic") {
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
        profiles: vec![profile.clone()],
        default_profile: None,
        ..Default::default()
    }
    .save()
    .unwrap();
    if !matches!(
        case,
        "local-unauthenticated" | "local-nonloopback" | "local-dns-name"
    ) {
        prepare_plan_credentials(case, &profile);
    }
    if case == "local-unauthenticated" {
        // Establish disposable custody before querying an empty vault.
        let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
        let (reference, authorization) = credential_binding(&profile).unwrap();
        let store = ServiceCredentials::open_in(&root);
        store
            .store(&reference, &authorization, "temporary-test-key")
            .unwrap();
        store.remove(&reference, &authorization).unwrap();
        assert!(
            selection_configured(
                "test-provider",
                "test-model",
                ProviderProtocol::OpenAiCompatible
            )
            .is_ok()
        );
        assert!(!secret_status("test-provider").unwrap());
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
    if matches!(
        case,
        "timeout" | "limits-owner-short" | "limits-owner-caller"
    ) {
        // A Plan deadline runs from the call's start, and the first turn of
        // a conversation also reads the profile and unseals its key, which
        // takes seconds on a loaded machine. Later turns take both from
        // `PLAN_PROFILES`. These cases are such a later turn, so their 100
        // or 200 ms usually go to the held peer rather than the vault.
        plan_profile("test-provider", "test-model").unwrap();
    }
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

fn child_request(case: &str, mut body: serde_json::Value) -> Request {
    if matches!(case, "stream" | "stream-deltas" | "anthropic-stream") {
        body["tools"] = if case == "anthropic-stream" {
            serde_json::json!([
                {"name":"list_operations", "input_schema":{"type":"object"}},
                {"name":"plan", "input_schema":{"type":"object"}}
            ])
        } else {
            serde_json::json!([
                {"type":"function", "function":{"name":"list_operations", "parameters":{"type":"object"}}},
                {"type":"function", "function":{"name":"plan", "parameters":{"type":"object"}}}
            ])
        };
    }
    if case.starts_with("complete-") {
        let mut request = agent_provider_config::protocol::Conversation::new("system", "hello")
            .request(Vec::new(), 64);
        if case == "complete-image" {
            request.messages[1].content = vec![agent_provider_config::protocol::Content::Image {
                media_type: "image/unsupported".into(),
                base64: "secret-never-echo".into(),
            }];
        }
        Request::Complete {
            profile_id: "test-provider".into(),
            model_id: "test-model".into(),
            request,
            timeout_ms: 10_000,
            stream: false,
        }
    } else if case.ends_with("transform") {
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
            stream: matches!(case, "stream" | "stream-deltas" | "anthropic-stream"),
            deltas: matches!(case, "stream-deltas" | "anthropic-stream"),
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

/// Whether the peer refuses request number `request` (from 1) of `case`
/// unprocessed: a `-once` case its first, `429-quota` every one.
fn refuses(case: &str, request: usize) -> bool {
    (case.ends_with("-once") && request == 1) || case == "429-quota"
}

fn refusal(case: &str) -> String {
    if case == "429-quota" {
        return format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{QUOTA_BODY}",
            QUOTA_BODY.len()
        );
    }
    format!(
        "HTTP/1.1 {} Provider Busy\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        case.trim_end_matches("-once")
    )
}

fn completion_error_reply(case: &str) -> String {
    match case {
        "complete-billing" => {
            let body = r#"{"error":{"code":"insufficient_quota","message":"secret-never-echo"}}"#;
            format!(
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        "complete-context" => {
            let body =
                r#"{"error":{"code":"context_length_exceeded","message":"secret-never-echo"}}"#;
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        "complete-malformed" => {
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"secret-never-echo"},"finish_reason":"unknown"}]}"#;
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        _ => panic!("unknown completion fixture: {case}"),
    }
}

fn temporary_config(test: &str, case: &str) -> TemporaryConfig {
    let nonce = tilo_clock::try_unix_now_ns().unwrap();
    let temp = TemporaryConfig(std::env::temp_dir().join(format!(
        "tilo-plan-{test}-{case}-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir(&temp.0).unwrap();
    temp
}

fn run_case(test: &str, case: &str) -> Option<Observed> {
    if let Ok(child_case) = std::env::var("PLAN_TEST_CASE") {
        child_plan(&child_case);
        return None;
    }
    let temp = temporary_config(test, case);
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
            let Some(request) = read_request(&mut stream) else {
                continue;
            };
            requests.push(request);
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
                case if case.starts_with("complete-") => completion_error_reply(case),
                "stream" | "stream-deltas" => STREAM_REPLY.into(),
                "anthropic-stream" => ANTHROPIC_STREAM_REPLY.into(),
                "400detail" => format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ERROR_BODY}",
                    ERROR_BODY.len()
                ),
                "redirect" => format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                case if refuses(case, requests.len()) => refusal(case),
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
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("service::plan_tests::{test}"),
            "--nocapture",
        ])
        // No ambient credentials, proxy settings, settings path, or secret-tool.
        .env_clear()
        .env("PATH", &temp.0)
        .env("XDG_CONFIG_HOME", &temp.0)
        .env("PLAN_TEST_CASE", case)
        .env("PLAN_TEST_ENDPOINT", endpoint)
        .output()
        .unwrap();
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
        events,
        stderr: String::from_utf8(child.stderr).unwrap(),
    })
}

#[test]
fn completion_service_preserves_typed_failures_without_replay_or_message_leaks() {
    use agent_provider_config::protocol::{ErrorKind, ModelError};
    for (case, kind, requests) in [
        ("complete-billing", ErrorKind::CreditsExhausted, 1),
        ("complete-context", ErrorKind::ContextLimit, 1),
        ("complete-malformed", ErrorKind::InvalidResponse, 1),
        ("complete-image", ErrorKind::InvalidRequest, 0),
    ] {
        let Some(observed) = run_case(
            "completion_service_preserves_typed_failures_without_replay_or_message_leaks",
            case,
        ) else {
            return;
        };
        assert_eq!(observed.requests.len(), requests, "{case}");
        assert!(!observed.response.ok, "{case}");
        let error = observed.response.failure_error().model_error();
        assert_eq!(error, ModelError::new(kind), "{case}");
        let wire = serde_json::to_string(&observed.response).unwrap();
        assert!(!wire.contains("secret-never-echo"), "{case}");
        assert_eq!(observed.response.model_error, Some(error));
    }
}

/// How long the call took by its own log line, not timed around the child
/// process, whose start-up on a loaded machine is not the call's.
fn done_ms(observed: &Observed) -> u64 {
    observed
        .stderr
        .split_whitespace()
        .find_map(|field| field.strip_prefix("done_ms="))
        .and_then(|ms| ms.parse().ok())
        .unwrap_or_else(|| panic!("the call logs its duration: {}", observed.stderr))
}

/// A call its deadline ended, not the held peer: never answered, asked once
/// at most, and unknown, never retried, once its POST was out. The deadline
/// runs on the wall clock from the call's start, so whether the POST goes
/// out within 100 or 200 ms is up to the machine's scheduler: on a loaded
/// one the call may end before it, which is as correct.
fn assert_ended_by_deadline(observed: Observed, case: &str) {
    assert!(observed.requests.len() <= 1, "{case}: never retried");
    if observed.requests.is_empty() {
        assert!(!observed.response.ok, "{case}");
        assert!(observed.response.output.is_none(), "{case}");
        let error = observed.response.error.unwrap();
        assert!(error.contains("timed out"), "{case}: {error}");
        assert!(error.len() <= 160, "bounded error: {error}");
        assert!(!error.contains("fabricated-test-key"));
    } else {
        assert_unknown(observed);
    }
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
            let done_ms = done_ms(&observed);
            assert!(done_ms < 5_000, "the call took {done_ms} ms");
            assert_ended_by_deadline(observed, case);
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
        // 10 s one the other cases pass, ends the call before this bound.
        // Timed by the call's own log line, not around the child process,
        // whose start-up on a loaded machine is not the call's.
        assert!(
            observed.stderr.contains("timeout_ms=200"),
            "{}",
            observed.stderr
        );
        let done_ms = done_ms(&observed);
        assert!(done_ms < 10_000, "the call took {done_ms} ms");
        assert_ended_by_deadline(observed, "timeout");
    }
}

#[test]
fn plan_dispatch_unreadable_response_is_unknown() {
    if let Some(observed) = run_case("plan_dispatch_unreadable_response_is_unknown", "truncated") {
        assert_unknown(observed);
    }
}

/// Every status that leaves open whether the provider did the work. A rate
/// limit (429) or overload (529) says it did not; see
/// `plan_dispatch_asks_again_only_when_the_provider_provably_did_not_take_it`.
#[test]
fn plan_dispatch_never_replays_provider_status() {
    for status in [503, 408, 500, 502, 504, 401] {
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
fn plan_dispatch_asks_again_only_when_the_provider_provably_did_not_take_it() {
    let name = "plan_dispatch_asks_again_only_when_the_provider_provably_did_not_take_it";
    // Phase: refused as rate-limited or overloaded, then answered: one
    // success, from the very same request sent again.
    for case in ["429-once", "529-once"] {
        let Some(observed) = run_case(name, case) else {
            return;
        };
        assert!(
            observed.response.ok,
            "{case}: {:?}",
            observed.response.error
        );
        assert_eq!(observed.response.output.as_deref(), Some(OUTPUT), "{case}");
        assert_eq!(observed.requests.len(), 2, "{case}");
        assert_eq!(observed.requests[0], observed.requests[1], "{case}");
    }

    // Phase: refused every time: bounded, and the error says how often.
    for case in ["429", "529"] {
        let Some(observed) = run_case(name, case) else {
            return;
        };
        assert_eq!(observed.requests.len(), 5, "{case}");
        assert!(!observed.response.ok);
        let error = observed.response.error.unwrap();
        assert!(error.contains(&format!("HTTP {case}")), "{error}");
        assert!(error.contains("(5 attempts)"), "{error}");
        assert!(!error.contains("outcome is unknown"), "{error}");
        assert!(!error.contains("fabricated-test-key"));
    }

    // Phase: an exhausted quota is a 429 that asking again cannot help.
    let Some(observed) = run_case(name, "429-quota") else {
        return;
    };
    assert_eq!(observed.requests.len(), 1);
    assert!(!observed.response.ok);
    let error = observed.response.error.unwrap();
    assert!(error.contains("provider credits are exhausted"), "{error}");
    assert!(error.contains("request was not retried"), "{error}");
}

struct Sending {
    left: Duration,
    waits: Vec<Duration>,
    calls: usize,
}

fn refused(status: Option<u16>, unprocessed: bool, asked_for: Option<Duration>) -> ProviderFailure {
    ProviderFailure {
        model_error: crate::openai::Failure::from_http(status.unwrap_or(0), "", None).model_error(),
        why: status.map_or_else(
            || "provider transport failed".into(),
            |status| format!("provider returned HTTP {status}"),
        ),
        again: Again::Soon(asked_for),
        outcome_unknown: !unprocessed,
        unprocessed,
        status,
    }
}

/// One fake attempt's outcome.
type Outcome = fn() -> Result<&'static str, ProviderFailure>;

/// [`plan_send`] over `outcomes` (the last repeats) on a fake clock, each
/// attempt taking 200 ms; `emitted` says whether a frame reached the caller.
fn send_plan(
    left: Duration,
    emitted: bool,
    outcomes: &[Outcome],
) -> (Result<&'static str, ProviderFailure>, Sending) {
    let sending = std::cell::RefCell::new(Sending {
        left,
        waits: Vec::new(),
        calls: 0,
    });
    let result = tilo_rt::block_on(plan_send(
        || {
            let sending = sending.borrow();
            (!sending.left.is_zero()).then_some(sending.left)
        },
        |duration| {
            let mut sending = sending.borrow_mut();
            sending.waits.push(duration);
            sending.left = sending.left.saturating_sub(duration);
            std::future::ready(())
        },
        &|| emitted,
        |_budget| {
            let mut sending = sending.borrow_mut();
            let index = sending.calls.min(outcomes.len().saturating_sub(1));
            sending.calls = sending.calls.saturating_add(1);
            sending.left = sending.left.saturating_sub(Duration::from_millis(200));
            std::future::ready(outcomes[index]())
        },
    ))
    .unwrap();
    (result, sending.into_inner())
}

/// Plan asks again only for a provider that provably did no work, never
/// once a frame reached the caller, at most five times in all, waiting what
/// the provider asked for (else a doubling pause from a second), and only
/// with room left for an answer.
#[test]
fn plan_send_replays_only_unprocessed_refusals_within_its_budget() {
    let answered: Outcome = || Ok("answer");
    let limited: Outcome = || Err(refused(Some(429), true, None));
    let overloaded_for_two: Outcome =
        || Err(refused(Some(529), true, Some(Duration::from_secs(2))));
    let unreachable: Outcome = || Err(refused(None, true, None));
    let failed: Outcome = || Err(refused(Some(500), false, None));

    // Phase: a rate limit, an overload or a refused connection, then an answer.
    for refusal in [limited, unreachable] {
        let (result, sending) = send_plan(Duration::from_secs(20), false, &[refusal, answered]);
        assert_eq!(result.ok(), Some("answer"));
        assert_eq!(sending.calls, 2);
        assert_eq!(sending.waits, [Duration::from_secs(1)]);
    }
    let (result, sending) = send_plan(
        Duration::from_secs(20),
        false,
        &[overloaded_for_two, answered],
    );
    assert_eq!(result.ok(), Some("answer"));
    assert_eq!(
        sending.waits,
        [Duration::from_secs(2)],
        "Retry-After is honoured"
    );

    // Phase: at most five attempts, and the error counts them.
    let (result, sending) = send_plan(Duration::from_secs(60), false, &[limited]);
    let failure = result.err().unwrap();
    assert_eq!(sending.calls, 5);
    assert_eq!(
        sending.waits,
        [1, 2, 4, 8].map(Duration::from_secs),
        "a busy shared quota is ridden out with a doubling pause"
    );
    assert_eq!(failure.status, Some(429));
    assert!(failure.why.ends_with("(5 attempts)"), "{}", failure.why);

    // Phase: a shorter budget stops asking once a pause would leave no room
    // to answer.
    let (result, sending) = send_plan(Duration::from_secs(20), false, &[limited]);
    assert!(result.is_err());
    assert_eq!(sending.calls, 4);
    assert_eq!(sending.waits, [1, 2, 4].map(Duration::from_secs));

    // Phase: a failure that may have done the work is never sent again.
    let (result, sending) = send_plan(Duration::from_secs(20), false, &[failed, answered]);
    let failure = result.err().unwrap();
    assert_eq!(sending.calls, 1);
    assert!(sending.waits.is_empty());
    assert_eq!(failure.why, "provider returned HTTP 500");

    // Phase: nothing is sent again once a frame reached the caller.
    let (result, sending) = send_plan(Duration::from_secs(20), true, &[limited, answered]);
    assert!(result.is_err());
    assert_eq!(sending.calls, 1);
    assert!(sending.waits.is_empty());

    // Phase: a wait that would leave no room to answer ends the Plan instead.
    let (result, sending) = send_plan(
        Duration::from_secs(6),
        false,
        &[overloaded_for_two, answered],
    );
    assert_eq!(result.err().unwrap().status, Some(529));
    assert_eq!(sending.calls, 1);
    assert!(sending.waits.is_empty());
}

/// A refused connection never reached the provider and a rate limit or
/// overload did no work there; an exhausted quota and every other status
/// leave the outcome as it was.
#[test]
fn only_a_refused_connection_a_rate_limit_or_an_overload_is_unprocessed() {
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut profile = ProviderProfile::local_vllm();
    profile.endpoint = format!("http://127.0.0.1:{closed}/v1");
    let http = provider_client().unwrap();
    let failure = tilo_rt::block_on(rig_adapter::legacy(
        &http,
        &profile,
        None,
        serde_json::json!({"model": "test-model"}),
        Duration::from_secs(5),
        None,
        true,
    ))
    .unwrap()
    .err()
    .unwrap();
    assert!(failure.unprocessed, "{}", failure.why);
    assert!(!failure.outcome_unknown, "{}", failure.why);
    assert_eq!(failure.status, None);

    assert!(refused_unprocessed(
        429,
        br#"{"error":{"type":"rate_limit_error"}}"#
    ));
    assert!(refused_unprocessed(429, b""));
    assert!(refused_unprocessed(
        529,
        br#"{"error":{"type":"overloaded_error"}}"#
    ));
    assert!(!refused_unprocessed(429, QUOTA_BODY.as_bytes()));
    for status in [400, 401, 402, 408, 409, 500, 502, 503, 504] {
        assert!(!refused_unprocessed(status, b""), "HTTP {status}");
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
        assert!(
            observed.response.ok,
            "{case}: {:?}",
            observed.response.error
        );
        assert!(observed.response.error.is_none());
        let request = &observed.requests[0];
        if case == "anthropic" {
            assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
            assert!(request.contains("x-api-key: fabricated-test-key\r\n"));
            assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
            assert_eq!(observed.response.output.as_deref(), Some(ANTHROPIC_OUTPUT));
            // The request on the wire carries the prompt-cache breakpoint.
            assert_eq!(
                request_body(request)["messages"][0]["content"][0]["cache_control"]["type"],
                "ephemeral"
            );
        } else {
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
            assert!(request.contains("authorization: Bearer fabricated-test-key\r\n"));
            assert_eq!(observed.response.output.as_deref(), Some(OUTPUT));
            assert!(!request.contains("cache_control"));
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
        (
            "unavailable-storage",
            "provider credential storage is unavailable; restore vault access and retry",
        ),
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
        error.contains("check model and provider settings"),
        "{error}"
    );
    assert!(!error.contains("test instruction"), "{error}");
    assert!(!error.contains("param=max_tokens"), "{error}");
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
            EVENT_TOOL_CALL_DELTA,
            EVENT_TOOL_CALL,
            EVENT_TOOL_CALL
        ]
    );
    assert_eq!(
        events[0].1,
        serde_json::json!({"index": 0, "id": "call_a", "name": "list_operations", "delta": r#"{"search":"x"}"#})
    );
    assert_eq!(
        events[1].1,
        serde_json::json!({"index":1,"id":"call_b","name":"plan","delta":"{}"})
    );
    assert_eq!(
        events[2].1,
        serde_json::json!({"id":"call_a","name":"list_operations","arguments":r#"{"search":"x"}"#})
    );
    assert_eq!(
        events[3].1,
        serde_json::json!({"id":"call_b","name":"plan","arguments":"{}"})
    );
    // The final body is unchanged by deltas.
    let assembled: serde_json::Value =
        serde_json::from_str(observed.response.output.as_deref().unwrap()).unwrap();
    let calls = assembled["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_a");
    assert_eq!(calls[0]["function"]["arguments"], r#"{"search":"x"}"#);
    assert_eq!(calls[1]["id"], "call_b");
    assert_eq!(calls[1]["function"]["arguments"], "{}");
}

/// SSE text for a list of `chat.completion.chunk` values.
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
fn plan_dispatch_streams_anthropic_tool_calls_then_the_message_body() {
    let Some(observed) = run_case(
        "plan_dispatch_streams_anthropic_tool_calls_then_the_message_body",
        "anthropic-stream",
    ) else {
        return;
    };
    assert_eq!(observed.requests.len(), 1);
    let request = &observed.requests[0];
    assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
    assert!(request.contains("x-api-key: fabricated-test-key\r\n"));
    let body = request_body(request);
    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());
    assert!(observed.response.ok, "{:?}", observed.response.error);
    assert!(observed.response.event.is_none());
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
            EVENT_TOOL_CALL_DELTA,
            EVENT_TOOL_CALL,
            EVENT_TOOL_CALL
        ],
        "{events:?}"
    );
    assert_eq!(
        events[2].1,
        serde_json::json!({"id": "toolu_a", "name": "list_operations", "arguments": "{\"search\": \"x\"}"})
    );
    // Deltas precede completion, but no completed call authorizes work until
    // Rig has validated the whole turn's authoritative finish.
    assert_eq!(
        events[1].1,
        serde_json::json!({"index": 1, "id": "toolu_b", "name": "plan", "delta": "{}"})
    );
    assert_eq!(
        events[3].1,
        serde_json::json!({"id": "toolu_b", "name": "plan", "arguments": "{}"})
    );
    // The final frame is the Messages body a non-streamed Plan returns.
    let assembled: serde_json::Value =
        serde_json::from_str(observed.response.output.as_deref().unwrap()).unwrap();
    assert_eq!(
        assembled,
        serde_json::json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "test-model-2026",
            "content": [
                {"type": "tool_use", "id": "toolu_a", "name": "list_operations", "input": {"search": "x"}},
                {"type": "tool_use", "id": "toolu_b", "name": "plan", "input": {}},
            ],
            "stop_reason": "tool_use", "stop_sequence": null,
            "usage": {"input_tokens": 5, "cache_read_input_tokens": 3, "output_tokens": 7},
        })
    );
    let timing = &observed.stderr;
    assert!(timing.contains(" stream=true "), "{timing}");
    for unmeasured in ["first_token_ms=-", "first_tool_call_ms=-", "plan_call_ms=-"] {
        assert!(!timing.contains(unmeasured), "{timing}");
    }
    assert!(
        timing.contains("prompt_tokens=5 completion_tokens=7"),
        "{timing}"
    );
    assert!(
        timing.contains("cached_tokens=3 cache_write_tokens=- tool_calls=2"),
        "{timing}"
    );
}

/// Anthropic SSE text for `events`, each after the `event:` line naming it,
/// as the provider writes them.
fn anthropic_sse(events: &[serde_json::Value]) -> String {
    let mut text = String::new();
    for event in events {
        text.push_str("event: ");
        text.push_str(event["type"].as_str().unwrap());
        text.push_str("\ndata: ");
        text.push_str(&event.to_string());
        text.push_str("\n\n");
    }
    text
}

/// A tool input as the model writes it: a unicode escape, escaped quotes and
/// backslash, a raw multi-byte character and an escaped surrogate pair.
const INVOKE_INPUT: &str = r#"{"operation":"broker:7:doc#type-text","parameters":{"text":"Café \"quoted\" \\ 🌊 🌊\nend"}}"#;
const PLAN_INPUT: &str = r#"{"reply":"Done.","done":true}"#;

/// A recorded-shape, non-streamed Messages body: thinking, text, a tool
/// call, more text, a second tool call.
fn anthropic_message() -> serde_json::Value {
    serde_json::json!({
        "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
        "type": "message",
        "role": "assistant",
        "model": "claude-test",
        "content": [
            {"type": "thinking", "thinking": "Read, then write.", "signature": "c2lnbmF0dXJl"},
            {"type": "text", "text": "Writing the paragraph — then I'm done."},
            {"type": "tool_use", "id": "toolu_01", "name": "invoke",
             "input": serde_json::from_str::<serde_json::Value>(INVOKE_INPUT).unwrap()},
            {"type": "text", "text": "Ünïcödé between calls."},
            {"type": "tool_use", "id": "toolu_02", "name": "plan",
             "input": serde_json::from_str::<serde_json::Value>(PLAN_INPUT).unwrap()},
        ],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {"input_tokens": 2095, "cache_creation_input_tokens": 0,
                  "cache_read_input_tokens": 1800, "output_tokens": 503},
    })
}

fn pieces(text: &str, cut: usize) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    chars
        .chunks(cut)
        .map(|piece| piece.iter().collect())
        .collect()
}

/// The events streaming `message` the way the provider does, every text,
/// thought and tool input (`inputs`, as written) cut into `cut`-character
/// deltas, so a cut lands inside escapes and multi-byte characters.
fn anthropic_events(
    message: &serde_json::Value,
    inputs: &[&str],
    cut: usize,
) -> Vec<serde_json::Value> {
    let mut opening = message.clone();
    opening["content"] = serde_json::json!([]);
    opening["stop_reason"] = serde_json::Value::Null;
    opening["usage"]["output_tokens"] = serde_json::json!(1);
    let mut events = vec![
        serde_json::json!({"type": "message_start", "message": opening}),
        serde_json::json!({"type": "ping"}),
    ];
    let mut inputs = inputs.iter();
    for (index, block) in message["content"].as_array().unwrap().iter().enumerate() {
        let delta = |delta: serde_json::Value| serde_json::json!({"type": "content_block_delta", "index": index, "delta": delta});
        let mut opened = block.clone();
        let streamed = match block["type"].as_str().unwrap() {
            "thinking" => {
                opened["thinking"] = "".into();
                opened.as_object_mut().unwrap().remove("signature");
                pieces(block["thinking"].as_str().unwrap(), cut)
                    .into_iter()
                    .map(|piece| delta(serde_json::json!({"type": "thinking_delta", "thinking": piece})))
                    .chain([delta(serde_json::json!({"type": "signature_delta", "signature": block["signature"]}))])
                    .collect::<Vec<_>>()
            }
            "text" => {
                opened["text"] = "".into();
                pieces(block["text"].as_str().unwrap(), cut)
                    .into_iter()
                    .map(|piece| delta(serde_json::json!({"type": "text_delta", "text": piece})))
                    .collect()
            }
            _ => {
                opened["input"] = serde_json::json!({});
                std::iter::once(String::new())
                    .chain(pieces(inputs.next().unwrap(), cut))
                    .map(|piece| {
                        delta(
                            serde_json::json!({"type": "input_json_delta", "partial_json": piece}),
                        )
                    })
                    .collect()
            }
        };
        events.push(serde_json::json!({"type": "content_block_start", "index": index, "content_block": opened}));
        events.extend(streamed);
        events.push(serde_json::json!({"type": "content_block_stop", "index": index}));
        events.push(serde_json::json!({"type": "ping"}));
    }
    events.push(serde_json::json!({
        "type": "message_delta",
        "delta": {"stop_reason": message["stop_reason"], "stop_sequence": null},
        "usage": {"output_tokens": message["usage"]["output_tokens"]},
    }));
    events.push(serde_json::json!({"type": "message_stop"}));
    events
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_rebuilds_the_non_streamed_message_however_it_is_cut() {
    let message = anthropic_message();
    for cut in [1, 2, 3, 7, 1000] {
        let events = anthropic_events(&message, &[INVOKE_INPUT, PLAN_INPUT], cut);
        for piece in [1, 2, 3, 5, 64, usize::MAX] {
            let (result, frames, timing) = rig_adapter::stream_tests::test_stream(
                anthropic_sse(&events).into_bytes(),
                ProviderProtocol::Anthropic,
                piece,
                true,
                true,
            )
            .await;
            let response = result.unwrap_or_else(|why| panic!("cut {cut}, piece {piece}: {why}"));
            assert_eq!(response.raw, message, "cut {cut}, piece {piece}");
            assert_eq!(response.usage.output_tokens, Some(503));
            assert_eq!(response.usage.cached_input_tokens, Some(1800));
            assert!(timing.first_reasoning.is_some() && timing.reasoning_chunks > 0);
            assert!(timing.first_token.is_some() && timing.plan_call.is_some());
            let calls = frames
                .iter()
                .filter(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL))
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 2);
            for (index, (call, (id, name, input))) in calls
                .iter()
                .zip([
                    ("toolu_01", "invoke", INVOKE_INPUT),
                    ("toolu_02", "plan", PLAN_INPUT),
                ])
                .enumerate()
            {
                let call: serde_json::Value =
                    serde_json::from_str(call.output.as_deref().unwrap()).unwrap();
                assert_eq!(call["id"], id);
                assert_eq!(call["name"], name);
                assert_eq!(call["arguments"], input);
                let mut delta = String::new();
                let mut completed = false;
                for frame in &frames {
                    let value: serde_json::Value =
                        serde_json::from_str(frame.output.as_deref().unwrap()).unwrap();
                    if frame.event.as_deref() == Some(EVENT_TOOL_CALL) && value["id"] == id {
                        completed = true;
                    }
                    if frame.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA)
                        && value["index"] == index
                    {
                        assert!(!completed, "all deltas precede authorization");
                        assert_eq!(value["id"], id);
                        delta.push_str(value["delta"].as_str().unwrap());
                    }
                    assert!(
                        !value.to_string().contains("Read, then")
                            && !value.to_string().contains("Writing")
                    );
                }
                assert_eq!(delta, input);
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_errors_and_eof_never_authorize_even_closed_tool_blocks() {
    let events = anthropic_events(&anthropic_message(), &[INVOKE_INPUT, PLAN_INPUT], 5);
    let first_stop = events
        .iter()
        .position(|event| event["type"] == "content_block_stop" && event["index"] == 2)
        .unwrap();
    let mut broken = Vec::new();
    for end in [events.len() - 1, first_stop + 1, first_stop - 1, 1] {
        broken.push(anthropic_sse(&events[..end]));
    }
    broken.push(anthropic_sse(&events[..=first_stop]) + "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"private-provider-content\"}}\n\n");
    let text = anthropic_sse(&events);
    broken.push(text.get(..text.rfind("\"usage\"").unwrap()).unwrap().into());
    let mut invalid_input = events[..first_stop].to_vec();
    invalid_input.push(serde_json::json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"tail\":"}}));
    invalid_input.extend_from_slice(&events[first_stop..]);
    broken.push(anthropic_sse(&invalid_input));
    for body in broken {
        let (result, frames, _) = rig_adapter::stream_tests::test_stream(
            body.into_bytes(),
            ProviderProtocol::Anthropic,
            3,
            true,
            true,
        )
        .await;
        let why = result.expect_err("a block stop is not a successful turn");
        assert!(!why.contains("private-provider-content"));
        assert!(
            frames
                .iter()
                .all(|frame| frame.event.as_deref() != Some(EVENT_TOOL_CALL))
        );
    }
}
