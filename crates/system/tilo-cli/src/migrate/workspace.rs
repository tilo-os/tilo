//! Where a migration lands: the person's workspace, reached as this computer's device.
//!
//! A migration is the agent on this computer acting for the person, so it goes the agent's way
//! in: a connection to this computer's peer, the service `tilo login` leaves running, which
//! holds the device's one session with the workspace for every `tilo mcp` and `tilo` command
//! of this computer ([`crate::mcp::held`], [`crate::service`]), asking the workspace's apps to
//! do their own ordinary actions. Every action of one run names that run, so History shows the run
//! as one entry under the agent's name, the person sees each record arrive in their tab, and
//! undoing the entry takes back exactly what the run brought.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};

/// How long one action may take before the run gives up on it.
const ACTION_WAIT: Duration = Duration::from_secs(120);
/// How often a step the workspace was too slow to answer is asked again.
const ACTION_TRIES: u32 = 4;
/// The largest piece of an upload one call carries (the session's own bound).
const PUT_CHUNK: usize = 512 * 1024;

/// What a run asks of the workspace.
pub trait Workspace {
    /// Run `action` (`contacts#add-contacts`) with `args` as a step of `run`; its answer.
    ///
    /// # Errors
    /// The workspace refused it or could not be reached.
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<String>;

    /// Keep `bytes` in the workspace's content; the `tilo.content:` reference actions name them
    /// by.
    ///
    /// # Errors
    /// The workspace refused them or could not be reached.
    fn put(&mut self, bytes: &[u8]) -> Result<String>;
}

/// One migration run: what History calls it, and the id every action of it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub title: String,
}

impl Run {
    /// The `_meta` an action of this run carries.
    fn meta(&self) -> Value {
        json!({"so.tilo.mcp": {"run": {"id": self.id, "title": self.title}}})
    }
}

/// The workspace through this device's held session.
pub struct Held {
    lines: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

impl Held {
    /// Connect to this computer's peer, starting it (as `tilo folders start` does) when it does
    /// not run.
    ///
    /// # Errors
    /// This computer is not logged in, or its peer could not be reached.
    pub fn connect() -> Result<Self> {
        use crate::mcp::held;
        let dir = crate::login::device_dir()?;
        let (key, _) =
            crate::login::load(&dir)?.ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
        crate::login::trust_test_certificates()?;
        let place =
            held::Place::of(&dir, &key.verifying_key().to_bytes()).map_err(|why| anyhow!(why))?;
        let stream = if let Some(stream) = held::connect(&place) {
            stream
        } else {
            crate::service::start().context("this computer's peer did not start")?;
            held::connect(&place)
                .ok_or_else(|| anyhow!("this computer's peer started but does not listen"))?
        };
        stream.set_read_timeout(Some(ACTION_WAIT))?;
        let writer = stream.try_clone()?;
        let mut held = Self {
            lines: BufReader::new(stream),
            writer,
            next: 0,
        };
        held.request(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "tilo migrate", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        held.notify("notifications/initialized")?;
        Ok(held)
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        writeln!(self.writer, "{line}")?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value> {
        self.next = self.next.saturating_add(1);
        let id = self.next;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.writer, "{line}")?;
        self.writer.flush()?;
        loop {
            let mut answer = String::new();
            let read = self
                .lines
                .read_line(&mut answer)
                .context("the workspace stopped answering")?;
            if read == 0 {
                bail!("the connection to the workspace closed");
            }
            let answer: Value = serde_json::from_str(&answer)?;
            if answer.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = answer.get("error") {
                bail!(
                    "{}",
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the workspace refused")
                );
            }
            return Ok(answer.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Call `tool` and answer its text, or why it failed.
    fn tool(&mut self, tool: &str, arguments: &Value, meta: Option<Value>) -> Result<String> {
        let mut params = json!({"name": tool, "arguments": arguments});
        if let (Some(meta), Some(fields)) = (meta, params.as_object_mut()) {
            fields.insert("_meta".into(), meta);
        }
        let result = self.request("tools/call", &params)?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|content| {
                content
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!("{text}");
        }
        Ok(text)
    }
}

/// Whether the workspace said it was too slow to answer, rather than that it refused.
fn slow(error: &anyhow::Error) -> bool {
    let said = format!("{error:#}");
    said.contains("did not answer within") || said.contains("not connected to the host")
}

impl Workspace for Held {
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<String> {
        let wait = u64::try_from(ACTION_WAIT.as_millis()).unwrap_or(u64::MAX);
        // The same step of the same run is the same invocation, which the workspace never runs
        // twice: a step asked again after its answer was lost is answered, not repeated.
        let step = blake3::hash(format!("{}\u{0}{action}\u{0}{args}", run.id).as_bytes());
        let invocation = uuid::Builder::from_random_bytes(
            step.as_bytes()
                .get(..16)
                .and_then(|bytes| bytes.try_into().ok())
                .unwrap_or([0; 16]),
        )
        .into_uuid()
        .to_string();
        let call =
            json!({"path": action, "args": args, "wait_ms": wait, "invocation_id": invocation});
        let mut tries = 0_u32;
        let answer = loop {
            match self.tool("tilo_do", &call, Some(run.meta())) {
                Err(error) if tries < ACTION_TRIES && slow(&error) => {
                    tries = tries.saturating_add(1);
                    std::thread::sleep(Duration::from_secs(2));
                }
                other => break other?,
            }
        };
        if let Some(why) = answer.strip_prefix("failed:") {
            bail!("{action}: {}", why.trim());
        }
        if answer.starts_with("pending") {
            bail!("{action} did not finish in time: {answer}");
        }
        // Anything but `ok` is the workspace saying why it did not run it (an app not running, a
        // path it does not have).
        if !answer.starts_with("ok") {
            bail!("{action}: {answer}");
        }
        Ok(answer)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        let begun = self.tool("tilo_put_begin", &json!({"size": bytes.len()}), None)?;
        let upload = serde_json::from_str::<Value>(&begun)
            .ok()
            .and_then(|begun| begun.get("upload").and_then(Value::as_u64))
            .ok_or_else(|| anyhow!("the workspace began no upload: {begun}"))?;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut offset = 0_usize;
        for chunk in bytes.chunks(PUT_CHUNK) {
            self.tool(
                "tilo_put_append",
                &json!({"upload": upload, "offset": offset, "chunk": engine.encode(chunk)}),
                None,
            )?;
            offset = offset.saturating_add(chunk.len());
        }
        let finished = self.tool("tilo_put_finish", &json!({"upload": upload}), None)?;
        let reference = serde_json::from_str::<Value>(&finished)
            .ok()
            .and_then(|value| match value {
                Value::String(text) => Some(text),
                other => other
                    .get("content")
                    .or_else(|| other.get("reference"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
            .unwrap_or_else(|| finished.trim().trim_matches('"').to_owned());
        if !reference.starts_with("tilo.content:") {
            bail!("the workspace kept the upload under no reference: {finished}");
        }
        Ok(reference)
    }
}
