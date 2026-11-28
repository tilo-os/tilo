//! The evaluation set for a request's decision, and the decision's budget.
//!
//! The set (`tests/data/instruction_mode_eval.jsonl`) carries instructions with the mode the
//! agent should decide for each: answered in place over the context it was given (`one-shot`),
//! or carried out as work (`task`). What is checked here is that the set is well formed, that
//! the production transform answers with exactly the two decisions it names, and that the
//! decision has a budget short enough to hold the composer rather than stall it.
//!
//! Recording a real model's decisions over the set is a measurement against a provider, which
//! no test has. This is the harness the recording would be scored by: every case through the
//! production transform, exactly as the composer asks it.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_provider_config::Interaction;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tilo_agent_session::transform::{self, RESPOND_TIMEOUT, TIMEOUT, TransformModel};
use tilo_automation::AGENT_RESPOND_ACTION;

/// The evaluation set, as the package carries it.
const SET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/instruction_mode_eval.jsonl"
);

/// A decision answered in place, over the context the request was given.
const ONE_SHOT: &str = "one-shot";
/// A decision carried out as work.
const TASK: &str = "task";

/// One instruction in the evaluation set.
#[derive(Debug, Deserialize)]
struct Case {
    instruction: String,
    context: String,
    app: String,
    /// [`ONE_SHOT`] or [`TASK`].
    expected: String,
}

fn cases() -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(SET)
        .map_err(|error| format!("the evaluation set was not read: {error}"))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|error| format!("a case is not a case: {error}"))
        })
        .collect()
}

/// Answers with the decision the caller scripted for it: what a model deciding the set
/// correctly would say, and what one answering everything as an answer would.
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
        let case = cases()?
            .into_iter()
            .find(|case| instructions.contains(&case.instruction))
            .ok_or_else(|| "the request is not in the instructions".to_owned())?;
        let mode = if self.0 == "as expected" {
            case.expected.as_str()
        } else {
            self.0
        };
        let decision: Value = if mode == TASK {
            json!({"result": {"work": case.instruction}})
        } else {
            json!({"result": {"answer": case.instruction}})
        };
        Ok(decision.to_string())
    }
}

/// Runs `model` over every instruction of the set as the composer asks it, and returns how
/// many of its decisions the set agrees with.
fn run_over(model: &dyn TransformModel) -> Result<(usize, usize), String> {
    let content = ui::content::ContentStore::memory();
    let (mut right, mut total) = (0_usize, 0_usize);
    for case in cases()? {
        let about = format!("{} in {}", case.context, case.app);
        let arguments = Map::from_iter([
            ("request".to_owned(), Value::from(case.instruction.clone())),
            ("input".to_owned(), Value::from(about)),
        ]);
        let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content)
            .map_err(|failure| format!("the respond transform did not bind: {failure:?}"))?;
        let output = transform::run_until(&call, model, &AtomicBool::new(false))
            .map_err(|failure| format!("a decision failed: {failure:?}"))?;
        let value = output
            .value
            .ok_or_else(|| "a decision answered nothing".to_owned())?;
        let decided = if value.get("work").is_some() {
            TASK
        } else {
            ONE_SHOT
        };
        right = right.saturating_add(usize::from(decided == case.expected));
        total = total.saturating_add(1);
    }
    Ok((right, total))
}

/// A decision that runs past its budget fails rather than holding the composer until a
/// transform's own budget. The model is stopped by the deadline it is handed, as the gateway
/// stops a real call.
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
        let _ = self
            .0
            .lock()
            .map_err(|_| "the seen budget was poisoned".to_owned())?
            .replace(timeout);
        Err("the deadline passed".to_owned())
    }
}

/// The set is big enough to say something, carries both decisions in numbers, and repeats no
/// instruction: one side decided by habit cannot be right by answering everything one way.
#[test]
fn the_evaluation_set_covers_both_decisions_and_is_well_formed() -> Result<(), String> {
    let cases = cases()?;
    if cases.len() < 20 {
        return Err(format!(
            "the set is too small to say anything about accuracy: {} cases",
            cases.len()
        ));
    }
    let (mut one_shot, mut task) = (0_usize, 0_usize);
    let mut instructions: Vec<&str> = Vec::new();
    for case in &cases {
        match case.expected.as_str() {
            ONE_SHOT => one_shot = one_shot.saturating_add(1),
            TASK => task = task.saturating_add(1),
            other => return Err(format!("`{other}` is not a decision: {}", case.instruction)),
        }
        if case.instruction.trim().is_empty() {
            return Err("an instruction says nothing".to_owned());
        }
        if case.app.trim().is_empty() {
            return Err(format!(
                "the instruction `{}` names no app",
                case.instruction
            ));
        }
        instructions.push(&case.instruction);
    }
    if one_shot < 5 {
        return Err(format!("only {one_shot} one-shot instructions"));
    }
    if task < 5 {
        return Err(format!("only {task} task instructions"));
    }
    instructions.sort_unstable();
    let total = instructions.len();
    instructions.dedup();
    if instructions.len() != total {
        return Err("the set repeats an instruction".to_owned());
    }
    Ok(())
}

