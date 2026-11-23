//! Where a migration lands: the person's workspace, through its apps' own ordinary actions.
//!
//! A migration is the person's agent on the computer that holds the source, acting for them: it
//! asks the workspace's apps to do their own actions. Every action of one run names that run, so
//! History shows the run as one entry under the agent's name, the person sees each record arrive
//! in their tab, and undoing the entry takes back exactly what the run brought. How the workspace
//! is reached (this computer's daemon, Tilo's API) is the caller's: it hands the engine a
//! [`Workspace`].
//!
//! Every call asks for the typed answer the workspace keeps beside its words, and decides on that:
//! how a call went, the value and the records it answered with, and the code of a refusal. What a
//! call did is never read back out of its text.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

/// How often a step the workspace said to ask again is asked again before the run gives up: each
/// ask waits for the workspace itself, or for the wait it said.
pub const ACTION_TRIES: u32 = 7;
/// The first pause before a step the workspace said to ask again is asked again, when it named
/// none: it doubles with each ask, up to [`ASK_AGAIN_MOST`].
const ASK_AGAIN_FIRST: Duration = Duration::from_secs(1);
/// The longest pause between two asks of one step.
const ASK_AGAIN_MOST: Duration = Duration::from_secs(30);
/// The code of a save the workspace's host did not confirm in time: it may or may not be kept, so
/// the same call under its same id is asked again, which runs it once.
pub const SAVE_UNCONFIRMED: &str = "save_unconfirmed";

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
    #[must_use]
    pub fn of(result: Option<Value>) -> Self {
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
    #[must_use]
    pub fn meta(&self) -> Value {
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

/// How one call went, as its typed answer says.
#[derive(Clone, Debug)]
pub struct Call {
    pub status: Status,
    pub result: Option<Value>,
    pub error: Option<Refused>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// It ran, and answered what it did.
    Ok,
    /// It did not run: its refusal, by code.
    Failed,
    /// It runs still, waiting for its person (to allow it in their tab, say).
    Pending,
}

impl Call {
    /// The answer `typed` says the call gave.
    pub fn of(typed: &Value) -> Result<Self> {
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

    #[must_use]
    pub fn answer(&self) -> Answer {
        Answer::of(self.result.clone())
    }
}

/// The refusal `typed` carries, in the words its code is answered beside.
pub fn refused_of(typed: &Value) -> Refused {
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
    pub code: String,
    pub message: String,
    pub retry_after_ms: Option<u64>,
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
    #[must_use]
    pub fn coded(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            retry_after_ms: None,
        }
    }

    /// The refusal said with the action it names, as a person reads it.
    #[must_use]
    pub fn with_action(mut self, action: &str) -> Self {
        self.message = format!("{action}: {}", self.message.trim());
        self
    }

    /// Whether the workspace said the same step works when asked again: it was not serving yet, or
    /// did not answer. Asked again under its same id, a step runs once.
    #[must_use]
    pub fn asks_again(&self) -> bool {
        matches!(self.code.as_str(), "not_ready" | "unanswered")
    }

    /// Say that `action` is asked again, and wait first: as long as the workspace said, or a pause
    /// that doubles with each of its `tries` so far. Asked again at once and in silence, an import
    /// into a workspace still catching up after its server restarted queued ask after ask behind
    /// the work it waited for, and showed nothing while it did.
    pub fn wait_to_ask_again(&self, action: &str, tries: u32) {
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
