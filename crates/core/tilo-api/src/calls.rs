//! Tilo's actions, as a program of the person's on this computer calls them: `tilo`'s workspace
//! commands, `tilolang repl`, a model ask, `tilolang push`. Every call is a tool of the agent
//! surface ([`tilo_mcp::McpServer`]), walked in the caller's own process, over whichever actions
//! the caller reaches ([`Calls::open`]):
//!
//! - **This computer**: the daemon's socket ([`daemon_socket`]), whose action
//!   frames a [`LineClient`] carries. The daemon decides every call as the person whose process
//!   connects, and keeps the device's one session with its host, so no program here opens a
//!   second one. It serves the workspace this computer logged in to, or the one it founded.
//! - **A key file** (`--key-file`, `TILO_KEY_FILE`): the key enters its host's session as itself
//!   ([`crate::direct::Direct`]), and the same walk runs there.
//!
//! A program inside a Tilo desktop reaches the same daemon at the same socket: the desktop is a
//! view of the daemon's device, never a second way in.
//!
//! A call the workspace was not ready for, or left unanswered, is asked again for a while; a call
//! that ran is never run twice, since an action is invoked under an id of its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_action_line::LineClient;
use tilo_action_line::handed::WORKSPACE_ENV;
use tilo_mcp::McpServer;

mod model_ask;

pub use model_ask::ask_model;
pub(crate) use model_ask::{follow, start_ask};

use crate::session::Provider;
use crate::v1::answers;
use crate::v1::errors::ApiError;

/// The account's declared actions, and the records their answers read as.
pub mod account_actions {
    tilo_resource_macros::bindings!(provider = "tilo.account", surface = "account-surface.json");
}

/// The workspace's vault, as the service that keeps it publishes its actions.
mod vault_actions {
    tilo_resource_macros::bindings!(
        provider = "tilo.vault",
        surface = "../workspace-records/vault-surface.json"
    );
}
use vault_actions::vault;

/// Names a key file a command acts as, as `--key-file` does.
pub const KEY_FILE_ENV: &str = "TILO_KEY_FILE";

/// How long a call the workspace was not ready for is asked again: a workspace that does not
/// serve this computer yet is on its way.
const READY_DEADLINE: Duration = Duration::from_secs(300);
/// How long an action waits for the person who is asked to allow it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// How long between asks.
const PAUSE: Duration = Duration::from_secs(1);
/// How long one call of an action waits for it to end before it is followed.
pub const WAIT_MS: u64 = 60_000;
/// The most bytes one put of many files carries.
const PUT_BYTES: usize = 384 * 1024;
/// The most files one put carries.
const PUT_FILES: usize = 256;
/// The most hashes one look asks about.
const HAVE_HASHES: usize = 1024;
/// How many many-files puts go at once. A workspace answers them one after another, so more in
/// flight only queue: the oldest waits a step for every call ahead of it.
const PUTS_AT_ONCE: usize = 4;
/// How many files too large for a many-files put go up at once.
const LARGE_AT_ONCE: usize = 32;
/// How many pieces of an upload in a row may fail before it gives up. Each failure goes on from
/// where the daemon says the upload stands, on a connection of its own, so no byte is sent twice.
const PIECE_TRIES: usize = 5;

/// What a command reaches Tilo with, as the person named it.
#[derive(Clone, Copy, Default)]
pub struct Reach<'a> {
    /// A key file this computer keeps, like `--key-file`.
    pub key_file: Option<&'a Path>,
    /// Whether the answer is JSON, typed as the tools give it.
    pub json: bool,
}

impl Reach<'_> {
    /// Whether these calls act as this computer: no key is named, or the key named is this
    /// computer's own device key, which only the daemon enters with.
    ///
    /// # Errors
    /// The key file cannot be read, or, with no key named, the login is damaged.
    pub fn as_device(self) -> Result<bool> {
        crate::direct::names_this_device(self.key_file, None)
    }
}

/// The workspace this computer's device enters through, which `-w` means when nobody named one:
/// its id, and the name the device keeps for it, as this computer's daemon says
/// ([`crate::daemon::who::home`]). `None` when these calls name a key file, which is someone
/// else's key, or no daemon of this computer's knows where its device enters.
///
/// # Errors
/// None: a daemon that does not answer knows no workspace here, which is `None`.
pub fn entered_workspace(reach: Reach<'_>) -> Result<Option<(String, String)>> {
    if reach.key_file.is_some() {
        return Ok(None);
    }
    Ok(crate::daemon::who::home().ok().flatten().map(|home| {
        let name = home
            .workspace_name
            .clone()
            .unwrap_or_else(|| home.workspace.clone());
        (home.workspace, name)
    }))
}

/// This computer's device key and certificates, where its session keeps them: what its login
/// is read with ([`tilo_device_identity::login::Place::here`]).
#[must_use]
pub fn device_keys() -> tilo_device_identity::IdentityPaths {
    tilo_identity::provisioning::collaboration_identity_paths()
}

/// A call that was refused: the code it answered and its words.
#[derive(Clone, Debug)]
pub struct Refusal {
    code: String,
    message: String,
}

impl Refusal {
    /// The refusal among `error`'s causes, when it was refused.
    #[must_use]
    pub fn of(error: &anyhow::Error) -> Option<&Self> {
        error.downcast_ref()
    }

