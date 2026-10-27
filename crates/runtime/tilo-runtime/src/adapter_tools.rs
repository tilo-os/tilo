//! The MCP face of [`crate::adapters`]: a task an agent worked out in a
//! legacy app becomes a recording that later runs replay without a model.
//!
//! Three calls make the loop: `adapter_learn` keeps the steps just taken,
//! `adapter_run` replays them and answers the drift it stopped at (the agent
//! then does the task live, as any fallback does), and `adapter_teach` hands
//! that live run back — a change too small for a person to notice is applied
//! and logged, a meaningful one is kept for review.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::adapters::{AdapterError, Adapters, AppId, Recorded, Recording, Repaired, Screen};
use crate::computer::{Action, Agent, Computer};

use std::sync::Arc;

/// The tools an agent learns and replays legacy-app work with.
pub fn tool_definitions() -> Vec<Value> {
    let effect = json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true});
    let reads = json!({"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":true});
    let cell = json!({"type":["array","string"],"description":"The picture's cells, as the frame that returned it carried them: the array, or its comma-separated string"});
    let step = json!({
        "type":"object",
        "properties":{
            "action":{"type":"object","description":"One computer action, with the computer tool's arguments."},
            "mark":cell,
            "expect":{"type":"boolean","description":"The step may only be taken from this screen, rather than leaving it."}
        },
        "required":["action","mark"],
        "additionalProperties":false
    });
    vec![
        json!({"name":"adapter_learn","description":"Keep a task you have just worked out in a legacy app, so a later run needs no model. Pass the steps you took in order, each the computer action and the cells of the frame it returned. Do this once the app shows the result you wanted: adapter_run then replays these steps and checks each one.","inputSchema":{"type":"object","properties":{"app":{"type":"string","description":"The app the steps act in, as its app_id"},"task":{"type":"string","description":"What a person asked for, in their words: how a later run reaches this adapter."},"steps":{"type":"array","items":step,"minItems":1,"maxItems":200}},"required":["app","task","steps"],"additionalProperties":false},"annotations":effect}),
        json!({"name":"adapter_run","description":"Do a task an earlier adapter_learn kept, with no model: replays the recorded steps and checks each screen as it goes. Answers the steps' frames, or — when a screen is not what the recording expects — the step that drifted and why, so you do the task live and hand the run to adapter_teach.","inputSchema":{"type":"object","properties":{"app":{"type":"string"},"task":{"type":"string"}},"required":["app","task"],"additionalProperties":false},"annotations":effect}),
        json!({"name":"adapter_teach","description":"Hand back the live run that did a task after adapter_run drifted, so the recording becomes what the app does now. A change too small for a person to notice is applied and logged; a meaningful one is kept for review.","inputSchema":{"type":"object","properties":{"app":{"type":"string"},"task":{"type":"string"},"steps":{"type":"array","items":step,"minItems":1,"maxItems":200}},"required":["app","task","steps"],"additionalProperties":false},"annotations":effect}),
        json!({"name":"adapter_list","description":"What this desktop has learned to do in one app: a line per task, with how many runs made it, how many fell back to a model, and whether a repair waits for review.","inputSchema":{"type":"object","properties":{"app":{"type":"string"}},"required":["app"],"additionalProperties":false},"annotations":reads}),
    ]
}

