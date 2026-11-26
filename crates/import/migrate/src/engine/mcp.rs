//! The workspace a run lands in, reached through sessions with it: its `tilo_do`, `tilo_read` and
//! upload tools, asked as JSON-RPC calls of a session the device that runs the migration opens
//! ([`Rpc`]). The migration's own session is its agent's, acting for the person; what only the
//! person may do (keeping a mail account and its sign-in) goes through a session of theirs.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};

use super::writer::{
    ACTION_TRIES, Answer, Call, Invocation, Refused, SAVE_UNCONFIRMED, Status, Workspace,
    refused_of, step_id,
};

/// How long one action may take before the run gives up on it.
const ACTION_WAIT: Duration = Duration::from_secs(120);
/// How long one ask of a step the person answers is held before it is read again: the record it
/// waits in says so until they answer it, which the step then reads.
const ASK_WAIT: Duration = Duration::from_secs(1);
/// How long a step waits for its person to allow it.
const PERSON_WAIT: Duration = Duration::from_secs(6 * 60);
/// What a step that nobody answered says: the account it would connect is left unconnected.
pub const UNANSWERED: &str = "unanswered";
/// The largest piece of an upload one call carries (the session's own bound).
const PUT_CHUNK: usize = 512 * 1024;

/// One session with the workspace: a JSON-RPC request answered, or `None` for a notification.
pub trait Rpc: Send {
    /// The answer to `request`.
    ///
    /// # Errors
    /// The session could not be asked.
    fn exchange(&mut self, request: Value) -> Result<Option<Value>>;
}

/// One session, opened.
struct Session {
    rpc: Box<dyn Rpc>,
    next: u64,
}

