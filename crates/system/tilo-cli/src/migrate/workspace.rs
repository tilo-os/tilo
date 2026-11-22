//! Where a migration lands: the person's workspace, reached as this computer's device or
//! through Tilo's API.
//!
//! A migration is the agent on this computer acting for the person, so it goes the agent's way
//! in: a connection to this computer's peer, the service `tilo login` leaves running, which
//! holds the device's one session with the workspace for every `tilo mcp` and `tilo` command
//! of this computer (`tilo-runtime`, [`crate::service`]), or, for a hosted workspace, the
//! API with the key this computer holds ([`crate::api`]). Either way it asks the workspace's apps
//! to do their own ordinary actions. Every action of one run names that run, so History shows the
//! run as one entry under the agent's name, the person sees each record arrive in their tab, and
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
/// How long one held read of an invocation waits before the run asks again.
const INVOCATION_WAIT: u64 = 60;
/// How often a step the workspace said to ask again is asked again before the run gives up: each
/// ask waits for the workspace itself ([`ACTION_WAIT`]), or for the wait it said.
const ACTION_TRIES: u32 = 7;
/// The first pause before a step the workspace said to ask again is asked again, when it named
/// none: it doubles with each ask, up to [`ASK_AGAIN_MOST`].
const ASK_AGAIN_FIRST: Duration = Duration::from_secs(1);
/// The longest pause between two asks of one step.
const ASK_AGAIN_MOST: Duration = Duration::from_secs(30);
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
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer>;

    /// Run `action` with `args` outside any run: a read, or taking a run back.
    ///
    /// # Errors
    /// The workspace refused it ([`Refused`], by its code) or could not be reached.
    fn ask(&mut self, action: &str, args: Value) -> Result<Answer>;

    /// Read `path` (`record:<app>:<id>`) as its typed answer.
    ///
    /// # Errors
    /// The workspace refused it or could not be reached.
    fn read(&mut self, path: &str) -> Result<Value> {
        bail!("{path}: this migration reads nothing of the workspace")
    }

    /// Ask `action` of `path` (`workspace/mail-accounts`) on the workspace's
    /// server, as the person on their own device rather than as their agent: what only they may
    /// do, such as keeping a mail account and its sign-in. Its answer, or why it was refused.
    ///
    /// # Errors
    /// The server refused it ([`Refused`], by its code) or could not be reached.
    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        let _ = args;
        bail!("{path}#{action}: this migration reaches no server")
    }

    /// Keep `bytes` in the workspace's content; the `tilo.content:` reference actions name them
    /// by.
    ///
    /// # Errors
    /// The workspace refused them or could not be reached.
    fn put(&mut self, bytes: &[u8]) -> Result<String>;

    /// Keep the files at `files` (a path here and the name each keeps) in the workspace's
    /// content: each one's `tilo.content:` reference, in order.
    ///
    /// # Errors
    /// A file could not be read, or the workspace refused it.
    fn put_files(&mut self, files: &[(std::path::PathBuf, String)]) -> Result<Vec<String>> {
        files
            .iter()
            .map(|(path, _)| {
                let bytes =
                    std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
                self.put(&bytes)
            })
            .collect()
    }
}

/// No workspace: a run whose files go into the workspace's folder here needs no session, and
/// asks it nothing.
pub struct Offline;

impl Workspace for Offline {
    fn act(&mut self, _: &Invocation, action: &str, _: Value) -> Result<Answer> {
        bail!("{action}: this run writes into the workspace's folder and reaches no session")
    }

    fn ask(&mut self, action: &str, _: Value) -> Result<Answer> {
        bail!("{action}: this run writes into the workspace's folder and reaches no session")
    }

    fn put(&mut self, _: &[u8]) -> Result<String> {
        bail!("this run writes into the workspace's folder and reaches no session")
    }
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

