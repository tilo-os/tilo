//! Where a migration lands: the person's workspace, reached as this computer's device.
//!
//! A migration is the agent on this computer acting for the person, so it goes the agent's way
//! in: a connection to this computer's peer, the service `tilo login` leaves running, which
//! holds the device's one session with the workspace for every `tilo mcp` and `tilo` command
//! of this computer ([`crate::mcp::held`], [`crate::service`]), asking the workspace's apps to
//! do their own ordinary actions. Every action of one run names that run, so History shows the run
//! as one entry under the agent's name, the person sees each record arrive in their tab, and
//! undoing the entry takes back exactly what the run brought.
//!
//! Every call asks for the typed answer the workspace keeps beside its words, and decides on that:
//! how a call went, the value and the records it answered with, and the code of a refusal. What a
//! call did is never read back out of its text.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};

/// How long one action may take before the run gives up on it.
const ACTION_WAIT: Duration = Duration::from_secs(120);
/// How long a step waits for its person to allow it in their tab.
const PERSON_WAIT: Duration = Duration::from_secs(6 * 60);
/// How often a step the workspace said to ask again is asked again before the run gives up: each
/// ask waits for the workspace itself ([`ACTION_WAIT`]), or for the wait it said.
const ACTION_TRIES: u32 = 7;
/// The largest piece of an upload one call carries (the session's own bound).
const PUT_CHUNK: usize = 512 * 1024;
/// The code of a save the workspace's host did not confirm in time: it may or may not be kept, so
/// the same call under its same id is asked again, which runs it once.
const SAVE_UNCONFIRMED: &str = "save_unconfirmed";

/// What a run asks of the workspace.
pub trait Workspace {
    /// Run `action` (`contacts#add-contacts`) with `args` as a step of `run`; its answer.
    ///
    /// # Errors
    /// The workspace refused it ([`Refused`], by its code) or could not be reached.
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<Answer>;

    /// Run `action` with `args` outside any run: a read, or taking a run back.
    ///
    /// # Errors
    /// The workspace refused it ([`Refused`], by its code) or could not be reached.
    fn ask(&mut self, action: &str, args: Value) -> Result<Answer>;

    /// Keep `bytes` in the workspace's content; the `tilo.content:` reference actions name them
    /// by.
    ///
    /// # Errors
    /// The workspace refused them or could not be reached.
    fn put(&mut self, bytes: &[u8]) -> Result<String>;
}

/// What an action that ran answered: its value, and what it made, each item
/// with its id and the address it is published under.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    result: Option<Value>,
}

impl Answer {
    /// An answer carrying `result`.
    pub(crate) fn of(result: Option<Value>) -> Self {
        Self { result }
    }

    /// The value the action answered, when it answered one.
    #[must_use]
    pub fn result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    /// Whether the action answered `true` under `field` (an account it found there already).
    #[must_use]
    pub fn said(&self, field: &str) -> bool {
        self.result
            .as_ref()
            .and_then(|result| result.get(field))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// One migration run: what History calls it, and the id every action of it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub title: String,
}

impl Run {
    /// The `_meta` an action of this run carries: the typed answer it asks for, and the group it is
    /// a step of.
    fn meta(&self) -> Value {
        json!({"so.tilo.mcp": {
            "structured": true,
            "invocation": {"id": self.id, "title": self.title},
        }})
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
            crate::service::start().map_err(|error| {
                tracing::debug!(%error, "Tilo's background sync did not start");
                anyhow!(
                    "Tilo's background sync didn't start on this computer. Run `tilo status` to \
                     see how it stands, then try again."
                )
            })?;
            held::connect(&place).ok_or_else(|| {
                anyhow!(
                    "Tilo's background sync isn't answering on this computer yet. Run `tilo \
                     status` to see how it stands, then try again."
                )
            })?
        };
        Self::over(stream)
    }

