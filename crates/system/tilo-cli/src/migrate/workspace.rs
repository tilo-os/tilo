//! The workspaces a migration from this computer lands in: the one this computer's device is in,
//! through the service `tilo login` leaves running, which holds the device's one session with the
//! workspace for every `tilo mcp` and `tilo` command of this computer ([`crate::service`]), or a
//! hosted one through Tilo's actions, as the key this computer keeps calls them ([`crate::api`]).

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};

use tilo_migrate::engine::writer::{
    ACTION_TRIES, Answer, Call, Invocation, Refused, SAVE_UNCONFIRMED, Status, Workspace,
    refused_of,
};

/// How long one action may take before the run gives up on it.
const ACTION_WAIT: Duration = Duration::from_secs(120);
/// How long a migration waits for this computer's daemon to answer, its service started.
const REACHING: Duration = Duration::from_secs(30);
/// How long a step waits for its person to allow it in their tab.
const PERSON_WAIT: Duration = Duration::from_secs(6 * 60);
/// How long one held read of an invocation waits before the run asks again.
const INVOCATION_WAIT: u64 = 60;
/// The largest piece of an upload one call carries (the session's own bound).
const PUT_CHUNK: usize = 512 * 1024;

/// The workspace through this device's held session.
pub struct Held {
    lines: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
    /// The person's own calls, over this computer's held session as its device: opened the
    /// first time one is asked.
    own: Option<Hosted>,
}