    /// Whether the action answered `true` under `field`.
    #[cfg(test)]
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
pub struct Invocation {
    pub id: String,
    pub title: String,
}

impl Invocation {
    /// The `_meta` an action of this run carries: the typed answer it asks for, and the group it is
    /// a step of.
    fn meta(&self) -> Value {
        json!({"so.tilo.mcp": {
            "structured": true,
            "invocation": {"id": self.id, "title": self.title},
        }})
    }
}

/// The id one step of a run is named by: the same step asked again is the same invocation, which a
/// workspace runs once.
pub fn step_id(run: &str, action: &str, args: &Value) -> String {
    let step = blake3::hash(format!("{run}\u{0}{action}\u{0}{args}").as_bytes());
    uuid::Builder::from_random_bytes(
        step.as_bytes()
            .get(..16)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or([0; 16]),
    )
    .into_uuid()
    .to_string()
}

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
        let device = crate::login::load_public(&login)?
            .ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
        crate::login::trust_test_certificates()?;
        let place = tilo_api::daemon::DaemonPlace::of(login.dir(), &device.cert.device_public_key)
            .map_err(|why| anyhow!(why))?;
        let stream = if let Some(stream) = place.connect() {
            stream
        } else {
            // A peer that is there is waited for, not replaced: it may be busy with a change, and
            // a connection that failed once is not a reason to stop it.
            crate::service::start()?;
            place.connect().ok_or_else(|| {
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

    /// The app's stable code for why it did not run (`note_already_imported`).
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

    /// Whether the workspace said the same step works when asked again: it was not serving yet, or
    /// did not answer. Asked again under its same id, a step runs once.
    pub(super) fn asks_again(&self) -> bool {
        matches!(self.code.as_str(), "not_ready" | "unanswered")
    }

    /// Say that `action` is asked again, and wait first: as long as the workspace said, or a pause
    /// that doubles with each of its `tries` so far. Asked again at once and in silence, an import
    /// into a workspace still catching up after its server restarted queued ask after ask behind
    /// the work it waited for, and showed nothing while it did.
    fn wait_to_ask_again(&self, action: &str, tries: u32) {
        let wait = self.retry_after_ms.map_or_else(
            || {
                ASK_AGAIN_FIRST
                    .saturating_mul(2_u32.saturating_pow(tries))
                    .min(ASK_AGAIN_MOST)
            },
            Duration::from_millis,
        );
        eprintln!(
            "{action}: {} Asking again in {} s ({} of {ACTION_TRIES}).",
            self.message.trim(),
            wait.as_secs().max(1),
            tries.saturating_add(1)
        );
        std::thread::sleep(wait);
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
                    let again = error.asks_again() || error.code == SAVE_UNCONFIRMED;
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
            let workspace =
                crate::login::load_public(&crate::login::Place::here(crate::identity::paths())?)?
                    .map(|device| device.workspace)
                    .ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
            self.own = Some(Hosted::connect(
                crate::api::Reach {
                    api: None,
                    key_file: None,
                    json: false,
                },
                &workspace,
            )?);
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

/// The workspace's accounts, as `workspace` reads their record.
///
/// # Errors
/// The record could not be read, or this computer does not hold it yet.
pub fn accounts(workspace: &mut dyn Workspace) -> Result<accounts::person::AccountList> {
    let path = format!(
        "record:{}:{}",
        accounts::person::ACCOUNTS_OWNER,
        collab_types::workspace_document_id(accounts::person::AccountList::kind())
    );
    let read = workspace.read(&path)?;
    let record = read
        .get("records")
        .and_then(Value::as_array)
        .and_then(|records| records.first())
        .ok_or_else(|| anyhow!("the workspace answered no accounts: {read}"))?;
    if record.get("absent").and_then(Value::as_bool) == Some(true) {
        return Ok(accounts::person::AccountList::default());
    }
    let value = record.get("value").ok_or_else(|| {
        anyhow!("the workspace's accounts are still arriving on this computer; try again shortly")
    })?;
    accounts::person::AccountList::from_readable(value).map_err(|error| anyhow!(error))
}

/// Make the edit `edit` (its address and arguments) of the workspace's accounts in `workspace`:
/// the action Settings' own controls make.
///
/// # Errors
/// The workspace refused it or could not be reached.
pub fn change_accounts(
    workspace: &mut dyn Workspace,
    (address, arguments): (
        &tilo_actions::ActionAddress,
        &serde_json::Map<String, Value>,
    ),
) -> Result<()> {
    let action = format!("{}/{}", SETTINGS_PATH, address.encode());
    workspace.ask(&action, Value::Object(arguments.clone()))?;
    Ok(())
}

/// Where the agent's paths name Settings, which keeps the workspace's accounts.
const SETTINGS_PATH: &str = "settings";

/// The workspace through Tilo's API, with the key this computer holds: a hosted workspace reached
/// over HTTPS (`TILO_API_KEY`, and the server `--api` names), or in process as this computer's own
/// key (`--key-file`, or the one `tilo login` kept).
pub struct Hosted {
    client: crate::api::Client,
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
        let client = crate::api::Client::open(reach)?;
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
        let client = crate::api::Client::open(reach)?;
        runtime.block_on(client.workspace_id(named))
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
                .block_on(
                    self.client
                        .invocation(&self.workspace, &invocation, INVOCATION_WAIT),
                )
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
                &run.id,
                &run.title,
                &step,
            )) {
                Ok(answer) => answer,
                Err(error) => {
                    let error = Self::refused(error, action);
                    let again = Refused::of(&error).is_some_and(|refusal| {
                        let asks_again = refusal.asks_again() || refusal.code == SAVE_UNCONFIRMED;
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
                    .act(&self.workspace, resource, verb, &args, action),
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
                    .act(&self.workspace, path, action, &args, &named),
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

    const WORKSPACE: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

    /// The API a hosted run reaches, as one key of a person's.
    fn keyed() -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(crate::api::KEY_ENV, "tilo_sk_test");
        env
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "every route answers, and the handler's shape is an option"
    )]
    fn reply(status: u16, body: &Value) -> Option<test_http::Reply> {
        Some(
            test_http::Reply::status(status)
                .header("content-type", "application/json")
                .body(body.to_string()),
        )
    }

    fn hosted(server: &test_http::Responder) -> Hosted {
        let url = server.url();
        Hosted::connect(
            crate::api::Reach {
                api: Some(&url),
                key_file: None,
                json: false,
            },
            "Studio",
        )
        .expect("the workspace")
    }

    fn hosted_run() -> Invocation {
        Invocation {
            id: "migrate-linear-1".into(),
            title: "From Linear: Engineering".into(),
        }
    }

    #[test]
    fn a_hosted_step_is_one_invocation_under_its_own_idempotency_key() {
        let _key = keyed();
        let server = test_http::Responder::handle(|request: &test_http::Request| {
            match (request.method.as_str(), request.target.as_str()) {
                ("POST", "/v1/resources/account/workspaces/actions/list") => reply(
                    200,
                    &json!({"result": {"workspaces": [{"id": WORKSPACE, "name": "Studio"}]}}),
                ),
                ("POST", route) if route.ends_with("/uploads") => {
                    reply(201, &json!({"content": "tilo.content:abc"}))
                }
                ("POST", _) => reply(
                    200,
                    &json!({"status": "ok", "result": {"taken": true},
                        "resources": ["tasks.tracker/issues/1"]}),
                ),
                _ => reply(
                    404,
                    &json!({"error": {"code": "not_found", "message": "no such route"}}),
                ),
            }
        });
        let mut workspace = hosted(&server);
        let args = json!({"title": "x"});
        let action = "tasks/tasks.tracker#create";
        let answer = workspace
            .act(&hosted_run(), action, args.clone())
            .expect("the workspace took it");
        assert!(answer.said("taken"));
        assert_eq!(
            answer.result().and_then(|result| result.get("taken")),
            Some(&json!(true)),
            "the action's value is what it answered"
        );

        let sent = server.drain_requests();
        assert_eq!(
            sent.len(),
            2,
            "the workspace is named, then the action runs"
        );
        let step = &sent[1];
        assert_eq!(
            step.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/tasks/tasks.tracker/actions/create")
        );
        assert_eq!(step.header("tilo-invocation"), Some("migrate-linear-1"));
        assert_eq!(
            step.header("tilo-invocation-title"),
            Some("From Linear: Engineering")
        );
        let key = step.header("idempotency-key").expect("a step id");
        assert_eq!(key, step_id("migrate-linear-1", action, &args).as_str());
        assert_eq!(step.header("prefer"), Some("wait=60"));

        // The same step asked again goes under the same key, so the workspace runs it once.
        workspace
            .act(&hosted_run(), action, args)
            .expect("the workspace answered the same step");
        let sent = server.drain_requests();
        assert_eq!(
            sent.last()
                .and_then(|again| again.header("idempotency-key")),
            Some(key)
        );

        // An upload names the content the workspace kept it under.
        assert_eq!(
            workspace.put(b"hello").expect("it is kept"),
            "tilo.content:abc"
        );
    }

    #[test]
    fn a_step_the_person_has_to_allow_is_waited_on_at_its_invocation() {
        let _key = keyed();
        let server = test_http::Responder::handle(|request: &test_http::Request| {
            match (request.method.as_str(), request.target.as_str()) {
                ("POST", "/v1/resources/account/workspaces/actions/list") => reply(
                    200,
                    &json!({"result": {"workspaces": [{"id": WORKSPACE, "name": "Studio"}]}}),
                ),
                ("POST", route) if route.contains("/actions/") => reply(
                    202,
                    &json!({"status": "pending", "invocation": "step-1",
                        "ask": {"url": "https://tilo.test/ask/step-1"}}),
                ),
                ("GET", route)
                    if route.starts_with("/v1/workspaces/")
                        && route.contains("/invocations/step-1") =>
                {
                    reply(200, &json!({"status": "ok", "result": {"allowed": true}}))
                }
                _ => reply(
                    404,
                    &json!({"error": {"code": "not_found", "message": "no such route"}}),
                ),
            }
        });
        let mut workspace = hosted(&server);
        let answer = workspace
            .act(
                &hosted_run(),
                "notes/notes.pages#import-markdown",
                json!({"url": "https://example.test/"}),
            )
            .expect("the person allowed it");
        assert!(answer.said("allowed"), "the held answer is the action's");

        let sent = server.drain_requests();
        let waited = sent.last().expect("the invocation is asked about");
        assert_eq!(waited.method, "GET");
        assert_eq!(
            waited.target,
            format!("/v1/workspaces/{WORKSPACE}/invocations/step-1?wait={INVOCATION_WAIT}")
        );
    }

    #[test]
    fn undoing_a_run_asks_the_api_and_takes_back_its_step() {
        let _key = keyed();
        let server = test_http::Responder::handle(|request: &test_http::Request| {
            match (request.method.as_str(), request.target.as_str()) {
                ("POST", "/v1/resources/account/workspaces/actions/list") => reply(
                    200,
                    &json!({"result": {"workspaces": [{"id": WORKSPACE, "name": "Studio"}]}}),
                ),
                ("POST", route) if route.ends_with("/actions/history") => reply(
                    200,
                    &json!({"status": "ok", "result": {"versions": [
                        {"change": "c1",
                         "actor": {"id": "agent:tilo", "declared": "migrate-linear-1"}},
                    ]}}),
                ),
                ("POST", route) if route.ends_with("/actions/undo") => {
                    reply(200, &json!({"status": "ok", "result": {}}))
                }
                _ => reply(
                    404,
                    &json!({"error": {"code": "not_found", "message": "no such route"}}),
                ),
            }
        });
        let mut workspace = hosted(&server);
        assert_eq!(
            super::super::undo_in(&mut workspace, "tasks/tasks.tracker", "migrate-linear-1")
                .expect("the record answered"),
            super::super::RunInHistory::InEffect("c1".to_owned())
        );
        let sent = server.drain_requests();
        let undo = sent.last().expect("the undo runs in the record");
        assert_eq!(
            undo.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/tasks/tasks.tracker/actions/undo")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&undo.body).expect("the step"),
            json!({"step": "c1"})
        );
    }
}
