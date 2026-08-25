//! `qos-agent-replay` — post a traced request body again, and say what came
//! back.
//!
//! The agent's decision is a conversation with a model, and when that
//! conversation fails the interesting half is the **input**: an instruction
//! somebody wrote, a survey of a room, an application's own words about its
//! own operations. A log line saying the model declined shows none of it.
//!
//! `QOS_AGENT_TRACE_DIR` writes every body down (see
//! [`qos_agent::model::send`]); this posts one of them back through the same
//! transport, with the same betas, and prints the answer's shape — the model
//! that served it, `stop_reason`, the refusal category when there is one, and
//! the tools it called. So a body that was declined can be cut down, edited
//! and posted again until the part that was read as something else is the
//! only part left, without a session, a compositor or a room.
//!
//! ```text
//! QOS_AGENT_TRACE_DIR=/some/dir cargo run -p agent --bin qos-agent   # trace
//! cargo run -p agent --bin qos-agent-replay -- /some/dir/000-request.json
//! ```
//!
//! The key is read exactly as the agent reads it and travels in a header. It
//! is never printed, and neither is a request body: this prints what came
//! back, and the file it was given is already on disk.

use std::time::Duration;

use serde_json::Value;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: qos-agent-replay <request-body.json> [--raw]")?;
    let raw = args.any(|a| a == "--raw");

    let body: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?,
    )
    .map_err(|e| format!("{path}: {e}"))?;

    let key = qos_agent::model::api_key().ok_or(
        "no ANTHROPIC_API_KEY in the environment or the operator's env file",
    )?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let text = qos_agent::model::send(&client, &key, &body).map_err(|e| e.why())?;
    if raw {
        println!("{text}");
        return Ok(());
    }

    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let at = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("—").to_string();
    println!("model:       {}", at("model"));
    println!("stop_reason: {}", at("stop_reason"));
    if let Some(d) = v.get("stop_details").filter(|d| !d.is_null()) {
        println!("stop_details: {d}");
    }
    for block in v.get("content").and_then(Value::as_array).into_iter().flatten() {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("?");
        match kind {
            "tool_use" => println!(
                "  tool_use {}  {}",
                block.get("name").and_then(Value::as_str).unwrap_or("?"),
                block.get("input").map(|i| i.to_string()).unwrap_or_default()
            ),
            "text" => println!(
                "  text {:?}",
                block.get("text").and_then(Value::as_str).unwrap_or("")
            ),
            other => println!("  {other} {block}"),
        }
    }
    Ok(())
}