/// Whether `name` is one of [`tool_definitions`].
pub fn is_adapter_tool(name: &str) -> bool {
    tool_definitions()
        .iter()
        .any(|definition| definition["name"].as_str() == Some(name))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    action: Action,
    mark: Value,
    #[serde(default)]
    expect: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Learn {
    app: String,
    task: String,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    app: String,
    task: String,
}

/// A live run handed back for a task that already has a recording.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Teach {
    app: String,
    task: String,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Of {
    app: String,
}

/// One adapter call, answered off the async runtime: a replay drives the
/// control socket, which blocks.
pub async fn call(
    name: &str,
    args: Value,
    adapters: &Adapters,
    computer: Option<Arc<Computer>>,
    agent: Option<Agent>,
) -> Result<Value, String> {
    let adapters = adapters.clone();
    match name {
        "adapter_learn" => tokio::task::spawn_blocking(move || learn(args, &adapters))
            .await
            .map_err(|error| error.to_string())?,
        "adapter_run" => tokio::task::spawn_blocking(move || {
            run(args, &adapters, computer.as_ref(), agent.as_ref())
        })
        .await
        .map_err(|error| error.to_string())?,
        "adapter_teach" => tokio::task::spawn_blocking(move || teach(args, &adapters))
            .await
            .map_err(|error| error.to_string())?,
        "adapter_list" => tokio::task::spawn_blocking(move || list(args, &adapters))
            .await
            .map_err(|error| error.to_string())?,
        _ => Err("Unknown tool".into()),
    }
}

/// The steps of one call as a recording of `app` doing `task`.
fn recording_of(app: AppId, task: String, steps: Vec<Step>) -> Result<Recording, String> {
    let mut recording = Recording::new(app, task, None);
    for step in steps {
        let mark = mark_of(&step.mark)?;
        recording
            .push(Recorded {
                action: step.action,
                check: Some(mark.clone()),
                expect: step.expect.then(|| mark.clone()),
                after: Some(mark),
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(recording)
}

fn mark_of(cells: &Value) -> Result<Screen, String> {
    crate::adapters::Mark::parse(cells)
        .map(|mark| mark.screen)
        .map_err(|error| error.to_string())
}

fn learn(args: Value, adapters: &Adapters) -> Result<Value, String> {
    let ask: Learn = parse(args)?;
    let recording = recording_of(
        AppId::parse(&ask.app).map_err(|error| error.to_string())?,
        ask.task,
        ask.steps,
    )?;
    adapters
        .save(&recording)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "app": recording.app.as_str(),
        "task": recording.task,
        "steps": recording.steps.len(),
        "learned": true,
    }))
}

fn run(
    args: Value,
    adapters: &Adapters,
    computer: Option<&Arc<Computer>>,
    agent: Option<&Agent>,
) -> Result<Value, String> {
    let ask: Named = parse(args)?;
    let app = AppId::parse(&ask.app).map_err(|error| error.to_string())?;
    let recording = adapters
        .find(&app, &ask.task)
        .map_err(|error| error.to_string())?;
    let Some(computer) = computer else {
        return Err("this session has no screen to act on".into());
    };
    match recording.replay(computer, agent) {
        Ok(frames) => Ok(json!({
            "app": app.as_str(),
            "task": recording.task,
            "steps": recording.steps.len(),
            "done": true,
            "frames": crate::computer::content(&frames, false),
        })),
        // A drifted replay is the fallback's own answer, not a failure of
        // the call: the agent is being told to do the task live.
        Err(AdapterError::Drifted(drift)) => Ok(json!({
            "app": app.as_str(),
            "task": recording.task,
            "done": false,
            "drifted_at": drift.step,
            "reason": drift.reason,
            "next": "the app changed: do the task live with the computer tool, then hand the run to adapter_teach",
        })),
        Err(error) => Err(error.to_string()),
    }
}

fn teach(args: Value, adapters: &Adapters) -> Result<Value, String> {
    let ask: Teach = parse(args)?;
    let app = AppId::parse(&ask.app).map_err(|error| error.to_string())?;
    let prior = adapters.find(&app, &ask.task).ok();
    let mut recording = recording_of(app, ask.task, ask.steps)?;
    if let Some(prior) = prior {
        // The live run replaces what the app used to do, and how many runs
        // reached the recording carries over: a repair is not a new task.
        recording.runs = prior.runs.saturating_add(1);
        recording.fallbacks = prior.fallbacks.saturating_add(1);
        recording.repaired = true;
        recording.drifted_at = prior.drifted_at;
        recording.baseline_steps = prior.steps.iter().map(Repaired::of).collect();
    }
    let (before, after) = recording.repair();
    let trivial = after.is_trivial(&before);
    adapters
        .save(&recording)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "app": recording.app.as_str(),
        "task": recording.task,
        "steps": recording.steps.len(),
        "runs": recording.runs,
        // Nothing a person would notice changed: applied and logged. A
        // meaningful change waits for them.
        "applied": trivial,
        "review": !trivial,
        "change": after.describe(&before),
    }))
}

fn list(args: Value, adapters: &Adapters) -> Result<Value, String> {
    let ask: Of = parse(args)?;
    let app = AppId::parse(&ask.app).map_err(|error| error.to_string())?;
    let learned: Vec<Value> = adapters
        .load(&app)
        .map_err(|error| error.to_string())?
        .recordings
        .into_iter()
        .map(|recording| {
            let (before, after) = recording.repair();
            json!({
                "task": recording.task,
                "steps": recording.steps.len(),
                "runs": recording.runs,
                "fallbacks": recording.fallbacks,
                "review": recording.repaired && !after.is_trivial(&before),
                "change": after.describe(&before),
            })
        })
        .collect();
    Ok(json!({"app": app.as_str(), "adapters": learned}))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}
