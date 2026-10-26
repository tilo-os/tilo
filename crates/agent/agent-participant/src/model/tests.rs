//! The model planner's tests: catalogue and briefing, the plan read from
//! an answer, the conversation over scripted turns (reads, looks, searches,
//! refusals), and the OpenAI-compatible transport over a local endpoint.

use super::converse::{
    Attempt, PlanTurns, Repeats, TURN_OUTPUT_TOKENS, planning_tools, usage_line,
};
use super::scripted::{clauses, names_app, words};
use super::*;
use crate::planner::{Answered, read_answer};
use crate::vision::LOOK_CONTENT;
use tilo_model_gateway::host_backend::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason,
    ToolCall, Usage,
};
use tilo_model_gateway::openai::Target as ApiTarget;

#[test]
fn a_task_names_an_app_by_the_word_for_its_records() {
    assert!(task_names_app(
        "Add a total row to the spreadsheet",
        "so.tilo.sheets"
    ));
    assert!(task_names_app(
        "Export the workbook as Excel",
        "so.tilo.sheets"
    ));
    assert!(task_names_app(
        "Proofread the document",
        "so.tilo.word-processor"
    ));
    assert!(task_names_app(
        "In the Word Processor, add a heading",
        "so.tilo.word-processor"
    ));
    assert!(task_names_app(
        "Summarize it in a new note",
        "so.tilo.notes"
    ));
    assert!(!task_names_app(
        "Insert the image into the page",
        "so.tilo.canvas"
    ));
    assert!(!task_names_app("Proofread the document", "so.tilo.sheets"));
    assert!(task_names_app(
        "Fix the title of the partner deck",
        "so.tilo.slides"
    ));
    assert!(task_names_app(
        "Export the presentation as PPTX",
        "so.tilo.slides"
    ));
    assert!(task_names_app(
        "Move the kickoff events to Friday",
        "so.tilo.calendar"
    ));
    assert!(task_names_app(
        "Add a calendar entry for the review",
        "so.tilo.calendar"
    ));
    assert!(!task_names_app_itself(
        "Fix the title of the partner deck",
        "so.tilo.slides"
    ));
    assert!(!task_names_app_itself(
        "Use the winning headline from the campaign spreadsheet",
        "so.tilo.sheets"
    ));
    assert!(task_names_app_itself(
        "In Sheets, create the plan",
        "so.tilo.sheets"
    ));
}

/// The same call answered the same way three turns in a row ends the
/// plan with that answer; a call that changes, or an answer that does,
/// does not.
#[test]
fn a_model_repeating_one_call_ends_its_plan_with_the_answer() {
    let mut repeats = Repeats::default();
    let turn = |input: serde_json::Value| {
        let mut seen = String::new();
        Repeats::call(&mut seen, "plan", &input, "refused: x is out of range");
        seen
    };
    let refused = "refused: x is out of range";
    assert!(
        repeats
            .note(turn(serde_json::json!({"x": 1})), refused)
            .is_ok()
    );
    assert!(
        repeats
            .note(turn(serde_json::json!({"x": 2})), refused)
            .is_ok()
    );
    assert!(
        repeats
            .note(turn(serde_json::json!({"x": 2})), refused)
            .is_ok()
    );
    let ended = repeats
        .note(turn(serde_json::json!({"x": 2})), refused)
        .unwrap_err();
    assert!(
        ended.contains("3 times") && ended.ends_with(refused),
        "{ended}"
    );
}

#[test]
fn cloud_names_the_metered_endpoint_and_needs_a_token() {
    let key = || ApiKey::Value("tilo-ai-x".into());
    let choice = ModelChoice::parse(
        "cloud:http://127.0.0.1:1/api/machines/m1/ai/v1",
        None,
        key(),
    )
    .unwrap();
    assert_eq!(
        choice,
        ModelChoice::OpenAi {
            base_url: "http://127.0.0.1:1/api/machines/m1/ai/v1".into(),
            model: CLOUD_MODEL.into(),
            api_key: key(),
            target: ApiTarget::CloudMetered,
            vault: None,
        }
    );
    assert!(ModelChoice::parse("cloud:http://x/v1", None, ApiKey::None).is_err());
    assert!(ModelChoice::parse("openai:http://x/v1", None, ApiKey::None).is_err());
    assert!(ModelChoice::parse("cloud:ftp://x", None, key()).is_err());
}

#[test]
fn anthropic_names_claude_by_its_messages_api_and_needs_a_model_and_key() {
    let key = || ApiKey::File("/run/keys/anthropic".into());
    let choice = ModelChoice::parse(
        "anthropic:https://api.anthropic.com",
        Some("claude-x"),
        key(),
    )
    .unwrap();
    assert_eq!(
        choice,
        ModelChoice::Anthropic {
            base_url: "https://api.anthropic.com".into(),
            model: "claude-x".into(),
            api_key: key(),
            vault: None,
        }
    );
    let kept = choice.keeping_keys_in("/tmp/bench-vault".into());
    assert!(
        matches!(&kept, ModelChoice::Anthropic { vault: Some(root), .. } if root.ends_with("bench-vault"))
    );
    assert!(ModelChoice::parse("anthropic:https://x", None, key()).is_err());
    assert!(ModelChoice::parse("anthropic:https://x", Some("c"), ApiKey::None).is_err());
    assert!(ModelChoice::parse("anthropic:ftp://x", Some("c"), key()).is_err());
}

/// The usage log keeps a call's cache reads and writes and its reasoning,
/// in the shape the benchmark's meter reads.
#[test]
fn the_usage_log_keeps_cached_input_and_reasoning() {
    let mut answer = called("p1", tilo_agent::tools::PLAN, &serde_json::json!({}));
    answer.usage = Usage {
        input_tokens: 1000,
        output_tokens: 30,
        total_tokens: 1030,
        cached_input_tokens: 900,
        cache_write_tokens: 60,
        reasoning_tokens: 20,
    };
    let line = usage_line(
        "claude-x",
        Some("ada"),
        (std::time::Duration::from_millis(5), (0, 0)),
        &Ok(answer),
        Attempt::default(),
    );
    assert_eq!(line["usage"]["input_tokens"], 1000);
    assert_eq!(line["usage"]["input_tokens_details"]["cached_tokens"], 900);
    assert_eq!(
        line["usage"]["input_tokens_details"]["cache_write_tokens"],
        60
    );
    assert_eq!(
        line["usage"]["output_tokens_details"]["reasoning_tokens"],
        20
    );
}

/// An endpoint answering `answers` in order, one connection each, and
/// reporting every request's head and body.
pub(crate) fn endpoint(
    answers: Vec<String>,
) -> (String, std::sync::mpsc::Receiver<(String, String)>) {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for answer in answers {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let (mut head, mut length) = (String::new(), 0);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
                head.push_str(&line.to_ascii_lowercase());
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            sent.send((head, String::from_utf8(body).unwrap())).unwrap();
            reader.into_inner().write_all(answer.as_bytes()).unwrap();
        }
    });
    (url, received)
}

fn http(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub(crate) fn open_notes() -> Catalogue {
    let (catalogue, _) = catalogue(&PlanContext {
        apps: vec!["so.tilo.notes".into()],
        ..PlanContext::default()
    });
    catalogue
}

#[test]
fn a_metered_turn_is_retried_under_one_key_and_plans() {
    let offer = open_notes().offers[0].id();
    let list = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "describe_operation",
                "arguments": serde_json::json!({"operation": offer}).to_string()}}]}, "finish_reason": "tool_calls"}]});
    let plan = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c2", "type": "function", "function": {"name": "plan",
                "arguments": serde_json::json!({"steps": [{"operation": offer, "arguments": {}}],
                    "why": "open", "then": "add a note"}).to_string()}}]}, "finish_reason": "tool_calls"}]});
    let (url, sent) = endpoint(vec![
        http(
            "429 Too Many Requests",
            r#"{"error":"slow down","code":"rate_limited"}"#,
        ),
        http("200 OK", &list.to_string()),
        http("200 OK", &plan.to_string()),
    ]);
    let mut model = OpenAiModel::new(
        &url,
        CLOUD_MODEL,
        ApiKey::None,
        tilo_test_env::hang_bound(),
        ApiTarget::CloudMetered,
    )
    .with_profile(crate::profile::Profile::preset(
        crate::profile::ModelClass::Small,
        crate::profile::Goal::Fastest,
    ));
    let answer = model
        .choose("open notes then add a note", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer]);
    assert_eq!(answer.then.as_deref(), Some("add a note"));
    let requests: Vec<(String, String)> = sent.try_iter().collect();
    let key = |head: &str| {
        head.lines()
            .find_map(|line| line.strip_prefix("idempotency-key: "))
            .map(str::to_owned)
            .unwrap()
    };
    assert_eq!(key(&requests[0].0), key(&requests[1].0));
    assert_ne!(key(&requests[1].0), key(&requests[2].0));
    assert_eq!(requests[0].1, requests[1].1, "retry the same shaped body");
    for (_, body) in &requests {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(body.get("service_tier").is_none(), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");
    }
    let second: serde_json::Value = serde_json::from_str(&requests[2].1).unwrap();
    // The metered endpoint caps a turn's output below what a turn asks.
    assert_eq!(
        second["max_completion_tokens"],
        TURN_OUTPUT_TOKENS.min(2048)
    );
    assert_eq!(second["messages"][3]["role"], "tool");
    assert!(second.get("max_tokens").is_none());
}

/// A chat-completions answer calling `plan` with one step, `operation`.
pub(crate) fn plan_answer(id: &str, operation: &str) -> String {
    let answer = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": id, "type": "function", "function": {"name": "plan",
                "arguments": serde_json::json!({"steps": [{"operation": operation, "arguments": {}}],
                    "why": "open", "then": "add a note"}).to_string()}}]}, "finish_reason": "tool_calls"}]});
    http("200 OK", &answer.to_string())
}

fn standard(url: &str) -> OpenAiModel {
    OpenAiModel::new(
        url,
        "pinned",
        ApiKey::None,
        tilo_test_env::hang_bound(),
        ApiTarget::Standard,
    )
}

/// `OpenAI` permits text content as either a string or typed text blocks.
fn message_text(message: &serde_json::Value, role: &str) -> String {
    assert_eq!(message["role"], role);
    let content = &message["content"];
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .expect("text content must be a string or text blocks")
        .iter()
        .map(|block| {
            assert_eq!(block["type"], "text");
            block["text"].as_str().expect("text block").to_owned()
        })
        .collect()
}

/// The published operations come with the task, so a task they answer
/// is planned in ONE request — no listing or describing turn first.
#[test]
fn an_operation_the_task_turn_lists_is_planned_in_one_request() {
    let offer = open_notes().offers[0].id();
    let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
    let answer = standard(&url)
        .choose("open notes then add a note", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer.clone()]);
    let requests: Vec<(String, String)> = sent.try_iter().collect();
    assert_eq!(requests.len(), 1, "one model turn");
    let first: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
    let task = message_text(&first["messages"][1], "user");
    assert!(task.contains(&offer), "{task}");
    assert!(task.contains("open notes then add a note"), "{task}");
    let system = message_text(&first["messages"][0], "system");
    assert!(!system.contains("before planning"), "{system}");
}

/// Environment settings are applied by the production planner factory.
/// Each case runs in its own process so parallel tests never share them.
#[test]
fn host_planner_resolves_environment_hints() {
    const CHILD_ENDPOINT: &str = "TILO_PRESET_TEST_ENDPOINT";
    if let Ok(url) = std::env::var(CHILD_ENDPOINT) {
        let choice =
            ModelChoice::parse(&format!("openai:{url}"), Some("mini"), ApiKey::None).unwrap();
        let steps = choice
            .planner()
            .plan(
                "open notes",
                &PlanContext {
                    apps: vec!["so.tilo.notes".into()],
                    ..PlanContext::default()
                },
            )
            .unwrap();
        assert!(matches!(steps[0].action, StepAction::Open));
        return;
    }
    for (preset, tier, effort) in [
        (Some("fastest"), Some("priority"), Some("low")),
        (None, None, None),
        (Some("invalid"), None, None),
    ] {
        let offer = open_notes().offers[0].id();
        let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "model::tests::host_planner_resolves_environment_hints",
            ])
            .env(CHILD_ENDPOINT, url)
            .env_remove(crate::profile::PRESET_ENV)
            .env_remove(crate::profile::MODEL_CLASS_ENV)
            .env_remove(crate::profile::SERVICE_TIER_ENV)
            .env_remove(crate::profile::REASONING_EFFORT_ENV);
        if let Some(preset) = preset {
            child.env(crate::profile::PRESET_ENV, preset);
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let (_, body) = sent.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body.get("service_tier").and_then(serde_json::Value::as_str),
            tier
        );
        assert_eq!(
            body.get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            effort
        );
        assert_eq!(sent.try_iter().count(), 0);
    }
}

