//! Tilo's actions, as a program of the person's on this computer calls them: `tilo`'s workspace
//! commands, `tilolang repl`, a model ask, `tilo-app push`. Every call is a tool of the agent
//! surface ([`tilo_mcp::McpServer`]), walked in the caller's own process, over whichever actions
//! the caller reaches ([`Calls::open`]):
//!
//! - **This computer**: the daemon's socket ([`daemon_socket`]), whose action
//!   frames a [`LineClient`] carries. The daemon decides every call as the person whose process
//!   connects, and keeps the device's one session with its host, so no program here opens a
//!   second one. It serves the workspace this computer logged in to, or the one it founded.
//! - **A key file** (`--key-file`, `TILO_KEY_FILE`): the key enters its host's session as itself
//!   ([`crate::direct::Direct`]), and the same walk runs there.
//! - **A Tilo desktop**: the desktop serves its own API socket ([`desktop_socket`]), and a call is
//!   one `tools/call` of the MCP session it serves there.
//!
//! A call the workspace was not ready for, or left unanswered, is asked again for a while; a call
//! that ran is never run twice, since an action is invoked under an id of its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_actions::remote::LineClient;
use tilo_device_identity::login as device;
use tilo_mcp::McpServer;

mod model_ask;

pub use model_ask::ask_model;
pub(crate) use model_ask::follow;

use crate::session::Provider;
use crate::v1::answers;
use crate::v1::errors::ApiError;

/// Names a key file a command acts as, as `--key-file` does.
pub const KEY_FILE_ENV: &str = "TILO_KEY_FILE";

/// Names the desktop's own API socket, as `TILO_MCP_SERVER` names the agent socket: a program of
/// a login whose runtime directory is not the one the desktop was started in finds it here.
pub const DESKTOP_ENV: &str = "TILO_API_SOCKET";

/// How long a call the workspace was not ready for is asked again: a workspace that does not
/// serve this computer yet is on its way.
const READY_DEADLINE: Duration = Duration::from_secs(300);
/// How long an action waits for the person who is asked to allow it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// How long between asks.
const PAUSE: Duration = Duration::from_secs(1);
/// How long one call of an action waits for it to end before it is followed.
const WAIT_MS: u64 = 60_000;
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

/// A call that was refused: the code it answered, its words, and the wait it asked for before
/// the same call works.
#[derive(Clone, Debug)]
pub struct Refusal {
    code: String,
    message: String,
    retry_after_ms: Option<u64>,
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

    /// How long the caller was asked to wait before asking again.
    #[must_use]
    pub fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
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
            retry_after_ms: body.retry_after_ms,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// Tilo's actions, as one caller reaches them.
pub struct Calls {
    at: At,
}

enum At {
    /// This computer's daemon, serving `workspace` to `person` over `line`; `again` when a lost
    /// connection is made again at its socket.
    Daemon {
        server: Box<McpServer<Provider>>,
        line: Arc<LineClient>,
        workspace: String,
        person: String,
        again: bool,
    },
    /// A key's own session with its host, and the program session each workspace's programs
    /// continue.
    Key(
        crate::direct::Direct,
        tokio::sync::Mutex<std::collections::HashMap<String, String>>,
    ),
    /// A Tilo desktop's own API socket.
    Desktop(Desktop),
}

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

    /// The actions `reach` names: a desktop's when one serves this login and no key is named;
    /// the key file's own session when it names a key other than this computer's; else this
    /// computer's daemon, connected to by `daemon` (which starts one when none listens).
    ///
    /// # Errors
    /// The key file cannot be read or is refused, or the daemon cannot be reached.
    pub async fn open_with(
        reach: Reach<'_>,
        daemon: impl FnOnce() -> Result<std::os::unix::net::UnixStream, String>,
    ) -> Result<Self> {
        if reach.key_file.is_none()
            && let Some(socket) = desktop_socket()
        {
            return Ok(Self {
                at: At::Desktop(Desktop::new(socket)),
            });
        }
        if let Some(path) = reach.key_file
            && !reach.as_device()?
        {
            let file = crate::direct::KeyFile::read(path).map_err(anyhow::Error::msg)?;
            let direct = crate::direct::Direct::new(&file)
                .map_err(|why| anyhow!("the key for {}: {why}", file.host))?;
            return Ok(Self {
                at: At::Key(direct, tokio::sync::Mutex::default()),
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
            },
        })
    }

