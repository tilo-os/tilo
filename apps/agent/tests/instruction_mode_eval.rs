//! The evaluation set for a request's decision, and the decision's budget.
//!
//! The set (`tests/data/instruction_mode_eval.jsonl`) carries instructions
//! with the mode the agent should decide for each. Recording a real model's
//! decisions over it is `examples/instruction_mode_eval.rs`'s job; what is
//! checked here is that the set is well formed, that the production transform
//! answers with exactly the two decisions it names, and that the decision has
//! a budget short enough to hold the composer rather than stall it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, json};
use tilo_agent::transform::{self, Interaction, RESPOND_TIMEOUT, TIMEOUT, TransformModel};
use tilo_automation::AGENT_RESPOND_ACTION;

/// The evaluation set, as the repository carries it.
const SET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/instruction_mode_eval.jsonl"
);

#[derive(Debug, serde::Deserialize)]
struct Case {
    instruction: String,
    context: String,
    app: String,
    expected: String,
}

fn cases() -> Vec<Case> {
    let text = std::fs::read_to_string(SET).unwrap();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<Case>, _>>()
        .unwrap()
}

/// Answers with the decision the caller scripted for it: what a model
/// deciding the set correctly would say, and what one answering everything
/// as an answer would.
struct Decides(&'static str);

impl TransformModel for Decides {
    fn answer(
        &self,
        _kind: Interaction,
        instructions: &str,
        _input: &str,
        _max_output_tokens: u32,
        _timeout: Duration,
    ) -> Result<String, String> {
        // The transform hands the model the request verbatim.
        let case = cases()
            .into_iter()
            .find(|case| instructions.contains(&case.instruction))
            .expect("the request is in the instructions");
        let decision = if self.0 == "as expected" {
            case.expected.as_str()
        } else {
            self.0
        };
        Ok(match decision {
            "task" => json!({"result": {"work": case.instruction}}),
            _ => json!({"result": {"answer": case.instruction}}),
        }
        .to_string())
    }
}

/// Runs `model` over every instruction of the set as the composer asks it,
/// and returns how many of its decisions the set agrees with.
fn run_over(model: &dyn TransformModel) -> (usize, usize) {
    let content = ui::content::ContentStore::memory();
    let mut right = 0;
    let mut total = 0;
    for case in cases() {
        let about = format!("{} in {}", case.context, case.app);
        let arguments = Map::from_iter([
            ("request".into(), case.instruction.clone().into()),
            ("input".into(), about.into()),
        ]);
        let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content).unwrap();
        let output = transform::run_until(&call, model, &AtomicBool::new(false)).unwrap();
        let value = output.value.unwrap();
        let decided = if value.get("work").is_some() {
            "task"
        } else {
            "one-shot"
        };
        right += usize::from(decided == case.expected);
        total += 1;
    }
    (right, total)
}

#[test]
fn the_evaluation_set_covers_both_decisions_and_is_well_formed() {
    let cases = cases();
    assert!(
        cases.len() >= 20,
        "the set is too small to say anything about accuracy: {} cases",
        cases.len()
    );
    let mut one_shot = 0;
    let mut task = 0;
    let mut instructions: Vec<&str> = Vec::new();
    for case in &cases {
        match case.expected.as_str() {
            "one-shot" => one_shot += 1,
            "task" => task += 1,
            other => panic!("`{other}` is not a decision: {}", case.instruction),
        }
        assert!(!case.instruction.trim().is_empty());
        assert!(!case.app.trim().is_empty());
        instructions.push(&case.instruction);
    }
    // One side decided by habit is not a set: both have to be present in
    // numbers, so neither can be right by answering everything one way.
    assert!(one_shot >= 5, "only {one_shot} one-shot instructions");
    assert!(task >= 5, "only {task} task instructions");
    instructions.sort_unstable();
    let unique = instructions.len();
    instructions.dedup();
    assert_eq!(instructions.len(), unique, "the set repeats an instruction");
}

#[test]
fn a_perfect_recording_scores_every_case_and_a_fixed_one_does_not() {
    let (right, total) = run_over(&Decides("as expected"));
    assert_eq!(
        (right, total),
        (total, total),
        "a correct decision per case"
    );

    let (right, total) = run_over(&Decides("one-shot"));
    assert_eq!(total, cases().len());
    assert!(
        right < total,
        "answering everything one way scored {right}/{total}: the set cannot tell the \
         decisions apart"
    );
}

/// The decision holds the composer, so its budget is a distinct, short one,
/// and the model is not asked for a transform's output on top of it.
#[test]
fn a_requests_decision_has_its_own_short_budget() {
    assert!(
        RESPOND_TIMEOUT < TIMEOUT,
        "a decision's budget ({RESPOND_TIMEOUT:?}) is not shorter than a transform's \
         ({TIMEOUT:?})"
    );
    assert!(
        RESPOND_TIMEOUT <= Duration::from_secs(10),
        "a decision may take {RESPOND_TIMEOUT:?}, which is long enough that the person \
         reads it as a stall"
    );
    let content = ui::content::ContentStore::memory();
    let arguments = Map::from_iter([("request".into(), "Summarise this".into())]);
    let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content).unwrap();
    assert_eq!(call.transform.budget(), RESPOND_TIMEOUT);
    assert!(call.transform.max_output_tokens() < 2048);

    let summarise = Map::from_iter([("input".into(), "some text".into())]);
    let call = transform::bind("summarise", &summarise, &content).unwrap();
    assert_eq!(call.transform.budget(), TIMEOUT);
}

/// A decision that runs past its budget fails as timed out rather than
/// holding the composer until a transform's own budget. The model is stopped
/// by the deadline it is handed, as the gateway stops a real call.
struct Slow(Arc<Mutex<Option<Duration>>>);

impl TransformModel for Slow {
    fn answer(
        &self,
        _kind: Interaction,
        _instructions: &str,
        _input: &str,
        _max_output_tokens: u32,
        timeout: Duration,
    ) -> Result<String, String> {
        *self.0.lock().unwrap() = Some(timeout);
        Err("the deadline passed".into())
    }
}

#[test]
fn a_slow_decision_is_given_only_the_decisions_budget() {
    let seen = Arc::new(Mutex::new(None));
    let content = ui::content::ContentStore::memory();
    let arguments = Map::from_iter([("request".into(), "Rewrite this".into())]);
    let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content).unwrap();
    let error =
        transform::run_until(&call, &Slow(Arc::clone(&seen)), &AtomicBool::new(false)).unwrap_err();
    assert_eq!(error.code, transform::code::MODEL_UNAVAILABLE);
    let left = seen.lock().unwrap().unwrap();
    assert!(
        left <= RESPOND_TIMEOUT,
        "the model was given {left:?}, more than the decision's {RESPOND_TIMEOUT:?}"
    );
}
