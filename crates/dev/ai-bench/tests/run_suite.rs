//! The benchmark end to end: a suite on disk, a stand-in endpoint on the
//! loopback, and the report that comes out — including the cases that
//! *fail*, which is what makes a green run mean something.
//!
//! Linux only, because `tilo-actions` — which both the shape check and the
//! vocabulary pin reach — is built there.

#![cfg(target_os = "linux")]
#![expect(
    clippy::expect_used,
    reason = "a fixture or a codec failing is the failure this test reports"
)]

use std::path::PathBuf;

use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_ai_bench::case::{Check, Suite};
use tilo_ai_bench::endpoint::{Endpoint, Endpoints, Price};
use tilo_ai_bench::report::Report;
use tilo_ai_bench::{run, score};

fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cases")
}

fn answer(body: &Value) -> Reply {
    Reply::status(200)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(body).expect("a fixture body is JSON"))
}

fn completion(text: &str, prompt: u64, written: u64) -> Reply {
    answer(&json!({
        "model": "stand-in",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}}],
        "usage": {"prompt_tokens": prompt, "completion_tokens": written},
    }))
}

fn endpoint(responder: &Responder, label: &str) -> Endpoint {
    Endpoint {
        url: format!("{}/v1/chat/completions", responder.url()),
        model: "stand-in".into(),
        label: label.into(),
        price: Price::default(),
    }
}

/// Every suite committed under `cases/`, loaded and checked.
fn committed_suites() -> Vec<Suite> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .expect("the cases directory ships with the tool")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no suites are committed");
    paths
        .iter()
        .map(|path| Suite::load(path).expect("a committed suite loads"))
        .collect()
}

#[test]
fn every_committed_suite_loads_and_declares_only_what_a_run_can_check() {
    let suites = committed_suites();
    let kinds: Vec<&str> = suites.iter().map(|suite| suite.kind.as_str()).collect();
    assert!(
        kinds.contains(&"one-shot") && kinds.contains(&"task"),
        "the suites cover the kinds a run names: {kinds:?}"
    );
    for suite in &suites {
        assert!(!suite.cases.is_empty(), "{}", suite.kind);
        assert!(
            !suite.description.trim().is_empty(),
            "{} says what it covers",
            suite.kind
        );
        for case in &suite.cases {
            assert!(
                !case.why.trim().is_empty(),
                "{} says why it earns its place",
                case.id
            );
            assert!(
                !case.checks.is_empty() || case.expected.is_some(),
                "{} is scored by something",
                case.id
            );
        }
    }
}

#[test]
fn a_suite_is_asked_in_order_and_every_answer_is_scored() {
    let mut suite = Suite {
        kind: tilo_ai_bench::InteractionKind::OneShot,
        description: "two rewrites".into(),
        cases: Vec::new(),
    };
    for (id, input) in [("first", "the cat sat"), ("second", "the dog ran")] {
        suite
            .insert(tilo_ai_bench::Case {
                id: id.into(),
                kind: tilo_ai_bench::InteractionKind::OneShot,
                instructions: "rewrite in the past tense".into(),
                input: input.into(),
                expected: None,
                checks: vec![Check::AtMostChars { chars: 80 }],
                source: None,
                why: "it proves the run asks every case, in order".into(),
            })
            .expect("the case belongs in its suite");
    }
    let responder = Responder::start(vec![
        completion("The cat sat.", 11, 4),
        completion("The dog ran.", 12, 4),
    ]);
    let client = tilo_ai_bench::endpoint::client().expect("a client");
    let report = Report {
        runs: vec![run::run(
            &client,
            &endpoint(&responder, "stand-in"),
            "",
            &suite,
            |_| {},
        )],
    };
    assert!(report.passed(), "{}", report.to_text());
    assert_eq!(report.runs[0].passed(), 2);
    assert_eq!(report.runs[0].usage.input_tokens, 23);
    let asked = responder.drain_requests();
    assert_eq!(asked.len(), 2);
    assert!(asked[0].body_text().contains("the cat sat"));
    assert!(asked[1].body_text().contains("the dog ran"));
    let text = report.to_text();
    assert!(
        text.contains("stand-in\n  2/2 cases passed on stand-in"),
        "{text}"
    );
    assert!(text.contains("one-shot: 2/2 cases passed"), "{text}");
}

