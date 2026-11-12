//! Hosted workspaces from a shell, with an API key: `tilo keys`,
//! `tilo account workspaces`, `tilo do -w`, `tilo files` and `tilo run`.
//!
//! These make the very calls an agent makes through the API server's MCP
//! endpoints (`/mcp` for the account, `/w/<workspace>/mcp` for one workspace),
//! asking for the typed answer, so a script sees what an agent sees. Making
//! workspaces and keys are actions on the account (`account/workspaces#create`,
//! `account/keys#mint`). A file goes in as one upload (`PUT
//! /w/<workspace>/content`), and the reference it answers is what actions take.
//!
//! The key comes from `TILO_API_KEY`, and the API server from `--api` or
//! `TILO_API_URL` (`https://mcp.tilo.sh`). `tilo keys create` gets a key: it
//! asks the person, who approves it in their Tilo.

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// The API server hosted Tilo answers at.
pub const DEFAULT_API_URL: &str = "https://mcp.tilo.sh";
pub const API_URL_ENV: &str = "TILO_API_URL";
pub const KEY_ENV: &str = "TILO_API_KEY";
const SESSION_HEADER: &str = "mcp-session-id";
const MAX_ANSWER: usize = 64 * 1024 * 1024;
/// How long a call keeps asking again while the host is busy with the
/// person's changes.
const RETRY_DEADLINE: Duration = Duration::from_secs(60);

#[derive(Subcommand)]
pub enum KeysCmd {
    /// Ask for a key: you approve it in your Tilo, and it prints here.
    #[command(
        after_help = "Example:\n  tilo keys create \"Visa backend\"\n  export TILO_API_KEY=tilo_sk_…\n\n\
                      A key reaching all your workspaces lasts until you revoke it (`tilo keys revoke`), \
                      unless --expires-in says when it expires. A key reaching one workspace (-w) lasts \
                      at most 89 days. It operates web pages in Browser unless --no-browser-pages."
    )]
    Create {
        /// What will use it, as your grants page will name it. With -w
        /// (an id), it reaches only that workspace.
        name: String,
        /// Seconds until it expires; without it, a key reaching all your
        /// workspaces lasts until revoked.
        #[arg(long)]
        expires_in: Option<u64>,
        /// The key may not operate web pages in Browser: every page action
        /// asks you first. Without it, it navigates, fills in, selects and
        /// clicks without asking, and submissions ask you anyway.
        #[arg(long)]
        no_browser_pages: bool,
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
pub enum MembersCmd {
    /// A link that brings one more person into the workspace (-w) as --role.
    #[command(
        after_help = "Example:\n  tilo members invite --role editor --email ops@example.com -w jane-doe\n\n\
                      Following the link signs them in and lands them in the workspace. A key you \
                      created invites into workspaces you administer, as an editor at most. No mail \
                      is sent: hand the link over yourself."
    )]
    Invite {
        /// viewer, commenter or editor.
        #[arg(long, default_value = "viewer")]
        role: String,
        /// Their address, which the invitation names.
        #[arg(long)]
        email: Option<String>,
    },
    /// The people in the workspace (-w), with their roles, and the
    /// invitations still open.
    Ls,
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
        /// What to call it, as your workspaces list will name it.
        name: String,
        /// The host to create it on, by name; the default host without one.
        #[arg(long)]
        host: Option<String>,
    },
    /// Delete a workspace of yours with everything in it, and revoke its
    /// minted keys: final, there is no undo.
    Rm { workspace: String },
}