    /// The actions of the Tilo desktop serving its own API socket at `socket`: a nested
    /// session's, which [`desktop_socket`] does not find.
    #[must_use]
    pub fn desktop(socket: PathBuf) -> Self {
        Self {
            at: At::Desktop(Desktop::new(socket)),
        }
    }

    /// The same actions, with a program session of their own: what a program run through them
    /// continues is what their own programs left, never what this one's did.
    #[must_use]
    pub fn apart(&self) -> Self {
        let at = match &self.at {
            At::Daemon {
                line,
                workspace,
                person,
                again,
                ..
            } => {
                let provider: Provider = line.clone();
                At::Daemon {
                    server: Box::new(served(provider, workspace, person)),
                    line: Arc::clone(line),
                    workspace: workspace.clone(),
                    person: person.clone(),
                    again: *again,
                }
            }
            At::Key(direct, _) => At::Key(direct.clone(), tokio::sync::Mutex::default()),
            At::Desktop(desktop) => At::Desktop(Desktop::new(desktop.socket.clone())),
        };
        Self { at }
    }

    /// The workspace this computer's daemon serves, for calls that reach it.
    #[must_use]
    pub fn served_workspace(&self) -> Option<&str> {
        match &self.at {
            At::Daemon { workspace, .. } => Some(workspace),
            At::Key(..) | At::Desktop(_) => None,
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
        let until = Instant::now().checked_add(READY_DEADLINE);
        loop {
            // Boxed: each way's call nests deep, and a command's futures are laid out whole.
            match Box::pin(self.typed_once(workspace, tool, arguments.clone(), group.clone())).await
            {
                Ok(answer) => return Ok(answer),
                Err(refused)
                    if matches!(refused.body.code.as_str(), "not_ready" | "unanswered")
                        && until.is_some_and(|until| Instant::now() < until) =>
                {
                    let pause = refused
                        .body
                        .retry_after_ms
                        .map_or(PAUSE, Duration::from_millis);
                    tokio::time::sleep(pause).await;
                }
                Err(refused) => return Err(anyhow::Error::new(Refusal::said(&refused))),
            }
        }
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
                workspace: served,
                ..
            } => {
                if let Some(asked) = workspace.filter(|asked| asked != served) {
                    return Err(ApiError::new(
                        "beyond_scope",
                        format!(
                            "this computer acts in workspace {served}, the one it logged in to, \
                             not {asked}"
                        ),
                    )
                    .next("leave -w out, or name that workspace's key with --key-file"));
                }
                Box::pin(server.typed(tool, arguments, group))
                    .await
                    .map_err(|refused| {
                        ApiError::of_answer(Some(&refused.error), refused.action.as_ref())
                    })
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
            At::Desktop(desktop) => desktop.typed(workspace, tool, arguments, group).await,
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
                json!({"program": program, "confirm": false}),
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
        shaped::<answers::Resource>(self.typed(workspace, "tilo_read", arguments, None).await?)
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
    /// workspace keeps: what it answered. One put to the person, who has not answered yet, is
    /// refused as waiting for them: it is done once they allow it.
    ///
    /// # Errors
    /// The action was refused or failed.
    pub async fn account(&self, resource: &str, action: &str, args: &Value) -> Result<Value> {
        let mut answer = self
            .invoke(None, &format!("{resource}#{action}"), args, None, None)
            .await?;
        if answer.get("status").and_then(Value::as_str) == Some("pending") {
            return Err(anyhow::Error::new(Refusal {
                code: crate::v1::AWAITING_APPROVAL.to_owned(),
                message: "Waiting for your approval: Tilo asks you on the devices that keep \
                          your identity (your Tilo tab). It is done once you allow it there."
                    .to_owned(),
                retry_after_ms: None,
            }));
        }
        Ok(answer
            .get_mut("result")
            .map_or_else(|| json!({}), Value::take))
    }

    /// The workspaces these calls reach, as `account/workspaces#list` lists them:
    /// `{"workspaces": [...]}`.
    ///
    /// # Errors
    /// The listing was refused.
    pub async fn workspaces(&self) -> Result<Value> {
        self.account("account/workspaces", "list", &json!({})).await
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
        let workspaces = listed
            .get("workspaces")
            .and_then(Value::as_array)
            .map_or(&[] as &[Value], Vec::as_slice);
        let wanted = slug(named);
        let matched: Vec<&Value> = workspaces
            .iter()
            .filter(|workspace| {
                workspace
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| slug(name) == wanted)
            })
            .collect();
        match matched.as_slice() {
            [one] => one
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("the workspace called {named:?} has no id")),
            [] if workspaces.is_empty() => Err(anyhow!(
                "no workspace called {named:?}: this key reaches no workspaces"
            )),
            [] => Err(anyhow!(
                "no workspace called {named:?}: this key reaches {}",
                named_workspaces(workspaces)
            )),
            several => Err(anyhow!(
                "more than one workspace is called {named:?}: {}; name one by its id",
                named_workspaces(several.iter().copied())
            )),
        }
    }