#[test]
fn a_model_that_answers_another_model_is_reported_as_such() {
    let responder = Responder::start(vec![answer(&json!({
        "model": "fallback-small",
        "choices": [{"message": {"role": "assistant", "content": "ok"}}],
    }))]);
    let mut suite = Suite {
        kind: tilo_ai_bench::InteractionKind::OneShot,
        description: "one case".into(),
        cases: Vec::new(),
    };
    suite
        .insert(tilo_ai_bench::Case {
            id: "say-ok".into(),
            kind: tilo_ai_bench::InteractionKind::OneShot,
            instructions: "say ok".into(),
            input: String::new(),
            expected: None,
            checks: vec![Check::Containing { text: "ok".into() }],
            source: None,
            why: "the endpoint's own report is part of the result".into(),
        })
        .expect("the case belongs in its suite");
    let client = tilo_ai_bench::endpoint::client().expect("a client");
    let report = Report {
        runs: vec![run::run(
            &client,
            &endpoint(&responder, "chosen"),
            "",
            &suite,
            |_| {},
        )],
    };
    assert!(
        report
            .to_text()
            .contains("1/1 cases passed on stand-in, served as fallback-small"),
        "{}",
        report.to_text()
    );
    let json = report.to_json().expect("a report");
    assert!(
        json.contains("\"served_model\": \"fallback-small\""),
        "{json}"
    );
}

#[test]
fn a_comparison_of_two_models_reports_both_and_their_cost() {
    let strong = Responder::start(vec![completion("The cat sat.", 10, 5)]);
    let weak = Responder::start(vec![completion("the cat sits", 10, 5)]);
    let priced = |responder: &Responder, label: &str| Endpoint {
        // One currency unit per million input tokens, two per million output.
        price: Price {
            input: 1_000_000,
            output: 2_000_000,
        },
        ..endpoint(responder, label)
    };
    let mut suite = Suite {
        kind: tilo_ai_bench::InteractionKind::OneShot,
        description: "a tense rewrite".into(),
        cases: Vec::new(),
    };
    suite
        .insert(tilo_ai_bench::Case {
            id: "tense".into(),
            kind: tilo_ai_bench::InteractionKind::OneShot,
            instructions: "rewrite in the past tense".into(),
            input: "the cat sits".into(),
            expected: None,
            checks: vec![
                Check::Text {
                    is: "The cat sat.".into(),
                },
                Check::NotEchoingInput { words: 3 },
            ],
            source: None,
            why: "an answer that repeats the input is not a rewrite".into(),
        })
        .expect("the case belongs in its suite");
    let client = tilo_ai_bench::endpoint::client().expect("a client");
    let report = Report {
        runs: vec![
            run::run(&client, &priced(&strong, "strong"), "", &suite, |_| {}),
            run::run(&client, &priced(&weak, "weak"), "", &suite, |_| {}),
        ],
    };
    assert!(!report.passed(), "one endpoint failed the case");
    assert_eq!(report.runs[0].passed(), 1);
    assert_eq!(report.runs[1].passed(), 0);
    let text = report.to_text();
    assert!(text.contains("strong\n  1/1 cases passed"), "{text}");
    assert!(text.contains("weak\n  0/1 cases passed"), "{text}");
    // 10 input at 1.0 per million + 5 output at 2.0 per million.
    assert!(
        text.contains("cost 0.000020 on 10 input and 5 output tokens"),
        "{text}"
    );
    assert!(
        text.contains("failed tense: answered \"the cat sits\""),
        "{text}"
    );
    assert!(
        text.contains("    not-echoing-input the answer repeats its input"),
        "{text}"
    );
    let json = report.to_json().expect("a report");
    let read = Report::from_json(&json).expect("the report reads back");
    assert_eq!(read.runs.len(), 2);
    assert_eq!(read.runs[1].passed(), 0);
}

