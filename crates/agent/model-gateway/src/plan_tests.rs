//! Real Plan dispatch tests. Each subprocess owns disposable configuration and
//! a fabricated vault key; the HTTP peer counts fully received requests.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::FromRawFd;
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
    if case == "concurrent" {
        child_concurrent_plans();
        return;
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

fn run_case(test: &str, case: &str) -> Option<Observed> {
    if let Ok(child_case) = std::env::var("PLAN_TEST_CASE") {
        child_plan(&child_case);
        return None;
    }
    let nonce = tilo_clock::try_unix_now_ns().unwrap();
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
    let server_root = temp.0.clone();
    let peer = std::thread::spawn(move || {
        let mut requests = Vec::new();
        let mut held: Vec<TcpStream> = Vec::new();
        while !server_stop.load(Ordering::Acquire) {
            if server_case == "concurrent" && server_root.join("chat-returned").exists() {
                for mut stream in std::mem::take(&mut held) {
                    let reply = successful_reply(OUTPUT);
                    stream.write_all(reply.as_bytes()).unwrap();
                }
            }
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("accept local Plan request: {error}"),
            };
            requests.push(read_request(&mut stream));
            if server_case == "concurrent" && requests.len() == 1 {
                held.push(stream);
                fs::write(server_root.join("task-accepted"), []).unwrap();
                continue;
            }
            match server_case.as_str() {
                "lost" => continue,
                "timeout" | "limits-owner-short" | "limits-owner-caller" => {
                    held.push(stream);
                    continue;
                }
                _ => {}
            }
            let reply = provider_reply(&server_case, &redirect, requests.len());
            stream.write_all(reply.as_bytes()).unwrap();
        }
        // Held connections stay open, unanswered, until the peer stops.
        drop(held);
        requests
    });
    let began = Instant::now();
    let child = plan_child(test, case, &temp.0, &endpoint);
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

fn plan_child(test: &str, case: &str, root: &Path, endpoint: &str) -> std::process::Output {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &format!("plan_tests::{test}"), "--nocapture"])
        // No ambient credentials, proxy settings, settings path, or secret-tool.
        .env_clear()
        .env("PATH", root)
        .env("XDG_CONFIG_HOME", root)
        .env("PLAN_TEST_CASE", case)
        .env("PLAN_TEST_ENDPOINT", endpoint);
    if case != "concurrent" {
        return command.output().unwrap();
    }
    let (app, server) = UnixStream::pair().unwrap();
    let app_fd = app.as_raw_fd();
    let server_fd = server.as_raw_fd();
    command
        .env(tilo_model_gateway::SERVICE_FD_ENV, app_fd.to_string())
        .env("PLAN_TEST_SERVER_FD", server_fd.to_string());
    // SAFETY: only async-signal-safe fcntl runs between fork and exec. The
    // child's descriptor environment is established before any threads exist.
    unsafe {
        command.pre_exec(move || {
            for fd in [app_fd, server_fd] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.output().unwrap()
}

fn provider_reply(case: &str, redirect: &str, request: usize) -> String {
    match case {
        "truncated" => {
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{partial".into()
        }
        "stream" | "stream-deltas" => STREAM_REPLY.into(),
        "anthropic-stream" => ANTHROPIC_STREAM_REPLY.into(),
        "400detail" => format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ERROR_BODY}",
            ERROR_BODY.len()
        ),
        "redirect" => format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        case if refuses(case, request) => refusal(case),
        value if value.parse::<u16>().is_ok() => format!(
            "HTTP/1.1 {value} Provider Error\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        "anthropic" => successful_reply(ANTHROPIC_OUTPUT),
        _ => successful_reply(OUTPUT),
    }
}

fn successful_reply(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn child_concurrent_plans() {
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap());
    let app_fd = std::env::var(tilo_model_gateway::SERVICE_FD_ENV)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let server_fd = std::env::var("PLAN_TEST_SERVER_FD")
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(app_fd >= 3 && server_fd >= 3 && app_fd != server_fd);
    // SAFETY: this isolated child owns these two distinct inherited sockets,
    // issued by plan_child, and transfers each descriptor exactly once.
    let mut client = unsafe { UnixStream::from_raw_fd(app_fd) };
    let server = unsafe { UnixStream::from_raw_fd(server_fd) };
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write_frame(
        &mut client,
        &serde_json::json!({"protocol":1,"app_id":AGENT_APP}),
    )
    .unwrap();
    server.set_nonblocking(true).unwrap();
    let gateway = std::thread::spawn(move || {
        let runtime = tilo_rt::daemon("gateway-concurrency-test")
            .workers(2)
            .build()
            .unwrap();
        runtime.block_on(async move {
            let stream = tokio::net::UnixStream::from_std(server).unwrap();
            serve_connection(stream, &provider_client().unwrap()).await;
        });
    });
    let (finished, task_result) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let result = tilo_model_gateway::plan(
            "test-provider",
            "test-model",
            serde_json::json!({"messages":[{"role":"user","content":"held task"}]}),
            Duration::from_secs(5),
        );
        finished.send(result).unwrap();
    });
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    while !root.join("task-accepted").exists() {
        assert!(
            Instant::now() < deadline,
            "actual task POST was not observed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let response = tilo_model_gateway::plan(
        "test-provider",
        "test-model",
        serde_json::json!({"messages":[{"role":"user","content":"chat during task"}]}),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(response, OUTPUT);
    assert!(
        matches!(
            task_result.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "chat completes while the original task POST remains held"
    );
    fs::write(root.join("chat-returned"), []).unwrap();
    assert_eq!(
        task_result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap(),
        OUTPUT
    );
    task.join().unwrap();
    // No content-bearing evidence is emitted; the existing harness keeps the
    // disposable request bodies private and counts the two actual POSTs.
    fs::write(
        root.join("response.json"),
        serde_json::to_vec(&succeeded(Some(response), None)).unwrap(),
    )
    .unwrap();
    fs::write(root.join("events.json"), b"[]").unwrap();
    client.shutdown(std::net::Shutdown::Both).unwrap();
    gateway.join().unwrap();
}

#[test]
fn concurrent_plans_share_one_authenticated_capability_without_blocking_chat() {
    if let Some(observed) = run_case(
        "concurrent_plans_share_one_authenticated_capability_without_blocking_chat",
        "concurrent",
    ) {
        assert!(observed.response.ok);
        assert_eq!(
            observed.requests.len(),
            2,
            "one task POST and one chat POST; no replay"
        );
        assert_eq!(
            request_body(&observed.requests[0])["messages"][0]["content"],
            "held task"
        );
        assert_eq!(
            request_body(&observed.requests[1])["messages"][0]["content"],
            "chat during task"
        );
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
        assert_eq!(observed.requests.len(), 3, "{case}");
        assert!(!observed.response.ok);
        let error = observed.response.error.unwrap();
        assert!(error.contains(&format!("HTTP {case}")), "{error}");
        assert!(error.contains("(3 attempts)"), "{error}");
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
    assert!(error.contains("insufficient_quota"), "{error}");
    assert!(error.contains("request was not retried"), "{error}");
}

struct Sending {
    left: Duration,
    waits: Vec<Duration>,
    calls: usize,
}

fn refused(status: Option<u16>, unprocessed: bool, asked_for: Option<Duration>) -> ProviderFailure {
    ProviderFailure {
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
/// once a frame reached the caller, at most three times in all, waiting what
/// the provider asked for, and only with room left for an answer.
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
        assert_eq!(sending.waits, [Duration::from_millis(500)]);
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

    // Phase: at most three attempts, and the error counts them.
    let (result, sending) = send_plan(Duration::from_secs(20), false, &[limited]);
    let failure = result.err().unwrap();
    assert_eq!(sending.calls, 3);
    assert_eq!(
        sending.waits,
        [Duration::from_millis(500), Duration::from_secs(1)]
    );
    assert_eq!(failure.status, Some(429));
    assert!(failure.why.ends_with("(3 attempts)"), "{}", failure.why);

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
    let failure = tilo_rt::block_on(send_provider(
        &http,
        &profile,
        &provider_url(&profile),
        None,
        &serde_json::json!({"model": "test-model"}),
        Duration::from_secs(5),
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
        assert!(observed.response.ok);
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

/// Every frame a stream emitted, as (event, output).
type Frames = Vec<(String, serde_json::Value)>;

/// Run `protocol`'s SSE core over `text`, delivered in transport pieces of
/// at most `piece` bytes (cut anywhere, mid-line and mid-character alike);
/// the clock reads `times_ms` (one per data line) from a fixed base.
/// Returns every emitted frame and the assembled body or why it failed.
fn run_sse(
    protocol: ProviderProtocol,
    text: &str,
    piece: usize,
    times_ms: &[u64],
    deltas: bool,
) -> (Frames, Result<String, String>) {
    let base = Instant::now();
    let mut times = times_ms.iter().copied();
    let mut clock = move || {
        base.checked_add(Duration::from_millis(times.next().unwrap_or(u64::MAX / 4)))
            .unwrap()
    };
    let mut frames = Vec::new();
    let mut timing = PlanTiming::start();
    let pieces = text.as_bytes().chunks(piece).collect::<Vec<_>>();
    let body = read_sse(
        SseReader::new(protocol, deltas),
        &pieces,
        &mut |event| {
            frames.push((
                event.event.clone().unwrap(),
                serde_json::from_str(event.output.as_deref().unwrap()).unwrap(),
            ));
            true
        },
        &mut timing,
        &mut clock,
    )
    .map_err(|failure| failure.why);
    (frames, body)
}

/// Run the OpenAI-compatible SSE core over `chunks`; the clock reads
/// `times_ms` (one per data line) from a fixed base.
fn feed(chunks: &[serde_json::Value], times_ms: &[u64], deltas: bool) -> (Frames, String) {
    let (frames, body) = run_sse(
        ProviderProtocol::OpenAiCompatible,
        &sse(chunks),
        usize::MAX,
        times_ms,
        deltas,
    );
    (frames, body.unwrap_or_else(|why| panic!("{why}")))
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
    let (frames, result) = run_sse(
        ProviderProtocol::OpenAiCompatible,
        &text,
        usize::MAX,
        &[],
        false,
    );
    assert!(result.is_err());
    assert!(frames.is_empty());
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
    let mut reader = SseReader::new(ProviderProtocol::OpenAiCompatible, false);
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
            EVENT_TOOL_CALL_DELTA,
            EVENT_TOOL_CALL
        ]
    );
    assert_eq!(
        events[2].1,
        serde_json::json!({"id": "toolu_a", "name": "list_operations", "arguments": "{\"search\": \"x\"}"})
    );
    // A call streamed without input carries the block's own empty input.
    assert_eq!(
        events[3].1,
        serde_json::json!({"index": 1, "id": "toolu_b", "name": "plan", "delta": "{}"})
    );
    assert_eq!(events[4].1["arguments"], "{}");
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

/// `text` in pieces of `cut` characters.
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

/// One clock reading per data line, each a full coalescing window apart.
fn windows(events: usize) -> Vec<u64> {
    (0..events as u64)
        .map(|line| line.saturating_mul(50))
        .collect()
}

#[test]
fn anthropic_stream_rebuilds_the_non_streamed_message_however_it_is_cut() {
    let message = anthropic_message();
    for cut in [1, 2, 3, 7, 1000] {
        let events = anthropic_events(&message, &[INVOKE_INPUT, PLAN_INPUT], cut);
        let text = anthropic_sse(&events);
        // Transport pieces cut lines, escapes and UTF-8 sequences anywhere.
        for piece in [1, 2, 3, 5, 64, usize::MAX] {
            let (frames, body) = run_sse(
                ProviderProtocol::Anthropic,
                &text,
                piece,
                &windows(events.len()),
                true,
            );
            let body: serde_json::Value =
                serde_json::from_str(&body.unwrap_or_else(|why| panic!("{why}"))).unwrap();
            assert_eq!(body, message, "cut {cut}, piece {piece}");
            let calls = frames
                .iter()
                .filter(|(kind, _)| kind == EVENT_TOOL_CALL)
                .map(|(_, call)| call)
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 2, "cut {cut}, piece {piece}");
            for (index, (call, (id, name, input))) in calls
                .iter()
                .zip([
                    ("toolu_01", "invoke", INVOKE_INPUT),
                    ("toolu_02", "plan", PLAN_INPUT),
                ])
                .enumerate()
            {
                assert_eq!(call["id"], id);
                assert_eq!(call["name"], name);
                assert_eq!(call["arguments"], input, "the input exactly as written");
                let deltas = frames
                    .iter()
                    .filter(|(kind, delta)| {
                        kind == EVENT_TOOL_CALL_DELTA && delta["index"] == index
                    })
                    .map(|(_, delta)| {
                        assert_eq!(delta["id"], id);
                        delta["delta"].as_str().unwrap()
                    })
                    .collect::<String>();
                assert_eq!(
                    deltas, input,
                    "every delta of call {index} precedes its call"
                );
            }
            // Thoughts and text never leave the gateway as frames.
            for (_, frame) in &frames {
                let frame = frame.to_string();
                assert!(
                    !frame.contains("Read, then") && !frame.contains("Writing"),
                    "{frame}"
                );
            }
        }
    }
}

#[test]
fn an_anthropic_tool_call_is_reported_at_its_own_block_stop() {
    let message = anthropic_message();
    let events = anthropic_events(&message, &[INVOKE_INPUT, PLAN_INPUT], 4);
    let mut sse = SseReader::new(ProviderProtocol::Anthropic, false);
    let mut timing = PlanTiming::start();
    let mut reported = Vec::new();
    for (position, event) in events.iter().enumerate() {
        let text = anthropic_sse(std::slice::from_ref(event));
        let flow = sse
            .feed(
                text.as_bytes(),
                &mut |frame| {
                    assert_eq!(frame.event.as_deref(), Some(EVENT_TOOL_CALL));
                    let call: serde_json::Value =
                        serde_json::from_str(frame.output.as_deref().unwrap()).unwrap();
                    reported.push((position, call["id"].as_str().unwrap().to_owned()));
                    true
                },
                &mut timing,
                &mut Instant::now,
            )
            .unwrap_or_else(|failure| panic!("{}", failure.why));
        assert_eq!(flow == Flow::Done, position + 1 == events.len());
    }
    let at = |kind: &str, index: usize| {
        events
            .iter()
            .position(|event| event["type"] == kind && event["index"] == index)
            .unwrap()
    };
    assert_eq!(
        reported,
        [
            (at("content_block_stop", 2), "toolu_01".to_owned()),
            (at("content_block_stop", 4), "toolu_02".to_owned()),
        ],
        "each call is reported the moment its block closes"
    );
    let message_delta = events
        .iter()
        .position(|event| event["type"] == "message_delta")
        .unwrap();
    assert!(reported[0].0 < at("content_block_start", 3));
    assert!(reported[1].0 < message_delta);
    assert!(timing.first_reasoning.is_some() && timing.reasoning_chunks > 0);
    assert!(timing.first_token.is_some() && timing.plan_call.is_some());
    let body = sse
        .finish(
            &mut |_| -> bool { panic!("every call was already reported") },
            &mut timing,
        )
        .unwrap_or_else(|failure| panic!("{}", failure.why));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        message
    );
    assert_eq!(timing.tool_calls, 2);
    assert_eq!(timing.completion_tokens, Some(503));
    assert_eq!(timing.cached_tokens, Some(1800));
    assert_eq!(timing.cache_write_tokens, Some(0));
}

#[test]
fn anthropic_stream_failures_are_provider_failures() {
    let message = anthropic_message();
    let events = anthropic_events(&message, &[INVOKE_INPUT, PLAN_INPUT], 5);
    let first_stop = events
        .iter()
        .position(|event| event["type"] == "content_block_stop" && event["index"] == 2)
        .unwrap();
    let run = |events: &[serde_json::Value], tail: &str| {
        let text = anthropic_sse(events) + tail;
        run_sse(ProviderProtocol::Anthropic, &text, 7, &[], false)
    };
    let calls = |frames: &Frames| {
        frames
            .iter()
            .map(|(_, call)| call["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    // An `error` event after a 200: only its type is passed on.
    let overloaded = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded near test instruction\"}}\n\n";
    let (frames, result) = run(&events[..=first_stop], overloaded);
    let why = result.unwrap_err();
    assert_eq!(why, "provider stream failed (type=overloaded_error)");
    assert_eq!(
        calls(&frames),
        ["toolu_01"],
        "a call completed before it stands"
    );

    // A stream that stops before `message_stop`, between or inside blocks.
    for (end, reported) in [
        (events.len() - 1, 2),
        (first_stop + 1, 1),
        (first_stop - 1, 0),
        (1, 0),
    ] {
        let (frames, result) = run(&events[..end], "");
        assert_eq!(
            result.unwrap_err(),
            "provider stream ended early",
            "end {end}"
        );
        assert_eq!(
            calls(&frames).len(),
            reported,
            "only finished calls, end {end}"
        );
    }
    // A connection cut in the middle of a data line.
    let text = anthropic_sse(&events);
    let cut = text.rfind("\"usage\"").unwrap();
    let cut = text.get(..cut).unwrap();
    let (frames, result) = run_sse(ProviderProtocol::Anthropic, cut, 3, &[], false);
    assert_eq!(result.unwrap_err(), "provider stream was unreadable");
    assert_eq!(calls(&frames), ["toolu_01", "toolu_02"]);

    // Malformed streams: input that is not JSON when its block closes, a
    // delta for a block never opened, a block before the message, a second
    // message.
    let mut broken = events[..first_stop].to_vec();
    broken.push(serde_json::json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"tail\":"}}));
    broken.extend_from_slice(&events[first_stop..]);
    let (frames, result) = run(&broken, "");
    assert_eq!(result.unwrap_err(), "provider tool input was not JSON");
    assert!(frames.is_empty());
    for stray in [
        serde_json::json!({"type": "content_block_delta", "index": 9, "delta": {"type": "text_delta", "text": "x"}}),
        events[0].clone(),
    ] {
        let mut broken = events[..3].to_vec();
        broken.push(stray);
        let (_, result) = run(&broken, "");
        assert_eq!(result.unwrap_err(), "provider stream was unreadable");
    }
    let (_, result) = run(&events[2..], "");
    assert_eq!(result.unwrap_err(), "provider stream was unreadable");
}