    /// The workspace a computer serves before a login exists: the one it founded, where the
    /// device actions are called, as `tilo login` and setup call them.
    ///
    /// # Errors
    /// The workspaces could not be listed, or none is served.
    pub async fn founded_workspace(&self) -> Result<String> {
        if let Some(served) = self.served_workspace() {
            return Ok(served.to_owned());
        }
        self.workspaces()
            .await?
            .pointer("/workspaces/0/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("this computer's daemon serves no workspace to log in to"))
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
    /// History, waited for a short while only: its answer, `pending` while it runs on, which
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
        self.invoke(
            workspace,
            &format!("{resource}#{action}"),
            args,
            Some(fresh_group(title)),
            None,
        )
        .await
    }

    /// Run `action` on `resource` in `workspace` as the step `step` of the invocation `run`,
    /// titled `title` in History: its answer, which is `pending` while it runs on. Every call
    /// under one `run` is grouped in History as one change, and a step asked again under the
    /// same `step` runs once.
    ///
    /// # Errors
    /// The action was refused or failed.
    pub async fn act_invoked(
        &self,
        workspace: &str,
        resource: &str,
        action: &str,
        args: &Value,
        (run, title, step): (&str, &str, &str),
    ) -> Result<Value> {
        self.invoke(
            Some(workspace),
            &format!("{resource}#{action}"),
            args,
            Some(json!({"id": run, "title": title})),
            Some(step),
        )
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
    /// none), waited for a while: its answer as the tools name it.
    async fn invoke(
        &self,
        workspace: Option<&str>,
        path: &str,
        args: &Value,
        group: Option<Value>,
        id: Option<&str>,
    ) -> Result<Value> {
        let id = id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
        let call = json!({"path": path, "args": args, "wait_ms": WAIT_MS, "invocation_id": id});
        action_answer(self.typed(workspace, "tilo_do", call, group).await?)
    }

    /// `answer`, followed while it waits for the person, for a while: it says once where they
    /// approve it.
    ///
    /// # Errors
    /// The invocation is not one these calls reach.
    pub async fn follow(&self, workspace: Option<&str>, mut answer: Value) -> Result<Value> {
        let until = Instant::now().checked_add(ANSWER_DEADLINE);
        let mut told = false;
        while answer.get("status").and_then(Value::as_str) == Some("pending")
            && until.is_some_and(|until| Instant::now() < until)
        {
            if !told {
                told = true;
                if let Some(url) = answer.pointer("/ask/url").and_then(Value::as_str) {
                    eprintln!("tilo: waiting for you: approve it at {url}");
                }
            }
            let Some(run) = answer
                .get("invocation")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                break;
            };
            tokio::time::sleep(PAUSE).await;
            answer = self.invocation(workspace, &run, 0).await?;
        }
        Ok(answer)
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

/// The account's workspaces as the daemon behind `provider` lists them to whoever connects:
/// before a login names one, the workspace it serves is the one it founded. The account's actions
/// are no workspace's, so they are invoked outright rather than walked in a workspace's tree.
/// The address the bytes of the file at `path` keep in a workspace, lowercase BLAKE3 hex: what
/// a put of it answers, read here so content the workspace has is not sent again.
fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

async fn founded(provider: &Provider) -> Result<Value> {
    use tilo_actions::remote::{InvokeRequest, ReceiptStatus, RemoteActionTarget, settle_with};
    let asked = InvokeRequest::plain(
        RemoteActionTarget {
            provider: crate::account::PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(crate::account::WORKSPACES, "list"),
        },
        serde_json::Map::new(),
    );
    let invocation = asked.invocation;
    // The daemon decides the call as whoever connects; the caller here names nobody.
    let caller = tilo_actions::remote::Caller::local_person("");
    let unanswered = || anyhow!("this computer's daemon did not answer");
    let mut status = provider
        .invoke(&caller, asked)
        .await
        .ok_or_else(unanswered)?
        .map_err(|error| anyhow!("this computer's daemon refused: {error}"))?
        .status;
    let now = tilo_clock::now();
    let until = now.checked_add(ANSWER_DEADLINE).unwrap_or(now);
    loop {
        match status {
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Completed { output } => return Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => bail!("{}", error.message),
            ReceiptStatus::Rejected { error } => bail!("this computer's daemon refused: {error}"),
            ReceiptStatus::Cancelled { .. } => bail!("listing the workspaces was stopped"),
        }
        status = settle_with(provider, &caller, invocation, None, until)
            .await
            .flatten()
            .ok_or_else(unanswered)?
            .receipt()
            .status;
    }
}

/// The agent surface over `provider` in `workspace`, as `person`.
fn served(provider: Provider, workspace: &str, person: &str) -> McpServer<Provider> {
    let catalogue = tilo_mcp::task_hosts::WorkspaceCatalogue::new(
        Arc::clone(&provider),
        tilo_actions::remote::Caller::local_person(person),
    );
    McpServer::for_person(provider, workspace.to_owned(), person).with_task_hosts(Some(Arc::new(
        tilo_mcp::task_hosts::TaskHosts::new(Some(person), Arc::new(catalogue)),
    )))
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

/// A name as `-w` matches it: letters and digits, lower case, words joined by `-`
/// (`Applicant: Jane Doe` and `applicant-jane-doe` match).
#[must_use]
pub fn slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// The workspaces a listing names, each as `Name (id ID)`: what an error says a key reaches, so
/// the person knows which name or id to type instead.
fn named_workspaces<'a>(workspaces: impl IntoIterator<Item = &'a Value>) -> String {
    workspaces
        .into_iter()
        .map(|workspace| {
            let name = workspace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed");
            match workspace.get("id").and_then(Value::as_str) {
                Some(id) => format!("{name} (id {id})"),
                None => name.to_owned(),
            }
        })
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

/// The socket this login's desktop serves its own API to its person's programs at, when one
/// serves it: `API_SOCKET_FILE` in the session's directory under the login's runtime directory
/// ([`tilo_paths::login::session_dir`]), or the one [`DESKTOP_ENV`] names.
#[must_use]
pub fn desktop_socket() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os(DESKTOP_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Some(named);
    }
    let directory = tilo_paths::login::session_dir(std::env::var_os("XDG_RUNTIME_DIR"))?;
    let socket = directory.join(tilo_paths::login::API_SOCKET_FILE);
    // A socket that answers is one this session serves still; one a session left behind is not.
    tilo_files::native::socket::is_bound(&socket).then_some(socket)
}

/// This computer's own key as a key file ([`crate::direct::KeyFile`]), named `name` or as the
/// person's devices name this computer: the key `tilo login` kept, its certificate in the
/// canonical bytes the in-process API reads, and the host it reaches. `None` when this computer
/// did not log in.
///
/// # Errors
/// The login is damaged.
pub fn login_key(name: Option<&str>) -> Result<Option<crate::direct::KeyFile>> {
    let Some((key, device)) = crate::daemon::who::here()? else {
        return Ok(None);
    };
    let canonical = tilo_user_identity::canonical::canonical_bytes(&device.cert)
        .map_err(|_| anyhow!(device::LOGIN_DAMAGED))?;
    Ok(Some(crate::direct::KeyFile {
        host: device.host_origin.clone(),
        // The site the device logged in with: links this key asks the host for are built on
        // it, as the site's own page builds them.
        site: Some(device.site.trim_end_matches('/').to_owned()),
        name: name.map_or_else(|| device.label().to_owned(), str::to_owned),
        key: hex::encode(key.to_bytes()),
        cert: hex::encode(canonical),
        reached_at: device
            .server_address
            .as_ref()
            .map(|address| crate::server::ServerInfo {
                server_cert: device.server_cert.clone(),
                address: Some(address.clone()),
                origin: Some(device.host_origin.clone()),
            }),
    }))
}

/// The key a script was approved (`token`, as `tilo do account/keys#create` answers it), as a key file
/// reaching where this computer's login reaches: a key of its own, never the device's, so the
/// script's session and the computer's each have one session. `None` when this computer did not
/// log in.
///
/// # Errors
/// The token is not a key's, or the login is damaged.
pub fn script_key(token: &str) -> Result<Option<crate::direct::KeyFile>> {
    let Some(file) = login_key(None)? else {
        return Ok(None);
    };
    Ok(Some(file.with_token(token).map_err(anyhow::Error::msg)?))
}

/// A Tilo desktop's MCP session at the API socket it serves: opened on the first call, and
/// asked one `tools/call` a call.
struct Desktop {
    socket: PathBuf,
    session: tokio::sync::Mutex<Option<String>>,
}

impl Desktop {
    fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            session: tokio::sync::Mutex::new(None),
        }
    }

    async fn typed(
        &self,
        workspace: Option<&str>,
        tool: &str,
        mut arguments: Value,
        group: Option<Value>,
    ) -> Result<Value, ApiError> {
        if let (Some(fields), Some(workspace)) = (arguments.as_object_mut(), workspace) {
            fields.insert(crate::session::WORKSPACE_ARGUMENT.into(), json!(workspace));
        }
        let mut meta = json!({"structured": true});
        if let (Some(fields), Some(group)) = (meta.as_object_mut(), group) {
            fields.insert("invocation".into(), group);
        }
        let session = self.session().await?;
        let (_, answer) = self
            .post(
                Some(&session),
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": tool, "arguments": arguments, "_meta": {"so.tilo.mcp": meta},
                }}),
            )
            .await?;
        if let Some(error) = answer.get("error") {
            return Err(ApiError::new(
                "failed",
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the desktop refused the call"),
            ));
        }
        let result = answer.get("result").cloned().unwrap_or_default();
        let failed = result.get("isError").and_then(Value::as_bool) == Some(true);
        match result.get("structuredContent").cloned() {
            Some(typed) if !failed || tool == "tilo" => Ok(typed),
            Some(typed) => Err(ApiError::of_answer(typed.get("error"), typed.get("action"))),
            None => {
                let said = result
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or("the desktop answered nothing");
                Err(ApiError::new(
                    if failed { "failed" } else { "not_found" },
                    said,
                ))
            }
        }
    }

    /// The MCP session, opened the first time.
    async fn session(&self) -> Result<String, ApiError> {
        let mut session = self.session.lock().await;
        if let Some(open) = session.as_ref() {
            return Ok(open.clone());
        }
        let (opened, answer) = self
            .post(
                None,
                &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "tilo", "version": env!("CARGO_PKG_VERSION")},
                }}),
            )
            .await?;
        let opened = opened.ok_or_else(|| {
            ApiError::new(
                "unavailable",
                answer
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the desktop opened no session"),
            )
        })?;
        *session = Some(opened.clone());
        drop(session);
        Ok(opened)
    }

    /// POST `request` to the desktop's MCP endpoint in `session`: the session it named, and its
    /// answer.
    async fn post(
        &self,
        session: Option<&str>,
        request: &Value,
    ) -> Result<(Option<String>, Value), ApiError> {
        use http_body_util::BodyExt as _;
        let unavailable = |why: String| ApiError::new("unavailable", why);
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .map_err(|error| {
                unavailable(format!(
                    "the desktop does not answer at {}: {error}",
                    self.socket.display()
                ))
            })?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(|error| unavailable(error.to_string()))?;
        tokio::spawn(connection);
        let mut asked = hyper::Request::post("/mcp")
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::ACCEPT, "application/json");
        if let Some(session) = session {
            asked = asked.header(crate::session::SESSION_HEADER, session);
        }
        let asked = asked
            .body(http_body_util::Full::new(hyper::body::Bytes::from(
                request.to_string(),
            )))
            .map_err(|error| unavailable(error.to_string()))?;
        let response = sender
            .send_request(asked)
            .await
            .map_err(|error| unavailable(error.to_string()))?;
        let named = response
            .headers()
            .get(crate::session::SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| unavailable(error.to_string()))?
            .to_bytes();
        let answer = serde_json::from_slice(&body).map_err(|_| {
            unavailable(format!(
                "the desktop answered {status}: {}",
                String::from_utf8_lossy(&body)
            ))
        })?;
        Ok((named, answer))
    }
}
