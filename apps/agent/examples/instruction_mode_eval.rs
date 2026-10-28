//! Score the agent's one-shot-or-task decision over the evaluation set.
//!
//! ```sh
//! cargo run --example instruction_mode_eval -p agent -- --record /tmp/decisions.jsonl
//! cargo run --example instruction_mode_eval -p agent -- --score /tmp/decisions.jsonl
//! ```
//!
//! `--record` asks the production `respond` transform — the same instructions,
//! the same validation, the same decision budget — once per instruction in
//! `apps/agent/tests/data/instruction_mode_eval.jsonl`, and writes what it
//! decided with how long that took. `--score` compares a recording with the
//! set's expectations and prints accuracy, the per-mode split, and the
//! decision's latency, which `apps/agent/tests/instruction_mode_eval.rs` holds
//! to the budget.
//!
//! It needs a provider: the model gateway the agent itself goes through, with
//! the provider Settings selected. Without one it says so and fails; it never
//! invents a decision to have something to score.

// A run of a few hundred decisions is a measurement, and the arithmetic here
// is a report about the numbers it just read, not app logic.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "see the note above"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_agent::transform::{self, GatewayModel, TransformModel};
use tilo_automation::AGENT_RESPOND_ACTION;

/// The evaluation set, as the repository carries it.
const SET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/instruction_mode_eval.jsonl"
);

/// A decision is answered in place, or carried out as work.
const ONE_SHOT: &str = "one-shot";
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

/// One decision the agent made about an instruction.
#[derive(Debug, Deserialize, Serialize)]
struct Decision {
    /// The profile and model that decided, as the gateway names them.
    model: String,
    instruction: String,
    /// [`ONE_SHOT`] or [`TASK`].
    decision: String,
    /// How long the decision took, the whole transform including any repair.
    millis: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (record, path) = match args.as_slice() {
        [flag, path] if flag == "--record" => (true, PathBuf::from(path)),
        [flag, path] if flag == "--score" => (false, PathBuf::from(path)),
        _ => {
            eprintln!("usage: instruction_mode_eval --record PATH | --score PATH");
            std::process::exit(2);
        }
    };
    let cases = read_cases(Path::new(SET))?;
    if record {
        record_decisions(&cases, &path)
    } else {
        score(&cases, &path)
    }
}

fn read_cases(path: &Path) -> Result<Vec<Case>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let cases = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<Case>, _>>()?;
    Ok(cases)
}

/// The model the decisions are recorded from, as the gateway names it.
fn recorded_model(model: &GatewayModel) -> String {
    model
        .launched
        .clone()
        .or_else(tilo_agent::providers::default_selection)
        .map_or_else(
            || "no provider configured".to_string(),
            |selection| format!("{}/{}", selection.profile_id, selection.model.id),
        )
}

fn record_decisions(cases: &[Case], path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let model: Arc<dyn TransformModel> = Arc::new(GatewayModel::default());
    let named = recorded_model(&GatewayModel::default());
    if named == "no provider configured" {
        return Err("no model provider is configured, so there is nothing to record".into());
    }
    let content = ui::content::ContentStore::memory();
    let mut lines = String::new();
    for case in cases {
        let mut arguments = Map::from_iter([("request".into(), case.instruction.clone().into())]);
        // What the request is about, as the composer hands it over: the
        // context and the app it came from.
        let about = format!("{} in {}", case.context, case.app);
        if !about.trim().is_empty() {
            arguments.insert("input".into(), about.into());
        }
        let call = transform::bind(AGENT_RESPOND_ACTION, &arguments, &content)
            .map_err(|error| format!("{}: {}", case.instruction, error.message))?;
        let began = Instant::now();
        let output = transform::run_until(&call, model.as_ref(), &AtomicBool::new(false))
            .map_err(|error| format!("{}: {}", case.instruction, error.message))?;
        let millis = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        let decision = decided(output.value.as_ref());
        let line = serde_json::to_string(&Decision {
            model: named.clone(),
            instruction: case.instruction.clone(),
            decision: decision.to_string(),
            millis,
        })?;
        lines.push_str(&line);
        lines.push('\n');
    }
    fs::write(path, lines)?;
    println!(
        "recorded {} decisions from {named} to {}",
        cases.len(),
        path.display()
    );
    Ok(())
}

/// Which way the agent decided, from the value it returned: a `work` title is
/// work, anything else is an answer.
fn decided(value: Option<&Value>) -> &'static str {
    match value {
        Some(value) if value.get("work").is_some() => TASK,
        _ => ONE_SHOT,
    }
}

/// The latency at `fraction` of the way through `millis`, sorted ascending.
fn percentile(millis: &[u64], fraction: f64) -> u64 {
    if millis.is_empty() {
        return 0;
    }
    let last = millis.len().saturating_sub(1);
    let index = (fraction * last as f64).round() as usize;
    millis.get(index.min(last)).copied().unwrap_or_default()
}

fn score(cases: &[Case], path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let decisions: Vec<Decision> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let models: BTreeSet<&str> = decisions
        .iter()
        .map(|decision| decision.model.as_str())
        .collect();
    let by_instruction: BTreeMap<&str, &Decision> = decisions
        .iter()
        .map(|decision| (decision.instruction.as_str(), decision))
        .collect();
    let mut right = 0_usize;
    let mut total = 0_usize;
    // (expected, decided) → (seen, right)
    let mut per_mode: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    let mut millis: Vec<u64> = Vec::new();
    for case in cases {
        let Some(decision) = by_instruction.get(case.instruction.as_str()) else {
            eprintln!("no decision recorded for `{}`", case.instruction);
            continue;
        };
        total = total.saturating_add(1);
        millis.push(decision.millis);
        let hit = usize::from(decision.decision == case.expected);
        right = right.saturating_add(hit);
        let counts = per_mode
            .entry((case.expected.as_str(), decision.decision.as_str()))
            .or_default();
        counts.0 = counts.0.saturating_add(1);
        counts.1 = counts.1.saturating_add(hit);
    }
    if total == 0 {
        return Err("the recording covers none of the evaluation set".into());
    }
    millis.sort_unstable();
    let mut report = String::new();
    let _ = writeln!(
        report,
        "model: {}",
        models.into_iter().collect::<Vec<_>>().join(", ")
    );
    let _ = writeln!(
        report,
        "accuracy: {right}/{total} ({:.1}%)",
        100.0 * right as f64 / total as f64
    );
    for ((expected, decided), (seen, hit)) in per_mode {
        let _ = writeln!(
            report,
            "  expected {expected}, decided {decided}: {hit}/{seen}"
        );
    }
    let _ = writeln!(
        report,
        "latency: median {} ms, p95 {} ms",
        percentile(&millis, 0.5),
        percentile(&millis, 0.95)
    );
    print!("{report}");
    Ok(())
}