#[derive(Subcommand)]
pub enum FilesCmd {
    /// Upload a file into a workspace: prints its reference, which actions
    /// take (`pdf-viewer#import-pdf-content content=tilo.content:…`).
    Put {
        /// The file to upload, by its path here.
        file: PathBuf,
    },
    /// Download a file of a workspace by its reference (an export's
    /// answer, `tilo.content:…`) into `--output`, or to standard output.
    Get {
        /// The reference to fetch, as an action or an upload answered it.
        content: String,
        /// Where to write it; standard output when absent.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

/// Where and as whom: the api, the key, and whether to answer JSON.
pub struct Client {
    api: String,
    key: String,
    json: bool,
    http: tilo_net::Client,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

fn api_url_of(api: Option<&str>) -> String {
    api.map(str::to_owned)
        .or_else(|| {
            std::env::var(API_URL_ENV)
                .ok()
                .filter(|api| !api.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_API_URL.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

impl Client {
    /// The client for `api`, with the key `TILO_API_KEY` holds.
    ///
    /// # Errors
    /// No key is set.
    pub fn from_env(api: Option<&str>, json: bool) -> Result<Self> {
        let key = std::env::var(KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "no key: set {KEY_ENV}. `tilo keys create \"<what will use it>\"` asks you for one"
                )
            })?;
        Ok(Self {
            api: api_url_of(api),
            key: key.trim().to_owned(),
            json,
            http: tilo_net::Client::new(),
        })
    }

    /// Open an MCP session at `path` (`/mcp`, or `/w/<workspace>/mcp`).
    async fn session(&self, path: &str) -> Result<Session<'_>> {
        let url = format!("{}{path}", self.api);
        let (status, headers, answer) = self
            .post_patiently(
                &url,
                None,
                &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                    "protocolVersion":"2025-06-18","capabilities":{},
                    "clientInfo":{"name":"tilo-cli","version":env!("CARGO_PKG_VERSION")}}}),
            )
            .await?;
        if status == 401 {
            bail!(
                "the API server does not take this key: it expired or was revoked. `tilo keys create` asks for a new one"
            );
        }
        if status == 403 {
            bail!("this key does not reach that workspace");
        }
        if let Some(error) = answer.get("error") {
            bail!(
                "{}",
                error["message"]
                    .as_str()
                    .unwrap_or("the API server refused")
            );
        }
        let id = headers
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!("the API server answered HTTP {status} and opened no session")
            })?;
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