    /// The session `stream` carries, opened: the workspace's end answers as it does.
    fn over(stream: UnixStream) -> Result<Self> {
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

    /// Call `tool` and answer its typed value, or why it failed.
    fn tool(&mut self, tool: &str, arguments: &Value, meta: Option<Value>) -> Result<Value> {
        let mut params = json!({"name": tool, "arguments": arguments});
        if let (Some(meta), Some(fields)) = (meta, params.as_object_mut()) {
            fields.insert("_meta".into(), meta);
        }
        let result = self.request("tools/call", &params)?;
        let typed = result.get("structuredContent").cloned().ok_or_else(|| {
            anyhow!(
                "{tool} answered no typed value: {}",
                result.get("content").cloned().unwrap_or_default()
            )
        })?;
        if result.get("isError").and_then(Value::as_bool) == Some(true)
            && typed.get("status").and_then(Value::as_str) != Some("failed")
        {
            // A tool the host refused before the action ran: its typed refusal is the answer.
            return Err(refused_of(&typed).into());
        }
        Ok(typed)
    }

    /// Call `tilo_do` and answer how it went, typed: what the action did, or why it did not run.
    fn call(&mut self, arguments: &Value, meta: Option<Value>) -> Result<Call> {
        Call::of(&self.tool("tilo_do", arguments, meta)?)
    }

    /// Call `tool` and read its answer as the JSON it is.
    fn text(&mut self, tool: &str, arguments: &Value) -> Result<Value> {
        let params = json!({"name": tool, "arguments": arguments});
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
            bail!("{tool}: {text}");
        }
        serde_json::from_str(&text)
            .with_context(|| format!("{tool} answered no value: {}", text.trim()))
    }
}

/// How one call went, as its typed answer says.
#[derive(Clone, Debug)]
pub(super) struct Call {
    pub(super) status: Status,
    pub(super) result: Option<Value>,
    pub(super) error: Option<Refused>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    /// It ran, and answered what it did.
    Ok,
    /// It did not run: its refusal, by code.
    Failed,
    /// It runs still, waiting for its person (to allow it in their tab, say).
    Pending,
}

impl Call {
    /// The answer `typed` says the call gave.
    pub(super) fn of(typed: &Value) -> Result<Self> {
        let status = match typed.get("status").and_then(Value::as_str) {
            Some("ok") => Status::Ok,
            Some("failed") => Status::Failed,
            Some("pending") => Status::Pending,
            other => bail!("the workspace answered no status: {other:?}"),
        };
        let result = typed
            .get("result")
            .filter(|value| !value.is_null())
            .cloned();
        let error = (status == Status::Failed).then(|| refused_of(typed));
        Ok(Self {
            status,
            result,
            error,
        })
    }

    pub(super) fn answer(&self) -> Answer {
        Answer::of(self.result.clone())
    }
}

/// The refusal `typed` carries, in the words its code is answered beside.
fn refused_of(typed: &Value) -> Refused {
    Refused {
        code: typed
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("failed")
            .to_owned(),
        message: typed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        retry_after_ms: typed
            .pointer("/error/retry_after_ms")
            .and_then(Value::as_u64),
    }
}

/// A call the workspace did not run, as its typed answer says: its code, its words, and the
/// wait it asked for.
#[derive(Clone, Debug)]
pub struct Refused {
    code: String,
    message: String,
    retry_after_ms: Option<u64>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

impl Refused {
    /// The workspace's refusal among `error`'s causes, when it refused.
    #[must_use]
    pub fn of(error: &anyhow::Error) -> Option<&Self> {
        error.downcast_ref()
    }

    /// The app's stable code for why it did not run (`message_already_held`).
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// A refusal named by `code`, in `message`, as a workspace answers one.
    #[cfg(test)]
    pub(super) fn coded(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            retry_after_ms: None,
        }
    }

    /// The refusal said with the action it names, as a person reads it.
    fn with_action(mut self, action: &str) -> Self {
        self.message = format!("{action}: {}", self.message.trim());
        self
    }

