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
/// How long a step waits for its person to allow it.
const PERSON_WAIT: Duration = Duration::from_secs(6 * 60);
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
    fn once(&mut self, action: &str, args: Value) -> Result<Answer> {
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

/// The workspace through the migration's session with it, and the person's for their own calls.
pub struct Mcp {
    session: Session,
    /// What opens the person's own session, the first time one of their calls is made.
    own: Option<Box<dyn FnOnce() -> Result<Box<dyn Rpc>> + Send>>,
    person: Option<Session>,
}

impl Mcp {
    /// The workspace through `rpc`, the migration's session; the person's own calls go through
    /// the session `own` opens.
    ///
    /// # Errors
    /// The migration's session did not open.
    pub fn open(
        rpc: Box<dyn Rpc>,
        own: Box<dyn FnOnce() -> Result<Box<dyn Rpc>> + Send>,
    ) -> Result<Self> {
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
            let call = self.session.call(&call, Some(run.meta()))?;
            match call.status {
                Status::Ok => return Ok(call.answer()),
                // An action the person allows first is pending while they are asked: the same
                // call under the same id waits for it to move, until their answer.
                Status::Pending => {
                    if std::time::Instant::now() >= until {
                        bail!(
                            "{action} is waiting for the person to allow it. Allow it, then start \
                             the migration again; what it did so far is kept."
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
        // Asked once: a sign-in the workspace refused is not tried again unasked.
        let answer = person.once(&format!("{path}#{action}"), args)?;
        Ok(answer.result().cloned().unwrap_or(Value::Null))
    }

    fn read(&mut self, path: &str) -> Result<Value> {
        self.session.tool("tilo_read", &json!({"path": path}), None)
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.session.once(action, args)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The workspace's end of a session: it records each call and answers the next of `answers`.
    struct Scripted {
        calls: Arc<Mutex<Vec<Value>>>,
        answers: std::vec::IntoIter<Value>,
    }

    impl Rpc for Scripted {
        fn exchange(&mut self, request: Value) -> Result<Option<Value>> {
            let Some(id) = request.get("id").cloned() else {
                return Ok(None);
            };
            let answer = if request.get("method").and_then(Value::as_str) == Some("tools/call") {
                self.calls.lock().expect("the calls").push(request);
                self.answers
                    .next()
                    .unwrap_or_else(|| json!({"content": [], "isError": true}))
            } else {
                json!({})
            };
            Ok(Some(json!({"jsonrpc": "2.0", "id": id, "result": answer})))
        }
    }

    /// The workspace reached through a session the test answers, and the calls it made.
    fn reached(answers: Vec<Value>) -> (Mcp, Arc<Mutex<Vec<Value>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let rpc = Scripted {
            calls: Arc::clone(&calls),
            answers: answers.into_iter(),
        };
        let mcp = Mcp::open(Box::new(rpc), Box::new(|| Err(anyhow!("no person here"))))
            .expect("the session");
        (mcp, calls)
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
            .act(&run(), "so.tilo.tasks#new_task", json!({"title": "x"}))
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

    #[test]
    fn the_persons_own_calls_go_through_their_own_session_opened_once() {
        let opened = Arc::new(Mutex::new(0_u32));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (own_opened, own_calls) = (Arc::clone(&opened), Arc::clone(&calls));
        let ok = |result: Value| {
            json!({"content": [], "isError": false,
                   "structuredContent": {"status": "ok", "result": result}})
        };
        let answers = vec![
            ok(json!({"accounts": []})),
            ok(json!({"address": "ada@example.com"})),
        ];
        let mut workspace = Mcp::open(
            Box::new(Scripted {
                calls: Arc::default(),
                answers: Vec::new().into_iter(),
            }),
            Box::new(move || {
                *own_opened.lock().expect("opened") += 1;
                Ok(Box::new(Scripted {
                    calls: own_calls,
                    answers: answers.into_iter(),
                }) as Box<dyn Rpc>)
            }),
        )
        .expect("the session");
        workspace
            .person("workspace/mail-accounts", "list", json!({}))
            .expect("listed");
        let added = workspace
            .person(
                "workspace/mail-accounts",
                "add",
                json!({"address": "ada@example.com"}),
            )
            .expect("added");
        assert_eq!(added["address"], "ada@example.com");
        assert_eq!(*opened.lock().expect("opened"), 1);
        let paths: Vec<Value> = calls
            .lock()
            .expect("the calls")
            .iter()
            .filter_map(|call| call.pointer("/params/arguments/path").cloned())
            .collect();
        assert_eq!(
            paths,
            [
                json!("workspace/mail-accounts#list"),
                json!("workspace/mail-accounts#add")
            ]
        );
    }
}