#[test]
fn preset_hints_reach_the_provider_without_changing_the_plan() {
    let offer = open_notes().offers[0].id();
    let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
    let profile = crate::profile::Profile::resolve("mini", &|name| {
        (name == crate::profile::PRESET_ENV).then(|| "fastest".into())
    });
    let answer = standard(&url)
        .with_profile(profile)
        .choose("open notes", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer]);
    let requests: Vec<_> = sent.try_iter().collect();
    assert_eq!(requests.len(), 1, "presets never duplicate requests");
    let body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["max_tokens"], TURN_OUTPUT_TOKENS);
}

#[test]
fn metered_targets_strip_preset_hints_before_sending() {
    let offer = open_notes().offers[0].id();
    let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
    let answer = OpenAiModel::new(
        &url,
        CLOUD_MODEL,
        ApiKey::None,
        tilo_test_env::hang_bound(),
        ApiTarget::CloudMetered,
    )
    .with_profile(crate::profile::Profile::preset(
        crate::profile::ModelClass::Small,
        crate::profile::Goal::Fastest,
    ))
    .choose("open notes", &open_notes(), MOST_STEPS)
    .unwrap();
    assert_eq!(answer.actions, vec![offer]);
    let requests: Vec<_> = sent.try_iter().collect();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
    assert!(body.get("service_tier").is_none(), "{body}");
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    assert_eq!(body["max_completion_tokens"], TURN_OUTPUT_TOKENS.min(2048));
    assert!(requests[0].0.contains("idempotency-key: "));
}

#[test]
fn default_planners_send_no_optional_hints() {
    let offer = open_notes().offers[0].id();
    let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
    standard(&url)
        .choose("open notes", &open_notes(), MOST_STEPS)
        .unwrap();
    let (_, body) = sent.recv().unwrap();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(body.get("service_tier").is_none(), "{body}");
    assert!(body.get("reasoning_effort").is_none(), "{body}");
}

/// An id the conversation never showed is refused back to the model with
/// what to name instead; the corrected plan is taken. The tools are the
/// same bytes on every turn, so the prompt prefix stays cacheable.
#[test]
fn an_unshown_operation_is_refused_back_to_the_model_with_stable_tools() {
    let offer = open_notes().offers[0].id();
    let invented = "app.so.tilo.sheets#open";
    let (url, sent) = endpoint(vec![plan_answer("c1", invented), plan_answer("c2", &offer)]);
    let answer = standard(&url)
        .choose("open notes then add a note", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer.clone()]);
    let requests: Vec<serde_json::Value> = sent
        .try_iter()
        .map(|(_, body)| serde_json::from_str(&body).unwrap())
        .collect();
    assert_eq!(requests.len(), 2);
    let refusal = &requests[1]["messages"][3];
    assert_eq!(refusal["role"], "tool");
    assert_eq!(refusal["tool_call_id"], "c1");
    let text = refusal["content"].as_str().unwrap();
    assert!(text.contains(invented), "{text}");
    assert!(text.contains("nothing was performed"), "{text}");
    assert!(
        text.contains(&format!("Listed ids for that action: {offer}")),
        "{text}"
    );
    let tools = |request: &serde_json::Value| serde_json::to_string(&request["tools"]).unwrap();
    assert_eq!(tools(&requests[0]), tools(&requests[1]));
    assert!(!tools(&requests[0]).contains("\"enum\""));
    assert_eq!(requests[0]["messages"][0], requests[1]["messages"][0]);
}

/// An endpoint that keeps each connection open and answers `answers` in
/// order across them, reporting how many requests each connection
/// carried (one count per accepted connection).
fn keep_alive_endpoint(
    answers: Vec<String>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let carried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let answers = std::sync::Arc::new(std::sync::Mutex::new(
        answers
            .into_iter()
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let counts = carried.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let connection = {
                let mut counts = counts.lock().unwrap();
                counts.push(0_usize);
                counts.len().saturating_sub(1)
            };
            let (answers, counts) = (answers.clone(), counts.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.unwrap());
                loop {
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse().unwrap();
                        }
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let mut served = counts.lock().unwrap();
                    served[connection] = served[connection].saturating_add(1);
                    drop(served);
                    let answer = answers.lock().unwrap().pop_front().unwrap();
                    reader.get_mut().write_all(answer.as_bytes()).unwrap();
                }
            });
        }
    });
    (url, carried)
}

/// Later turns of a task go over a connection an earlier turn opened:
/// the model keeps one client, not one per call (which would carry
/// exactly one request on every connection).
#[test]
fn later_turns_reuse_an_earlier_turns_connection() {
    let offer = open_notes().offers[0].id();
    let list = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "describe_operation",
                "arguments": serde_json::json!({"operation": offer}).to_string()}}]}, "finish_reason": "tool_calls"}]});
    let plan = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c2", "type": "function", "function": {"name": "plan",
                "arguments": serde_json::json!({"steps": [{"operation": offer, "arguments": {}}],
                    "why": "open", "then": null}).to_string()}}]}, "finish_reason": "tool_calls"}]});
    let kept_open = |body: &serde_json::Value| {
        let body = body.to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let mut second_list = list.clone();
    second_list["choices"][0]["message"]["tool_calls"][0]["id"] = "c-middle".into();
    let (url, carried) = keep_alive_endpoint(vec![
        kept_open(&list),
        kept_open(&second_list),
        kept_open(&plan),
    ]);
    let mut model = OpenAiModel::new(
        &url,
        "test-model",
        ApiKey::None,
        tilo_test_env::hang_bound(),
        ApiTarget::Standard,
    );
    let answer = model
        .choose("open notes", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer]);
    let carried = carried.lock().unwrap().clone();
    assert_eq!(carried.iter().sum::<usize>(), 3, "{carried:?}");
    // A connection opened speculatively may carry nothing; one turn's
    // connection must have carried a later turn too.
    assert!(carried.iter().any(|&requests| requests >= 2), "{carried:?}");
}

#[test]
fn running_out_of_credit_ends_the_task_with_a_plain_reason() {
    let (url, sent) = endpoint(vec![http(
        "402 Payment Required",
        r#"{"error":"Included credit is insufficient for this request","code":"insufficient_credit"}"#,
    )]);
    let mut model = OpenAiModel::new(
        &url,
        CLOUD_MODEL,
        ApiKey::None,
        tilo_test_env::hang_bound(),
        ApiTarget::CloudMetered,
    );
    let error = model
        .choose("add a note", &open_notes(), MOST_STEPS)
        .unwrap_err();
    assert!(
        error.starts_with(tilo_model_gateway::openai::OUT_OF_CREDITS),
        "{error}"
    );
    assert_eq!(sent.try_iter().count(), 1);
}

#[test]
fn typed_credit_failure_maps_to_cli_without_retry_or_provider_text() {
    use tilo_model_gateway::host_backend::protocol::{ErrorKind, ModelError};

    let expected = converse::model_error_text(&ModelError::new(ErrorKind::CreditsExhausted));
    assert!(
        expected.starts_with(tilo_model_gateway::openai::OUT_OF_CREDITS),
        "{expected}"
    );
    assert!(expected.contains("choose another model"), "{expected}");
    // Classification comes from the machine code, not the provider's prose.
    let (url, sent) = endpoint(vec![http(
        "402 Payment Required",
        r#"{"error":"private upstream detail","code":"insufficient_credit"}"#,
    )]);
    let error = OpenAiModel::new(
        &url,
        CLOUD_MODEL,
        ApiKey::None,
        tilo_test_env::hang_bound(),
        ApiTarget::CloudMetered,
    )
    .choose("add a note", &open_notes(), MOST_STEPS)
    .unwrap_err();
    assert_eq!(error, expected);
    assert!(!error.contains("private upstream detail"));
    assert_eq!(
        sent.try_iter().count(),
        1,
        "credit failures are not retried"
    );

    let other = ModelError::new(ErrorKind::Authentication);
    assert_eq!(converse::model_error_text(&other), other.to_string());
}

/// Answers with whatever it was built with, unchecked.
struct Canned(Answer);

impl PlanModel for Canned {
    fn choose(&mut self, _: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        Ok(self.0.clone())
    }
}

fn canned(actions: &[&str], why: &str, then: Option<&str>) -> ModelPlanner {
    ModelPlanner::new(Box::new(Canned(Answer {
        actions: actions.iter().map(|id| (*id).to_owned()).collect(),
        arguments: vec![serde_json::Map::new(); actions.len()],
        why: why.to_owned(),
        then: then.map(str::to_owned),
        reply: None,
        wait: None,
        ..Answer::default()
    })))
}

pub(crate) fn route(app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: "room".into(),
        record: record.into(),
    }
}

/// Notes open on `n1` with one operation, and a sheet the room has.
fn context() -> PlanContext {
    let (open, _) = catalogue(&PlanContext {
        open: Vec::new(),
        known: Vec::new(),
        apps: vec!["so.tilo.notes".into()],
        produced: Vec::new(),
        observed: Vec::new(),
        ..PlanContext::default()
    });
    let mut edit = open.offers[0].descriptor.clone();
    edit.address = ActionAddress::new("notes.page", "edit");
    PlanContext {
        open: vec![(route("so.tilo.notes", "n1"), vec![edit])],
        known: vec![(route("so.tilo.sheets", "s1"), None)],
        apps: vec!["so.tilo.notes".into(), "so.tilo.sheets".into()],
        produced: Vec::new(),
        observed: Vec::new(),
        ..PlanContext::default()
    }
}

fn offer_id(context: &PlanContext, address: &str) -> String {
    catalogue(context)
        .0
        .offers
        .iter()
        .map(Offer::id)
        .find(|id| id.ends_with(address))
        .unwrap()
}

#[test]
fn nothing_fits_after_an_open_plans_the_rest_again() {
    let context = context();
    let open = offer_id(&context, "record.so.tilo.sheets.s1#open");
    let none = tilo_agent::plan::NOTHING_FITS;
    let steps = canned(&[&open, none], "", Some("sum column B"))
        .plan("open the sheet and sum column B", &context)
        .unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(steps[0].app, "so.tilo.sheets");
    assert_eq!(steps[0].record.as_deref(), Some("s1"));
    assert_eq!(steps[0].action, StepAction::Open);
    assert_eq!(steps[1].action, StepAction::Continue("sum column B".into()));
}

/// A plan of no steps is the check's "all done", taken as no steps;
/// a first plan of none is refused, and so is one the model wrote
/// without a reply, through the same reading the model's calls get.
#[test]
fn no_steps_are_planned_only_where_nothing_left_is_an_answer() {
    let mut context = context();
    let mut planner = canned(&[], "", None);
    let error = planner.plan("add a heading", &context).unwrap_err();
    assert!(error.starts_with("nothing offered fits"), "{error}");
    context.may_plan_nothing = true;
    let steps = planner.plan("check the work", &context).unwrap();
    assert!(steps.is_empty(), "{steps:?}");

    let revealed = tilo_agent::tools::Revealed::new();
    let done = serde_json::json!({"reply": "All done.", "steps": [], "why": "", "then": null});
    let check = PlanContext {
        may_plan_nothing: true,
        ..PlanContext::default()
    };
    assert!(read_answer(&done, &revealed, MOST_STEPS, &check).is_ok());
    assert!(read_answer(&done, &revealed, MOST_STEPS, &PlanContext::default()).is_err());
    let silent = serde_json::json!({"steps": [], "why": "", "then": null});
    assert!(read_answer(&silent, &revealed, MOST_STEPS, &check).is_err());
}