    /// Whether the workspace said the same step works when asked again: it was busy (after the
    /// wait it said), not serving yet, or did not answer. Asked again under its same id, a step
    /// runs once.
    pub(super) fn asks_again(&self) -> bool {
        matches!(
            self.code.as_str(),
            "rate_limited" | "not_ready" | "unanswered"
        )
    }
}

impl Workspace for Held {
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<Answer> {
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
        let until = std::time::Instant::now()
            .checked_add(PERSON_WAIT)
            .ok_or_else(|| anyhow!("no time to wait for the person in"))?;
        loop {
            let call = self.call(&call, Some(run.meta()))?;
            match call.status {
                Status::Ok => return Ok(call.answer()),
                // An action the person allows first (an outside one, as adding an account is) is
                // pending while their tab asks them: the same call under the same id waits for it
                // to move, held by the workspace, until their answer.
                Status::Pending => {
                    if std::time::Instant::now() >= until {
                        bail!(
                            "{action} is waiting for the person to allow it in their Tilo tab. Allow it \
                             there, then run the same command again; what it did so far is kept."
                        );
                    }
                }
                Status::Failed => {
                    let error = call.error.unwrap_or_else(|| Refused {
                        code: String::from("failed"),
                        message: String::new(),
                        retry_after_ms: None,
                    });
                    // A save its host did not confirm in time may or may not be kept: asked again
                    // under its same id, it runs once.
                    let again = error.asks_again() || error.code == SAVE_UNCONFIRMED;
                    if again && tries < ACTION_TRIES {
                        if let Some(wait) = error.retry_after_ms {
                            std::thread::sleep(Duration::from_millis(wait));
                        }
                        tries = tries.saturating_add(1);
                        continue;
                    }
                    return Err(error.with_action(action).into());
                }
            }
        }
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        let wait = u64::try_from(ACTION_WAIT.as_millis()).unwrap_or(u64::MAX);
        let call = self.call(
            &json!({"path": action, "args": args, "wait_ms": wait}),
            None,
        )?;
        match call.status {
            Status::Ok => Ok(call.answer()),
            Status::Pending => bail!("{action} did not finish in time"),
            Status::Failed => Err(call
                .error
                .unwrap_or_else(|| Refused {
                    code: String::from("failed"),
                    message: String::new(),
                    retry_after_ms: None,
                })
                .with_action(action)
                .into()),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        let begun = self.text("tilo_put_begin", &json!({"size": bytes.len()}))?;
        let upload = begun
            .get("upload")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("the workspace began no upload: {begun}"))?;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut offset = 0_usize;
        for chunk in bytes.chunks(PUT_CHUNK) {
            self.text(
                "tilo_put_append",
                &json!({"upload": upload, "offset": offset, "chunk": engine.encode(chunk)}),
            )?;
            offset = offset.saturating_add(chunk.len());
        }
        let finished = self.text("tilo_put_finish", &json!({"upload": upload}))?;
        let reference = finished
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!("the workspace kept the upload under no reference: {finished}")
            })?;
        if !reference.starts_with("tilo.content:") {
            bail!("the workspace kept the upload under no reference: {finished}");
        }
        Ok(reference.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The workspace's end of a session: it records each call and answers the next of `answers`.
    fn serve(stream: UnixStream, calls: &Arc<Mutex<Vec<Value>>>, answers: Vec<Value>) {
        let mut lines = BufReader::new(stream.try_clone().expect("a reading end"));
        let mut out = stream;
        let mut answers = answers.into_iter();
        loop {
            let mut line = String::new();
            if lines.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let request: Value = serde_json::from_str(&line).expect("a request");
            let Some(id) = request.get("id").cloned() else {
                continue;
            };
            let answer = if request.get("method").and_then(Value::as_str) == Some("tools/call") {
                calls.lock().expect("the calls").push(request);
                answers
                    .next()
                    .unwrap_or_else(|| json!({"content": [], "isError": true}))
            } else {
                json!({})
            };
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": answer});
            writeln!(out, "{reply}").expect("an answer");
        }
    }

    /// The workspace reached over a socket the test answers on, and the calls it made.
    fn reached(answers: Vec<Value>) -> (Held, Arc<Mutex<Vec<Value>>>) {
        let (ours, theirs) = UnixStream::pair().expect("a socket pair");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        std::thread::spawn(move || serve(theirs, &recorded, answers));
        (Held::over(ours).expect("the session"), calls)
    }

    fn run() -> Run {
        Run {
            id: "run-1".into(),
            title: "From Thunderbird".into(),
        }
    }

    #[test]
    fn a_step_that_waits_is_asked_again_under_its_id_and_its_refusal_keeps_its_code() {
        let (mut workspace, calls) = reached(vec![
            json!({"content": [], "structuredContent": {
                "status": "pending", "invocation_id": "asked",
                "waiting_for": "the person to allow it",
            }, "isError": false}),
            json!({"content": [], "structuredContent": {
                "status": "failed",
                "error": {"code": tilo_actions::codes::MESSAGE_ALREADY_HELD, "message": "held"},
            }, "isError": true}),
        ]);
        let error = workspace
            .act(
                &run(),
                "mail/mail.mailbox#deliver-message",
                json!({"from": "ada@example.com"}),
            )
            .expect_err("the workspace refused");
        assert_eq!(
            Refused::of(&error).expect("a refusal").code(),
            tilo_actions::codes::MESSAGE_ALREADY_HELD
        );
        let calls = calls.lock().expect("the calls");
        assert_eq!(calls.len(), 2, "the pending step is asked about again");
        let ids = (
            calls[0].pointer("/params/arguments/invocation_id").cloned(),
            calls[1].pointer("/params/arguments/invocation_id").cloned(),
        );
        let structured = calls[0]
            .pointer("/params/_meta/so.tilo.mcp/structured")
            .cloned();
        let named = calls[0]
            .pointer("/params/_meta/so.tilo.mcp/invocation/id")
            .cloned();
        drop(calls);
        assert_eq!(
            ids.0, ids.1,
            "the same step is the same invocation, which runs once"
        );
        assert_eq!(
            structured,
            Some(json!(true)),
            "the caller asks for the typed answer it decides on"
        );
        assert_eq!(
            named,
            Some(json!("run-1")),
            "the step is named as the run's"
        );
    }

    #[test]
    fn a_step_that_ran_answers_its_value_and_the_records_it_names() {
        let (mut workspace, _) = reached(vec![json!({
            "content": [], "isError": false,
            "structuredContent": {
                "status": "ok",
                "result": {"already": true},
                "resources": ["tasks.tracker/issues/abc"],
            },
        })]);
        let answer = workspace
            .act(&run(), "tasks/tasks.issues#create", json!({"title": "x"}))
            .expect("it ran");
        assert!(answer.said("already"));
    }

    #[test]
    fn an_upload_is_kept_under_the_reference_its_answer_names() {
        let (mut workspace, calls) = reached(vec![
            json!({"content": [{"type": "text", "text": "{\"upload\":7}"}], "isError": false}),
            json!({"content": [{"type": "text", "text": "{\"upload\":7,\"written\":5,\"size\":5}"}], "isError": false}),
            json!({"content": [{"type": "text", "text": "{\"content\":\"tilo.content:abc\"}"}], "isError": false}),
        ]);
        assert_eq!(
            workspace.put(b"hello").expect("it is kept"),
            "tilo.content:abc"
        );
        let calls = calls.lock().expect("the calls");
        let last = calls
            .last()
            .and_then(|call| call.pointer("/params/name"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        drop(calls);
        assert_eq!(last.as_deref(), Some("tilo_put_finish"));
    }
}
