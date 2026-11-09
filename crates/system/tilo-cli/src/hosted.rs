//! Hosted workspaces from a shell, with an API key: `tilo keys`,
//! `tilo account workspaces`, `tilo do -w`, `tilo files` and `tilo run`.
//!
//! These make the very calls an agent makes through the front door's MCP
//! endpoints (`/mcp` for the account, `/w/<workspace>/mcp` for one workspace),
//! asking for the typed answer, so a script sees what an agent sees. Making
//! workspaces and keys are actions on the account (`account/workspaces#create`,
//! `account/keys#mint`). A file goes in as one upload (`PUT
//! /w/<workspace>/content`), and the reference it answers is what actions take.
//!
//! The key comes from `TILO_API_KEY`, and the door from `--door` or
//! `TILO_DOOR` (`https://mcp.tilo.sh`). `tilo keys create` gets a key: it
//! asks the person, who approves it in their Tilo.

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// The door hosted Tilo answers at.
pub const DEFAULT_DOOR: &str = "https://mcp.tilo.sh";
pub const DOOR_ENV: &str = "TILO_DOOR";
pub const KEY_ENV: &str = "TILO_API_KEY";
const SESSION_HEADER: &str = "mcp-session-id";
const MAX_ANSWER: usize = 64 * 1024 * 1024;

#[derive(Subcommand)]
pub enum KeysCmd {
    /// Ask for a key: you approve it in your Tilo, and it prints here.
    #[command(
        after_help = "Example:\n  tilo keys create \"Visa backend\"\n  export TILO_API_KEY=tilo_sk_…"
    )]
    Create {
        /// What will use it, as your grants page will name it. With -w
        /// (an id), it reaches only that workspace.
        name: String,
    },
    /// This key and the keys it minted.
    Ls,
    /// A key for one workspace, for an agent there: its MCP URL and token.
    #[command(
        after_help = "Example:\n  tilo keys mint -w jane-doe --name \"Jane Doe's agent\"\n\n\
                            Hand the agent the URL and the token; it reaches that workspace (-w) and nothing \
                            else, until this key is revoked."
    )]
    Mint {
        /// How History and your grants page name the agent.
        #[arg(long)]
        name: Option<String>,
        /// Seconds until it expires; at most this key's own lifetime.
        #[arg(long)]
        expires_in: Option<u64>,
    },
    /// Revoke this key or one it minted, and every key it minted.
    Revoke {
        /// The grant, as `tilo keys ls` names it.
        grant: String,
    },
}

#[derive(Subcommand)]
pub enum AccountCmd {
    /// Your workspaces.
    #[command(subcommand)]
    Workspaces(WorkspacesCmd),
}

#[derive(Subcommand)]
pub enum WorkspacesCmd {
    /// The workspaces this key reaches, and the hosts you can create on.
    Ls,
    /// Create a workspace; the host may refuse (quota, payment).
    Create {
        name: String,
        #[arg(long)]
        host: Option<String>,
    },
    /// Delete a workspace.
    Rm { workspace: String },
}

#[derive(Subcommand)]
pub enum FilesCmd {
    /// Upload a file into a workspace: prints its reference, which actions
    /// take (`pdf-viewer#import-pdf-content content=tilo.content:…`).
    Put { file: PathBuf },
}

