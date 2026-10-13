//! A fixed two-turn developer acceptance probe, not a coding-task runner.
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tilo_model_gateway::{FRAMING, Frame, RPC_VERSION, Request, Response, openai};

const TOOL: &str = "read_probe_receipt";

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    settings: PathBuf,
    #[arg(long)]
    profile: String,
    #[arg(long)]
    model: String,
    /// A private JSON object with a fresh unpredictable `marker` string.
    #[arg(long)]
    receipt: PathBuf,
    /// A new directory; existing evidence is never overwritten.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u32).range(1..=4096))]
    max_output_tokens: u32,
    #[arg(long, default_value_t = 60_000, value_parser = clap::value_parser!(u64).range(1..=900_000))]
    timeout_ms: u64,
}

fn main() -> std::process::ExitCode {
    match run(&Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("local tool acceptance failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    fn digit(value: u8) -> char {
        char::from(if value < 10 {
            b'0'.saturating_add(value)
        } else {
            b'a'.saturating_add(value.saturating_sub(10))
        })
    }
    Sha256::digest(bytes)
        .iter()
        .flat_map(|byte| [digit(byte >> 4), digit(byte & 15)])
        .collect()
}

fn save(root: &Path, name: &str, value: &Value) -> Result<String, String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(name))
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(hash(&bytes))
}

fn fixed_tool() -> Value {
    json!({"type":"function","function":{
        "name":TOOL,"description":"Read the private acceptance receipt; returns its unpredictable marker.",
        "parameters":{"type":"object","properties":{},"additionalProperties":false}
    }})
}

fn checked_call(first: &Value) -> Result<openai::Turn, String> {
    let arguments = first
        .pointer("/choices/0/message/tool_calls/0/function/arguments")
        .and_then(Value::as_str)
        .ok_or("tool arguments absent or not a string")?;
    let input: Value = serde_json::from_str(arguments).map_err(|_| "invalid tool argument JSON")?;
    if input != json!({}) {
        return Err("read-only tool arguments must be empty".into());
    }
    let turn = openai::read_turn(first)?;
    if turn.calls.len() != 1 {
        return Err("expected exactly one read-only tool call".into());
    }
    let call = turn.calls.first().ok_or("tool call absent")?;
    if call.name != TOOL || call.input != json!({}) || call.id.is_empty() {
        return Err("model did not call the fixed read-only tool with empty arguments".into());
    }
    Ok(turn)
}

fn replay(first: &Value, receipt: &Value, messages: &mut Vec<Value>) -> Result<(), String> {
    let turn = checked_call(first)?;
    let call = turn.calls.first().ok_or("tool call absent")?;
    let id = call.id.clone();
    messages.push(turn.message);
    messages.push(json!({"role":"tool","tool_call_id":id,"content":receipt.to_string()}));
    Ok(())
}

fn check_final(body: &Value, marker: &str) -> Result<(), String> {
    let turn = openai::read_turn(body)?;
    if !turn.calls.is_empty() || turn.text.as_deref().map(str::trim) != Some(marker) {
        return Err("final answer did not reproduce the actually read receipt marker".into());
    }
    Ok(())
}