    /// [`Self::post`], asked again while the API server answers that the host is
    /// busy with the person's changes (it ran nothing): after the wait it
    /// says, with jitter, for at most [`RETRY_DEADLINE`]. Past it, the busy
    /// answer stands.
    async fn post_patiently(
        &self,
        url: &str,
        session: Option<&str>,
        body: &Value,
    ) -> Result<(u16, tilo_net::HeaderMap, Value)> {
        let deadline = std::time::Instant::now().checked_add(RETRY_DEADLINE);
        let mut attempt = 0_u32;
        loop {
            let answered = self.post(url, session, body).await?;
            let Some(wait) = busy_wait(&answered.2) else {
                return Ok(answered);
            };
            let floor = Duration::from_millis(50 << attempt.min(5)).min(Duration::from_secs(2));
            let delay = wait.max(floor).mul_f64(1.0 + jitter() / 2.0);
            let late = deadline.is_none_or(|deadline| {
                std::time::Instant::now()
                    .checked_add(delay)
                    .is_none_or(|ready| ready > deadline)
            });
            if late {
                return Ok(answered);
            }
            attempt = attempt.saturating_add(1);
            tokio::time::sleep(delay).await;
        }
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
            .with_context(|| format!("reaching {}", self.api))?;
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

/// One MCP session at the api.
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
            .post_patiently(
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
            .ok_or_else(|| anyhow!("the API server answered HTTP {status} with no result"))
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

/// How long the API server asks to wait before the same call works, when `answer`
/// says the host was busy with the person's changes and ran nothing: a
/// JSON-RPC error's `data`, or a tool's typed failure.
fn busy_wait(answer: &Value) -> Option<Duration> {
    let said = [
        answer.pointer("/error/data"),
        answer.pointer("/result/structuredContent/error"),
    ]
    .into_iter()
    .flatten()
    .find(|fields| fields["code"] == "rate_limited")?;
    Some(Duration::from_millis(
        said["retry_after_ms"].as_u64().unwrap_or(1000),
    ))
}

/// A fraction in `[0, 1)` that differs between callers asking at once.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
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
pub(crate) fn run(future: impl std::future::Future<Output = Result<ExitCode>>) -> ExitCode {
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
    api: Option<&str>,
    json: bool,
    workspace: &str,
    path: &str,
    args: &[String],
) -> ExitCode {
    run(async {
        let client = Client::from_env(api, json)?;
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
pub fn program(api: Option<&str>, json: bool, workspace: &str, file: &Path) -> ExitCode {
    run(async {
        let client = Client::from_env(api, json)?;
        let program =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let workspace = client.workspace(workspace).await?;
        let session = client.session(&format!("/w/{workspace}/mcp")).await?;
        let result = session.call("tilo", json!({"program": program})).await;
        session.close().await;
        Ok(client.show(&result?))
    })
}

/// The surfaces of the apps installed in `workspace`: each app's `surface.toml`, as the
/// workspace's own read of the app answers it (`tilo sdk gen -w`).
pub fn surfaces(api: Option<&str>, workspace: &str) -> Result<Vec<crate::sdk::App>> {
    runtime()?.block_on(read_surfaces(api, workspace))
}

/// [`surfaces`], over the workspace's MCP endpoint.
async fn read_surfaces(api: Option<&str>, workspace: &str) -> Result<Vec<crate::sdk::App>> {
    let client = Client::from_env(api, false)?;
    let workspace = client.workspace(workspace).await?;
    let session = client.session(&format!("/w/{workspace}/mcp")).await?;
    let apps = installed(&session).await;
    session.close().await;
    apps
}

/// The apps `session`'s workspace lists, each with the surface its read of the app answers.
/// An app whose surface the workspace does not hold (one that has not run there) is left out,
/// and said so.
async fn installed(session: &Session<'_>) -> Result<Vec<crate::sdk::App>> {
    let listing = session.call("tilo_ls", json!({})).await?;
    let rows = listing
        .pointer("/structuredContent/rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut apps = Vec::new();
    for row in rows {
        if row.get("kind").and_then(Value::as_str) != Some("app") {
            continue;
        }
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let read = session.call("tilo_read", json!({"path": path})).await?;
        let typed = read.get("structuredContent").unwrap_or(&Value::Null);
        let Some(surface) = typed.get("surface").and_then(Value::as_str) else {
            eprintln!(
                "tilo: {path} publishes no surface in that workspace: no app of it has run there"
            );
            continue;
        };
        apps.push(crate::sdk::App {
            id: typed
                .get("app")
                .and_then(Value::as_str)
                .unwrap_or(path)
                .to_owned(),
            title: row
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            surface: surface.to_owned(),
        });
    }
    Ok(apps)
}

/// `tilo account …`.
pub fn account(api: Option<&str>, json: bool, cmd: AccountCmd) -> ExitCode {
    run(async {
        let client = Client::from_env(api, json)?;
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
pub fn files(api: Option<&str>, json: bool, workspace: &str, cmd: FilesCmd) -> ExitCode {
    run(async {
        let client = Client::from_env(api, json)?;
        let file = match cmd {
            FilesCmd::Put { file } => file,
            FilesCmd::Get { content, output } => {
                return download(&client, workspace, &content, output.as_deref()).await;
            }
        };
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        let workspace = client.workspace(workspace).await?;
        let response = client
            .http
            .put(format!("{}/w/{workspace}/content", client.api))
            .bearer_auth(&client.key)
            .timeout(Duration::from_secs(10 * 60))
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("reaching {}", client.api))?;
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

/// The bytes of `content` in `workspace`, a piece per `tilo_get`, written
/// to `output` or standard output.
async fn download(
    client: &Client,
    workspace: &str,
    content: &str,
    output: Option<&Path>,
) -> Result<ExitCode> {
    use base64::Engine as _;
    use std::io::Write as _;
    let workspace = client.workspace(workspace).await?;
    let session = client.session(&format!("/w/{workspace}/mcp")).await?;
    let mut bytes = Vec::new();
    let fetched: Result<()> = async {
        loop {
            let offset = bytes.len();
            let result = session
                .call("tilo_get", json!({"content": content, "offset": offset}))
                .await?;
            let piece = result.get("structuredContent").unwrap_or(&Value::Null);
            if result.get("isError").and_then(Value::as_bool) == Some(true) {
                bail!(
                    "{}",
                    piece
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .or_else(|| result.pointer("/content/0/text").and_then(Value::as_str))
                        .unwrap_or("the workspace refused the download")
                );
            }
            let (Some(encoded), Some(size)) = (
                piece.get("bytes").and_then(Value::as_str),
                piece.get("size").and_then(Value::as_u64),
            ) else {
                bail!("the API server answered no bytes for {content}");
            };
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .context("the API server's bytes are not base64")?;
            if decoded.is_empty() && (bytes.len() as u64) < size {
                bail!("the download stopped at {} of {size} bytes", bytes.len());
            }
            bytes.extend_from_slice(&decoded);
            if bytes.len() as u64 >= size {
                return Ok(());
            }
        }
    }
    .await;
    session.close().await;
    fetched?;
    match output {
        Some(path) => {
            std::fs::write(path, &bytes).with_context(|| format!("writing {}", path.display()))?;
            if client.json {
                println!(
                    "{}",
                    json!({"content": content, "size": bytes.len(), "file": path})
                );
            }
        }
        None => std::io::stdout()
            .write_all(&bytes)
            .context("writing to standard output")?,
    }
    Ok(ExitCode::SUCCESS)
}

/// `tilo members … -w <workspace>`: the workspace's `workspace/members`
/// actions, as `tilo do` runs any action there.
pub fn members(api: Option<&str>, json: bool, workspace: &str, cmd: MembersCmd) -> ExitCode {
    let (path, args) = match cmd {
        MembersCmd::Invite { role, email } => (
            "workspace/members#invite",
            std::iter::once(format!("role={role}"))
                .chain(email.map(|email| format!("email={email}")))
                .collect(),
        ),
        MembersCmd::Ls => ("workspace/members#list", Vec::new()),
    };
    act(api, json, workspace, path, &args)
}

/// `tilo keys …`.
pub fn keys(api: Option<&str>, json: bool, workspace: Option<&str>, cmd: KeysCmd) -> ExitCode {
    run(async {
        let client = match cmd {
            KeysCmd::Create {
                name,
                expires_in,
                no_browser_pages,
            } => {
                return create_key(
                    &api_url_of(api),
                    json,
                    &name,
                    workspace,
                    expires_in,
                    !no_browser_pages,
                )
                .await;
            }
            _ => Client::from_env(api, json)?,
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
    api: &str,
    json: bool,
    name: &str,
    workspace: Option<&str>,
    expires_in: Option<u64>,
    browser_pages: bool,
) -> Result<ExitCode> {
    let http = tilo_net::Client::new();
    let asked = http
        .post(format!("{api}/keys/new"))
        .header("content-type", "application/json")
        .body(
            json!({
                "name": name,
                "workspace": workspace,
                "expires_in": expires_in,
                "browser_pages": browser_pages,
            })
            .to_string(),
        )
        .send()
        .await
        .with_context(|| format!("reaching {api}"))?;
    let status = asked.status().as_u16();
    let asked: Value =
        serde_json::from_slice(&asked.bytes_limited(MAX_ANSWER).await?).unwrap_or_default();
    if status != 201 {
        bail!(
            "{}",
            asked
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the API server refused")
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
            .post(format!("{api}/keys/poll"))
            .header("content-type", "application/json")
            .body(json!({"poll": poll}).to_string())
            .send()
            .await
            .with_context(|| format!("reaching {api}"))?;
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
    fn a_busy_host_is_read_off_either_answer() {
        let typed = json!({"result": {"structuredContent": {"status": "failed",
            "error": {"code": "rate_limited", "retry_after_ms": 480}}}});
        assert_eq!(busy_wait(&typed), Some(Duration::from_millis(480)));
        let entering = json!({"error": {"code": -32000, "message": "busy",
            "data": {"code": "rate_limited", "retry_after_ms": 20}}});
        assert_eq!(busy_wait(&entering), Some(Duration::from_millis(20)));
        let other = json!({"result": {"structuredContent": {"status": "failed",
            "error": {"code": "refused"}}}});
        assert_eq!(busy_wait(&other), None);
    }

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