/// A plan that waits for a message ends in a wait for it, carrying
/// what `then` left; a plan of only a wait is an answer, its seconds are
/// kept within the bound, and a place no message arrives is refused back
/// to the model.
#[test]
fn a_plan_that_waits_ends_in_a_wait_for_the_message() {
    let context = context();
    let edit = offer_id(&context, "notes.page#edit");
    let mut planner = ModelPlanner::new(Box::new(Canned(Answer {
        actions: vec![edit],
        arguments: vec![serde_json::Map::new()],
        then: Some("note the code".into()),
        wait: Some(tilo_agent::tools::Wait {
            app: Some("Messages".into()),
            contains: Some("code".into()),
            seconds: 600,
        }),
        ..Answer::default()
    })));
    let steps = planner.plan("note the login code", &context).unwrap();
    let [edit, wait] = steps.as_slice() else {
        panic!("the step, then the wait: {steps:?}");
    };
    assert!(matches!(edit.action, StepAction::Invoke { .. }));
    assert_eq!(
        wait.action,
        StepAction::Wait(Wait {
            apps: vec!["so.tilo.messages".into()],
            contains: Some("code".into()),
            within: std::time::Duration::from_secs(tilo_agent::tools::MOST_WAIT_SECONDS),
            rest: Some("note the code".into()),
        })
    );

    let revealed = tilo_agent::tools::Revealed::new();
    let only = serde_json::json!({"steps": [], "then": null,
            "wait_for": {"app": "chat", "seconds": 0}});
    let answer = read_answer(&only, &revealed, MOST_STEPS, &PlanContext::default()).unwrap();
    let read = answer.wait.unwrap();
    assert_eq!((read.app.as_deref(), read.seconds), (Some("chat"), 1));
    let elsewhere = serde_json::json!({"steps": [], "then": null,
            "wait_for": {"app": "sms", "seconds": 30}});
    let error =
        read_answer(&elsewhere, &revealed, MOST_STEPS, &PlanContext::default()).unwrap_err();
    assert!(error.contains("mail, chat, messages"), "{error}");
    let untimed = serde_json::json!({"steps": [], "then": null, "wait_for": {}});
    assert!(read_answer(&untimed, &revealed, MOST_STEPS, &PlanContext::default()).is_err());
}

#[test]
fn nothing_fits_alone_is_refused_with_the_models_reason() {
    let none = tilo_agent::plan::NOTHING_FITS;
    let error = canned(&[none], "no app here draws", Some("draw a cat"))
        .plan("draw a cat", &context())
        .unwrap_err();
    assert_eq!(error, "nothing offered fits: no app here draws; draw a cat");
    let error = canned(&[none], "", None)
        .plan("draw a cat", &context())
        .unwrap_err();
    assert!(error.starts_with("nothing offered fits"), "{error}");
}

/// "Nothing fits" that goes on to say what to do is planned once more,
/// asked to plan what it said with the listed operations; saying it
/// again then ends the task with the model's reason.
#[test]
fn nothing_fits_with_a_plan_of_its_own_is_planned_again_once() {
    let none = tilo_agent::plan::NOTHING_FITS;
    let said = "Open the Word Processor document, read its text, then fill section A";
    let mut planner = canned(&[none], "", Some(said));
    let steps = planner.plan("fill the form", &context()).unwrap();
    let [step] = steps.as_slice() else {
        panic!("only the plan again: {steps:?}");
    };
    let StepAction::Continue(rest) = &step.action else {
        panic!("{step:?}");
    };
    assert!(
        rest.starts_with(PLAN_WHAT_IT_SAID) && rest.ends_with(said),
        "{rest}"
    );
    let error = planner.plan(rest, &context()).unwrap_err();
    assert!(error.starts_with("nothing offered fits"), "{error}");
}

#[test]
fn nothing_fits_after_an_operation_plans_the_task_again() {
    let context = context();
    let edit = offer_id(&context, "notes.page#edit");
    let none = tilo_agent::plan::NOTHING_FITS;
    let task = "edit the page and then something else";
    let steps = canned(&[&edit, none, &edit], "", None)
        .plan(task, &context)
        .unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(matches!(
        &steps[0].action,
        StepAction::Invoke { address, .. } if address.action == "edit"
    ));
    assert_eq!(steps[1].action, StepAction::Continue(task.into()));
}

#[test]
fn a_model_that_keeps_deferring_is_stopped() {
    let context = context();
    let open = offer_id(&context, "record.so.tilo.sheets.s1#open");
    let none = tilo_agent::plan::NOTHING_FITS;
    let mut planner = canned(&[&open, none], "", None);
    let task = "open the sheet and sum column B";
    for _ in 0..MOST_REPLANS {
        let steps = planner.plan(task, &context).unwrap();
        assert_eq!(steps[1].action, StepAction::Continue(task.into()));
    }
    let error = planner.plan(task, &context).unwrap_err();
    assert!(error.contains("without finishing"), "{error}");
    // A different task starts counting afresh.
    assert!(planner.plan("open the sheet", &context).is_ok());
}

/// Remembers what it was asked, and answers with one operation.
struct Recording {
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    answer: String,
}

impl PlanModel for Recording {
    fn choose(&mut self, task: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        self.asked.lock().unwrap().push(task.to_owned());
        Ok(Answer {
            actions: vec![self.answer.clone()],
            arguments: vec![serde_json::Map::new()],
            why: String::new(),
            then: None,
            reply: None,
            wait: None,
            ..Answer::default()
        })
    }
}

#[test]
fn the_model_sees_what_was_produced_and_a_content_step_carries() {
    let mut context = context();
    let (_, descriptors) = &mut context.open[0];
    let mut place = descriptors[0].clone();
    place.address = ActionAddress::new("notes.page", "place-image");
    place.input = tilo_actions::InputSpec::Text {
        prompt: "Image".into(),
        placeholder: "content hash".into(),
        collects: tilo_actions::Collects::Content,
    };
    descriptors.push(place);
    let asked = std::sync::Arc::default();
    let mut planner = ModelPlanner::new(Box::new(Recording {
        asked: std::sync::Arc::clone(&asked),
        answer: offer_id(&context, "notes.page#place-image"),
    }));
    let task = "place the picture";
    let steps = planner.plan(task, &context).unwrap();
    assert!(steps[0].carries.is_some(), "{steps:?}");
    context.produced = vec!["tilo.content:abc".into(), "tilo.content:def".into()];
    planner.plan(task, &context).unwrap();
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked[0], task, "nothing produced, nothing added");
    assert!(
        asked[1].starts_with(task)
            && asked[1].contains("Content produced so far: tilo.content:abc, tilo.content:def"),
        "{}",
        asked[1]
    );

    let edit = offer_id(&context, "notes.page#edit");
    let steps = canned(&[&edit], "", None).plan(task, &context).unwrap();
    assert!(
        steps[0].carries.is_none(),
        "an operation that takes words carries nothing"
    );
}

/// A plan's obligations reach the host once, each step tagged with the
/// one it fulfils, and a plan of the rest is shown the task's checklist
/// after the request, as context rather than more of the request.
#[test]
fn a_plan_hands_on_its_obligations_and_a_replan_is_shown_the_checklist() {
    let mut context = context();
    let edit = offer_id(&context, "notes.page#edit");
    let mut planner = ModelPlanner::new(Box::new(Canned(Answer {
        actions: vec![edit.clone(), edit.clone()],
        arguments: vec![serde_json::Map::new(); 2],
        obligations: vec!["write the note".into(), "sign it".into()],
        serves: vec![Some(1), None],
        set_aside: vec![(2, "not possible: no name given".into())],
        ..Answer::default()
    })));
    let steps = planner.plan("write and sign the note", &context).unwrap();
    let serves: Vec<Option<usize>> = steps.iter().map(|step| step.serves).collect();
    assert_eq!(serves, [Some(1), None]);
    let declared = planner.take_obligations();
    assert_eq!(declared.items, ["write the note", "sign it"]);
    assert_eq!(
        declared.set_aside,
        [(2, "not possible: no name given".into())]
    );
    assert_eq!(
        planner.take_obligations(),
        crate::planner::Obligations::default()
    );

    let asked = std::sync::Arc::default();
    let mut replanning = ModelPlanner::new(Box::new(Recording {
        asked: std::sync::Arc::clone(&asked),
        answer: edit,
    }));
    context.request = Some("write and sign the note".into());
    context.obligations =
        format!("{OBLIGATIONS_INTRO}, as you listed them: 1. write the note (done); 2. sign it.");
    replanning.plan("sign it", &context).unwrap();
    let asked = asked.lock().unwrap().clone();
    assert!(
        asked[0].ends_with("1. write the note (done); 2. sign it."),
        "{}",
        asked[0]
    );
    assert_eq!(requested(&asked[0]), "sign it");
}