/// The production transform answers every case, and the set tells the two decisions apart: a
/// perfect recording scores every case, and answering everything one way does not.
#[test]
fn a_perfect_recording_scores_every_case_and_a_fixed_one_does_not() -> Result<(), String> {
    let (right, total) = run_over(&Decides("as expected"))?;
    if (right, total) != (total, total) {
        return Err(format!(
            "a correct decision per case scored {right}/{total}"
        ));
    }

    let (right, total) = run_over(&Decides(ONE_SHOT))?;
    if total != cases()?.len() {
        return Err(format!(
            "the run decided {total} cases, the set holds {}",
            cases()?.len()
        ));
    }
    if right >= total {
        return Err(format!(
            "answering everything one way scored {right}/{total}: the set cannot tell the \
             decisions apart"
        ));
    }
    Ok(())
}

/// The decision holds the composer, so its budget is a distinct, short one, and the model is
/// not asked for a transform's output on top of it.
#[test]
fn a_requests_decision_has_its_own_short_budget() -> Result<(), String> {
    if RESPOND_TIMEOUT >= TIMEOUT {
        return Err(format!(
            "a decision's budget ({RESPOND_TIMEOUT:?}) is not shorter than a transform's \
             ({TIMEOUT:?})"
        ));
    }
    if RESPOND_TIMEOUT > Duration::from_secs(10) {
        return Err(format!(
            "a decision may take {RESPOND_TIMEOUT:?}, which is long enough that the person reads \
             it as a stall"
        ));
    }
    let content = ui::content::ContentStore::memory();
    let arguments = Map::from_iter([("request".to_owned(), Value::from("Summarise this"))]);
    let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content)
        .map_err(|failure| format!("the respond transform did not bind: {failure:?}"))?;
    if call.transform.budget() != RESPOND_TIMEOUT {
        return Err(format!(
            "a decision's budget is {:?}, not {RESPOND_TIMEOUT:?}",
            call.transform.budget()
        ));
    }
    if call.transform.max_output_tokens() >= 2048 {
        return Err(format!(
            "a decision may write {} tokens, which is a transform's answer, not a decision",
            call.transform.max_output_tokens()
        ));
    }

    let summarise = Map::from_iter([("input".to_owned(), Value::from("some text"))]);
    let call = transform::bind("summarise", &summarise, &content)
        .map_err(|failure| format!("the summarise transform did not bind: {failure:?}"))?;
    if call.transform.budget() != TIMEOUT {
        return Err(format!(
            "a transform's budget is {:?}, not {TIMEOUT:?}",
            call.transform.budget()
        ));
    }
    Ok(())
}

/// A decision that fails at its deadline is given only the decision's budget, and fails as the
/// model being unavailable rather than as a stall.
#[test]
fn a_slow_decision_is_given_only_the_decisions_budget() -> Result<(), String> {
    let seen = Arc::new(Mutex::new(None));
    let content = ui::content::ContentStore::memory();
    let arguments = Map::from_iter([("request".to_owned(), Value::from("Rewrite this"))]);
    let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content)
        .map_err(|failure| format!("the respond transform did not bind: {failure:?}"))?;
    let failure = transform::run_until(&call, &Slow(Arc::clone(&seen)), &AtomicBool::new(false))
        .err()
        .ok_or_else(|| "a decision that ran out of its budget failed".to_owned())?;
    if failure.code != transform::code::MODEL_UNAVAILABLE {
        return Err(format!(
            "a decision past its deadline failed as `{}`, not as a model unavailable",
            failure.code
        ));
    }
    let locked = seen
        .lock()
        .map_err(|_| "the seen budget was poisoned".to_owned());
    let left = locked?.ok_or_else(|| "the model was not asked".to_owned())?;
    if left > RESPOND_TIMEOUT {
        return Err(format!(
            "the model was given {left:?}, more than the decision's {RESPOND_TIMEOUT:?}"
        ));
    }
    Ok(())
}