fn ask(cli: &Cli, messages: &[Value]) -> Result<(Value, Value, u128), String> {
    let body = json!({"model":cli.model,"messages":messages,"tools":[fixed_tool()],
        "tool_choice":"auto","max_tokens":cli.max_output_tokens,
        "chat_template_kwargs":{"enable_thinking":true,"preserve_reasoning":true}});
    let mut stream = UnixStream::connect(&cli.socket).map_err(|error| error.to_string())?;
    let timeout = Some(Duration::from_millis(cli.timeout_ms.saturating_add(5000)));
    stream
        .set_read_timeout(timeout)
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(timeout)
        .map_err(|error| error.to_string())?;
    // Only this explicitly developer-created listener accepts this handoff.
    // Normal applications receive their identity and descriptor from launcher.
    FRAMING
        .write(
            &mut stream,
            &serde_json::to_vec(&json!({"protocol":1,"app_id":"so.tilo.agent"}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let request = Frame {
        version: RPC_VERSION,
        value: Request::Plan {
            profile_id: cli.profile.clone(),
            model_id: cli.model.clone(),
            body: body.clone(),
            timeout_ms: cli.timeout_ms,
            stream: true,
            deltas: false,
            trace: None,
        },
    };
    let began = Instant::now();
    FRAMING
        .write(
            &mut stream,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    for _ in 0..64 {
        let remaining = Duration::from_millis(cli.timeout_ms.saturating_add(5000))
            .checked_sub(began.elapsed())
            .ok_or("gateway probe deadline elapsed")?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|error| error.to_string())?;
        let frame = FRAMING
            .read(&mut stream)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "gateway closed before a final response".to_string())?;
        let response: Response =
            serde_json::from_slice(&frame).map_err(|error| error.to_string())?;
        if !response.ok {
            return Err(response
                .error
                .unwrap_or_else(|| "gateway refused probe".into()));
        }
        if response.event.is_some() {
            continue;
        }
        let output = response
            .output
            .ok_or_else(|| "gateway returned no provider body".to_string())?;
        let provider: Value = serde_json::from_str(&output).map_err(|error| error.to_string())?;
        if provider.get("model").and_then(Value::as_str) != Some(cli.model.as_str()) {
            return Err("provider did not report the exact configured model alias".into());
        }
        return Ok((body, provider, began.elapsed().as_millis()));
    }
    Err("gateway probe exceeded 64 response frames".into())
}

fn read_receipt(path: &Path) -> Result<Value, String> {
    let file = tilo_files::native::nofollow::open_read(path).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("probe receipt must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 4096 {
        return Err("probe receipt exceeds 4096 bytes".into());
    }
    let receipt: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let marker = receipt
        .get("marker")
        .and_then(Value::as_str)
        .ok_or("receipt lacks marker")?;
    if !(16..=128).contains(&marker.len())
        || !marker
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("receipt marker must be 16–128 ASCII identifier characters".into());
    }
    Ok(receipt)
}

fn run(cli: &Cli) -> Result<(), String> {
    let config = agent_provider_config::ProviderConfig::load_at(&cli.settings);
    if !config.validation_errors().is_empty() {
        return Err("project provider config is invalid".into());
    }
    let profile = config
        .profiles
        .iter()
        .find(|p| p.id == cli.profile && p.enabled)
        .ok_or("profile absent")?;
    let model = profile
        .models
        .iter()
        .find(|m| m.id == cli.model)
        .ok_or("model absent")?;
    let endpoint = url::Url::parse(&profile.endpoint).map_err(|error| error.to_string())?;
    let local = match endpoint.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    let pinned = model.revision.as_deref().is_some_and(|revision| {
        matches!(revision.len(), 40 | 64)
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if profile.location != agent_provider_config::EndpointLocation::ThisDevice
        || profile.protocol != agent_provider_config::ProviderProtocol::OpenAiCompatible
        || !local
        || !pinned
    {
        return Err("probe requires an explicitly local pinned model profile".into());
    }
    fs::create_dir(&cli.out).map_err(|error| error.to_string())?;
    fs::set_permissions(&cli.out, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let mut messages = vec![
        json!({"role":"system","content":"Call read_probe_receipt exactly once before answering. Then answer with only its marker, no other text. This is a bounded read-only acceptance check."}),
        json!({"role":"user","content":"Read the acceptance receipt and return its marker."}),
    ];
    let (request1, first, elapsed1) = ask(cli, &messages)?;
    let request1_hash = save(&cli.out, "request-1.json", &request1)?;
    let first_hash = save(&cli.out, "response-1.json", &first)?;
    let _ = checked_call(&first)?; // refuse malformed/absent call before any tool file access
    let receipt = read_receipt(&cli.receipt)?;
    replay(&first, &receipt, &mut messages)?;
    let (request2, second, elapsed2) = ask(cli, &messages)?;
    let request2_hash = save(&cli.out, "request-2.json", &request2)?;
    let second_hash = save(&cli.out, "response-2.json", &second)?;
    check_final(
        &second,
        receipt
            .get("marker")
            .and_then(Value::as_str)
            .ok_or("receipt lacks marker")?,
    )?;
    save(
        &cli.out,
        "acceptance.json",
        &json!({"schema_version":1,"evidence":"observed_developer_gateway_tool_round_trip",
        "model":cli.model,"model_revision":model.revision,"profile":cli.profile,
        "settings_sha256":hash(&fs::read(&cli.settings).map_err(|error|error.to_string())?),
        "probe_source_sha256":hash(include_str!("local-tool-acceptance.rs").as_bytes()),
        "request_sha256":[request1_hash,request2_hash],"response_sha256":[first_hash,second_hash],
        "elapsed_ms":[elapsed1,elapsed2],"max_output_tokens":cli.max_output_tokens,
        "timeout_ms":cli.timeout_ms,"thinking_requested":true,"preserve_reasoning_requested":true,
        "assistant_reasoning_present":first.pointer("/choices/0/message/reasoning_content").is_some_and(Value::is_string),
        "coding_quality_measured":false,"action_broker_exercised":false,"production_launcher_handoff_exercised":false}),
    )?;
    println!(
        "gateway read-only tool round trip passed; private evidence: {}",
        cli.out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_requires_real_call_and_preserves_production_reasoning() -> Result<(), String> {
        let first = json!({"choices":[{"message":{"content":null,"reasoning_content":"actual fixture reasoning","tool_calls":[{"id":"c1","type":"function","function":{"name":TOOL,"arguments":"{}"}}]}}]});
        let mut messages = vec![];
        replay(
            &first,
            &json!({"marker":"unpredictable_marker"}),
            &mut messages,
        )?;
        assert_eq!(
            messages
                .first()
                .and_then(|m| m.get("reasoning_content"))
                .ok_or("reasoning absent")?,
            "actual fixture reasoning"
        );
        assert_eq!(
            messages
                .get(1)
                .and_then(|m| m.get("tool_call_id"))
                .ok_or("tool result absent")?,
            "c1"
        );
        for arguments in ["{\"path\":\"/etc/passwd\"}", "not-json"] {
            let mut bad = first.clone();
            *bad.pointer_mut("/choices/0/message/tool_calls/0/function/arguments")
                .ok_or("arguments absent")? = json!(arguments);
            assert!(replay(&bad, &json!({}), &mut vec![]).is_err());
        }
        assert!(
            replay(
                &json!({"choices":[{"message":{"content":"claimed success"}}]}),
                &json!({}),
                &mut vec![]
            )
            .is_err()
        );
        assert!(
            check_final(
                &json!({"choices":[{"message":{"content":"wrong marker"}}]}),
                "unpredictable_marker"
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn receipt_is_a_real_bounded_file_and_evidence_cannot_be_overwritten()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("receipt.json");
        fs::write(&path, b"{\"marker\":\"unpredictable_marker\"}")?;
        let receipt = read_receipt(&path)?;
        assert_eq!(
            receipt.get("marker").and_then(Value::as_str),
            Some("unpredictable_marker")
        );
        let digest = save(dir.path(), "evidence.json", &receipt)?;
        assert!(save(dir.path(), "evidence.json", &json!({"marker":"different"})).is_err());
        assert_eq!(digest, hash(&fs::read(dir.path().join("evidence.json"))?));
        fs::write(&path, vec![b'x'; 4097])?;
        assert!(read_receipt(&path).is_err());
        let link = dir.path().join("linked-receipt.json");
        std::os::unix::fs::symlink(&path, &link)?;
        assert!(read_receipt(&link).is_err());
        Ok(())
    }
}