#[test]
fn a_plan_after_a_view_is_made_with_what_it_answered() {
    let mut context = context();
    let (_, descriptors) = &mut context.open[0];
    let mut inspect = descriptors[0].clone();
    inspect.address = ActionAddress::new("notes.page", "inspect");
    inspect.kind = tilo_actions::OperationKind::View;
    descriptors.push(inspect);
    let asked = std::sync::Arc::default();
    let mut planner = ModelPlanner::new(Box::new(Recording {
        asked: std::sync::Arc::clone(&asked),
        answer: offer_id(&context, "notes.page#edit"),
    }));
    let task = "add a band over the right 38% of the picture";
    planner.plan(task, &context).unwrap();
    let described = tilo_agent::tools::answer(
        tilo_agent::tools::DESCRIBE_OPERATION,
        &serde_json::json!({"operation": offer_id(&context, "notes.page#inspect")}),
        &catalogue(&context).0,
        &mut tilo_agent::tools::Revealed::new(),
    );
    assert!(
        described.contains("A view: it changes nothing") && described.contains("`then`"),
        "the model is told how to plan with a view's answer: {described}"
    );
    let answered = tilo_actions::ActionOutput {
        value: Some(serde_json::json!({"artwork": {"width": 640, "height": 480}})),
        resources: Vec::new(),
    };
    context.observed.push(Answered {
        seen: crate::planner::observation(
            "so.tilo.notes",
            &ActionAddress::new("notes.page", "inspect"),
            &answered,
        )
        .unwrap(),
        untrusted: false,
    });
    planner.plan(task, &context).unwrap();
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked[0], task, "nothing answered yet, nothing added");
    assert!(
        asked[1].contains("What the steps so far answered")
            && asked[1].contains(r#""width":640"#)
            && asked[1].contains(r#""height":480"#),
        "the re-plan sees the dimensions the view found: {}",
        asked[1]
    );
}

#[test]
fn a_clause_names_an_app_whose_name_has_two_words() {
    let clause = words("in the word processor replace the selection");
    assert!(names_app(&clause, "so.tilo.word-processor"));
    assert!(!names_app(&clause, "so.tilo.sheets"));
    assert!(names_app(&words("add a sheet in sheets"), "so.tilo.sheets"));
}

/// An open of a document the room already has is an open of its app,
/// whatever its record id holds, and names that record.
#[test]
fn a_record_open_opens_its_app() {
    for (app, record) in [
        (
            "so.tilo.word-processor",
            "uuid:7d0c5e1a-2b4f-4c38-9a1e-5f6b7c8d9e0f",
        ),
        ("so.tilo.sheets", "file:/home/ada/plans.v2.xlsx"),
        ("so.tilo.notes", "derived:default@tab:0000000000000000"),
        ("so.tilo.notes", "plain"),
    ] {
        let address = surface::open_record_address(app, record);
        assert_eq!(
            address.resource,
            format!("record.{app}.{record}"),
            "{address:?}"
        );
        assert_eq!(surface::opens(&address), Some(app), "{address:?}");
        assert_eq!(
            surface::opens_record(&address),
            Some((app, record)),
            "{address:?}"
        );
        let offer = Offer {
            app: app.to_owned(),
            broker_peer_id: None,
            broker_provider: None,
            descriptor: open_record_descriptor(app, record, None),
        };
        assert_eq!(tilo_agent::tools::opened_app(&offer), Some(app));
    }
    let new = ActionAddress::new("app.so.tilo.notes", surface::OPEN_ACTION);
    assert_eq!(surface::opens(&new), Some("so.tilo.notes"));
    assert_eq!(surface::opens_record(&new), None);
    let edit = ActionAddress::new("record.so.tilo.notes.plain", "edit");
    assert_eq!(surface::opens(&edit), None);
    assert_eq!(surface::opens_record(&edit), None);
}

/// The room's documents are offered by their titles, also while another
/// record of their app is open, and a model reading the applications
/// list is shown them apart from the open that creates a new one.
#[test]
fn existing_documents_are_offered_by_their_titles() {
    const WRITER: &str = "so.tilo.word-processor";
    let mut context = context();
    context.apps.push(WRITER.into());
    context.known = vec![
        (route(WRITER, "uuid:launch"), Some("Launch page".into())),
        (route(WRITER, "uuid:plain"), None),
        (route("so.tilo.notes", "n2"), Some("Plans".into())),
    ];
    let (catalogue, _) = catalogue(&context);
    let label = |address: &str| {
        catalogue
            .offers
            .iter()
            .find(|offer| offer.id() == address)
            .map(|offer| offer.descriptor.action_label.clone())
    };
    assert_eq!(
        label("record.so.tilo.word-processor.uuid:launch#open").as_deref(),
        Some(
            "open the room's so.tilo.word-processor document \"Launch page\" (then export, read or edit it)"
        )
    );
    assert!(
        label("record.so.tilo.word-processor.uuid:plain#open")
            .is_some_and(|label| label.contains("uuid:plain"))
    );
    assert!(label("app.so.tilo.word-processor#open").is_some());
    // Notes is open on n1: its other document is offered, a new one not.
    assert!(label("record.so.tilo.notes.n2#open").is_some());
    assert!(label("app.so.tilo.notes#open").is_none());

    // The list the model is shown says a record's open opens that
    // document, apart from the open that makes a new one.
    let summary = tilo_agent::tools::catalogue_summary(
        &catalogue,
        &mut tilo_agent::tools::Revealed::new(),
        64,
    );
    assert!(
        summary.contains(
            "record.so.tilo.word-processor.uuid:launch#open — open the room's \
                 so.tilo.word-processor document \"Launch page\" (then export, read or edit it) \
                 (opens this document of so.tilo.word-processor the room already has"
        ),
        "{summary}"
    );

    let open = offer_id(&context, "record.so.tilo.word-processor.uuid:launch#open");
    let steps = canned(&[&open], "", Some("add a paragraph"))
        .plan("add a paragraph to the Launch page", &context)
        .unwrap();
    assert_eq!(steps[0].app, WRITER);
    assert_eq!(steps[0].record.as_deref(), Some("uuid:launch"));
    assert_eq!(steps[0].action, StepAction::Open);
    assert_eq!(
        steps[0].log_names(),
        (
            "record.so.tilo.word-processor.uuid:launch".into(),
            "open".into()
        )
    );
}

/// An operation of `resource` with `params`, as a record publishes it.
pub(crate) fn operation(
    resource: &str,
    action: &str,
    kind: tilo_actions::OperationKind,
    params: Vec<tilo_actions::ActionParam>,
) -> ActionDescriptor {
    let mut descriptor = open_notes().offers[0].descriptor.clone();
    descriptor.address = ActionAddress::new(resource, action);
    descriptor.resource_kind = resource.into();
    descriptor.resource_label = resource.into();
    descriptor.action_label = action.into();
    descriptor.description = Some(format!("What {action} does."));
    descriptor.kind = kind;
    descriptor.params = params;
    descriptor.contract = tilo_actions::ActionContract::default();
    descriptor
}

/// A request naming exactly one operation of an open record, with every
/// required argument among its choices, is that operation without the
/// model; anything vaguer, ambiguous or asking for more is not.
#[test]
fn a_request_for_one_stated_operation_needs_no_model() {
    use tilo_actions::{ActionParam, OperationKind, ParamType};
    let mut xlsx = operation(
        "sheets.workbook",
        "export-xlsx-content",
        OperationKind::Effect,
        vec![],
    );
    xlsx.action_label = "Export Excel workbook as content".into();
    let mut docx = operation(
        "tilo.rich-text",
        "export-content",
        OperationKind::Effect,
        vec![
            ActionParam::required("format", ParamType::Text, "Format")
                .with_choices(["docx", "pdf"]),
        ],
    );
    docx.action_label = "Export as content".into();
    let mut rename = operation(
        "sheets.sheet:a",
        "rename",
        OperationKind::Document,
        vec![ActionParam::required("value", ParamType::Text, "Name")],
    );
    rename.action_label = "Rename sheet".into();
    let context = PlanContext {
        open: vec![
            (route("so.tilo.sheets", "s1"), vec![xlsx, rename]),
            (route("so.tilo.word-processor", "w1"), vec![docx]),
        ],
        apps: vec!["so.tilo.sheets".into(), "so.tilo.word-processor".into()],
        ..PlanContext::default()
    };
    let (catalogue, targets) = catalogue(&context);
    let plan = |task: &str| single_operation(task, &catalogue, &targets);
    let excel = plan(
        "Export the Sheets spreadsheet \"Campaign plan\" with all its sheets as an Excel \
             workbook, kept as content.",
    )
    .unwrap();
    assert!(
        excel.actions[0].ends_with("sheets.workbook#export-xlsx-content"),
        "{excel:?}"
    );
    assert!(excel.arguments[0].is_empty());
    let page = plan("Export the launch page as DOCX, kept as content").unwrap();
    assert_eq!(page.arguments[0]["format"], "docx");
    // A free-text argument, two things, an unnamed choice: the model plans.
    assert!(plan("Rename sheet to Budget").is_none());
    assert!(plan("Export the page as DOCX, then export the workbook as Excel").is_none());
    assert!(plan("Export the page as content").is_none());
}

/// The same operation on two resources is the one the task names.
#[test]
fn the_named_resource_disambiguates_a_single_operation() {
    use tilo_actions::OperationKind;
    let export = |page: &str, title: &str| {
        let mut op = operation(
            &format!("notes.page:{page}"),
            "export-markdown-content",
            OperationKind::Effect,
            vec![],
        );
        op.action_label = "Export Markdown as content".into();
        op.resource_label = title.into();
        op
    };
    let context = PlanContext {
        open: vec![(
            route("so.tilo.notes", "n1"),
            vec![export("a", "Welcome"), export("b", "Launch headline")],
        )],
        apps: vec!["so.tilo.notes".into()],
        ..PlanContext::default()
    };
    let (catalogue, targets) = catalogue(&context);
    let answer = single_operation(
        "Export the note \"Launch headline\" as Markdown, kept as content.",
        &catalogue,
        &targets,
    )
    .unwrap();
    assert!(answer.actions[0].contains("notes.page:b#"), "{answer:?}");
    assert!(
        single_operation("Export the note as Markdown content", &catalogue, &targets).is_none()
    );
}

/// A view answering with what it finds, as Canvas `inspect` is.
pub(crate) fn inspect_view(resource: &str) -> ActionDescriptor {
    let mut inspect = operation(
        resource,
        "inspect",
        tilo_actions::OperationKind::View,
        vec![tilo_actions::ActionParam::optional(
            "full",
            tilo_actions::ParamType::Boolean,
            "Every mark",
        )],
    );
    inspect.contract = tilo_actions::ActionContract {
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        },
        ..tilo_actions::ActionContract::view()
    };
    inspect
}

/// A canvas open on `c1` publishing `draw` (a large, typed parameter),
/// `inspect` and a huge generic `apply`.
pub(crate) fn canvas_context() -> PlanContext {
    use tilo_actions::{ActionParam, OperationKind, ParamType};
    let draw = operation(
        "canvas.artwork",
        "draw",
        OperationKind::Document,
        vec![
            ActionParam::required("marks", ParamType::Array, "The marks to draw").with_schema(
                serde_json::json!({"type": "array", "items": {"type": "object",
                        "properties": {"shape": {"oneOf": [{"kind": {"const": "rect"}}]}}}}),
            ),
            ActionParam::optional("layer", ParamType::Text, "The layer to draw on")
                .with_choices(["paint", "ink"]),
        ],
    );
    let apply = operation(
        "canvas.artwork",
        "apply",
        OperationKind::Document,
        vec![
            ActionParam::required("action", ParamType::Object, "Any action")
                .with_schema(serde_json::json!({"description": "x".repeat(MOST_OPERATION_CHARS)})),
        ],
    );
    let published = vec![draw, apply, inspect_view("canvas.artwork")];
    PlanContext {
        open: vec![(route("so.tilo.canvas", "c1"), published.clone())],
        apps: vec!["so.tilo.canvas".into()],
        reference: vec![("so.tilo.canvas".into(), published)],
        room: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
        ..PlanContext::default()
    }
}

