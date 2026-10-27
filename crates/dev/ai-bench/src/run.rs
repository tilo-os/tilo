//! Asking a suite's cases of one endpoint, in order, and scoring each answer.
//!
//! The run is deliberately dumb: it reads the case, asks, scores, and records
//! what the endpoint said about itself. No retry, no fallback to another
//! endpoint, no re-ask with a "please answer properly" nudge. A model that
//! fails a case fails it, which is the whole point — a harness that quietly
//! improves an answer measures the harness.

use serde_json::json;

use crate::case::Suite;
use crate::endpoint::{self, Endpoint, Usage};
use crate::report::{Answered, CaseRun, ModelRun, Report};
use crate::score;

/// Run `suite` against `endpoint`, handing each case to `show` as it is
/// scored.
///
/// `key` is the endpoint's credential, or empty for a local runtime that
/// takes none.
///
/// # Errors
///
/// Nothing: a case whose endpoint could not be reached is recorded as failing
/// with the reason, because an unreachable endpoint is a result about the
/// endpoint, not an error of the run.
pub fn run(
    client: &reqwest::blocking::Client,
    endpoint: &Endpoint,
    key: &str,
    suite: &Suite,
    show: impl Fn(&CaseRun),
) -> ModelRun {
    let mut cases = Vec::with_capacity(suite.cases.len());
    let mut usage = Usage::default();
    for case in &suite.cases {
        let run = match endpoint::ask(client, endpoint, key, &case.instructions, &case.input) {
            Ok(answer) => {
                usage = usage.saturating_add(answer.usage);
                CaseRun {
                    case: case.id.clone(),
                    score: Some(score::score(case, &answer.text)),
                    answer: Ok(Answered {
                        text: answer.text,
                        served_model: answer.served_model,
                        request: answer.request,
                    }),
                }
            }
            Err(why) => CaseRun {
                case: case.id.clone(),
                // A case with no answer has no checks to run, and still does
                // not pass: `ScoredCase::passed` is false on an empty list
                // only for a case that declared checks, so record the reason
                // here and let the report count it as failed.
                score: Some(score::no_answer(case, &why.reason)),
                answer: Err(why.reason),
            },
        };
        show(&run);
        cases.push(run);
    }
    ModelRun {
        endpoint: endpoint.clone(),
        cases,
        usage,
        cost: endpoint.price.of(&usage),
    }
}

/// Run `suite` against every endpoint, one endpoint after another.
pub fn run_all(
    client: &reqwest::blocking::Client,
    endpoints: &[(&Endpoint, String)],
    suite: &Suite,
    show: impl Fn(&CaseRun),
) -> Report {
    Report {
        runs: endpoints
            .iter()
            .map(|(endpoint, key)| run(client, endpoint, key, suite, &show))
            .collect(),
    }
}

/// One case's verdict as a single JSON line, for a job that reads a run's
/// progress without parsing the whole report.
#[must_use]
pub fn summary_line(run: &CaseRun) -> String {
    json!({
        "case": run.case,
        "passed": run.passed(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{Case, Check, InteractionKind};
    use test_http::{Reply, Responder};

    fn case() -> Case {
        Case {
            id: "greet".into(),
            kind: InteractionKind::OneShot,
            instructions: "greet the person".into(),
            input: "Dana".into(),
            expected: None,
            checks: vec![Check::Containing {
                text: "Dana".into(),
            }],
            source: None,
            why: "a greeting names who it greets".into(),
        }
    }

    fn suite() -> Suite {
        let mut suite = Suite {
            kind: InteractionKind::OneShot,
            description: "greetings".into(),
            cases: Vec::new(),
        };
        suite.insert(case()).unwrap();
        suite
    }

    fn answering(text: &str) -> Responder {
        let body = serde_json::json!({
            "model": "small-served",
            "choices": [{"message": {"role": "assistant", "content": text}}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 5},
        });
        Responder::start(vec![json_reply(&body)])
    }

    fn json_reply(body: &serde_json::Value) -> Reply {
        Reply::status(200)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(body).unwrap_or_default())
    }

    fn endpoint(responder: &Responder) -> Endpoint {
        Endpoint {
            url: format!("{}/v1/chat/completions", responder.url()),
            model: "small".into(),
            label: "test".into(),
            price: crate::endpoint::Price::default(),
        }
    }

    #[test]
    fn a_run_records_the_answer_the_checks_and_what_was_spent() {
        let responder = answering("Hello Dana");
        let client = endpoint::client().unwrap();
        let run = run(&client, &endpoint(&responder), "", &suite(), |_| {});
        assert_eq!(run.cases.len(), 1);
        assert!(run.cases[0].passed());
        assert_eq!(run.passed(), 1);
        assert_eq!(run.usage.input_tokens, 20);
        assert_eq!(run.usage.output_tokens, 5);
        match &run.cases[0].answer {
            Ok(answer) => {
                assert_eq!(answer.text, "Hello Dana");
                assert_eq!(answer.served_model.as_deref(), Some("small-served"));
            }
            Err(why) => panic!("the case has an answer: {why}"),
        }
        let asked = responder.drain_requests();
        assert_eq!(asked.len(), 1);
        let sent = asked[0].body_text();
        assert!(sent.contains("greet the person"), "{sent}");
        assert!(sent.contains("Dana"), "{sent}");
    }

    #[test]
    fn a_wrong_answer_fails_the_case_and_the_run() {
        let responder = answering("Hello there");
        let client = endpoint::client().unwrap();
        let report = run_all(
            &client,
            &[(&endpoint(&responder), String::new())],
            &suite(),
            |_| {},
        );
        assert!(!report.passed());
        assert!(
            report
                .to_text()
                .contains("failed greet: answered \"Hello there\"")
        );
    }

    #[test]
    fn an_endpoint_that_answers_nothing_fails_its_cases_and_never_skips_them() {
        let refused =
            Reply::status(401).body(br#"{"error": {"code": "invalid_api_key"}}"#.to_vec());
        let responder = Responder::start(vec![refused]);
        let client = endpoint::client().unwrap();
        let run = run(&client, &endpoint(&responder), "", &suite(), |_| {});
        assert_eq!(run.cases.len(), 1, "the case ran, and failed");
        assert!(!run.cases[0].passed());
        let reported = format!("{:?}", run.cases[0].answer);
        assert!(reported.contains("status 401"), "{reported}");
        assert!(reported.contains("code=invalid_api_key"), "{reported}");
    }
}