impl Session {
    /// The session `rpc` is, opened.
    fn open(rpc: Box<dyn Rpc>) -> Result<Self> {
        let mut session = Self { rpc, next: 0 };
        session.request(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "tilo migrate", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        session
            .rpc
            .exchange(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(session)
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value> {
        self.next = self.next.saturating_add(1);
        let answer = self
            .rpc
            .exchange(
                json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}),
            )?
            .ok_or_else(|| anyhow!("the workspace did not answer {method}"))?;
        if let Some(error) = answer.get("error") {
            bail!(
                "{}",
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the workspace refused")
            );
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
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
        let result = self.request("tools/call", &json!({"name": tool, "arguments": arguments}))?;
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

    /// `action` asked once, waited for while it runs.
    fn once(&mut self, action: &str, args: &Value) -> Result<Answer> {
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
}

/// What opens the person's own session, the first time one of their calls is made.
pub type OpensOwn = Box<dyn FnOnce() -> Result<Box<dyn Rpc>> + Send>;

/// The workspace through the migration's session with it, and the person's for their own calls.
pub struct Mcp {
    session: Session,
    /// What opens the person's own session, the first time one of their calls is made.
    own: Option<OpensOwn>,
    person: Option<Session>,
}

impl Mcp {
    /// The workspace through `rpc`, the migration's session; the person's own calls go through
    /// the session `own` opens.
    ///
    /// # Errors
    /// The migration's session did not open.
    pub fn open(rpc: Box<dyn Rpc>, own: OpensOwn) -> Result<Self> {
        Ok(Self {
            session: Session::open(rpc)?,
            own: Some(own),
            person: None,
        })
    }
}

impl Workspace for Mcp {
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        let wait = u64::try_from(ACTION_WAIT.as_millis()).unwrap_or(u64::MAX);
        // The same step of the same run is the same invocation, which the workspace never runs
        // twice: a step asked again after its answer was lost is answered, not repeated.
        let invocation = step_id(&run.id, action, &args);
        let call =
            json!({"path": action, "args": args, "wait_ms": wait, "invocation_id": invocation});
        let mut tries = 0_u32;
        let until = std::time::Instant::now()
            .checked_add(PERSON_WAIT)
            .ok_or_else(|| anyhow!("no time to wait for the person in"))?;
        loop {
            let typed = self.session.tool("tilo_do", &call, Some(run.meta()))?;
            match typed.get("status").and_then(Value::as_str) {
                Some("ok") => {
                    return Ok(Answer::of(
                        typed
                            .get("result")
                            .filter(|value| !value.is_null())
                            .cloned(),
                    ));
                }
                // An action the person allows first is kept as its asked record: the same call
                // under the same id waits for it to move, until their answer.
                Some("asked" | "pending") => {
                    if std::time::Instant::now() >= until {
                        return Err(Refused::coded(
                            UNANSWERED,
                            format!(
                                "{action} is waiting for the person to allow it. Allow it, then \
                                 start the migration again; what it did so far is kept."
                            ),
                        )
                        .into());
                    }
                }
                Some("failed") => {
                    let error = refused_of(&typed);
                    // A save its host did not confirm in time may or may not be kept: asked again
                    // under its same id, it runs once.
                    let again = error.asks_again() || error.code() == SAVE_UNCONFIRMED;
                    if again && tries < ACTION_TRIES {
                        error.wait_to_ask_again(action, tries);
                        tries = tries.saturating_add(1);
                        continue;
                    }
                    return Err(error.with_action(action).into());
                }
                other => bail!("the workspace answered no status: {other:?}"),
            }
        }
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        if self.person.is_none() {
            let own = self
                .own
                .take()
                .ok_or_else(|| anyhow!("the person's own session did not open"))?;
            self.person = Some(Session::open(own()?)?);
        }
        let person = self
            .person
            .as_mut()
            .ok_or_else(|| anyhow!("the person's own session did not open"))?;
        let action = format!("{path}#{action}");
        // The same step asked again is the same invocation, which the workspace runs once: a
        // call that waits in its asked record keeps that record, and its answer is read from it.
        let invocation = step_id("person", &action, &args);
        let wait = u64::try_from(ASK_WAIT.as_millis()).unwrap_or(u64::MAX);
        let call =
            json!({"path": action, "args": args, "wait_ms": wait, "invocation_id": invocation});
        let until = std::time::Instant::now()
            .checked_add(PERSON_WAIT)
            .ok_or_else(|| anyhow!("no time to wait for the person in"))?;
        loop {
            let typed = person.tool("tilo_do", &call, None)?;
            // A call the person's grants leave to them is kept as its asked record until they
            // answer it: this reads the record the workspace says it waits in, and asks again
            // under the same invocation id until the answer moves it on.
            match typed.get("status").and_then(Value::as_str) {
                Some("ok") => {
                    return Ok(typed
                        .get("result")
                        .filter(|value| !value.is_null())
                        .cloned()
                        .unwrap_or(Value::Null));
                }
                Some("asked" | "pending") => {
                    if std::time::Instant::now() >= until {
                        return Err(Refused::coded(
                            UNANSWERED,
                            format!(
                                "{action} waits for your yes: answer it, then start the migration \
                                 again; what it did so far is kept"
                            ),
                        )
                        .into());
                    }
                }
                Some("failed") => {
                    let error = refused_of(&typed);
                    if matches!(error.code(), "cancelled" | "declined") {
                        // The person said no, or stopped waiting: the account is not connected,
                        // and the step says so rather than failing the run.
                        return Err(Refused::coded(UNANSWERED, error.message).into());
                    }
                    return Err(error.with_action(&action).into());
                }
                other => bail!("the workspace answered no status: {other:?}"),
            }
        }
    }

    fn read(&mut self, path: &str) -> Result<Value> {
        self.session.tool("tilo_read", &json!({"path": path}), None)
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.session.once(action, &args)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        let session = &mut self.session;
        let begun = session.text("tilo_put_begin", &json!({"size": bytes.len()}))?;
        let upload = begun
            .get("upload")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("the workspace began no upload: {begun}"))?;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut offset = 0_usize;
        for chunk in bytes.chunks(PUT_CHUNK) {
            session.text(
                "tilo_put_append",
                &json!({"upload": upload, "offset": offset, "chunk": engine.encode(chunk)}),
            )?;
            offset = offset.saturating_add(chunk.len());
        }
        let finished = session.text("tilo_put_finish", &json!({"upload": upload}))?;
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