/// The first turn carries the apps' operations with their parameters in
/// full (types, required, choices, constraints), then the operations
/// published now, then what the record holds, then the request — the
/// stable part first — and reveals what it shows; an operation whose
/// schema alone is beyond the budget is left to `describe_operation`.
#[test]
fn the_first_turn_shows_full_schemas_and_what_the_record_holds() {
    let mut context = canvas_context();
    context.snapshots = vec![
        "(document c1) so.tilo.canvas canvas.artwork#inspect answered \
             {\"artwork\":{\"width\":100,\"height\":50}}"
            .into(),
    ];
    let (offered, _) = catalogue(&context);
    let task = "draw a teal rectangle on the right";
    let briefed = briefing(&context, &offered);
    let text = &briefed.reference;
    assert!(text.starts_with(REFERENCE_INTRO), "{text}");
    assert!(
        text.contains("## so.tilo.canvas\ncanvas.artwork#draw — draw. What draw does.")
            && text.contains("    marks: array — The marks to draw; shape: ")
            && text.contains(
                "    layer: text (optional) — The layer to draw on; one of \"paint\"|\"ink\""
            ),
        "{text}"
    );
    assert!(
        !text.contains("xxxxxxxxxx"),
        "the oversized apply is left out: {text}"
    );
    // The generic apply is not offered where typed edits cover it.
    assert!(!text.contains("canvas.artwork#apply"), "{text}");
    assert!(
        briefed.state.starts_with(SNAPSHOT_INTRO) && briefed.state.contains(r#""width":100"#),
        "{}",
        briefed.state
    );
    assert!(briefed.shown.contains(&"canvas.artwork#draw".to_owned()));
    assert!(!briefed.shown.contains(&"canvas.artwork#apply".to_owned()));

    // The reference, the operations, the state, then the request; what
    // the reference shows may be planned.
    let mut revealed = tilo_agent::tools::Revealed::new();
    let turn = first_turn_text(task, &briefed, &offered, &mut revealed);
    assert!(turn.starts_with(REFERENCE_INTRO), "{turn}");
    let operations = turn.find("Operations published right now").unwrap();
    let state = turn.find(SNAPSHOT_INTRO).unwrap();
    let request = turn.find(task).unwrap();
    assert!(operations < state && state < request, "{turn}");
    assert!(revealed.contains("canvas.artwork#draw"));

    // Nothing granted with operations: no briefing beyond its key.
    let (offered, _) = catalogue(&PlanContext {
        apps: vec!["so.tilo.notes".into()],
        ..PlanContext::default()
    });
    let empty = briefing(&PlanContext::default(), &offered);
    assert!(empty.text().is_empty() && empty.shown.is_empty());
}

/// A video project open with `clips` clips on its timeline, each
/// publishing the same 25 operations, beside the project's own; the
/// reference is what an empty project publishes.
fn timeline_context(clips: usize) -> PlanContext {
    use tilo_actions::OperationKind;
    let app = "so.tilo.video-editor";
    let mut project: Vec<ActionDescriptor> = ["add-clip", "add-title", "move-clip"]
        .into_iter()
        .map(|action| {
            operation(
                "video_editor.project",
                action,
                OperationKind::Document,
                vec![],
            )
        })
        .collect();
    project.push(operation(
        "video_editor.project",
        "export-video-content",
        OperationKind::Effect,
        vec![],
    ));
    let mut published = project.clone();
    for n in 0..clips {
        for op in 0..25 {
            let action = if op == 0 {
                "set-fade-out".to_owned()
            } else {
                format!("set-op-{op}")
            };
            let mut clip = operation(
                &format!("video.clip:{n:08x}-1111-4000-8000-000000000000"),
                &action,
                OperationKind::Document,
                vec![],
            );
            clip.resource_kind = "video.clip".into();
            clip.resource_label = format!("Clip {n}");
            published.push(clip);
        }
    }
    PlanContext {
        open: vec![(route(app, "v1"), published)],
        apps: vec![app.into()],
        reference: vec![(app.into(), project)],
        room: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
        ..PlanContext::default()
    }
}

/// Clips a plan's steps added publish many operations each: a re-plan
/// lists the clip whose id an earlier step returned, and every clip's
/// operations in one line each, so the project's export is not crowded
/// out; the cached reference stays the same bytes as before the clips.
#[test]
fn a_replan_lists_the_clips_earlier_steps_added_beside_the_projects_export() {
    let mut context = timeline_context(40);
    context.request = Some("cut the promo and fade the last clip out".into());
    context.observed = vec![Answered {
        seen: "so.tilo.video-editor video_editor.project#add-clip answered              {\"clip\":\"00000027-1111-4000-8000-000000000000\",\"track\":\"V1\"}"
            .into(),
        untrusted: false,
    }];
    let (offered, _) = catalogue(&context);
    assert!(offered.offers.len() > 10 * MOST_PUBLISHED);
    let briefed = briefing(&context, &offered);
    let mut revealed = tilo_agent::tools::Revealed::new();
    let task = with_results("fade the last clip out, then export", &context);
    let turn = first_turn_text(&task, &briefed, &offered, &mut revealed);
    assert!(
        revealed.contains("video.clip:00000027-1111-4000-8000-000000000000#set-fade-out"),
        "the clip the step returned may be named"
    );
    assert!(
        revealed.contains("video.clip:00000003-1111-4000-8000-000000000000#set-op-24"),
        "every clip's operations are listed in their line"
    );
    assert!(
        revealed.contains("video_editor.project#export-video-content"),
        "the project's export is not crowded out: {turn}"
    );
    assert!(
        turn.contains("40 video.clip resources"),
        "the clips share a line: {turn}"
    );
    let before = briefing(&timeline_context(0), &catalogue(&timeline_context(0)).0);
    assert_eq!(
        briefed.reference, before.reference,
        "the cached prefix is stable"
    );
    assert!(turn.starts_with(&briefed.reference));
}

/// The casework room after a wait: Chat open on the room's record with
/// Lena's channel, Mail with `messages` messages and Sheets
/// with `sheets` sheets, each resource publishing a dozen operations.
fn casework_context(messages: usize, sheets: usize) -> PlanContext {
    use tilo_actions::OperationKind;
    let alike = |resource: &str, kind: &str, label: &str, actions: &[&str]| {
        actions
            .iter()
            .map(|action| {
                let mut descriptor = operation(resource, action, OperationKind::Document, vec![]);
                descriptor.resource_kind = kind.into();
                descriptor.resource_label = label.into();
                descriptor
            })
            .collect::<Vec<_>>()
    };
    let chat = alike(
        "chat.channel:wa-lena-hoffmann",
        "chat.channel",
        "#WhatsApp: Lena Hoffmann (+49 151 2345 6789)",
        &[
            "share-resource",
            "deliver-message",
            "submit-feedback",
            "post",
            "rename",
            "archive",
            "mark-read",
            "delete",
        ],
    );
    let ops: Vec<String> = (0..12).map(|op| format!("op-{op}")).collect();
    let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
    let mut mail = Vec::new();
    for n in 0..messages {
        mail.extend(alike(
            &format!("mail.message:{n:08x}-2222-4000-8000-000000000000"),
            "mail.message",
            &format!("Claim HM-4821{n}"),
            &ops,
        ));
    }
    let mut workbook = Vec::new();
    for n in 0..sheets {
        // Each sheet its own operations, so none share a line.
        let own: Vec<String> = ops.iter().map(|op| format!("{op}-{n}")).collect();
        let own: Vec<&str> = own.iter().map(String::as_str).collect();
        workbook.extend(alike(
            &format!("sheets.sheet:{n:08x}-3333-4000-8000-000000000000"),
            "sheets.sheet",
            &format!("Sheet {n}"),
            &own,
        ));
    }
    PlanContext {
        open: vec![
            (route("so.tilo.chat", ""), chat),
            (route("so.tilo.mail", "m1"), mail),
            (route("so.tilo.sheets", "w1"), workbook),
        ],
        apps: vec![
            "so.tilo.chat".into(),
            "so.tilo.mail".into(),
            "so.tilo.sheets".into(),
        ],
        room: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
        ..PlanContext::default()
    }
}

/// The plan after a wait carries what the first plan's reads answered,
/// which names every mail message and sheet: more operations than the
/// listing's cap. They are listed on top of each app's share, so the
/// Chat channel the reply goes to still offers `post`, named by its
/// label without Chat's `#` or not named at all (r2-casework on v19:
/// seven of eight models never replied in Chat).
#[test]
fn a_replan_whose_reads_name_many_records_still_lists_the_channel_to_reply_in() {
    let mut context = casework_context(8, 8);
    let ids: Vec<String> = context
        .open
        .iter()
        .flat_map(|(_, descriptors)| descriptors)
        .filter(|descriptor| !descriptor.address.resource.starts_with("chat."))
        .map(|descriptor| descriptor.address.resource.clone())
        .collect();
    context.observed = vec![Answered {
        seen: format!("the reads answered {}", ids.join(", ")),
        untrusted: false,
    }];
    let (offered, _) = catalogue(&context);
    assert!(offered.offers.len() > MOST_PUBLISHED + 50);
    let briefed = briefing(&context, &offered);
    for (request, rest) in [
        (
            "Reply briefly in her \"WhatsApp: Lena Hoffmann (+49 151 2345 6789)\" channel in Chat",
            "What you waited for arrived in mail. Then: reply in her WhatsApp channel",
        ),
        (
            "Update the case, then reply to her in Chat",
            "What you waited for arrived in mail. Then: reply to her",
        ),
    ] {
        context.request = Some(request.into());
        let mut revealed = tilo_agent::tools::Revealed::new();
        let task = with_results(rest, &context);
        let turn = first_turn_text(&task, &briefed, &offered, &mut revealed);
        assert!(
            revealed.contains("chat.channel:wa-lena-hoffmann#post"),
            "the channel is not crowded out ({request}): {turn}"
        );
        assert!(
            revealed.contains("mail.message:00000007-2222-4000-8000-000000000000#op-11"),
            "every message the reads named is listed past the cap: {turn}"
        );
    }
}

/// The reference is the same bytes whatever the task, the records open
/// and what they hold — only the apps granted shape it — and stays
/// within its budget per app, document operations first.
#[test]
fn the_reference_is_stable_across_tasks_and_records_and_bounded() {
    use tilo_actions::{ActionParam, OperationKind, ParamType};
    let many: Vec<ActionDescriptor> = (0..120)
        .map(|at| {
            operation(
                "sheets.workbook",
                &format!("operation-{at}"),
                if at % 2 == 0 {
                    OperationKind::Effect
                } else {
                    OperationKind::Document
                },
                vec![
                    ActionParam::required("value", ParamType::Text, "A value")
                        .with_schema(serde_json::json!({"description": "y".repeat(400)})),
                ],
            )
        })
        .collect();
    let previewed = PlanContext {
        previews: vec![("so.tilo.sheets".into(), many.clone())],
        apps: vec!["so.tilo.sheets".into()],
        reference: vec![("so.tilo.sheets".into(), many.clone())],
        ..PlanContext::default()
    };
    let mut open = PlanContext {
        open: vec![(route("so.tilo.sheets", "s1"), many.clone())],
        apps: vec!["so.tilo.sheets".into()],
        reference: vec![("so.tilo.sheets".into(), many)],
        snapshots: vec!["(document s1) so.tilo.sheets … answered {}".into()],
        ..PlanContext::default()
    };
    let first = briefing(&previewed, &catalogue(&previewed).0);
    let second = briefing(&open, &catalogue(&open).0);
    open.request = Some("something else entirely".into());
    let third = briefing(&open, &catalogue(&open).0);
    assert_eq!(first.reference, second.reference);
    assert_eq!(second.reference, third.reference);
    assert_ne!(first.state, second.state);
    assert!(
        first
            .state
            .contains("not open yet: planning any of its operations"),
        "{}",
        first.state
    );
    let section = first.reference.split_once("## so.tilo.sheets\n").unwrap().1;
    assert!(
        section.chars().count() <= MOST_REFERENCE_CHARS + 300,
        "{}",
        section.chars().count()
    );
    assert!(
        section.starts_with("sheets.workbook#operation-1 —"),
        "documents first"
    );
    assert!(!first.shown.is_empty(), "what fits is shown");
}

/// While a document of an app is open its bare open is not offered:
/// its operations are. Once an import into it was refused for what it
/// holds, a new, empty document of the app is offered beside it, said
/// to be a second document (not the one to use instead), and planned it
/// is an open naming no record, which opens a new one.
#[test]
fn a_refused_import_offers_a_new_document_beside_the_open_one() {
    use tilo_actions::{ActionParam, OperationKind, ParamType};
    let writer = "so.tilo.word-processor";
    let import = operation(
        "tilo.rich-text",
        "import-docx-content",
        OperationKind::Document,
        vec![ActionParam::required("content", ParamType::Text, "A .docx")],
    );
    let mut context = PlanContext {
        open: vec![(route(writer, "evidence"), vec![import])],
        apps: vec![writer.into()],
        ..PlanContext::default()
    };
    let new_open = format!("app.{writer}#open");
    let offered = |context: &PlanContext| {
        let (catalogue, targets) = catalogue(context);
        let ids: Vec<String> = catalogue.offers.iter().map(Offer::id).collect();
        (catalogue, targets, ids)
    };
    let (_, _, ids) = offered(&context);
    assert!(!ids.contains(&new_open), "{ids:?}");

    context.new_documents = vec![writer.into()];
    let (catalogue, targets, ids) = offered(&context);
    assert!(ids.contains(&new_open), "{ids:?}");
    assert!(matches!(targets.get(&new_open), Some(Target::Open(app)) if app == writer));
    let summary = tilo_agent::tools::catalogue_summary(
        &catalogue,
        &mut tilo_agent::tools::Revealed::new(),
        64,
    );
    assert!(
        summary.contains("opens a second window on a new, empty document"),
        "{summary}"
    );
    assert!(
        !summary.contains("this would open a second window)"),
        "{summary}"
    );
}

/// An app the task is about with nothing open is offered with what a
/// new record of it publishes, in place of its bare open: a step
/// naming one runs on a new record the step opens.
#[test]
fn a_previewed_app_is_planned_without_opening_it_first() {
    use tilo_actions::{ActionParam, OperationKind, ParamType};
    let set_cells = operation(
        "sheets.sheet:1",
        "set-cells",
        OperationKind::Document,
        vec![ActionParam::required("rows", ParamType::Array, "Rows")],
    );
    let context = PlanContext {
        previews: vec![("so.tilo.sheets".into(), vec![set_cells])],
        known: vec![(route("so.tilo.sheets", "s9"), Some("Old plan".into()))],
        apps: vec!["so.tilo.sheets".into()],
        ..PlanContext::default()
    };
    let ids: Vec<String> = catalogue(&context).0.offers.iter().map(Offer::id).collect();
    assert!(
        ids.contains(&"sheets.sheet:1#set-cells".to_owned()),
        "{ids:?}"
    );
    assert!(
        !ids.contains(&"app.so.tilo.sheets#open".to_owned()),
        "{ids:?}"
    );
    assert!(
        ids.contains(&"record.so.tilo.sheets.s9#open".to_owned()),
        "the room's documents stay offered: {ids:?}"
    );
    let steps = canned(&["sheets.sheet:1#set-cells"], "", None)
        .plan("put Visits in A1", &context)
        .unwrap();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].app, "so.tilo.sheets");
    assert_eq!(steps[0].record, None, "the step opens a new record");
    assert!(matches!(
        &steps[0].action,
        StepAction::Invoke { address, .. } if address.action == "set-cells"
    ));
}

/// A re-plan's rest is shown with the request as it was given, so a
/// clause the model left out of `then` is still asked for; a first plan
/// is not given it twice.
#[test]
fn a_replan_sees_the_original_request() {
    let mut context = context();
    let asked = std::sync::Arc::default();
    let mut planner = ModelPlanner::new(Box::new(Recording {
        asked: std::sync::Arc::clone(&asked),
        answer: offer_id(&context, "notes.page#edit"),
    }));
    let original = "open the notes, add a page \"Plans\" and tag it \"launch\"";
    context.request = Some(original.into());
    planner.plan(original, &context).unwrap();
    planner.plan("add a page \"Plans\"", &context).unwrap();
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked[0], original, "the request itself is not repeated");
    assert!(
        asked[1].starts_with("add a page \"Plans\"")
            && asked[1].contains(&format!("{ORIGINAL_INTRO} {original}")),
        "{}",
        asked[1]
    );
    // The scripted model reads the rest, not the original beside it.
    assert_eq!(
        clauses(requested(&asked[1])),
        vec!["add a page \"Plans\"".to_owned()]
    );
}

fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// The size of a first turn for a real two-app agent (Canvas and Word
/// Processor, as the campaign's designer has), before and after: before,
/// the compact list alone, which took open, re-plan and describe turns
/// to use; after, the apps' reference in full, the list and what an open
/// artwork holds, planned in one turn. Printed, and bounded.
#[test]
fn a_first_turn_for_real_apps_is_bounded_and_its_prefix_shared() {
    let catalogue_of_apps = crate::AppCatalogue::native();
    let survey = |app: &str| {
        crate::worker::ConfinedReplica::open(
            &catalogue_of_apps,
            app,
            "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71",
            "measure",
            &crate::ReplicaActor::Agent,
            crate::Seed::New,
        )
        .unwrap()
        .survey()
    };
    let (canvas, writer) = ("so.tilo.canvas", "so.tilo.word-processor");
    let (canvas_ops, writer_ops) = (survey(canvas), survey(writer));
    let task = "In Canvas, export the current artwork as a PNG at scale 1 kept as content, \
                    then in the Word Processor document \"Launch page\" insert that exported \
                    image after the introduction paragraph.";
    let before_context = PlanContext {
        open: vec![(route(canvas, "c1"), canvas_ops.clone())],
        known: vec![(route(writer, "w1"), Some("Launch page".into()))],
        apps: vec![canvas.into(), writer.into()],
        ..PlanContext::default()
    };
    let mut revealed = tilo_agent::tools::Revealed::new();
    let before =
        tilo_agent::model::action_user_turn(task, &catalogue(&before_context).0, &mut revealed)
            .text();
    let after_context = PlanContext {
        open: vec![
            (route(canvas, "c1"), canvas_ops.clone()),
            (route(writer, "w1"), writer_ops.clone()),
        ],
        apps: vec![canvas.into(), writer.into()],
        reference: vec![(canvas.into(), canvas_ops), (writer.into(), writer_ops)],
        snapshots: vec![
            format!(
                "(document c1) {canvas} canvas.artwork#inspect answered {}",
                "x".repeat(3_000)
            ),
            format!(
                "(document w1) {writer} tilo.rich-text#read-text answered {}",
                "y".repeat(3_000)
            ),
        ],
        room: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
        ..PlanContext::default()
    };
    let (offered, _) = catalogue(&after_context);
    let briefed = briefing(&after_context, &offered);
    let mut revealed = tilo_agent::tools::Revealed::new();
    let after = first_turn_text(task, &briefed, &offered, &mut revealed);
    eprintln!(
        "first turn, Canvas + Word Processor: before {} tokens (list only; more turns \
             needed), after {} tokens (reference {} + list + state {}); stable prefix {} tokens",
        tokens(&before),
        tokens(&after),
        tokens(&briefed.reference),
        tokens(&briefed.state),
        tokens(&briefed.reference),
    );
    // The generic `apply` schemas are shown in full; cached, they cost
    // little, and the model plans without asking for them.
    assert!(tokens(&after) < 40_000, "{}", tokens(&after));
    assert!(tokens(&briefed.reference) > tokens(&briefed.state));
    assert!(after.contains("word-processor.document#"), "{after}");
    assert!(after.contains("canvas.artwork#export"), "{after}");
    // The same reference for another task of the same agent.
    let again = first_turn_text("Draw a teal band", &briefed, &offered, &mut revealed);
    assert!(again.starts_with(&briefed.reference));
    assert!(after.starts_with(&briefed.reference));
}

/// Answers with whatever it was built with, and is shown pictures.
struct SeeingCanned(Answer, bool);

impl PlanModel for SeeingCanned {
    fn choose(&mut self, _: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        Ok(self.0.clone())
    }

    fn sees(&self) -> bool {
        self.1
    }
}

/// A model that sees and plans frames, then a sign-off that depends on
/// them, in one plan: the plan ends after the frames and the sign-off is
/// planned again with them shown, told what it had meant to write. A
/// model that does not see, or a plan already shown the pictures, runs
/// as planned.
#[test]
fn a_write_planned_after_pictures_waits_for_the_pictures_to_be_seen() {
    use tilo_actions::{ActionParam, OperationKind, ParamType};
    let editor = "so.tilo.video-editor";
    let frame = operation(
        "video_editor.project",
        "view-frame-content",
        OperationKind::Effect,
        vec![ActionParam::required("at", ParamType::Number, "Seconds")],
    );
    let sign_off = operation(
        "video_editor.project",
        "set-title",
        OperationKind::Document,
        vec![ActionParam::required("text", ParamType::Text, "Words")],
    );
    let context = PlanContext {
        open: vec![(route(editor, "cut"), vec![frame, sign_off])],
        apps: vec![editor.into()],
        ..PlanContext::default()
    };
    let (offered, _) = catalogue(&context);
    let id = |action: &str| {
        offered
            .offers
            .iter()
            .find(|offer| offer.address().action == action)
            .map(Offer::id)
            .unwrap()
    };
    let argument =
        |key: &str, value: serde_json::Value| serde_json::Map::from_iter([(key.to_owned(), value)]);
    let answer = Answer {
        actions: vec![
            id("view-frame-content"),
            id("view-frame-content"),
            id("set-title"),
        ],
        arguments: vec![
            argument("at", 1.5.into()),
            argument("at", 4.5.into()),
            argument("text", "HERON approved".into()),
        ],
        why: "check the cut".into(),
        then: Some("post the verdict".into()),
        reply: None,
        wait: None,
        ..Answer::default()
    };
    let plan = |sees: bool, context: &PlanContext| {
        ModelPlanner::new(Box::new(SeeingCanned(answer.clone(), sees)))
            .plan("review the cut and sign it off", context)
            .unwrap()
    };

    let steps = plan(true, &context);
    let actions: Vec<String> = steps
        .iter()
        .filter_map(|step| match &step.action {
            StepAction::Invoke { address, .. } => Some(address.action.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(actions, ["view-frame-content", "view-frame-content"]);
    let Some(StepAction::Continue(rest)) = steps.last().map(|step| &step.action) else {
        panic!("the rest is planned again: {steps:?}");
    };
    assert!(
        rest.contains("written before the pictures were seen")
            && rest.contains("set-title {\"text\":\"HERON approved\"}")
            && rest.ends_with("Then: post the verdict"),
        "{rest}"
    );

    assert_eq!(plan(false, &context).len(), 4, "three steps and the rest");
    let mut shown = context;
    shown.pictures = vec![crate::vision::tests::address('a')];
    let again = plan(true, &shown);
    assert!(
        again.iter().any(|step| matches!(&step.action,
                StepAction::Invoke { address, .. } if address.action == "set-title")),
        "a plan shown the pictures is not cut again: {again:?}"
    );
}

/// A canvas open on `c1` publishing `look` beside `draw` and `inspect`.
pub(crate) fn look_context() -> PlanContext {
    let mut context = canvas_context();
    let mut look = inspect_view("canvas.artwork");
    look.address.action = "look".into();
    look.action_label = "look".into();
    context.open[0].1.push(look.clone());
    context.reference[0].1.push(look);
    context
}

/// Reads whose every read renders the picture at `rendered`, with the
/// room's content in `content`; it keeps what it was asked to look at.
pub(crate) struct Looks {
    pub(crate) rendered: String,
    pub(crate) content: BTreeMap<String, Vec<u8>>,
    pub(crate) looked: Vec<String>,
}

impl Looks {
    pub(crate) fn new(rendered: char) -> Self {
        let rendered = crate::vision::tests::address(rendered);
        let mut content = BTreeMap::new();
        content.insert(
            crate::vision::content_hash(&rendered).unwrap(),
            crate::vision::tests::png(8, 6),
        );
        Self {
            rendered,
            content,
            looked: Vec::new(),
        }
    }

    /// Keep `bytes` as the room's content at the address of `fill`.
    pub(crate) fn keep(&mut self, fill: char, bytes: Vec<u8>) -> String {
        let address = crate::vision::tests::address(fill);
        self.content
            .insert(crate::vision::content_hash(&address).unwrap(), bytes);
        address
    }
}

impl Reads for Looks {
    fn read(
        &mut self,
        _: &DocumentRoute,
        _: &ActionAddress,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"width": 8, "height": 6,
                    "content": self.rendered, "revision": "r1"})),
            resources: vec![self.rendered.clone()],
        })
    }

    fn look(&mut self, hash: &str) -> Result<Vec<u8>, String> {
        self.looked.push(hash.to_owned());
        self.content
            .get(hash)
            .cloned()
            .ok_or_else(|| "the room keeps no such content".into())
    }
}

/// Turns answered from a script, every request kept.
struct Script {
    answers: std::collections::VecDeque<Result<ModelResponse, ModelError>>,
    sent: std::sync::Arc<std::sync::Mutex<Vec<ModelRequest>>>,
}

impl PlanTurns for Script {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.sent.lock().unwrap().push(request.clone());
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(ModelError::new(ErrorKind::Unavailable)))
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// A model planning in the conversation over scripted turns, shown
/// pictures when it `sees`.
struct ScriptModel {
    turns: Script,
    sees: bool,
}

impl PlanModel for ScriptModel {
    fn choose(&mut self, task: &str, catalogue: &Catalogue, most: usize) -> Result<Answer, String> {
        self.choose_briefed(
            task,
            &Briefing::default(),
            catalogue,
            most,
            &mut NoPlanReads,
        )
    }

    fn choose_briefed(
        &mut self,
        task: &str,
        briefing: &Briefing,
        catalogue: &Catalogue,
        most_steps: usize,
        read: &mut ReadView<'_>,
    ) -> Result<Answer, String> {
        let sees = self.sees;
        converse::plan(
            &mut self.turns,
            &converse::Asked {
                task,
                briefing,
                catalogue,
                most_steps,
                sees,
                timeout: tilo_test_env::hang_bound(),
            },
            read,
        )
    }

    fn sees(&self) -> bool {
        self.sees
    }
}

pub(crate) type Sent = std::sync::Arc<std::sync::Mutex<Vec<ModelRequest>>>;

/// A planner over `answers`, and the requests it sends.
pub(crate) fn scripted(answers: Vec<ModelResponse>, sees: bool) -> (ModelPlanner, Sent) {
    let sent = Sent::default();
    let model = ScriptModel {
        turns: Script {
            answers: answers.into_iter().map(Ok).collect(),
            sent: std::sync::Arc::clone(&sent),
        },
        sees,
    };
    (ModelPlanner::new(Box::new(model)), sent)
}

/// A turn calling `name` with `input`.
pub(crate) fn called(id: &str, name: &str, input: &serde_json::Value) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: id.into(),
                name: name.into(),
                input: input.clone(),
                extra_content: None,
            })],
        },
        stop_reason: StopReason::ToolCalls,
        usage: Usage::default(),
    }
}

/// A turn that only says `text`, stopping for `stop`.
fn said(text: &str, stop: StopReason) -> ModelResponse {
    ModelResponse {
        message: Message::text(Role::Assistant, text),
        stop_reason: stop,
        usage: Usage::default(),
    }
}

/// The tool results of the last tool message of `request`, in order.
pub(crate) fn results(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Tool)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::ToolResult(result) => Some(result.content.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The text of `message`.
pub(crate) fn text_of(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// How many pictures `message` carries.
pub(crate) fn pictures(message: &Message) -> usize {
    message
        .content
        .iter()
        .filter(|content| matches!(content, Content::Image { .. }))
        .count()
}

fn tool_names(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

/// Reads the record names, answering an artwork's size.
struct Measures {
    read: Vec<(DocumentRoute, ActionAddress)>,
}

impl Reads for Measures {
    fn read(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.read.push((route.clone(), address.clone()));
        Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"artwork": {"width": 640, "height": 480},
                "image": format!("data:image/png;base64,{}", "A".repeat(10_000))})),
            resources: Vec::new(),
        })
    }
}