#[test]
fn the_report_is_the_same_file_a_ci_job_keeps_and_a_program_compares() {
    let responder = Responder::start(vec![completion("The cat sat.", 3, 3)]);
    let mut suite = Suite {
        kind: tilo_ai_bench::InteractionKind::OneShot,
        description: "one case".into(),
        cases: Vec::new(),
    };
    suite
        .insert(tilo_ai_bench::Case {
            id: "tense".into(),
            kind: tilo_ai_bench::InteractionKind::OneShot,
            instructions: "rewrite".into(),
            input: "the cat sits".into(),
            expected: None,
            checks: vec![Check::AtMostChars { chars: 20 }],
            source: None,
            why: "it keeps the report round-tripping".into(),
        })
        .expect("the case belongs in its suite");
    let client = tilo_ai_bench::endpoint::client().expect("a client");
    let report = Report {
        runs: vec![run::run(
            &client,
            &endpoint(&responder, "stand-in"),
            "",
            &suite,
            |_| {},
        )],
    };
    let text = report.to_json().expect("a report");
    assert!(report.passed(), "{text}");
    assert!(text.ends_with("}\n"), "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("the report is JSON");
    assert_eq!(parsed["endpoints"][0]["label"], "stand-in");
    assert_eq!(parsed["endpoints"][0]["cases"], 1);
    assert_eq!(parsed["endpoints"][0]["case"][0]["id"], "tense");
    let request = parsed["endpoints"][0]["case"][0]["request"]
        .as_str()
        .expect("each case records the request it asked");
    assert_eq!(request.len(), 16);
}

#[test]
fn a_suite_declaring_a_shape_tilo_refuses_is_refused_before_a_run_starts() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("broken.json");
    std::fs::write(
        &path,
        r#"{"kind": "one-shot", "cases": [
            {"id": "greet", "kind": "one-shot", "instructions": "say hi",
             "checks": [{"shape": {"schema": {"type": "string", "unknownKeyword": true}}}]}
        ]}"#,
    )
    .expect("the fixture is written");
    let refused = Suite::load(&path).expect_err("an unknown keyword is refused");
    assert!(refused.to_string().contains("greet"), "{refused}");
    assert!(refused.reason.contains("unknownKeyword"), "{refused}");
}

#[test]
fn a_captured_bad_run_becomes_a_case_the_suite_keeps() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("one-shot.json");
    std::fs::write(
        &path,
        r#"{"kind": "one-shot", "description": "captured", "cases": [
            {"id": "summarise", "kind": "one-shot", "instructions": "summarise",
             "input": "a long paragraph", "why": "it summarised the wrong document",
             "source": {"verdict": "bad", "answer": "The weather was fine."},
             "checks": [{"at-most-chars": {"chars": 11}}, {"not-echoing-input": {"words": 3}}]}
        ]}"#,
    )
    .expect("the fixture is written");
    let suite = Suite::load(&path).expect("a captured case loads");
    let source = suite.cases[0].source.as_ref().expect("its provenance");
    assert!(source.verdict.is_bad());
    assert_eq!(source.answer.as_deref(), Some("The weather was fine."));
    // The captured answer itself fails the case, which is why it is kept.
    let scored = score::score(&suite.cases[0], "The weather was fine.");
    assert!(!scored.passed());
    let refused: String = scored
        .checks
        .iter()
        .filter_map(|check| check.refused.clone())
        .collect();
    assert!(refused.contains("characters"), "{refused}");
    assert!(
        refused.contains("characters, past the 11 asked for"),
        "{refused}"
    );
}

#[test]
fn a_suite_naming_two_endpoints_by_one_label_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("endpoints.json");
    std::fs::write(
        &path,
        r#"{"endpoints": [
            {"label": "local", "url": "http://127.0.0.1:11434/v1/chat/completions", "model": "qwen"},
            {"label": "local", "url": "http://127.0.0.1:8000/v1/chat/completions", "model": "other"}
        ]}"#,
    )
    .expect("the fixture is written");
    let refused = Endpoints::load(&path).expect_err("one label, one endpoint");
    assert!(refused.contains("labelled"), "{refused}");
}