    /// The stable code for why it was refused.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// `error` in the words a person at a command reads: its message, its code, and the calls
    /// that would work instead.
    fn said(error: &ApiError) -> Self {
        let body = &error.body;
        let mut said = format!("{} ({})", body.message, body.code);
        for step in &body.next_steps {
            said.push_str("\n  ");
            said.push_str(step);
        }
        // The tools name themselves and their own routes; a person at a command types neither.
        Self {
            code: body.code.clone(),
            message: crate::v1::names::said(&said, crate::v1::names::Surface::Cli),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// What the daemon's API answered one request with: its status and headers, and its body.
pub struct ApiAnswer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    body: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Result<Option<Vec<u8>>, String>>>,
}

impl ApiAnswer {
    /// The next piece of the body; `None` once it ended.
    ///
    /// # Errors
    /// The connection closed before the body ended.
    pub async fn next(&self) -> Result<Option<Vec<u8>>, String> {
        self.body.lock().await.recv().await.unwrap_or(Ok(None))
    }
}

/// Tilo's actions, as one caller reaches them.
pub struct Calls {
    at: At,
    /// Whether this is an app's connection: the daemon started the process and handed it the
    /// one connection it acts on. An app reaches `vault#web-call` on that connection, as the
    /// process whose held connection it is.
    app: bool,
    /// Who composes a message where, as the workspace's live session told this app's connection
    /// ([`Self::composing_at`]).
    composers: std::sync::Mutex<collab_types::presence::Composers<Instant>>,
}

enum At {
    /// This computer's daemon, serving `workspace` to `person` over `line`; `again` when a lost
    /// connection is made again at its socket. The same line reaches every other workspace the
    /// device holds, each call naming its own: `elsewhere` keeps the server of each one called.
    Daemon {
        server: Box<McpServer<Provider>>,
        line: Arc<LineClient>,
        workspace: String,
        person: String,
        again: bool,
        elsewhere: Elsewhere,
    },
    /// A key's own session with its host, and the program session each workspace's programs
    /// continue.
    Key(
        crate::direct::Direct,
        tokio::sync::Mutex<std::collections::HashMap<String, String>>,
    ),
}

/// The servers of the workspaces besides its own a daemon's calls reached, by workspace.
type Elsewhere = std::sync::Mutex<std::collections::HashMap<String, Arc<McpServer<Provider>>>>;

impl Calls {
    /// The actions `reach` names, this computer's daemon reached at its socket when one listens
    /// there ([`Self::open_with`]).
    ///
    /// # Errors
    /// The key file cannot be read or is refused, or no daemon listens.
    pub async fn open(reach: Reach<'_>) -> Result<Self> {
        Self::open_with(reach, || {
            let socket = daemon_socket()?;
            tilo_files::native::socket::connect(&socket).map_err(|error| {
                format!(
                    "this computer's daemon does not answer at {} ({error}): run `tilo status` to \
                     start it",
                    socket.display()
                )
            })
        })
        .await
    }

    /// The actions `reach` names: the key file's own session when it names a key other than this computer's; else this
    /// computer's daemon, connected to by `daemon` (which starts one when none listens).
    ///
    /// # Errors
    /// The key file cannot be read or is refused, or the daemon cannot be reached.
    pub async fn open_with(
        reach: Reach<'_>,
        daemon: impl FnOnce() -> Result<tilo_files::native::socket::Stream, String>,
    ) -> Result<Self> {
        if let Some(path) = reach.key_file
            && !reach.as_device()?
        {
            let file = crate::direct::KeyFile::read(path).map_err(anyhow::Error::msg)?;
            let direct = crate::direct::Direct::new(&file)
                .map_err(|why| anyhow!("the key for {}: {why}", file.host))?;
            return Ok(Self {
                at: At::Key(direct, tokio::sync::Mutex::default()),
                app: false,
                composers: std::sync::Mutex::default(),
            });
        }
        let stream = daemon().map_err(anyhow::Error::msg)?;
        let mut calls = Self::over(Arc::new(LineClient::of_stream(stream)?)).await?;
        if let At::Daemon { again, .. } = &mut calls.at {
            *again = true;
        }
        Ok(calls)
    }

    /// The actions `provider` reaches, this computer's daemon's: the workspace its login entered,
    /// or, with no login, the one the daemon founded.
    ///
    /// # Errors
    /// The daemon serves no workspace.
    pub async fn over(line: Arc<LineClient>) -> Result<Self> {
        let provider: Provider = line.clone();
        // Who this account is to the daemon, and where its device enters: none while it
        // serves no person yet, when the workspace it founded is the one.
        let who = crate::daemon::who::who().ok();
        let person = who
            .as_ref()
            .and_then(|who| who.person)
            .map(hex::encode)
            .unwrap_or_default();
        let workspace = match who.and_then(|who| who.home) {
            Some(home) => home.workspace,
            None => founded(&provider)
                .await?
                .pointer("/workspaces/0/id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    anyhow!("this computer's daemon serves no workspace to log in to")
                })?,
        };
        Ok(Self {
            at: At::Daemon {
                server: Box::new(served(provider, &workspace, &person)),
                line,
                workspace,
                person,
                again: false,
                elsewhere: Elsewhere::default(),
            },
            app: false,
            composers: std::sync::Mutex::default(),
        })
    }

    /// A program's connection to this computer's daemon, as the SDKs make it: the stream the
    /// launcher connected for an app it started ([`NATIVE_FD_ENV`](tilo_action_line::handed::NATIVE_FD_ENV), admitted as that app in the
    /// workspace [`WORKSPACE_ENV`] names), else `socket`, else this computer's daemon
    /// ([`daemon_socket`]), admitted as the account connecting. `workspace` names the one
    /// workspace the connection acts in when it knows it; without it, an app's comes from
    /// [`WORKSPACE_ENV`] and a person's daemon is asked where this account enters.
    ///
    /// # Errors
    /// No daemon answers, or it serves this account no workspace.
    pub async fn connect(socket: Option<&Path>, workspace: Option<&str>) -> Result<Self> {
        let handed = (socket.is_none() && tilo_action_line::handed::was_handed())
            .then(|| {
                tilo_action_line::handed::connection().ok_or_else(|| {
                    anyhow!(
                        "this app's connection to the daemon could not be opened, or names no \
                     workspace in {WORKSPACE_ENV}"
                    )
                })
            })
            .transpose()?;
        let (line, workspace, app) = match handed {
            Some(handed) => (
                handed.line,
                Some(workspace.map_or(handed.workspace, str::to_owned)),
                true,
            ),
            None => (
                Arc::new(LineClient::of_stream(connected(socket)?)?),
                workspace.map(str::to_owned),
                false,
            ),
        };
        let mut calls = match workspace {
            Some(workspace) => Self::in_workspace(line, &workspace),
            None => Self::over(line).await?,
        };
        calls.app = app;
        Ok(calls)
    }

    /// The actions `line` reaches in `workspace`, which the connection already knows it acts in:
    /// an app the daemon started, admitted in the one workspace it was started in, which asks the
    /// daemon nothing but frames. Its calls name no person: the daemon decides who they are by
    /// the connection. A connection that is an app's is marked so by [`Self::connect`].
    #[must_use]
    pub fn in_workspace(line: Arc<LineClient>, workspace: &str) -> Self {
        let provider: Provider = line.clone();
        Self {
            at: At::Daemon {
                server: Box::new(served(provider, workspace, "")),
                line,
                workspace: workspace.to_owned(),
                person: String::new(),
                again: false,
                elsewhere: Elsewhere::default(),
            },
            app: false,
            composers: std::sync::Mutex::default(),
        }
    }

    /// The workspace this computer's daemon serves, for calls that reach it.
    #[must_use]
    pub fn served_workspace(&self) -> Option<&str> {
        match &self.at {
            At::Daemon { workspace, .. } => Some(workspace),
            At::Key(..) => None,
        }
    }

    /// Call `tool` with `arguments` in `workspace` (`None`: the account's own tree), as a step
    /// of `group` (`{id, title}`, which History groups the call under) when one is named: its
    /// typed answer. A call the workspace was not ready for, or left unanswered, is asked again
    /// for a while.
    ///
    /// # Errors
    /// The call was refused ([`Refusal`]), or could not be made.
    pub async fn typed(
        &self,
        workspace: Option<&str>,
        tool: &str,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<Value> {
        self.answered(workspace, tool, arguments, group)
            .await
            .map_err(|refused| anyhow::Error::new(Refusal::said(&refused)))
    }

    /// [`Self::typed`], its refusal answered as a program reads one: `{"error": {"code",
    /// "message", "next_steps", "retry_after_ms", ...}}`, the shape every SDK raises its typed
    /// refusals from.
    ///
    /// # Errors
    /// The call was refused, or could not be made: its refusal's body.
    pub async fn call(
        &self,
        workspace: Option<&str>,
        tool: &str,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<Value, Value> {
        self.answered(workspace, tool, arguments, group)
            .await
            .map_err(|refused| refused.json())
    }

    /// Make the web call `arguments` names with the key the vault item it names keeps, as this
    /// connection acts in `workspace` (`None`: the one it acts in). An app's held connection is
    /// answered for the process the daemon started: the vault makes the request itself, so the
    /// secret never reaches the app, and the app's first call for a service asks its person
    /// once, per app, item and place. A connection that is no app's makes the same call as the
    /// `tilo_do` tool, which is how a program of the person's, or their agent's, reaches it.
    /// The answer is the action's typed answer, with the service's status, headers and body.
    ///
    /// # Errors
    /// The call was refused, or could not be made: its refusal's body.
    pub async fn web_call(
        &self,
        workspace: Option<&str>,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<Value, Value> {
        if !self.app {
            let mut tool = serde_json::Map::new();
            tool.insert(
                "path".into(),
                Value::String("vault/vault#web-call".to_owned()),
            );
            tool.insert("args".into(), Value::Object(arguments));
            tool.insert("wait_ms".into(), json!(10_000));
            tool.insert(
                "invocation_id".into(),
                Value::String(uuid::Uuid::new_v4().to_string()),
            );
            return self
                .call(workspace, "tilo_do", Value::Object(tool), None)
                .await;
        }
        let (provider, caller) = self.on_daemon(
            workspace,
            "a web call with a vault item's key is made only on an app's connection to this \
                 computer's daemon",
        )?;
        let answer = settled(&provider, caller, vault::web_call(), arguments)
            .await
            .map_err(|refused| refused.json())?;
        Ok(json!({"status": "completed", "result": answer}))
    }

    /// The secret of the vault item `item` (`vault.item:…`) in `workspace` (`None`: the one
    /// this connection acts in), released to this process. The vault releases one only to the
    /// process an app's connection was handed to, for an item the workspace granted that app; it
    /// refuses every agent, so the call is one action frame to the vault, never a tool. A first
    /// release of an item asks the app's person, and is refused until they allow it. The secret
    /// is answered to the caller alone: nothing here keeps or logs it.
    ///
    /// # Errors
    /// The release was refused, or could not be asked for: its refusal's body.
    pub async fn release(&self, workspace: Option<&str>, item: &str) -> Result<Value, Value> {
        let (provider, caller) = self.on_daemon(
            workspace,
            "a vault item's secret is released only to an app this computer's daemon \
                 started, over the connection it was handed",
        )?;
        let target = tilo_actions::remote::RemoteActionTarget {
            provider: vault_actions::PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(item, vault::RELEASE),
        };
        settled(&provider, caller, target, serde_json::Map::new())
            .await
            .map_err(|refused| refused.json())
    }

    /// The reference of the vault item tagged for the app `app` and the purpose `purpose` in
    /// `workspace` (`None`: the one this connection acts in), found on this app process's own
    /// connection ([`Self::find`]): `{reference}` or `{}`. A find is answered only to the app's
    /// held connection, as a release is; the secret stays in the vault.
    pub async fn find(
        &self,
        workspace: Option<&str>,
        app: &str,
        purpose: &str,
    ) -> Result<Value, Value> {
        let (provider, caller) = self.on_daemon(
            workspace,
            "a vault item is found only by an app this computer's daemon started, over the \
                 connection it was handed",
        )?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("app".into(), Value::String(app.to_owned()));
        arguments.insert("purpose".into(), Value::String(purpose.to_owned()));
        let target = vault::find();
        settled(&provider, caller, target, arguments)
            .await
            .map_err(|refused| refused.json())
    }

    /// What the person's Servers workspace answers the machine step `step`
    /// ([`tilo_action_services::machines`]) with `arguments`, read by this computer's daemon
    /// where it holds that workspace: the read a desktop makes, with or without one.
    ///
    /// # Errors
    /// These calls reach no daemon of this computer's, or the daemon refused or failed the read.
    pub async fn machines(
        &self,
        step: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<Value> {
        let At::Daemon { line, .. } = &self.at else {
            return Err(anyhow!(
                "your machines are read through this computer's daemon, and these calls reach none"
            ));
        };
        let provider: Provider = line.clone();
        // The daemon decides the call as whoever connects; the caller here names nobody.
        settled(
            &provider,
            tilo_actions::remote::Caller::local_person(""),
            tilo_action_services::machines::target(step),
            arguments,
        )
        .await
        .map_err(|refused| anyhow!("{}", refused.body.message))
    }

    /// This computer's daemon and who it decides a call as, in `workspace` when one is named:
    /// whoever connects, so the caller names nobody. Any other connection is refused with `why`.
    fn on_daemon(
        &self,
        workspace: Option<&str>,
        why: &str,
    ) -> Result<(Provider, tilo_actions::remote::Caller), Value> {
        let At::Daemon { line, .. } = &self.at else {
            return Err(ApiError::new("not_permitted", why).json());
        };
        let caller = tilo_actions::remote::Caller::local_person("");
        let caller = match workspace {
            Some(workspace) => caller.in_workspace(workspace),
            None => caller,
        };
        Ok((line.clone(), caller))
    }

    /// The call lane of this app's connection ([`LineClient::carry_call`]): an app the daemon
    /// started carries one outside service's call into a workspace call on it. Any other
    /// connection carries none, and its calls are refused with why.
    fn lane(&self) -> Result<&Arc<LineClient>, Value> {
        match &self.at {
            At::Daemon { line, .. } if self.app => Ok(line),
            _ => Err(ApiError::new(
                "not_permitted",
                "a call is carried only by an app this computer's daemon started, over the \
                 connection it was handed",
            )
            .json()),
        }
    }

    /// Carry the outside call `pair` names into the workspace call it names, or let go of the
    /// one carried (`None`): `{}`, or the refusal's body. The first carry joins the call as the
    /// app, which the workspace's grants decide.
    ///
    /// # Errors
    /// The device refused it, or this is no app's connection: the refusal's body.
    pub async fn carry_call(&self, pair: Value) -> Result<Value, Value> {
        let line = Arc::clone(self.lane()?);
        let pair: Option<tilo_actions::remote::call::BridgedCall> = serde_json::from_value(pair)
            .map_err(|error| {
                ApiError::new(
                    "invalid_request",
                    format!("a carried call is {{outside, tilo}} or null: {error}"),
                )
                .json()
            })?;
        tokio::task::spawn_blocking(move || line.carry_call(pair))
            .await
            .map_err(|error| ApiError::new("unavailable", error.to_string()).json())?
            .map(|()| Value::Object(serde_json::Map::new()))
            .map_err(|error| lane_refusal(&error))
    }

    /// Say who is in the outside calls and where the app stands in them: `{}`, or the refusal's
    /// body.
    ///
    /// # Errors
    /// The report is not one an app may make, or this is no app's connection.
    pub async fn report_call(&self, report: Value) -> Result<Value, Value> {
        let line = Arc::clone(self.lane()?);
        let report: tilo_actions::remote::call::CallReport = serde_json::from_value(report)
            .map_err(|error| {
                ApiError::new(
                    "invalid_request",
                    format!("a call report is {{calls, link, status}}: {error}"),
                )
                .json()
            })?;
        tokio::task::spawn_blocking(move || line.report_call(report))
            .await
            .map_err(|error| ApiError::new("unavailable", error.to_string()).json())?
            .map(|()| Value::Object(serde_json::Map::new()))
            .map_err(|error| lane_refusal(&error))
    }

    /// Hand the workspace's call one frame an outside person said.
    ///
    /// # Errors
    /// This is no app's connection, or it closed.
    pub fn hear_call(&self, speaker: String, opus: Vec<u8>) -> Result<(), Value> {
        self.lane()?
            .hear_call(tilo_actions::remote::call::CallAudio {
                speaker,
                opus,
                skipped: 0,
            })
            .map_err(|error| lane_refusal(&error))
    }

    /// The frames the workspace's call said since the last time, and where the device last
    /// asked the app to be, if that is new.
    ///
    /// # Errors
    /// This is no app's connection.
    pub fn said_call(&self) -> Result<CallSaid, Value> {
        let line = self.lane()?;
        Ok(CallSaid {
            frames: line.said_call(),
            asked: line.asked_call().map(|asked| (asked.call, asked.status)),
        })
    }

    /// Say one presence word of the app's own person in its workspace (`said`, a
    /// `collab_types::presence::Said` as JSON), as its windows say theirs: `{}`, or the
    /// refusal's body.
    ///
    /// # Errors
    /// The device refused the word, or this is no app's connection: the refusal's body.
    pub async fn say_presence(&self, said: Value) -> Result<Value, Value> {
        let line = Arc::clone(self.lane()?);
        let said: collab_types::presence::Said = serde_json::from_value(said).map_err(|error| {
            ApiError::new(
                "invalid_request",
                format!("a presence word is {{key, payload, persona?}}: {error}"),
            )
            .json()
        })?;
        let said = serde_json::to_value(&said)
            .map_err(|error| ApiError::new("invalid_request", error.to_string()).json())?;
        tokio::task::spawn_blocking(move || line.say_presence(&said))
            .await
            .map_err(|error| ApiError::new("unavailable", error.to_string()).json())?
            .map(|()| Value::Object(serde_json::Map::new()))
            .map_err(|error| lane_refusal(&error))
    }

    /// Who is composing a message in `resource` now, whichever app they said it in, sorted:
    /// what a window drawing the conversation shows as "… is typing", folded the same way from
    /// the words the workspace's live session hands this app
    /// ([`collab_types::presence::Composers`]). `resource` is its id, or the published address
    /// an action names it by (`tl:<app>:<id>`). The app's own words are never handed back.
    ///
    /// # Errors
    /// This is no app's connection.
    pub fn composing_at(
        &self,
        resource: String,
    ) -> Result<Vec<tilo_member_types::MemberId>, Value> {
        use collab_types::presence::{Heard, Payload};
        let line = self.lane()?;
        // A published address names the id inside it, which is what a window says it under.
        let id = match resource
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
        {
            Some((app, id)) if !app.is_empty() && !id.is_empty() => id.to_owned(),
            _ => resource,
        };
        let now = Instant::now();
        let mut composers = self
            .composers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for heard in line.presence_heard() {
            let Heard {
                member,
                key,
                payload,
                ..
            } = match serde_json::from_str(&heard) {
                Ok(heard) => heard,
                Err(error) => {
                    tracing::warn!(%error, "a presence word the device handed does not read");
                    continue;
                }
            };
            match payload {
                Payload::Composing { composing } => composers.heard(key, member, composing, now),
                Payload::Left => composers.left(&key, &member),
                _ => {}
            }
        }
        composers.prune(now);
        Ok(composers.at_resource(&id, now))
    }

    /// Have this computer's daemon answer `method` on `target` (a `/v1` path with its query) with
    /// `headers` and `body`, as its API answers whoever this connection is: an app with its own
    /// grants, the person as their login's programs are. Its status and headers, and its body as
    /// it comes, which a stream of events goes on sending.
    ///
    /// # Errors
    /// These calls name a key, which reaches the API over HTTP, or the daemon did not answer.
    pub async fn request(
        &self,
        method: &str,
        target: &str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<ApiAnswer, String> {
        let At::Daemon { line, .. } = &self.at else {
            return Err("a key reaches the API over HTTP, at its host".to_owned());
        };
        let (line, method, target) = (Arc::clone(line), method.to_owned(), target.to_owned());
        let (head, hearing) =
            tokio::task::spawn_blocking(move || line.api_request(&method, &target, headers, &body))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
        // The body arrives on the line's reader; a thread of its own hands it to async readers.
        let (pieces, body) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("tilo-api-answer".into())
            .spawn(move || {
                loop {
                    let piece = match hearing.recv() {
                        Ok(tilo_action_line::ApiHeard::Piece(bytes)) => Ok(Some(bytes)),
                        Ok(tilo_action_line::ApiHeard::End) => Ok(None),
                        Err(_) => Err("the connection to the daemon closed".to_owned()),
                    };
                    let last = !matches!(piece, Ok(Some(_)));
                    if pieces.send(piece).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(ApiAnswer {
            status: head.status,
            headers: head.headers,
            body: tokio::sync::Mutex::new(body),
        })
    }

    async fn answered(
        &self,
        workspace: Option<&str>,
        tool: &str,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<Value, ApiError> {
        let until = Instant::now().checked_add(READY_DEADLINE);
        let mut told = false;
        loop {
            // Boxed: each way's call nests deep, and a command's futures are laid out whole.
            match Box::pin(self.typed_once(workspace, tool, arguments.clone(), group.clone())).await
            {
                Ok(answer) => return Ok(answer),
                Err(refused)
                    if matches!(refused.body.code.as_str(), "not_ready" | "unanswered")
                        && until.is_some_and(|until| Instant::now() < until) =>
                {
                    // A person waiting on a computer that is still starting is told it is.
                    if !std::mem::replace(&mut told, true) {
                        eprintln!("tilo: Tilo is still starting on this computer; waiting for it");
                    }
                    let pause = refused
                        .body
                        .retry_after_ms
                        .map_or(PAUSE, Duration::from_millis);
                    tokio::time::sleep(pause).await;
                }
                Err(refused) => return Err(refused),
            }
        }
    }

    /// The tools these calls reach, as their MCP server's `tools/list` answers them.
    ///
    /// # Errors
    /// The listing was refused, or could not be asked for.
    pub async fn tools(&self) -> Result<Value> {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let answer = match &self.at {
            At::Daemon { server, .. } => Box::pin(server.handle(request)).await.unwrap_or_default(),
            At::Key(direct, _) => direct.mcp(request).await.map_err(anyhow::Error::msg)?,
        };
        answer.pointer("/result/tools").cloned().ok_or_else(|| {
            anyhow!(
                "{}",
                answer
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the workspace listed no tools")
            )
        })
    }

    async fn typed_once(
        &self,
        workspace: Option<&str>,
        tool: &str,
        arguments: Value,
        group: Option<Value>,
    ) -> Result<Value, ApiError> {
        match &self.at {
            At::Daemon {
                server,
                workspace: own,
                line,
                person,
                elsewhere,
                ..
            } => {
                // Another workspace goes over the same line, its calls naming it: the daemon
                // admits them there as it admits this account, or refuses them.
                let answer = match workspace.filter(|asked| asked != own) {
                    None => Box::pin(server.typed(tool, arguments, group)).await,
                    Some(asked) => {
                        let there = Arc::clone(
                            elsewhere
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .entry(asked.to_owned())
                                .or_insert_with(|| Arc::new(served(line.clone(), asked, person))),
                        );
                        Box::pin(there.typed(tool, arguments, group)).await
                    }
                };
                answer.map_err(ApiError::from)
            }
            // A program goes on in the program session the workspace's last one left, as it
            // does in the one session the other ways keep.
            At::Key(direct, programs) => match (tool, workspace) {
                ("tilo", Some(workspace)) => {
                    let mut programs = programs.lock().await;
                    let continued = programs.get(workspace).cloned();
                    let ran = direct
                        .program(workspace, continued.as_deref(), arguments, group)
                        .await;
                    let answer = match ran {
                        Ok((answer, session)) => {
                            programs.insert(workspace.to_owned(), session);
                            Ok(answer)
                        }
                        Err(error) => {
                            // A session the workspace no longer keeps is gone with its lets: the
                            // next program starts a new one.
                            if error.body.message.contains("program session ended") {
                                programs.remove(workspace);
                            }
                            Err(error)
                        }
                    };
                    drop(programs);
                    answer
                }
                _ => direct.typed(workspace, tool, arguments, group).await,
            },
        }
    }

    /// Run `program` as one step of `workspace`, titled `title` in History: its answer, the
    /// program's own words when it was refused before it ran.
    ///
    /// # Errors
    /// The program could not be run.
    pub async fn run_program(&self, workspace: &str, program: &str, title: &str) -> Result<Value> {
        let mut answer = self
            .typed(
                Some(workspace),
                "tilo",
                json!({"program": program}),
                Some(fresh_group(title)),
            )
            .await?;
        if let Some(fields) = answer.as_object_mut()
            && !fields.contains_key("text")
            && let Some(why) = fields
                .get("error")
                .and_then(|error| error.get("message"))
                .cloned()
        {
            fields.insert("text".into(), why);
        }
        shaped::<answers::ProgramAnswer>(answer)
    }

    /// What `path` lists in `workspace` (the account with none; the workspace itself with no
    /// path), from `cursor`, as `tilo_ls` lists it.
    ///
    /// # Errors
    /// The listing was refused.
    pub async fn list(
        &self,
        workspace: Option<&str>,
        path: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<Value> {
        let mut arguments = json!({});
        put(&mut arguments, "path", path);
        put(&mut arguments, "cursor", cursor);
        shaped::<answers::Listing>(self.typed(workspace, "tilo_ls", arguments, None).await?)
    }

    /// The resource `path` names in `workspace` (the account with none), from `cursor`, as
    /// `tilo_read` reads it.
    ///
    /// # Errors
    /// The read was refused.
    pub async fn read(
        &self,
        workspace: Option<&str>,
        path: &str,
        cursor: Option<&str>,
    ) -> Result<Value> {
        let mut arguments = json!({"path": path});
        put(&mut arguments, "cursor", cursor);
        read_answer(
            path,
            self.typed(workspace, "tilo_read", arguments, None).await?,
        )
    }

    /// What the app's window shows for `path` in `workspace`, as `tilo_view` reads it, with a
    /// picture of it when `picture` says so.
    ///
    /// # Errors
    /// The view was refused.
    pub async fn view(&self, workspace: &str, path: &str, picture: bool) -> Result<Value> {
        shaped::<answers::View>(
            self.typed(
                Some(workspace),
                "tilo_view",
                json!({"path": path, "picture": picture}),
                None,
            )
            .await?,
        )
    }

    /// Run `action` on the account's `resource` (`account/workspaces`, `create`), which no
    /// workspace keeps: what it answered. One put to the person is followed while they decide,
    /// for a while ([`Self::follow`]); one they have not answered by then is refused as waiting
    /// for them: it is done once they allow it.
    ///
    /// # Errors
    /// The action was refused or failed, the person declined it, or it still waits for them.
    pub async fn account(&self, resource: &str, action: &str, args: &Value) -> Result<Value> {
        let answer = self
            .invoke(
                None,
                &format!("{resource}#{action}"),
                args,
                None,
                (None, WAIT_MS),
            )
            .await?;
        let mut answer = self.follow(None, answer).await?;
        if answer.get("status").and_then(Value::as_str) == Some("pending") {
            return Err(anyhow::Error::new(Refusal {
                code: crate::v1::AWAITING_APPROVAL.to_owned(),
                message: "Waiting for your approval: Tilo asks you on the devices that keep \
                          your identity (your Tilo tab). It is done once you allow it there."
                    .to_owned(),
            }));
        }
        Ok(answer
            .get_mut("result")
            .map_or_else(|| json!({}), Value::take))
    }

    /// The workspaces these calls reach, as `account/workspaces#list` lists them.
    ///
    /// # Errors
    /// The listing was refused, or answered other than its declaration says.
    pub async fn workspaces(&self) -> Result<account_actions::AccountWorkspaces> {
        let listed = self
            .account(
                account_actions::account_workspaces::RESOURCE,
                account_actions::account_workspaces::LIST,
                &json!({}),
            )
            .await?;
        serde_json::from_value(listed)
            .map_err(|error| anyhow!("the account's workspace list: {error}"))
    }

    /// The workspace `named`: an id as it is, or the workspace of that name these calls reach,
    /// as `account/workspaces#list` lists them. A name two of them share is refused, each named
    /// by its id, and so is one none of them answers to, with the ones they do reach.
    ///
    /// # Errors
    /// No one workspace is called so, or the listing was refused.
    pub async fn workspace_id(&self, named: &str) -> Result<String> {
        if is_uuid(named) {
            return Ok(named.to_owned());
        }
        let listed = self.workspaces().await?;
        let matched: Vec<&account_actions::Workspace> = listed
            .workspaces
            .iter()
            .filter(|workspace| names(named, &workspace.name))
            .collect();
        match matched.as_slice() {
            [one] => Ok(one.id.clone()),
            [] if listed.workspaces.is_empty() => Err(anyhow!(
                "no workspace called {named:?}: this key reaches no workspaces"
            )),
            [] => Err(anyhow!(
                "no workspace called {named:?}: this key reaches {}",
                named_workspaces(&listed.workspaces)
            )),
            several => Err(anyhow!(
                "more than one workspace is called {named:?}: {}; name one by its id",
                named_workspaces(several.iter().copied())
            )),
        }
    }

    /// The person's workspace this computer holds: the one founded with their root, on the
    /// computer that made it, or the one a login entered. The device actions are called there, as
    /// `tilo login` and setup call them. Nothing founds one here.
    ///
    /// # Errors
    /// The workspaces could not be listed, or this computer holds none of the person's.
    pub async fn founded_workspace(&self) -> Result<String> {
        if let Some(served) = self.served_workspace() {
            return Ok(served.to_owned());
        }
        self.workspaces()
            .await?
            .workspaces
            .first()
            .map(|workspace| workspace.id.clone())
            .ok_or_else(|| {
                anyhow!(
                    "This computer holds none of your workspaces. Add it from a device of yours \
                     that has them: run `tilo device add` there, then `tilo login <ticket>` here."
                )
            })
    }

    /// Run `action` on `resource` in `workspace` (the account with none), titled `title` in
    /// History: its answer, which is `pending` when the person has not answered it within a
    /// while.
    ///
    /// # Errors
    /// The action was refused or failed.
    pub async fn act(
        &self,
        workspace: Option<&str>,
        resource: &str,
        action: &str,
        args: &Value,
        title: &str,
    ) -> Result<Value> {
        let answer = self.start(workspace, resource, action, args, title).await?;
        self.follow(workspace, answer).await
    }

    /// Run `action` on `resource` in `workspace` (the account with none), titled `title` in
    /// History, waited for a while only: its answer, `pending` while it runs on, which
    /// [`Self::follow`] follows.
    ///
    /// # Errors
    /// The action was refused or failed.
    pub async fn start(
        &self,
        workspace: Option<&str>,
        resource: &str,
        action: &str,
        args: &Value,
        title: &str,
    ) -> Result<Value> {
        self.start_waiting(
            workspace,
            &format!("{resource}#{action}"),
            args,
            title,
            WAIT_MS,
        )
        .await
    }

    /// [`Self::start`] of `path` (`resource#action`), waited for `wait_ms` at most.
    ///
    /// # Errors
    /// The action was refused or failed.
    pub async fn start_waiting(
        &self,
        workspace: Option<&str>,
        path: &str,
        args: &Value,
        title: &str,
        wait_ms: u64,
    ) -> Result<Value> {
        let group = Some(fresh_group(title));
        self.invoke(workspace, path, args, group, (None, wait_ms))
            .await
    }

    /// The invocation `run` in `workspace` (the account with none), followed while it is
    /// pending for up to `wait` seconds: its answer, `pending` while it still is.
    ///
    /// # Errors
    /// The invocation is not one these calls reach.
    pub async fn invocation(&self, workspace: Option<&str>, run: &str, wait: u64) -> Result<Value> {
        let until = Instant::now().checked_add(Duration::from_secs(wait));
        loop {
            let answer = action_answer(
                self.typed(
                    workspace,
                    "tilo_status",
                    json!({"invocation_id": run}),
                    None,
                )
                .await?,
            )?;
            let pending = answer.get("status").and_then(Value::as_str) == Some("pending");
            if !pending || until.is_none_or(|until| Instant::now() >= until) {
                return Ok(answer);
            }
            tokio::time::sleep(PAUSE).await;
        }
    }

    /// `path` (`resource#action`) with `args`, under the invocation id `id` (a new one when
    /// none), waited for `wait_ms` at most: its answer as the tools name it.
    async fn invoke(
        &self,
        workspace: Option<&str>,
        path: &str,
        args: &Value,
        group: Option<Value>,
        (id, wait_ms): (Option<&str>, u64),
    ) -> Result<Value> {
        let id = id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
        let call = json!({"path": path, "args": args, "wait_ms": wait_ms, "invocation_id": id});
        action_answer(self.typed(workspace, "tilo_do", call, group).await?)
    }

    /// `answer`, followed until it ends: where it waits for the person, it says once where they
    /// answer it.
    ///
    /// # Errors
    /// The invocation is not one these calls reach.
    pub async fn follow(&self, workspace: Option<&str>, answer: Value) -> Result<Value> {
        self.follow_asked(workspace, answer, async |record: &str| {
            eprintln!("tilo: waiting for your answer on a device of yours: it waits in {record}");
            Ok(())
        })
        .await
    }

    /// `answer`, followed until it ends, however long its person takes: where it waits in their
    /// asked record, `asked` is handed that record (`<workspace>/<record>`) once.
    ///
    /// # Errors
    /// The invocation is not one these calls reach, or `asked` failed.
    pub async fn follow_asked(
        &self,
        workspace: Option<&str>,
        mut answer: Value,
        mut asked: impl AsyncFnMut(&str) -> Result<()>,
    ) -> Result<Value> {
        let Some(run) = answer
            .get("invocation")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(answer);
        };
        let (mut told, mut first) = (false, true);
        let mut record: Option<String> = None;
        while matches!(
            answer.get("status").and_then(Value::as_str),
            Some("pending" | "asked")
        ) {
            if !told {
                if let Some(url) = answer.pointer("/ask/url").and_then(Value::as_str) {
                    told = true;
                    eprintln!("tilo: waiting for you: approve it at {url}");
                } else if let Some(record) = &record {
                    told = true;
                    asked(record).await?;
                }
            }
            // The first look reads the record a call the person is asked about waits in.
            if !first {
                tokio::time::sleep(PAUSE).await;
            }
            first = false;
            // The invocation as it stands, the record it waits in among it: the typed answer
            // keeps the status alone.
            let status = self
                .typed(
                    workspace,
                    "tilo_status",
                    json!({"invocation_id": run}),
                    None,
                )
                .await?;
            if let Some(waits_in) = status.get("record").and_then(Value::as_str) {
                record = Some(waits_in.to_owned());
            }
            answer = action_answer(status)?;
        }
        Ok(answer)
    }

    /// Resolve the question that waits in `record` (`<workspace>/<record>`) on this connection,
    /// approved or declined: a program's answer, which the daemon refuses, since only the person
    /// answers where Tilo asks them.
    ///
    /// # Errors
    /// The answer was refused.
    pub async fn answer_asked(&self, record: &str, approve: bool) -> Result<()> {
        use tilo_actions::remote::{ActionProvider as _, Caller, InvokeRequest, ReceiptStatus};
        let At::Daemon { line, person, .. } = &self.at else {
            bail!("{}", tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM);
        };
        let (workspace, record) = record
            .split_once('/')
            .ok_or_else(|| anyhow!("{record} names no workspace it waits in"))?;
        let mut resolution = serde_json::Map::new();
        resolution.insert(
            "resolution".into(),
            json!(if approve { "approve" } else { "decline" }),
        );
        let request = InvokeRequest::plain(
            tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_INVOCATIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    record,
                    tilo_actions::remote::asked::RESOLVE,
                ),
            },
            resolution,
        );
        let caller = Caller::local_person(person).in_workspace(workspace);
        let receipt = line
            .invoke(&caller, request)
            .await
            .ok_or_else(|| anyhow!("this computer's daemon did not answer"))?
            .map_err(|refused| anyhow!("{refused}"))?;
        match receipt.status {
            ReceiptStatus::Rejected { error } => bail!("{error}"),
            ReceiptStatus::Failed { error } => bail!("{}", error.message),
            _ => Ok(()),
        }
    }

    /// Put `bytes` into `workspace`: `{content, size}`, the `tilo.content:` reference actions
    /// take a file by.
    ///
    /// # Errors
    /// The workspace keeps no content for these calls, or refused it.
    pub async fn upload(&self, workspace: &str, bytes: Vec<u8>) -> Result<Value> {
        let size = bytes.len();
        let content = self.put_contents(workspace, &[bytes]).await?;
        let content = content
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("the workspace answered no content for the file"))?;
        Ok(json!({"content": content, "size": size}))
    }

    /// Which of `hashes` (lowercase BLAKE3 hex) `workspace` already has, in order.
    ///
    /// # Errors
    /// The workspace keeps no content for these calls, or refused the look.
    pub async fn have_contents(&self, workspace: &str, hashes: &[String]) -> Result<Vec<bool>> {
        let mut present = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(HAVE_HASHES) {
            let answer = self
                .typed(
                    Some(workspace),
                    "tilo_put_have",
                    json!({"hashes": chunk}),
                    None,
                )
                .await?;
            let have = answer
                .get("have")
                .and_then(Value::as_array)
                .filter(|have| have.len() == chunk.len())
                .ok_or_else(|| anyhow!("the workspace answered the look in another shape"))?;
            present.extend(have.iter().map(|have| have.as_bool().unwrap_or(false)));
        }
        Ok(present)
    }

    /// Put `files` into `workspace`, as many to a call as one carries: each one's `tilo.content:`
    /// reference, in order.
    ///
    /// # Errors
    /// The workspace keeps no content for these calls, or refused it.
    pub async fn put_contents(&self, workspace: &str, files: &[Vec<u8>]) -> Result<Vec<String>> {
        let mut contents = Vec::with_capacity(files.len());
        let mut group: Vec<String> = Vec::new();
        let mut bytes = 0_usize;
        for file in files {
            if !group.is_empty()
                && (bytes.saturating_add(file.len()) > PUT_BYTES || group.len() >= PUT_FILES)
            {
                contents.extend(self.put_many(workspace, std::mem::take(&mut group)).await?);
                bytes = 0;
            }
            if file.len() > PUT_BYTES {
                contents.push(
                    self.put_large(workspace, file.len(), &mut &file[..])
                        .await?,
                );
                continue;
            }
            bytes = bytes.saturating_add(file.len());
            group.push(base64_of(file));
        }
        if !group.is_empty() {
            contents.extend(self.put_many(workspace, group).await?);
        }
        Ok(contents)
    }

    async fn put_many(&self, workspace: &str, files: Vec<String>) -> Result<Vec<String>> {
        let count = files.len();
        let answer = self
            .typed(
                Some(workspace),
                "tilo_put_many",
                json!({"files": files}),
                None,
            )
            .await?;
        let contents: Vec<String> = answer
            .get("contents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if contents.len() != count {
            bail!("the workspace kept {} of the {count} files", contents.len());
        }
        Ok(contents)
    }

    /// Put the files at `paths` into `workspace`: each one's `tilo.content:` reference, in order.
    /// The workspace is asked first which of them it has, and none of those is sent; the rest go
    /// many to a call, a few calls at once, and a file too large for one goes in pieces. `told`
    /// hears the bytes of each file as it is done.
    ///
    /// # Errors
    /// A file cannot be read, or the workspace keeps no content for these calls, or refused one.
    pub async fn put_files(
        &self,
        workspace: &str,
        paths: &[PathBuf],
        told: &mut (dyn FnMut(u64) + Send),
    ) -> Result<Vec<String>> {
        use futures_util::StreamExt as _;
        let sizes: Vec<u64> = paths
            .iter()
            .map(|path| std::fs::metadata(path).map(|meta| meta.len()))
            .collect::<std::io::Result<_>>()?;
        let hashes: Vec<String> = paths
            .iter()
            .map(|path| hash_file(path))
            .collect::<Result<_>>()?;
        let have = self.have_contents(workspace, &hashes).await?;
        let mut contents: Vec<Option<String>> = vec![None; paths.len()];
        let mut groups: Vec<Vec<(usize, &Path)>> = Vec::new();
        let mut larges: Vec<(usize, &Path)> = Vec::new();
        let mut group_bytes = 0_u64;
        for (index, (((hash, size), had), path)) in
            hashes.iter().zip(&sizes).zip(have).zip(paths).enumerate()
        {
            let file = (index, path.as_path());
            if had {
                if let Some(content) = contents.get_mut(index) {
                    *content = Some(format!("tilo.content:{hash}"));
                }
                told(*size);
            } else if *size > PUT_BYTES as u64 {
                larges.push(file);
            } else if let Some(group) = groups.last_mut().filter(|group| {
                group.len() < PUT_FILES && group_bytes.saturating_add(*size) <= PUT_BYTES as u64
            }) {
                group.push(file);
                group_bytes = group_bytes.saturating_add(*size);
            } else {
                groups.push(vec![file]);
                group_bytes = *size;
            }
        }
        let mut many = futures_util::stream::iter(groups)
            .map(|group| async {
                let files = group
                    .iter()
                    .map(|(_, path)| std::fs::read(path).map(|bytes| base64_of(&bytes)))
                    .collect::<std::io::Result<Vec<_>>>()?;
                anyhow::Ok((self.put_many(workspace, files).await?, group))
            })
            .buffer_unordered(PUTS_AT_ONCE);
        while let Some(put) = many.next().await {
            let (refs, group) = put?;
            for ((index, _), content) in group.into_iter().zip(refs) {
                if let Some(kept) = contents.get_mut(index) {
                    *kept = Some(content);
                }
                told(sizes.get(index).copied().unwrap_or_default());
            }
        }
        drop(many);
        let mut large = futures_util::stream::iter(larges)
            .map(|(index, path)| async move {
                let content = self
                    .upload_file(workspace, path)
                    .await
                    .with_context(|| format!("putting {}", path.display()));
                (index, content)
            })
            .buffer_unordered(LARGE_AT_ONCE);
        while let Some((index, content)) = large.next().await {
            if let Some(kept) = contents.get_mut(index) {
                *kept = Some(content?);
            }
            told(sizes.get(index).copied().unwrap_or_default());
        }
        drop(large);
        contents
            .into_iter()
            .map(|content| content.ok_or_else(|| anyhow!("a file was answered no content")))
            .collect()
    }

    /// Put the file at `path` into `workspace`, piece by piece: its `tilo.content:` reference.
    ///
    /// # Errors
    /// The file cannot be read, or the workspace keeps no content for these calls, or refused it.
    pub async fn upload_file(&self, workspace: &str, path: &Path) -> Result<String> {
        if let At::Daemon { line, again, .. } = &self.at {
            let (line, again, path) = (Arc::clone(line), *again, path.to_owned());
            return tokio::task::spawn_blocking(move || upload_over(&line, again, &path)).await?;
        }
        let mut file = std::fs::File::open(path)?;
        let size = usize::try_from(file.metadata()?.len())?;
        self.put_large(workspace, size, &mut file).await
    }

    /// One file of `size` bytes, read from `file`, too large for a put of many: begun, appended
    /// piece by piece, and finished.
    async fn put_large(
        &self,
        workspace: &str,
        size: usize,
        file: &mut (dyn std::io::Read + Send),
    ) -> Result<String> {
        let begun = self
            .typed(
                Some(workspace),
                "tilo_put_begin",
                json!({"size": size}),
                None,
            )
            .await?;
        let upload = begun
            .get("upload")
            .cloned()
            .ok_or_else(|| anyhow!("the workspace began no upload"))?;
        let mut offset = 0_usize;
        let mut buffer = vec![0_u8; PUT_BYTES];
        while offset < size {
            let length = PUT_BYTES.min(size.saturating_sub(offset));
            let piece = buffer
                .get_mut(..length)
                .ok_or_else(|| anyhow!("a piece larger than its buffer"))?;
            file.read_exact(piece)?;
            self.typed(
                Some(workspace),
                "tilo_put_append",
                json!({"upload": upload, "offset": offset, "chunk": base64_of(piece)}),
                None,
            )
            .await?;
            offset = offset.saturating_add(piece.len());
        }
        let finished = self
            .typed(
                Some(workspace),
                "tilo_put_finish",
                json!({"upload": upload}),
                None,
            )
            .await?;
        finished
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the workspace finished the upload with no content"))
    }

    /// Write `content`'s bytes from `workspace` into `output`, a piece at a time, telling `told`
    /// how many are written and how many the file is: how many were written.
    ///
    /// # Errors
    /// The workspace does not have it, or `output` cannot be written.
    #[expect(
        clippy::future_not_send,
        reason = "the writer is this command's own, used on its one thread"
    )]
    pub async fn download(
        &self,
        workspace: &str,
        content: &str,
        output: &mut dyn std::io::Write,
        told: &mut impl FnMut(u64, Option<u64>),
    ) -> Result<u64> {
        use base64::Engine as _;
        let mut written = 0_u64;
        loop {
            let piece = self
                .typed(
                    Some(workspace),
                    "tilo_get",
                    json!({"content": content, "offset": written}),
                    None,
                )
                .await?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    piece
                        .get("bytes")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
                .map_err(|error| {
                    anyhow!("the workspace answered bytes that are not base64: {error}")
                })?;
            let size = piece.get("size").and_then(Value::as_u64);
            output.write_all(&bytes)?;
            written = written.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            told(written, size);
            if bytes.is_empty() || size.is_none_or(|size| written >= size) {
                return Ok(written);
            }
        }
    }
}

/// Put the file at `path` into the workspace the daemon behind `line` serves, piece by piece: its
/// `tilo.content:` reference. A piece that fails is sent again from where the daemon says the
/// upload stands, on a connection made `again` at its socket.
fn upload_over(line: &LineClient, again: bool, path: &Path) -> Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    use tilo_actions::remote::ContentPut as _;
    let refused = |error: tilo_actions::remote::ActionCallError| anyhow!("{error}");
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut put = line.begin_put(size).map_err(refused)?;
    let mut piece = vec![0_u8; PUT_BYTES];
    let mut failed = 0_usize;
    while put.written() < size {
        let at = put.written();
        let length = usize::try_from(size.saturating_sub(at))
            .unwrap_or(usize::MAX)
            .min(PUT_BYTES);
        let piece = piece
            .get_mut(..length)
            .ok_or_else(|| anyhow!("a piece larger than its buffer"))?;
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(piece)?;
        match put.append(at, piece) {
            Ok(_) => failed = 0,
            Err(error) if again && failed < PIECE_TRIES => {
                failed = failed.saturating_add(1);
                let socket = daemon_socket().map_err(|_| refused(error.clone()))?;
                let stream = tilo_files::native::socket::connect(&socket)
                    .map_err(|_| refused(error.clone()))?;
                put = LineClient::of_stream(stream)?
                    .resume_put(put.upload(), size)
                    .map_err(|_| refused(error))?;
            }
            Err(error) => return Err(refused(error)),
        }
    }
    Box::new(put).finish().map_err(refused)
}

/// The address the bytes of the file at `path` keep in a workspace, lowercase BLAKE3 hex: what
/// a put of it answers, read here so content the workspace has is not sent again.
fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// The account's workspaces as the daemon behind `provider` lists them to whoever connects:
/// before a login names one, the workspace it serves is the one it founded. The account's actions
/// are no workspace's, so they are invoked outright rather than walked in a workspace's tree.
async fn founded(provider: &Provider) -> Result<Value> {
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: crate::account::PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(crate::account::WORKSPACES, "list"),
    };
    // The daemon decides the call as whoever connects; the caller here names nobody.
    settled(
        provider,
        tilo_actions::remote::Caller::local_person(""),
        target,
        serde_json::Map::new(),
    )
    .await
    .map_err(|refused| anyhow!("this computer's daemon refused: {}", refused.body.message))
}

/// What a carried call said since an app last looked ([`Calls::said_call`]).
pub struct CallSaid {
    /// Each frame a workspace participant said, by their account here, with its Opus bytes and
    /// the frames of their stream dropped just before it.
    pub frames: Vec<tilo_actions::remote::call::CallAudio>,
    /// Where the device last asked the app to be in the outside call (none: in no call), and the
    /// line its people are shown there about who is on the workspace's side, when that is new.
    pub asked: Option<(Option<String>, Option<String>)>,
}

/// The body a refused call of the lane answers, as the SDKs raise their typed errors from.
fn lane_refusal(error: &tilo_actions::remote::ActionCallError) -> Value {
    match error {
        tilo_actions::remote::ActionCallError::Refused { reason } => {
            ApiError::new("not_permitted", reason.clone()).json()
        }
        other => ApiError::new(other.code(), other.to_string()).json(),
    }
}

/// `target` invoked with `arguments` as `caller` over `provider`, waited for until it settles:
/// what it completed with, or its refusal as the API answers one.
async fn settled(
    provider: &Provider,
    caller: tilo_actions::remote::Caller,
    target: tilo_actions::remote::RemoteActionTarget,
    arguments: serde_json::Map<String, Value>,
) -> Result<Value, ApiError> {
    use tilo_action_calls::settle_with;
    use tilo_actions::remote::{ActionCallError, InvokeRequest, ReceiptStatus};
    // A provider's own refusal (the vault's among them) is what the caller may not do.
    let refused = |error: ActionCallError| match error {
        ActionCallError::Refused { reason } => ApiError::new("not_permitted", reason),
        other => ApiError::new(other.code(), other.to_string()),
    };
    let unanswered = || ApiError::new("unanswered", "this computer's daemon did not answer");
    let asked = InvokeRequest::plain(target, arguments);
    let invocation = asked.invocation;
    let mut status = provider
        .invoke(&caller, asked)
        .await
        .ok_or_else(unanswered)?
        .map_err(refused)?
        .status;
    let now = tilo_clock::now();
    let until = now.checked_add(ANSWER_DEADLINE).unwrap_or(now);
    loop {
        match status {
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Completed { output } => return Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => {
                return Err(ApiError::new(&error.code, error.message));
            }
            ReceiptStatus::Rejected { error } => return Err(refused(error)),
            ReceiptStatus::Cancelled { .. } => {
                return Err(ApiError::new("action_failed", "the call was stopped"));
            }
        }
        status = settle_with(provider, &caller, invocation, None, until)
            .await
            .flatten()
            .ok_or_else(unanswered)?
            .receipt()
            .status;
    }
}

/// A connection to the daemon at `socket`, else this computer's.
fn connected(socket: Option<&Path>) -> Result<tilo_files::native::socket::Stream> {
    let socket = match socket {
        Some(socket) => socket.to_owned(),
        None => daemon_socket().map_err(anyhow::Error::msg)?,
    };
    tilo_files::native::socket::connect(&socket).map_err(|error| {
        anyhow!(
            "this computer's daemon does not answer at {} ({error}): run `tilo status` to start \
             it",
            socket.display()
        )
    })
}

/// The agent surface over `provider` in `workspace`, as `person`. It starts no task's host of
/// its own: its programs run in the daemon, as the person, in this server's program session there.
fn served(provider: Provider, workspace: &str, person: &str) -> McpServer<Provider> {
    McpServer::for_person(provider, workspace.to_owned(), person).with_programs_on_device()
}

/// A group of its own for one call, titled `title`: History names it so.
fn fresh_group(title: &str) -> Value {
    json!({"id": uuid::Uuid::new_v4().simple().to_string(), "title": title})
}

/// An action's answer as the tools name it, its invocation as `invocation`.
fn action_answer(mut answer: Value) -> Result<Value> {
    if let Some(fields) = answer.as_object_mut()
        && let Some(invocation) = fields.remove("invocation_id")
    {
        fields.insert("invocation".into(), invocation);
    }
    shaped::<answers::ActionAnswer>(answer)
}

/// What `tilo_read` answered for `path`, as the resource [`Session::read`] answers: a record's
/// reading is the resource at its path, as the API's read answers it too.
pub(crate) fn read_answer(path: &str, read: Value) -> Result<Value> {
    shaped::<answers::Resource>(answers::record_resource(path, read))
}

/// `value` as `T` answers it, or why it does not.
fn shaped<T: serde::de::DeserializeOwned + serde::Serialize>(value: Value) -> Result<Value> {
    answers::typed::<T>(value).map_err(|why| {
        anyhow!(
            "the workspace answered {} in an unexpected shape: {why}",
            std::any::type_name::<T>()
        )
    })
}

fn put(arguments: &mut Value, name: &str, value: Option<&str>) {
    if let (Some(fields), Some(value)) = (
        arguments.as_object_mut(),
        value.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        fields.insert(name.to_owned(), json!(value));
    }
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Whether `text` is written as a workspace's id is.
#[must_use]
pub fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// Whether `named`, a workspace as a person or an agent writes it (`-w`, a tool's `workspace`),
/// names the workspace called `name`: the same letters and digits in any case, words broken by
/// anything else (`applicant-jane-doe` names `Applicant: Jane Doe`).
#[must_use]
pub fn names(named: &str, name: &str) -> bool {
    let words = |text: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    words(named) == words(name)
}

/// The workspaces a listing names, each as `Name (id ID)`: what an error says a key reaches, so
/// the person knows which name or id to type instead.
fn named_workspaces<'a>(
    workspaces: impl IntoIterator<Item = &'a account_actions::Workspace>,
) -> String {
    workspaces
        .into_iter()
        .map(|workspace| format!("{} (id {})", workspace.name, workspace.id))
        .collect::<Vec<_>>()
        .join(", ")
}

/// This computer's daemon's native endpoint ([`crate::daemon::DaemonPlace::native`]).
///
/// # Errors
/// This computer keeps no login place, or its sockets' directory is not this account's own.
pub fn daemon_socket() -> Result<PathBuf, String> {
    Ok(crate::daemon::this_computer()?.native())
}

/// The key a script was approved (`token`, as `tilo do account/keys#create` answers it), as a key file
/// reaching where this computer's login reaches: a key of its own, never the device's, so the
/// script's session and the computer's each have one session. `None` when this computer did not
/// log in.
///
/// # Errors
/// The token is not a key's, or the login is damaged.
pub fn script_key(token: &str) -> Result<Option<crate::direct::KeyFile>> {
    let Some(device) = crate::daemon::who::home().map_err(anyhow::Error::msg)? else {
        return Ok(None);
    };
    let file = crate::direct::KeyFile {
        host: device.host_origin.clone(),
        // The site the device logged in with: links this key asks the host for are built on
        // it, as the site's own page builds them.
        site: Some(device.site.trim_end_matches('/').to_owned()),
        // The token names the key, its certificate and the name its runs go by.
        name: String::new(),
        key: String::new(),
        cert: String::new(),
        reached_at: device
            .server_address
            .as_ref()
            .map(|address| crate::server::ServerInfo {
                server_cert: device.server_cert.clone(),
                address: Some(address.clone()),
                origin: Some(device.host_origin.clone()),
            }),
    };
    Ok(Some(file.with_token(token).map_err(anyhow::Error::msg)?))
}

#[cfg(test)]
mod tests {
    /// The command and an agent's tools find a workspace by the same names.
    #[test]
    fn a_workspace_is_found_by_its_name_as_a_person_writes_it() {
        for named in [
            "Applicant: Jane Doe",
            "applicant-jane-doe",
            "applicant jane  doe",
        ] {
            assert!(super::names(named, "Applicant: Jane Doe"), "{named}");
        }
        assert!(!super::names("applicant", "Applicant: Jane Doe"));
    }
}