/// `read_view` runs a view of the open record in the planning
/// conversation: its answer (the artwork's size, encoded data left out) is
/// the tool result of the same conversation, which then plans the edit —
/// no re-plan. A view that is not a read, or an unshown id, is refused
/// without running anything.
#[test]
fn read_view_answers_in_the_same_conversation() {
    let context = canvas_context();
    let (mut planner, sent) = scripted(
        vec![
            called(
                "r0",
                READ_VIEW,
                &serde_json::json!({"operation": "canvas.artwork#draw"}),
            ),
            called(
                "r1",
                READ_VIEW,
                &serde_json::json!({"operation": "canvas.artwork#inspect", "arguments": {}}),
            ),
            called(
                "p1",
                tilo_agent::tools::PLAN,
                &serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
                    "arguments": {"marks": [{"shape": {"kind": "rect", "x": 396, "y": 0,
                        "width": 244, "height": 480}}]}}], "then": ""}),
            ),
        ],
        false,
    );
    let mut reads = Measures { read: Vec::new() };
    let steps = planner
        .plan_reading(
            "add a band over the right 38% of the picture",
            &context,
            &mut reads,
        )
        .unwrap();
    assert_eq!(steps.len(), 1, "planned at once, no re-plan: {steps:?}");
    assert!(matches!(
        &steps[0].action,
        StepAction::Invoke { address, arguments }
            if address.action == "draw" && arguments["marks"][0]["shape"]["x"] == 396
    ));
    assert_eq!(
        reads.read,
        vec![(
            route("so.tilo.canvas", "c1"),
            ActionAddress::new("canvas.artwork", "inspect")
        )],
        "only the read ran"
    );
    let requests = sent.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(tool_names(&requests[0]).contains(&READ_VIEW.to_owned()));
    assert_eq!(requests[0].tools, requests[2].tools, "stable tools");
    let refused = &results(&requests[1])[0];
    assert!(
        refused.starts_with("Not read:") && refused.contains("plan it as a step"),
        "{refused}"
    );
    let answered = &results(&requests[2])[0];
    assert!(
        answered.contains("canvas.artwork#inspect answered")
            && answered.contains(r#""width":640"#)
            && answered.contains(r#""height":480"#)
            && !answered.contains("AAAAAAAA"),
        "{answered}"
    );
    let first = text_of(&requests[0].messages[1]);
    assert!(first.contains("marks: array — "), "{first}");
    let system = text_of(&requests[0].messages[0]);
    assert!(
        system.contains("WHOLE request in one `plan`") && system.contains("read_view"),
        "{system}"
    );

    // Without reads (a plain `plan`), a read is answered as not run.
    let (mut planner, sent) = scripted(
        vec![
            called(
                "r1",
                READ_VIEW,
                &serde_json::json!({"operation": "canvas.artwork#inspect"}),
            ),
            called(
                "p1",
                tilo_agent::tools::PLAN,
                &serde_json::json!({"steps": [{"operation": "canvas.artwork#inspect",
                    "arguments": {}}], "then": "draw the band"}),
            ),
        ],
        false,
    );
    let steps = planner.plan("add a band", &context).unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    let second = sent.lock().unwrap()[1].clone();
    assert!(results(&second)[0].starts_with("Not read:"));
}

/// A room with many word-processor documents (more than are offered by
/// title), among them the Q3 partner brief, a stale copy and a decoy.
fn searched_context() -> (PlanContext, DocumentRoute) {
    let writer = "so.tilo.word-processor";
    let mut context = context();
    context.apps.push(writer.into());
    let mut searchable = Vec::new();
    for at in 0..KNOWN_OFFERED.saturating_add(4) {
        let record = format!("uuid:w{at}");
        context
            .known
            .push((route(writer, &record), Some(format!("Notes {at}"))));
        let text = match at {
            14 => "Agenda for the Q3 partner review with Northwind: revenue share.",
            3 => "Q2 partner review (superseded).",
            5 => "Code review checklist.",
            _ => "Weekly sync.",
        };
        searchable.push(crate::search::SearchDoc {
            route: route(writer, &record),
            title: Some(format!("Notes {at}")),
            text: text.into(),
            changed: Some(u64::try_from(at).unwrap()),
            open: false,
        });
    }
    context.searchable = searchable;
    (context, route(writer, "uuid:w14"))
}

/// `search_room` finds a record by what it holds, beyond the titles
/// offered and among a stale copy and a decoy; the plan opens it by the id
/// the answer showed, which no plan could name before the search.
#[test]
fn search_room_finds_a_record_and_the_plan_opens_it() {
    let (context, found) = searched_context();
    let open = crate::search::open_id(&found);
    let (catalogue, _) = catalogue(&context);
    assert!(
        !catalogue.offers.iter().any(|offer| offer.id() == open),
        "beyond the records offered by title"
    );
    let plan = serde_json::json!({"steps": [{"operation": open, "arguments": {}}],
        "then": "make the deck from it"});
    let (mut planner, sent) = scripted(
        vec![
            called("p0", tilo_agent::tools::PLAN, &plan),
            called(
                "s1",
                crate::search::SEARCH_ROOM,
                &serde_json::json!({"query": "Q3 partner review", "app": "word processor"}),
            ),
            called("p1", tilo_agent::tools::PLAN, &plan),
        ],
        false,
    );
    let steps = planner
        .plan("make a deck for the Q3 partner review", &context)
        .unwrap();
    assert_eq!(
        steps[0],
        Step {
            app: found.app_id,
            record: Some(found.record),
            action: StepAction::Open,
            carries: None,
            serves: None,
        }
    );
    assert!(matches!(&steps[1].action, StepAction::Continue(rest) if rest.contains("deck")));
    let requests = sent.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(tool_names(&requests[0]).contains(&crate::search::SEARCH_ROOM.to_owned()));
    assert_eq!(requests[0].tools, requests[2].tools, "stable tools");
    let refused = &results(&requests[1])[0];
    assert!(refused.contains("Plan refused"), "unsearched: {refused}");
    let found_text = &results(&requests[2])[0];
    assert!(
        found_text.contains("\n1. so.tilo.word-processor \"Notes 14\" (record uuid:w14,"),
        "the brief first: {found_text}"
    );
    assert!(
        found_text.contains(&open) && found_text.contains("Northwind"),
        "{found_text}"
    );
    let system = text_of(&requests[0].messages[0]);
    assert!(
        system.contains("when it is offered, only some are, so call `search_room` first"),
        "{system}"
    );
}

/// What `search_room` adds to every request, in tokens: bounded.
#[test]
fn the_search_tool_costs_little() {
    let definition = serde_json::to_string(&crate::search::definition()).unwrap();
    assert!(tokens(&definition) < 120, "{definition}");
}

fn look_read() -> serde_json::Value {
    serde_json::json!({"operation": "canvas.artwork#look", "arguments": {}})
}

fn draw_plan() -> serde_json::Value {
    serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
        "arguments": {"marks": []}}], "then": ""})
}