/// Where and as whom: the door, the key, and whether to answer JSON.
pub struct Client {
    door: String,
    key: String,
    json: bool,
    http: tilo_net::Client,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

fn door_of(door: Option<&str>) -> String {
    door.map(str::to_owned)
        .or_else(|| std::env::var(DOOR_ENV).ok().filter(|door| !door.is_empty()))
        .unwrap_or_else(|| DEFAULT_DOOR.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

impl Client {
    /// The client for `door`, with the key `TILO_API_KEY` holds.
    ///
    /// # Errors
    /// No key is set.
    pub fn from_env(door: Option<&str>, json: bool) -> Result<Self> {
        let key = std::env::var(KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "no key: set {KEY_ENV}. `tilo keys create \"<what will use it>\"` asks you for one"
                )
            })?;
        Ok(Self {
            door: door_of(door),
            key: key.trim().to_owned(),
            json,
            http: tilo_net::Client::new(),
        })
    }

    /// Open an MCP session at `path` (`/mcp`, or `/w/<workspace>/mcp`).
    async fn session(&self, path: &str) -> Result<Session<'_>> {
        let url = format!("{}{path}", self.door);
        let (status, headers, answer) = self
            .post(
                &url,
                None,
                &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                    "protocolVersion":"2025-06-18","capabilities":{},
                    "clientInfo":{"name":"tilo-cli","version":env!("CARGO_PKG_VERSION")}}}),
            )
            .await?;
        if status == 401 {
            bail!(
                "the door does not take this key: it expired or was revoked. `tilo keys create` asks for a new one"
            );
        }
        if status == 403 {
            bail!("this key does not reach that workspace");
        }
        if let Some(error) = answer.get("error") {
            bail!(
                "{}",
                error["message"].as_str().unwrap_or("the door refused")
            );
        }
        let id = headers
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the door answered HTTP {status} and opened no session"))?;
        let session = Session {
            client: self,
            url,
            id,
        };
        session
            .notify(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        Ok(session)
    }

    async fn post(
        &self,
        url: &str,
        session: Option<&str>,
        body: &Value,
    ) -> Result<(u16, tilo_net::HeaderMap, Value)> {
        let mut request = self
            .http
            .post(url)
            .bearer_auth(&self.key)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .timeout(Duration::from_secs(10 * 60))
            .body(body.to_string());
        if let Some(session) = session {
            request = request.header(SESSION_HEADER, session);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("reaching {}", self.door))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes_limited(MAX_ANSWER).await?;
        Ok((status, headers, last_message(&bytes)))
    }

    /// The workspace `named`: an id as it is, or the account's workspace of
    /// that name.
    async fn workspace(&self, named: &str) -> Result<String> {
        if is_uuid(named) {
            return Ok(named.to_owned());
        }
        let session = self.session("/mcp").await?;
        let listed = session
            .call("tilo_do", json!({"path": "account/workspaces#list"}))
            .await?;
        let wanted = slug(named);
        listed
            .pointer("/structuredContent/result/workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|workspace| {
                workspace["name"]
                    .as_str()
                    .is_some_and(|name| slug(name) == wanted)
            })
            .and_then(|workspace| workspace["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!("no workspace called {named:?}: `tilo account workspaces ls` names them")
            })
    }

    /// Print a tool's result: its text, or with `--json` its typed answer.
    fn show(&self, result: &Value) -> ExitCode {
        let failed = result.get("isError").and_then(Value::as_bool) == Some(true);
        if self.json {
            let typed = result.get("structuredContent").unwrap_or(result);
            println!(
                "{}",
                serde_json::to_string_pretty(typed).unwrap_or_default()
            );
        } else {
            let text = result
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if failed {
                eprintln!("{text}");
            } else {
                println!("{text}");
            }
        }
        if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// One MCP session at the door.
struct Session<'a> {
    client: &'a Client,
    url: String,
    id: String,
}

impl Session<'_> {
    async fn notify(&self, message: &Value) {
        let _ = self.client.post(&self.url, Some(&self.id), message).await;
    }

    /// Call `tool` with `arguments`, asking for the typed answer: the tool's
    /// result.
    async fn call(&self, tool: &str, arguments: Value) -> Result<Value> {
        let (status, _, answer) = self
            .client
            .post(
                &self.url,
                Some(&self.id),
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name": tool, "arguments": arguments,
                    "_meta": {"so.tilo.mcp": {"structured": true}}}}),
            )
            .await?;
        if let Some(error) = answer.get("error") {
            bail!(
                "{}",
                error["message"].as_str().unwrap_or("the workspace refused")
            );
        }
        answer
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("the door answered HTTP {status} with no result"))
    }

    async fn close(self) {
        let _ = self
            .client
            .http
            .delete(&self.url)
            .bearer_auth(&self.client.key)
            .header(SESSION_HEADER, &self.id)
            .send()
            .await;
    }
}

/// The JSON-RPC message an answer carries: the body, or an event stream's
/// last message.
fn last_message(bytes: &[u8]) -> Value {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        return value;
    }
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .next_back()
        .unwrap_or(Value::Null)
}

fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// A name as `-w` matches it: letters and digits, lower case, words joined
/// by `-` (`Applicant: Jane Doe` and `applicant-jane-doe` match).
fn slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// `name=value` options: each value as JSON when it is, else as text.
pub fn arguments(args: &[String]) -> Result<Value> {
    let mut object = serde_json::Map::new();
    for arg in args {
        let (name, value) = arg
            .split_once('=')
            .ok_or_else(|| anyhow!("{arg:?} is not name=value"))?;
        let value = serde_json::from_str::<Value>(value).unwrap_or_else(|_| json!(value));
        object.insert(name.to_owned(), value);
    }
    Ok(Value::Object(object))
}

/// Run `future` on a fresh runtime, printing an error as the command's.
fn run(future: impl std::future::Future<Output = Result<ExitCode>>) -> ExitCode {
    match runtime().and_then(|runtime| runtime.block_on(future)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tilo: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// `tilo do <path> name=value… -w <workspace>`.
pub fn act(
    door: Option<&str>,
    json: bool,
    workspace: &str,
    path: &str,
    args: &[String],
) -> ExitCode {
    run(async {
        let client = Client::from_env(door, json)?;
        let arguments = arguments(args)?;
        let workspace = client.workspace(workspace).await?;
        let session = client.session(&format!("/w/{workspace}/mcp")).await?;
        let result = session
            .call("tilo_do", json!({"path": path, "args": arguments}))
            .await;
        session.close().await;
        Ok(client.show(&result?))
    })
}

/// `tilo run <file.tl> -w <workspace>`: the program as one task, one step
/// for undo.
pub fn program(door: Option<&str>, json: bool, workspace: &str, file: &Path) -> ExitCode {
    run(async {
        let client = Client::from_env(door, json)?;
        let program =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let workspace = client.workspace(workspace).await?;
        let session = client.session(&format!("/w/{workspace}/mcp")).await?;
        let result = session.call("tilo", json!({"program": program})).await;
        session.close().await;
        Ok(client.show(&result?))
    })
}

/// `tilo account …`.
pub fn account(door: Option<&str>, json: bool, cmd: AccountCmd) -> ExitCode {
    run(async {
        let client = Client::from_env(door, json)?;
        let AccountCmd::Workspaces(cmd) = cmd;
        let (path, args) = match cmd {
            WorkspacesCmd::Ls => ("account/workspaces#list", json!({})),
            WorkspacesCmd::Create { name, host } => (
                "account/workspaces#create",
                json!({"name": name, "host": host}),
            ),
            WorkspacesCmd::Rm { workspace } => {
                let workspace = client.workspace(&workspace).await?;
                ("account/workspaces#delete", json!({"workspace": workspace}))
            }
        };
        let session = client.session("/mcp").await?;
        let result = session
            .call("tilo_do", json!({"path": path, "args": args}))
            .await;
        session.close().await;
        Ok(client.show(&result?))
    })
}

/// `tilo files …`.
pub fn files(door: Option<&str>, json: bool, workspace: &str, cmd: FilesCmd) -> ExitCode {
    run(async {
        let client = Client::from_env(door, json)?;
        let FilesCmd::Put { file } = cmd;
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        let workspace = client.workspace(workspace).await?;
        let response = client
            .http
            .put(format!("{}/w/{workspace}/content", client.door))
            .bearer_auth(&client.key)
            .timeout(Duration::from_secs(10 * 60))
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("reaching {}", client.door))?;
        let status = response.status().as_u16();
        let answer: Value =
            serde_json::from_slice(&response.bytes_limited(MAX_ANSWER).await?).unwrap_or_default();
        if !(200..300).contains(&status) {
            bail!(
                "{} {}",
                answer
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the upload failed"),
                answer
                    .pointer("/error/next_steps/0")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            );
        }
        if json {
            println!("{answer}");
        } else {
            println!(
                "{}",
                answer
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            );
        }
        Ok(ExitCode::SUCCESS)
    })
}

/// `tilo keys …`.
pub fn keys(door: Option<&str>, json: bool, workspace: Option<&str>, cmd: KeysCmd) -> ExitCode {
    run(async {
        let client = match cmd {
            KeysCmd::Create { name } => {
                return create_key(&door_of(door), json, &name, workspace).await;
            }
            _ => Client::from_env(door, json)?,
        };
        let (path, args) = match cmd {
            KeysCmd::Create { .. } => return Ok(ExitCode::FAILURE),
            KeysCmd::Ls => ("account/keys#list", json!({})),
            KeysCmd::Mint { name, expires_in } => {
                let workspace =
                    workspace.ok_or_else(|| anyhow!("name the workspace: -w <name or id>"))?;
                let workspace = client.workspace(workspace).await?;
                (
                    "account/keys#mint",
                    json!({"workspace": workspace, "name": name, "expires_in": expires_in}),
                )
            }
            KeysCmd::Revoke { grant } => ("account/keys#revoke", json!({"grant": grant})),
        };
        let session = client.session("/mcp").await?;
        let result = session
            .call("tilo_do", json!({"path": path, "args": args}))
            .await;
        session.close().await;
        Ok(client.show(&result?))
    })
}

/// Ask the person for a key and wait until they answer.
async fn create_key(
    door: &str,
    json: bool,
    name: &str,
    workspace: Option<&str>,
) -> Result<ExitCode> {
    let http = tilo_net::Client::new();
    let asked = http
        .post(format!("{door}/keys/new"))
        .header("content-type", "application/json")
        .body(json!({"name": name, "workspace": workspace}).to_string())
        .send()
        .await
        .with_context(|| format!("reaching {door}"))?;
    let status = asked.status().as_u16();
    let asked: Value =
        serde_json::from_slice(&asked.bytes_limited(MAX_ANSWER).await?).unwrap_or_default();
    if status != 201 {
        bail!(
            "{}",
            asked
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the door refused")
        );
    }
    let field = |name: &str| asked.get(name).and_then(Value::as_str).unwrap_or_default();
    let poll = field("poll").to_owned();
    let interval = Duration::from_secs(
        asked
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .max(1),
    );
    eprintln!(
        "Approve the key in your Tilo:\n\n  {}\n\nWaiting for you…",
        field("approve_url")
    );
    loop {
        tokio::time::sleep(interval).await;
        let answer = http
            .post(format!("{door}/keys/poll"))
            .header("content-type", "application/json")
            .body(json!({"poll": poll}).to_string())
            .send()
            .await
            .with_context(|| format!("reaching {door}"))?;
        let status = answer.status().as_u16();
        let answer: Value =
            serde_json::from_slice(&answer.bytes_limited(MAX_ANSWER).await?).unwrap_or_default();
        match status {
            202 => {}
            200 => {
                if json {
                    println!("{answer}");
                } else {
                    let key = answer
                        .get("key")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    eprintln!("Approved. Your key (shown once):");
                    println!("export {KEY_ENV}={key}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            _ => bail!(
                "{}",
                answer
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("no key was made")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_json_where_they_parse() {
        assert_eq!(
            arguments(&[
                "name=Jane Doe".into(),
                "count=3".into(),
                "ok=true".into(),
                "edit={\"op\":\"set\"}".into()
            ])
            .unwrap(),
            json!({"name": "Jane Doe", "count": 3, "ok": true, "edit": {"op": "set"}})
        );
        assert!(arguments(&["nothing".into()]).is_err());
    }

    #[test]
    fn a_workspace_is_named_by_its_words() {
        assert_eq!(slug("Applicant: Jane Doe"), "applicant-jane-doe");
        assert_eq!(slug("applicant-jane-doe"), "applicant-jane-doe");
        assert!(is_uuid("3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b"));
        assert!(!is_uuid("jane-doe"));
    }

    #[test]
    fn an_event_stream_answers_its_last_message() {
        let stream = b"event: message\ndata: {\"a\":1}\n\nevent: message\ndata: {\"b\":2}\n\n";
        assert_eq!(last_message(stream), json!({"b": 2}));
        assert_eq!(last_message(b"{\"c\":3}"), json!({"c": 3}));
    }
}