impl Held {
    /// Connect to this computer's peer, starting it (as `tilo folders start` does) when it does
    /// not run.
    ///
    /// # Errors
    /// This computer is not logged in, or its peer could not be reached.
    pub fn connect() -> Result<Self> {
        let login = crate::login::Place::here(crate::identity::paths())?;
        crate::login::served_device_key(login.keys())
            .ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
        crate::login::trust_test_certificates()?;
        // A daemon that is there is waited for, not replaced: it may be busy with a change.
        let stream = tilo_api::daemon::this_computer()
            .and_then(|place| place.reach(REACHING))
            .map_err(anyhow::Error::msg)?;
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
            own: None,
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

impl Workspace for Held {
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
                    let again = error.asks_again() || error.code() == SAVE_UNCONFIRMED;
                    if again && tries < ACTION_TRIES {
                        error.wait_to_ask_again(action, tries);
                        tries = tries.saturating_add(1);
                        continue;
                    }
                    return Err(error.with_action(action).into());
                }
            }
        }
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        // The device's own API, over the session this computer already holds: the person's
        // calls are answered as the person there, never as the agent a migration acts as.
        if self.own.is_none() {
            let workspace = crate::api::entered_workspace(crate::api::Reach::default())?
                .map(|(workspace, _)| workspace)
                .ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
            self.own = Some(Hosted::connect(crate::api::Reach::default(), &workspace)?);
        }
        self.own
            .as_mut()
            .ok_or_else(|| anyhow!("no session of this computer's device"))?
            .person(path, action, args)
    }

    fn read(&mut self, path: &str) -> Result<Value> {
        self.tool("tilo_read", &json!({"path": path}), None)
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

/// The workspace through Tilo's actions, as a key file this computer keeps (`--key-file`) calls
/// them, or this computer's own login through its daemon.
pub struct Hosted {
    client: crate::api::Calls,
    workspace: String,
    runtime: tokio::runtime::Runtime,
}

impl Hosted {
    /// The workspace `named` (an id, or a name its key's account lists), reached by `reach`'s key.
    ///
    /// # Errors
    /// The key reaches no such workspace, or the workspace list could not be read.
    pub fn connect(reach: crate::api::Reach<'_>, named: &str) -> Result<Self> {
        let runtime = runtime()?;
        let client = runtime.block_on(crate::api::Calls::open_with(
            reach,
            crate::mcp::reach_daemon,
        ))?;
        let workspace = runtime.block_on(client.workspace_id(named))?;
        Ok(Self {
            client,
            workspace,
            runtime,
        })
    }

    /// The id `named` names, as `reach`'s key sees it.
    ///
    /// # Errors
    /// The key reaches no such workspace, or the workspace list could not be read.
    pub fn workspace_id(reach: crate::api::Reach<'_>, named: &str) -> Result<String> {
        let runtime = runtime()?;
        runtime.block_on(async {
            crate::api::Calls::open_with(reach, crate::mcp::reach_daemon)
                .await?
                .workspace_id(named)
                .await
        })
    }

    /// `answer` as the answer an action that ran gives.
    fn answered(answer: &Value) -> Answer {
        Answer::of(
            answer
                .get("result")
                .filter(|value| !value.is_null())
                .cloned(),
        )
    }

    /// The answer the pending `answer` names, held while it is pending for the person.
    fn awaited(&self, action: &str, answer: &Value) -> Result<Answer> {
        let invocation = answer
            .get("invocation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{action} is pending but names no invocation"))?
            .to_owned();
        let until = std::time::Instant::now()
            .checked_add(PERSON_WAIT)
            .ok_or_else(|| anyhow!("no time to wait for the person in"))?;
        loop {
            let answer = self
                .runtime
                .block_on(self.client.invocation(
                    Some(&self.workspace),
                    &invocation,
                    INVOCATION_WAIT,
                ))
                .map_err(|error| Self::refused(error, action))?;
            match status(&answer) {
                "ok" => return Ok(Self::answered(&answer)),
                "pending" => {
                    if std::time::Instant::now() >= until {
                        bail!(
                            "{action} is waiting for the person to allow it in their Tilo. Allow it \
                             there, then run the same command again; what it did so far is kept."
                        );
                    }
                }
                other => bail!("{action} ended {other:?}"),
            }
        }
    }

    /// `error` as the refusal the run decides on, naming `action`.
    fn refused(error: anyhow::Error, action: &str) -> anyhow::Error {
        match crate::api::Refusal::of(&error) {
            Some(refusal) => Refused {
                code: refusal.code().to_owned(),
                message: error.to_string(),
                retry_after_ms: refusal.retry_after_ms(),
            }
            .with_action(action)
            .into(),
            None => error,
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

/// How the call `answer` says it went.
fn status(answer: &Value) -> &str {
    answer
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `action` as the resource it runs on and the verb, which the API's route takes apart.
fn split(action: &str) -> Result<(&str, &str)> {
    action.rsplit_once('#').ok_or_else(|| {
        anyhow!("{action} is no action: name it as the resource and the action, `resource#action`")
    })
}

impl Workspace for Hosted {
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        let (resource, verb) = split(action)?;
        // The same step of the same run names the same invocation, which the workspace never runs
        // twice: a step asked again after its answer was lost is answered, not repeated.
        let step = step_id(&run.id, action, &args);
        let mut tries = 0_u32;
        loop {
            let answer = match self.runtime.block_on(self.client.act_invoked(
                &self.workspace,
                resource,
                verb,
                &args,
                (&run.id, &run.title, &step),
            )) {
                Ok(answer) => answer,
                Err(error) => {
                    let error = Self::refused(error, action);
                    let again = Refused::of(&error).is_some_and(|refusal| {
                        let asks_again = refusal.asks_again() || refusal.code() == SAVE_UNCONFIRMED;
                        if asks_again && tries < ACTION_TRIES {
                            refusal.wait_to_ask_again(action, tries);
                            true
                        } else {
                            false
                        }
                    });
                    if again {
                        tries = tries.saturating_add(1);
                        continue;
                    }
                    return Err(error);
                }
            };
            match status(&answer) {
                "ok" => return Ok(Self::answered(&answer)),
                "pending" => return self.awaited(action, &answer),
                other => bail!("{action} answered {other:?}"),
            }
        }
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        let (resource, verb) = split(action)?;
        let answer = self
            .runtime
            .block_on(
                self.client
                    .act(Some(&self.workspace), resource, verb, &args, action),
            )
            .map_err(|error| Self::refused(error, action))?;
        match status(&answer) {
            "ok" => Ok(Self::answered(&answer)),
            "pending" => bail!("{action} did not finish in time"),
            other => bail!("{action} ended {other:?}"),
        }
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        let named = format!("{path}#{action}");
        // Asked once: a sign-in the server refused is not tried again unasked.
        let answer = self
            .runtime
            .block_on(
                self.client
                    .act(Some(&self.workspace), path, action, &args, &named),
            )
            .map_err(|error| Self::refused(error, &named))?;
        match status(&answer) {
            "ok" => Ok(answer.get("result").cloned().unwrap_or(Value::Null)),
            other => bail!("{named} ended {other:?}"),
        }
    }

    fn put_files(&mut self, files: &[(std::path::PathBuf, String)]) -> Result<Vec<String>> {
        let uploading = files
            .iter()
            .map(|(path, name)| crate::hosted::Uploading {
                path: path.clone(),
                name: name.clone(),
            })
            .collect();
        let uploaded = self.runtime.block_on(crate::hosted::upload_all(
            &self.client,
            &self.workspace,
            uploading,
        ))?;
        Ok(uploaded.into_iter().map(|(_, content)| content).collect())
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        let answer = self
            .runtime
            .block_on(self.client.upload(&self.workspace, bytes.to_vec()))
            .map_err(|error| Self::refused(error, "the upload"))?;
        let reference = answer
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("the workspace kept the upload under no reference: {answer}"))?;
        if !reference.starts_with("tilo.content:") {
            bail!("the workspace kept the upload under no reference: {answer}");
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

    fn run() -> Invocation {
        Invocation {
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
                "error": {"code": tilo_actions::codes::NOTE_ALREADY_IMPORTED, "message": "kept"},
            }, "isError": true}),
        ]);
        let error = workspace
            .act(
                &run(),
                "notes/notes.pages#import-markdown",
                json!({"url": "https://example.test/"}),
            )
            .expect_err("the workspace refused");
        assert_eq!(
            Refused::of(&error).expect("a refusal").code(),
            tilo_actions::codes::NOTE_ALREADY_IMPORTED
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