/// A tool result takes text alone: a look's picture follows the tool
/// results in one user message, which names it first.
#[test]
fn a_look_read_shows_its_picture_after_the_tool_results() {
    let (mut planner, sent) = scripted(
        vec![
            called("r1", READ_VIEW, &look_read()),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        true,
    );
    let mut reads = Looks::new('a');
    planner
        .plan_reading("look, then draw", &look_context(), &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    assert!(tool_names(&requests[0]).contains(&LOOK_CONTENT.to_owned()));
    let messages = &requests[1].messages;
    assert_eq!(messages[3].role, Role::Tool);
    assert!(results(&requests[1])[0].contains("picture shown below, 8×6 PNG"));
    assert_eq!(messages[4].role, Role::User);
    assert!(text_of(&messages[4]).starts_with("The pictures the calls above showed"));
    assert_eq!(pictures(&messages[4]), 1);
}

/// A model that is not shown pictures is not offered `look_content`, reads
/// that a look's picture was not shown, and nothing is fetched.
#[test]
fn a_model_that_does_not_see_gets_no_look_content_and_is_told_so() {
    let (mut planner, sent) = scripted(
        vec![
            called("r1", READ_VIEW, &look_read()),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        false,
    );
    let mut reads = Looks::new('a');
    planner
        .plan_reading("look, then draw", &look_context(), &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    assert!(!tool_names(&requests[0]).contains(&LOOK_CONTENT.to_owned()));
    let answered = &results(&requests[1])[0];
    assert!(
        answered.contains(&reads.rendered) && answered.contains("picture not shown"),
        "{answered}"
    );
    assert_eq!(requests[1].messages.len(), 4, "no pictures message");
    assert!(reads.looked.is_empty(), "nothing fetched for a blind model");
    assert!(
        requests[1]
            .messages
            .iter()
            .all(|message| pictures(message) == 0)
    );
}

/// `look_content` looks only at an address the conversation named, and at
/// a room's own picture only where the agent may show them; a PDF is
/// pointed to pdf-viewer. Allowed, a room's picture is written out again in
/// the confined worker and shown.
#[test]
fn look_content_refuses_unshown_addresses_and_room_pictures_unless_allowed() {
    let mut reads = Looks::new('a');
    let scan = reads.keep('b', crate::vision::tests::png(10, 10));
    let pdf = reads.keep('c', b"%PDF-1.4\n".to_vec());
    let unshown = crate::vision::tests::address('d');
    let look = |address: &str| serde_json::json!({"content": address});
    let (mut planner, sent) = scripted(
        vec![
            called("l1", LOOK_CONTENT, &look(&unshown)),
            called("l2", LOOK_CONTENT, &look(&scan)),
            called("l3", LOOK_CONTENT, &look(&pdf)),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        true,
    );
    let task = format!("trace the scan {scan} onto the canvas; the brief is {pdf}");
    planner
        .plan_reading(&task, &look_context(), &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    let unnamed = &results(&requests[1])[0];
    assert!(
        unnamed.starts_with("Not shown:") && unnamed.contains("was not named"),
        "{unnamed}"
    );
    let room = &results(&requests[2])[0];
    assert!(room.contains("not shown the room's pictures"), "{room}");
    let document = &results(&requests[3])[0];
    assert!(
        document.contains("is a PDF") && document.contains("look-page"),
        "{document}"
    );
    assert!(
        !reads
            .looked
            .contains(&crate::vision::content_hash(&unshown).unwrap()),
        "an unnamed address is never fetched"
    );

    let (mut planner, sent) = scripted(
        vec![
            called("l1", LOOK_CONTENT, &look(&scan)),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        true,
    );
    let mut context = look_context();
    context.room_pictures = true;
    planner.plan_reading(&task, &context, &mut reads).unwrap();
    let requests = sent.lock().unwrap().clone();
    let shown = &results(&requests[1])[0];
    assert!(shown.contains("10×10 JPEG picture"), "{shown}");
    assert_eq!(pictures(&requests[1].messages[4]), 1);
}

/// A re-plan's first turn shows the newest picture the task's steps looked
/// at, after the text, and the text says it is shown.
#[test]
fn a_replan_shows_the_newest_looked_at_picture_in_its_first_turn() {
    let (mut planner, sent) = scripted(
        vec![called("p1", tilo_agent::tools::PLAN, &draw_plan())],
        true,
    );
    let mut reads = Looks::new('a');
    let older = reads.keep('e', crate::vision::tests::png(4, 4));
    let mut context = look_context();
    context.request = Some("draw a band, then check it looks right".into());
    context.pictures = vec![older, reads.rendered.clone()];
    planner
        .plan_reading("check it looks right", &context, &mut reads)
        .unwrap();
    let first = sent.lock().unwrap()[0].messages[1].clone();
    assert_eq!(pictures(&first), 1, "only the newest");
    assert!(
        matches!(first.content.last(), Some(Content::Image { .. })),
        "after the text"
    );
    let text = text_of(&first);
    assert!(
        text.contains(&format!("{} picture shown below, 8×6 PNG", reads.rendered)),
        "{text}"
    );
}

/// A refusal ends the plan at once, saying the model refused in its own
/// words; it is not asked again. The usage line says why each answer
/// stopped and what was refused, and never holds a picture's bytes.
#[test]
fn a_refusal_ends_the_plan_saying_so_and_is_logged() {
    let (mut planner, sent) = scripted(vec![said("I won't do that.", StopReason::Refusal)], false);
    let error = planner.plan("open notes", &context()).unwrap_err();
    assert_eq!(error, "the model refused: I won't do that.");
    assert_eq!(sent.lock().unwrap().len(), 1);

    let refused = Ok(said("", StopReason::Refusal));
    let line = usage_line(
        "claude-fable",
        Some("pia"),
        (std::time::Duration::from_millis(5), (1, 400)),
        &refused,
        Attempt::default(),
    );
    assert_eq!(line["stop_reason"], "refusal");
    assert_eq!(line["refusal"], "");
    assert_eq!(
        (line["pictures"].as_u64(), line["picture_bytes"].as_u64()),
        (Some(1), Some(400))
    );
    let plan_turn = Ok(called(
        "p1",
        tilo_agent::tools::PLAN,
        &serde_json::json!({}),
    ));
    let line = usage_line(
        "gpt",
        None,
        (std::time::Duration::from_millis(5), (0, 0)),
        &plan_turn,
        Attempt {
            hedge: true,
            retry: false,
        },
    );
    assert_eq!(line["stop_reason"], "tool_calls");
    assert!(line["refusal"].is_null());
    assert_eq!(line["hedge"], true);
    assert_eq!(line["retry"], false);
    assert_eq!(line["usage"]["output_tokens"], 0);
}

/// A plan beside other calls in one turn runs none of them: each is told
/// the plan comes alone, and the plan the model then sends alone is taken.
/// A turn cut short is asked again once.
#[test]
fn a_mixed_or_cut_short_turn_is_asked_again() {
    let offer = open_notes().offers[0].id();
    let plan = serde_json::json!({"steps": [{"operation": offer, "arguments": {}}], "then": ""});
    let mut mixed = called("p0", tilo_agent::tools::PLAN, &plan);
    mixed.message.content.push(Content::ToolCall(ToolCall {
        id: "l0".into(),
        name: tilo_agent::tools::LIST_APPLICATIONS.into(),
        input: serde_json::json!({}),
        extra_content: None,
    }));
    let mut model = ScriptModel {
        turns: Script {
            answers: vec![
                Ok(mixed),
                Ok(said("", StopReason::OutputLimit)),
                Ok(called("p1", tilo_agent::tools::PLAN, &plan)),
            ]
            .into(),
            sent: Sent::default(),
        },
        sees: false,
    };
    let sent = std::sync::Arc::clone(&model.turns.sent);
    let answer = model
        .choose("open notes", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer]);
    let requests = sent.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let told = results(&requests[1]);
    assert_eq!(told.len(), 2);
    assert!(
        told.iter()
            .all(|text| text.starts_with("Not run: `plan` must be the only call"))
    );
    assert_eq!(
        requests[1], requests[2],
        "the cut-short turn is asked again as it was"
    );
}

/// A turn that could not be read (a call its server could not parse) asks
/// for the plan as plain JSON text, with no tools offered; the plan written
/// out is taken.
#[test]
fn an_unreadable_turn_asks_for_the_plan_as_text() {
    let offer = open_notes().offers[0].id();
    let written = format!(
        "```json\n{{\"steps\": [{{\"operation\": \"{offer}\", \"arguments\": {{}}}}], \"then\": [\"\"]}}\n```"
    );
    let mut model = ScriptModel {
        turns: Script {
            answers: vec![
                Err(ModelError::new(ErrorKind::InvalidResponse)),
                Ok(said(&written, StopReason::EndTurn)),
            ]
            .into(),
            sent: Sent::default(),
        },
        sees: false,
    };
    let sent = std::sync::Arc::clone(&model.turns.sent);
    let answer = model
        .choose("open notes", &open_notes(), MOST_STEPS)
        .unwrap();
    assert_eq!(answer.actions, vec![offer]);
    assert_eq!(answer.then, None);
    let requests = sent.lock().unwrap().clone();
    assert!(
        requests[1].tools.is_empty(),
        "asked for text, offered no tools"
    );
    let asked = requests[1].messages.last().unwrap();
    assert_eq!(asked.role, Role::User);
    assert!(text_of(asked).contains("argument object as JSON"));
}

/// The plan's tools carry `wait_for` naming the message apps, no `why`,
/// and a `then` that may be left out.
#[test]
fn the_plan_tool_waits_and_says_little() {
    let tools = planning_tools(MOST_STEPS, false, false, false);
    let plan = tools
        .iter()
        .find(|tool| tool.name == tilo_agent::tools::PLAN)
        .unwrap();
    let schema = &plan.input_schema;
    assert_eq!(
        schema["properties"]["wait_for"]["properties"]["app"]["description"],
        "Where it arrives: mail, chat, messages; omit for any."
    );
    assert!(schema["properties"].get("why").is_none());
    assert_eq!(schema["required"], serde_json::json!(["steps"]));
    assert_eq!(schema["properties"]["obligations"]["type"], "array");
    assert_eq!(
        schema["properties"]["steps"]["items"]["properties"]["for"]["type"],
        "integer"
    );
    assert_eq!(
        schema["properties"]["set_aside"]["items"]["required"],
        serde_json::json!(["item", "reason"])
    );
    assert!(plan.description.ends_with(&format!(
        "{}{}",
        super::converse::OBLIGATIONS_NOTE,
        super::converse::FACTS_NOTE
    )));
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, [READ_VIEW, tilo_agent::tools::PLAN]);
    let all = planning_tools(MOST_STEPS, true, true, true);
    let names: Vec<&str> = all.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        [
            tilo_agent::tools::DESCRIBE_OPERATION,
            crate::search::SEARCH_ROOM,
            READ_VIEW,
            LOOK_CONTENT,
            tilo_agent::tools::PLAN
        ]
    );
}

/// The first turn's text, its stable part first.
fn first_turn_text(
    task: &str,
    briefing: &Briefing,
    catalogue: &Catalogue,
    revealed: &mut tilo_agent::tools::Revealed,
) -> String {
    let (stable, rest) = converse::first_turn_parts(task, briefing, catalogue, revealed);
    stable + &rest
}

/// Turns answered from a script, keeping each call's key and whether it
/// was a turn asked again after a lost call.
#[derive(Default)]
struct Lossy {
    answers: std::collections::VecDeque<Result<ModelResponse, ModelError>>,
    calls: Vec<(String, bool)>,
}

impl PlanTurns for Lossy {
    fn turn(
        &mut self,
        _: &ModelRequest,
        key: &str,
        _: std::time::Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.calls.push((key.to_owned(), retry));
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(ModelError::new(ErrorKind::Unavailable)))
    }

    fn model(&self) -> &'static str {
        "lossy"
    }
}

fn lossy_plan(
    answers: Vec<Result<ModelResponse, ModelError>>,
    briefing: &Briefing,
) -> (Result<Answer, String>, Vec<(String, bool)>) {
    let mut turns = Lossy {
        answers: answers.into(),
        calls: Vec::new(),
    };
    let planned = converse::plan(
        &mut turns,
        &converse::Asked {
            task: "open notes",
            briefing,
            catalogue: &open_notes(),
            most_steps: MOST_STEPS,
            sees: false,
            timeout: tilo_test_env::hang_bound(),
        },
        &mut NoPlanReads,
    );
    (planned, turns.calls)
}

fn open_notes_plan() -> ModelResponse {
    let offer = open_notes().offers[0].id();
    called(
        "p1",
        tilo_agent::tools::PLAN,
        &serde_json::json!({"steps": [{"operation": offer, "arguments": {}}], "then": ""}),
    )
}

/// The turn keeps its key, so a metered endpoint bills it once.
#[test]
fn a_planning_call_lost_to_the_network_is_asked_once_more() {
    for kind in [ErrorKind::Transport, ErrorKind::TimedOut] {
        let briefing = Briefing::default();
        let (planned, calls) = lossy_plan(
            vec![Err(ModelError::new(kind)), Ok(open_notes_plan())],
            &briefing,
        );
        let answer = planned.unwrap();
        assert_eq!(answer.actions, vec![open_notes().offers[0].id()]);
        let [(first, false), (again, true)] = calls.as_slice() else {
            panic!("one call, then the same asked again: {calls:?}");
        };
        assert_eq!(first, again, "the turn keeps its key");
        assert_eq!(
            briefing.retries.left(),
            crate::planner::MOST_TURN_RETRIES - 1
        );
    }
    let line = usage_line(
        "claude-x",
        Some("ana"),
        (std::time::Duration::from_millis(5), (0, 0)),
        &Err(ModelError::new(ErrorKind::Transport)),
        Attempt {
            hedge: false,
            retry: true,
        },
    );
    assert_eq!(line["retry"], true);
    assert_eq!(line["ok"], false);
}

#[test]
fn a_call_lost_again_or_past_the_budget_ends_the_plan() {
    let lost = || Err(ModelError::new(ErrorKind::Transport));
    let briefing = Briefing::default();
    let (planned, calls) = lossy_plan(vec![lost(), lost(), Ok(open_notes_plan())], &briefing);
    assert_eq!(
        planned.unwrap_err(),
        ModelError::new(ErrorKind::Transport).to_string()
    );
    assert_eq!(calls.len(), 2, "{calls:?}");

    let spent = Briefing::default();
    while spent.retries.take() {}
    let (planned, calls) = lossy_plan(vec![lost(), Ok(open_notes_plan())], &spent);
    assert_eq!(
        planned.unwrap_err(),
        ModelError::new(ErrorKind::Transport).to_string()
    );
    assert_eq!(calls.len(), 1, "{calls:?}");
}

#[test]
fn a_call_answered_with_another_failure_is_not_asked_again() {
    for kind in [
        ErrorKind::Refused,
        ErrorKind::Authentication,
        ErrorKind::PermissionDenied,
        ErrorKind::InvalidRequest,
        ErrorKind::ContextLimit,
        ErrorKind::CreditsExhausted,
        ErrorKind::RateLimited,
        ErrorKind::Provider,
        ErrorKind::Unavailable,
        ErrorKind::Cancelled,
        ErrorKind::Truncated,
    ] {
        let briefing = Briefing::default();
        let (planned, calls) = lossy_plan(
            vec![Err(ModelError::new(kind)), Ok(open_notes_plan())],
            &briefing,
        );
        assert!(planned.is_err(), "{kind:?}");
        assert_eq!(calls.len(), 1, "{kind:?}");
        assert_eq!(
            briefing.retries.left(),
            crate::planner::MOST_TURN_RETRIES,
            "{kind:?}"
        );
    }
}

#[test]
fn a_task_spends_its_own_retries_across_its_plans() {
    let context = context();
    let open = offer_id(&context, "record.so.tilo.sheets.s1#open");
    let plan = serde_json::json!({"steps": [{"operation": open, "arguments": {}}], "then": ""});
    let mut planner = ModelPlanner::new(Box::new(ScriptModel {
        turns: Script {
            answers: vec![
                Err(ModelError::new(ErrorKind::Transport)),
                Ok(called("p1", tilo_agent::tools::PLAN, &plan)),
            ]
            .into(),
            sent: Sent::default(),
        },
        sees: false,
    }));
    let steps = planner.plan("open the sheet", &context).unwrap();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(
        context.retries.left(),
        crate::planner::MOST_TURN_RETRIES - 1
    );
}

/// Answers an artwork 640 wide now and 320 wide at any earlier version.
struct Versions;

impl Reads for Versions {
    fn read(
        &mut self,
        _: &DocumentRoute,
        _: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let width = if arguments.contains_key("version") {
            320
        } else {
            640
        };
        Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"artwork": {"width": width, "height": 480}})),
            resources: Vec::new(),
        })
    }
}

/// `read_view` with `compare` runs the read again with those arguments
/// and lists, beside what the read found, what differs from that answer
/// to this one; the plan tool says to check a record by its facts.
#[test]
fn read_view_compares_two_answers_of_one_read() {
    let context = canvas_context();
    let (mut planner, sent) = scripted(
        vec![
            called(
                "r1",
                READ_VIEW,
                &serde_json::json!({"operation": "canvas.artwork#inspect", "arguments": {},
                    "compare": {"version": "v1"}}),
            ),
            called(
                "p1",
                tilo_agent::tools::PLAN,
                &serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
                    "arguments": {"marks": [{"shape": {"kind": "rect", "x": 0, "y": 0,
                        "width": 640, "height": 480}}]}}], "then": ""}),
            ),
        ],
        false,
    );
    planner
        .plan_reading("cover the artwork as it is now", &context, &mut Versions)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    let compared = &results(&requests[1])[0];
    assert!(
        compared.contains("canvas.artwork#inspect answered")
            && compared.ends_with(
                r#"Changed from its answer to {"version":"v1"} to this one: artwork.width: 320 -> 640"#
            ),
        "{compared}"
    );
    let plan = requests[0]
        .tools
        .iter()
        .find(|tool| tool.name == tilo_agent::tools::PLAN)
        .unwrap();
    assert!(
        plan.description
            .contains("To check a record against a request, read its `facts`"),
        "{}",
        plan.description
    );
}
