//! Hosted workspaces from a shell: `tilo keys`, `tilo account workspaces`,
//! `tilo do -w`, `tilo files` and `tilo run`.
//!
//! Each command is one call of Tilo's API (`/v1`, `docs/developers/api.md`),
//! made by [`crate::api::Client`]: a workspace and its people, keys, programs
//! and files are routes there, and an action is `POST
//! …/resources/{path}/actions/{action}` with its arguments as the body. The
//! key is `TILO_API_KEY` (at the server `--api` or `TILO_API_URL` names), a
//! key file `--key-file` names, or, when this computer logged in (`tilo
//! login`), its own device key, in process.
//!
//! `tilo keys create` is the one command that asks the person: it waits at
//! the API server's own key page until they approve one in their Tilo.

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::Value;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::api::{self, Client, Reach};

/// The most one answer may be.
const MAX_ANSWER: usize = 64 * 1024 * 1024;

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
    /// Write this computer's own key as a key file, for a program here.
    #[command(after_help = "Example:\n  tilo keys export --out visa.json\n\n\
                      It writes the key `tilo login` gave this computer, with its certificate and the \
                      host it reaches: a key file `tilo --key-file visa.json …` acts with, and the API \
                      in process reads, with no server between.\n\n\
                      One key serves one program at a time: a host keeps one session per key, so two \
                      programs sharing it cut each other off. A separate program wants its own key.")]
    Export {
        /// Where to write it, readable by this account only.
        #[arg(long)]
        out: PathBuf,
        /// What the host's people see it as; this computer's own name
        /// without it.
        #[arg(long)]
        name: Option<String>,
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
    /// The workspaces this key reaches.
    Ls,
    /// Create a workspace; the host may refuse (quota, payment).
    Create {
        /// What to call it, as your workspaces list will name it.
        name: String,
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

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
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

/// A field of `value` as text: empty when it is not there.
fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// `value` as JSON on one line.
fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Print `typed` as the API gave it when `json`, else `said`.
fn show(json: bool, typed: &Value, said: &str) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(typed).unwrap_or_default()
        );
    } else {
        println!("{said}");
    }
    ExitCode::SUCCESS
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

/// The workspace `named`: an id as it is, or the account's workspace of
/// that name.
async fn workspace_id(client: &Client, named: &str) -> Result<String> {
    if is_uuid(named) {
        return Ok(named.to_owned());
    }
    let listed = client.get("/v1/workspaces").await?;
    let wanted = slug(named);
    listed
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|workspace| {
            workspace
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| slug(name) == wanted)
        })
        .and_then(|workspace| workspace.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("no workspace called {named:?}: `tilo account workspaces ls` names them")
        })
}

/// One workspace as a line: its name, its id, and where it lives.
fn workspace_line(row: &Value) -> String {
    let mut line = format!("{} (id {}", text(row, "name"), text(row, "id"));
    if let Some(host) = row.get("host").and_then(Value::as_str) {
        let _ = write!(line, ", on {host}");
    }
    if let Some(role) = row.get("role").and_then(Value::as_str) {
        let _ = write!(line, ", {role}");
    }
    line.push(')');
    line
}

/// `row`'s `field` as a date, or `none` when it is not there.
fn when(row: &Value, field: &str, none: &str) -> String {
    row.get(field)
        .and_then(Value::as_u64)
        .map_or_else(|| none.to_owned(), tilo_api::pages::date)
}

/// `tilo do <path> name=value… -w <workspace>`: one action of a hosted
/// workspace, as `app/resource#action`.
pub fn act(reach: Reach<'_>, workspace: &str, path: &str, args: &[String]) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let Some((resource, action)) = path.rsplit_once('#') else {
            bail!("{path:?} names no action: write it as app/resource#action, like notes#open");
        };
        let workspace = workspace_id(&client, workspace).await?;
        let arguments = arguments(args)?;
        let answer = client
            .act(&workspace, resource, action, &arguments, path)
            .await?;
        Ok(show(reach.json, &answer, &action_text(&answer)))
    })
}

/// An action's answer as the lines a person reads.
fn action_text(answer: &Value) -> String {
    let path = text(answer, "path");
    // An action names the resource it resolved to; one that names none says
    // its status alone.
    let named = |word: &str| {
        if path.is_empty() {
            word.to_owned()
        } else {
            format!("{word} {path}")
        }
    };
    let mut said = match text(answer, "status") {
        "ok" => named("ok"),
        "pending" => {
            let mut said = format!("pending {}: it waits for you", text(answer, "run"));
            if let Some(url) = answer.pointer("/ask/url").and_then(Value::as_str) {
                let _ = write!(said, ", at {url}");
            }
            if !path.is_empty() {
                let _ = write!(said, " ({path})");
            }
            said
        }
        other => named(other),
    };
    if let Some(result) = answer.get("result").filter(|result| !result.is_null()) {
        let _ = write!(said, "\n{}", compact(result));
    }
    let links = answer
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(answer.get("link").and_then(Value::as_str));
    for link in links {
        let _ = write!(said, "\n{link}");
    }
    said
}

/// `tilo run <file.tl> -w <workspace>`: the program as one task, one step
/// for undo.
pub fn program(reach: Reach<'_>, workspace: &str, file: &Path) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let source =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let workspace = workspace_id(&client, workspace).await?;
        let title = format!("tilo run {}", file.display());
        let answer = client.run_program(&workspace, &source, &title).await?;
        Ok(show(reach.json, &answer, &program_text(&answer)))
    })
}

/// A program's answer: its lines, and what it changed.
fn program_text(answer: &Value) -> String {
    let mut said = String::new();
    for line in answer
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        said.push_str(line);
        said.push('\n');
    }
    match text(answer, "status") {
        "preview" => said.push_str(
            "this program would change much or destroy something, so nothing ran: run it again \
             when you mean it",
        ),
        "failed" => said.push_str("the program failed"),
        _ => match answer.get("changed").and_then(Value::as_u64) {
            Some(1) => said.push_str("changed 1 step"),
            Some(changed) => {
                let _ = write!(said, "changed {changed} steps");
            }
            None => said.push_str("changed nothing"),
        },
    }
    said
}

/// The surfaces of the apps installed in `workspace`: each app's
/// `surface.toml`, as the workspace's own read of the app answers it
/// (`tilo sdk gen -w`).
pub fn surfaces(reach: Reach<'_>, workspace: &str) -> Result<Vec<crate::sdk::App>> {
    let client = Client::open(reach)?;
    runtime()?.block_on(read_surfaces(&client, workspace))
}

async fn read_surfaces(client: &Client, workspace: &str) -> Result<Vec<crate::sdk::App>> {
    let workspace = workspace_id(client, workspace).await?;
    let listing = client
        .get(&format!("/v1/workspaces/{workspace}/resources"))
        .await?;
    let mut apps = Vec::new();
    for row in listing
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if text(row, "kind") != "app" {
            continue;
        }
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let read = client
            .get(&format!(
                "/v1/workspaces/{workspace}/resources/{}",
                api::encode_path(path)
            ))
            .await?;
        let Some(surface) = read.get("surface").and_then(Value::as_str) else {
            eprintln!(
                "tilo: {path} publishes no surface in that workspace: no app of it has run there"
            );
            continue;
        };
        apps.push(crate::sdk::App {
            id: read
                .get("app")
                .and_then(Value::as_str)
                .filter(|app| !app.is_empty())
                .unwrap_or(path)
                .to_owned(),
            title: text(row, "label").to_owned(),
            surface: surface.to_owned(),
        });
    }
    Ok(apps)
}

/// `tilo account …`.
pub fn account(reach: Reach<'_>, cmd: AccountCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let AccountCmd::Workspaces(cmd) = cmd;
        match cmd {
            WorkspacesCmd::Ls => {
                let listed = client.get("/v1/workspaces").await?;
                let rows = listed
                    .get("workspaces")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(workspace_line)
                    .collect::<Vec<_>>();
                let said = if rows.is_empty() {
                    "no workspaces".to_owned()
                } else {
                    rows.join("\n")
                };
                Ok(show(reach.json, &listed, &said))
            }
            WorkspacesCmd::Create { name } => {
                let made = client
                    .post("/v1/workspaces", &serde_json::json!({"name": name}))
                    .await?;
                Ok(show(reach.json, &made, &workspace_line(&made)))
            }
            WorkspacesCmd::Rm { workspace: named } => {
                let id = workspace_id(&client, &named).await?;
                let deleted = client.delete(&format!("/v1/workspaces/{id}")).await?;
                let mut said = format!("deleted {}", text(&deleted, "deleted"));
                let revoked: Vec<&str> = deleted
                    .get("revoked_keys")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !revoked.is_empty() {
                    let _ = write!(said, "; revoked {}", revoked.join(", "));
                }
                Ok(show(reach.json, &deleted, &said))
            }
        }
    })
}

/// `tilo files …`.
pub fn files(reach: Reach<'_>, workspace: &str, cmd: FilesCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = workspace_id(&client, workspace).await?;
        match cmd {
            FilesCmd::Put { file } => {
                let bytes =
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
                let put = client.upload(&workspace, bytes).await?;
                Ok(show(reach.json, &put, text(&put, "content")))
            }
            FilesCmd::Get { content, output } => {
                let bytes = client.content(&workspace, &content).await?;
                write_content(reach.json, &content, &bytes, output.as_deref())
            }
        }
    })
}

/// The bytes of `content`, written to `output` or standard output.
fn write_content(
    json: bool,
    content: &str,
    bytes: &[u8],
    output: Option<&Path>,
) -> Result<ExitCode> {
    match output {
        Some(path) => {
            std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"content": content, "size": bytes.len(), "file": path})
                );
            }
        }
        None => std::io::stdout()
            .write_all(bytes)
            .context("writing to standard output")?,
    }
    Ok(ExitCode::SUCCESS)
}

/// `tilo members … -w <workspace>`: the workspace's people, and inviting one
/// more.
pub fn members(reach: Reach<'_>, workspace: &str, cmd: MembersCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = workspace_id(&client, workspace).await?;
        match cmd {
            MembersCmd::Ls => {
                let listed = client
                    .get(&format!("/v1/workspaces/{workspace}/members"))
                    .await?;
                Ok(show(reach.json, &listed, &members_text(&listed)))
            }
            MembersCmd::Invite { role, email } => {
                let mut body = serde_json::json!({"role": role});
                if let (Some(fields), Some(email)) = (body.as_object_mut(), email) {
                    fields.insert("email".into(), Value::String(email));
                }
                let invited = client
                    .post(
                        &format!("/v1/workspaces/{workspace}/members/invitations"),
                        &body,
                    )
                    .await?;
                let mut said = text(&invited, "link").to_owned();
                if let Some(next) = invited.get("next").and_then(Value::as_str) {
                    let _ = write!(said, "\n{next}");
                }
                Ok(show(reach.json, &invited, &said))
            }
        }
    })
}

/// A workspace's people and its open invitations, as lines.
fn members_text(listed: &Value) -> String {
    let members = listed
        .get("members")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut said = String::from("members:");
    for member in &members {
        let mut what = text(member, "role").to_owned();
        for (field, word) in [("owner", "owner"), ("administrator", "administrator")] {
            if member.get(field).and_then(Value::as_bool) != Some(true) || what.contains(word) {
                continue;
            }
            if what.is_empty() {
                what.push_str(word);
            } else {
                let _ = write!(what, ", {word}");
            }
        }
        let _ = write!(said, "\n  {} — {what}", text(member, "name"));
    }
    if members.is_empty() {
        said.push_str("\n  nobody yet");
    }
    let invitations = listed
        .get("invitations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if invitations.is_empty() {
        said.push_str("\ninvitations: none open");
        return said;
    }
    said.push_str("\ninvitations:");
    for invitation in &invitations {
        let _ = write!(
            said,
            "\n  {} invitation {}, {}",
            text(invitation, "role"),
            text(invitation, "status"),
            text(invitation, "id")
        );
    }
    said
}

/// `tilo keys …`.
pub fn keys(reach: Reach<'_>, workspace: Option<&str>, cmd: KeysCmd) -> ExitCode {
    run(async {
        match cmd {
            KeysCmd::Create {
                name,
                expires_in,
                no_browser_pages,
            } => {
                create_key(
                    reach.api,
                    reach.json,
                    &name,
                    workspace,
                    expires_in,
                    !no_browser_pages,
                )
                .await
            }
            KeysCmd::Export { out, name } => export(&out, name.as_deref(), reach.json),
            KeysCmd::Ls => {
                let listed = Client::open(reach)?.get("/v1/keys").await?;
                let rows = listed
                    .get("keys")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|key| {
                        format!(
                            "{} (grant {}, reaches {}, last used {}, expires {})",
                            text(key, "name"),
                            text(key, "grant"),
                            text(key, "scope"),
                            when(key, "last_used_at_ms", "never"),
                            when(key, "expires_at_ms", "never"),
                        )
                    })
                    .collect::<Vec<_>>();
                let said = if rows.is_empty() {
                    "no keys".to_owned()
                } else {
                    rows.join("\n")
                };
                Ok(show(reach.json, &listed, &said))
            }
            KeysCmd::Mint { name, expires_in } => {
                let Some(named) = workspace else {
                    bail!("name the workspace: -w <name or id>");
                };
                let client = Client::open(reach)?;
                let id = workspace_id(&client, named).await?;
                let mut body = serde_json::json!({"workspace": id});
                if let (Some(fields), Some(name)) = (body.as_object_mut(), name) {
                    fields.insert("name".into(), Value::String(name));
                }
                if let (Some(fields), Some(expires_in)) = (body.as_object_mut(), expires_in) {
                    fields.insert("expires_in".into(), serde_json::json!(expires_in));
                }
                let minted = client.post("/v1/keys", &body).await?;
                let said = format!(
                    "{} (grant {}, reaches {}, expires {})\n  MCP {}\n  key {} (shown once)",
                    text(&minted, "name"),
                    text(&minted, "grant"),
                    text(&minted, "workspace"),
                    when(&minted, "expires_at_ms", "never"),
                    text(&minted, "mcp_url"),
                    text(&minted, "token"),
                );
                Ok(show(reach.json, &minted, &said))
            }
            KeysCmd::Revoke { grant } => {
                let revoked = Client::open(reach)?
                    .delete(&format!("/v1/keys/{}", api::encode_path(&grant)))
                    .await?;
                Ok(show(
                    reach.json,
                    &revoked,
                    &format!("revoked {}", text(&revoked, "revoked")),
                ))
            }
        }
    })
}

/// `tilo keys export --out <path>`: this computer's own key, as the key file
/// a program here hands to the in-process API, readable by this account only.
fn export(path: &Path, name: Option<&str>, json: bool) -> Result<ExitCode> {
    let Some(file) = api::login_key(name)? else {
        bail!(
            "this computer is not logged in: `tilo login` makes it one of your devices, and its \
             key is what this writes"
        );
    };
    write_key_file(path, &file)?;
    let said = format!("{}: {}, reaching {}", path.display(), file.name, file.host);
    let typed = serde_json::json!({"file": path, "name": file.name, "host": file.host});
    Ok(show(json, &typed, &said))
}

/// Write `file` at `path`, readable by this account only, replacing what is
/// there.
fn write_key_file(path: &Path, file: &tilo_api::direct::KeyFile) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(path);
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    out.write_all(&serde_json::to_vec_pretty(file)?)
        .with_context(|| format!("writing {}", path.display()))?;
    out.sync_all()
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Ask the person for a key and wait until they answer.
async fn create_key(
    api: Option<&str>,
    json: bool,
    name: &str,
    workspace: Option<&str>,
    expires_in: Option<u64>,
    browser_pages: bool,
) -> Result<ExitCode> {
    let api = api::api_url(api);
    let http = tilo_net::Client::new();
    let asked = http
        .post(format!("{api}/keys/new"))
        .header("content-type", "application/json")
        .body(
            serde_json::json!({
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
            .body(serde_json::json!({"poll": poll}).to_string())
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
                    eprintln!("Approved. Your key (shown once):");
                    println!("export {}={}", api::KEY_ENV, text(&answer, "key"));
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

/// `name=value` options: each value as JSON when it is, else as text.
pub fn arguments(args: &[String]) -> Result<Value> {
    let mut object = serde_json::Map::new();
    for arg in args {
        let (name, value) = arg
            .split_once('=')
            .ok_or_else(|| anyhow!("{arg:?} is not name=value"))?;
        let value = serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::from(value));
        object.insert(name.to_owned(), value);
    }
    Ok(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    /// The workspace the fake API answers with.
    const WORKSPACE: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

    /// What the fake API answers for one request.
    fn answer(request: &test_http::Request) -> Option<test_http::Reply> {
        let json_reply = |status: u16, body: Value| {
            Some(
                test_http::Reply::status(status)
                    .header("content-type", "application/json")
                    .body(body.to_string()),
            )
        };
        let route = request.target.as_str();
        match (request.method.as_str(), route) {
            ("GET", "/v1/workspaces") => json_reply(
                200,
                json!({"workspaces": [{"id": WORKSPACE, "name": "Applicant: Jane Doe",
                    "host": "eu1.tilo.sh", "role": "owner"}]}),
            ),
            ("POST", "/v1/workspaces") => json_reply(
                201,
                json!({"id": WORKSPACE, "name": "Applicant: Jane Doe", "host": "eu1.tilo.sh",
                    "role": "owner"}),
            ),
            ("POST", route) if route.ends_with("/uploads") => json_reply(
                201,
                json!({"content": "tilo.content:abc", "size": request.body.len()}),
            ),
            ("POST", route) if route.ends_with("/actions/open") => json_reply(
                200,
                json!({"status": "ok", "path": "pdf-viewer#open", "run": "run-1"}),
            ),
            ("GET", route) if route.contains("/content/") => {
                Some(test_http::Reply::status(200).body(b"hello".to_vec()))
            }
            _ => json_reply(
                404,
                json!({"error": {"code": "not_found", "message": "no such route"}}),
            ),
        }
    }

    /// A fake API server that answers the routes above, and keeps what it
    /// was sent.
    fn fake_api() -> test_http::Responder {
        test_http::Responder::handle(answer)
    }

    /// Every command below reaches the same key, at whatever server its
    /// `Reach` names.
    fn keyed() -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(api::KEY_ENV, "tilo_sk_test");
        env
    }

    /// The hosted commands are the API's routes: every one of them a call of
    /// `docs/developers/api.md`, with the headers it takes.
    #[test]
    fn the_hosted_commands_are_the_apis_routes() {
        let _key = keyed();
        let api_server = fake_api();
        let url = api_server.url();
        let reach = Reach {
            api: Some(&url),
            key_file: None,
            json: false,
        };

        // A workspace is made as `POST /v1/workspaces`, once per idempotency key.
        account(
            reach,
            AccountCmd::Workspaces(WorkspacesCmd::Create {
                name: "Applicant: Jane Doe".into(),
            }),
        );
        let made = api_server.drain_requests();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].method, "POST");
        assert_eq!(made[0].target, "/v1/workspaces");
        assert_eq!(made[0].header("authorization"), Some("Bearer tilo_sk_test"));
        assert!(
            made[0].header("idempotency-key").is_some(),
            "a call that makes something names its invocation"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&made[0].body).unwrap(),
            json!({"name": "Applicant: Jane Doe"})
        );

        // A file goes in as one upload, and the reference it answers is what
        // the command prints.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("passport.png");
        std::fs::write(&file, b"hello").unwrap();
        files(reach, "applicant-jane-doe", FilesCmd::Put { file });
        let put = api_server.drain_requests();
        let upload = put.last().expect("the upload");
        assert_eq!(upload.target, format!("/v1/workspaces/{WORKSPACE}/uploads"));
        assert_eq!(upload.method, "POST");
        assert_eq!(upload.body, b"hello");
        assert_eq!(
            upload.header("content-type"),
            Some("application/octet-stream")
        );
        assert!(upload.header("idempotency-key").is_some());

        // An action is `POST …/resources/{path}/actions/{action}`: its
        // arguments are the body, and its run is named.
        act(reach, "applicant-jane-doe", "pdf-viewer#open", &[]);
        let acted = api_server.drain_requests();
        assert_eq!(
            acted.len(),
            2,
            "the workspace is named by a read of the list, then the action runs"
        );
        let action = acted.last().expect("the action");
        assert_eq!(
            action.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/pdf-viewer/actions/open")
        );
        assert_eq!(action.header("prefer"), Some("wait=60"));
        assert_eq!(action.header("tilo-run-title"), Some("pdf-viewer#open"));
        assert!(
            action
                .header("tilo-run")
                .is_some_and(|run| run == action.header("idempotency-key").unwrap_or_default()),
            "one invocation, one run, one idempotency key"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&action.body).unwrap(),
            json!({})
        );

        // A file comes back by the reference, its bytes as they were.
        let out = dir.path().join("passport.back");
        files(
            reach,
            "applicant-jane-doe",
            FilesCmd::Get {
                content: "tilo.content:abc".into(),
                output: Some(out.clone()),
            },
        );
        assert_eq!(std::fs::read(&out).unwrap(), b"hello");
        let got = api_server.drain_requests();
        assert_eq!(
            got.last().expect("the read").target,
            format!("/v1/workspaces/{WORKSPACE}/content/tilo.content%3Aabc")
        );
    }

    /// A host busy with the person's changes answers `429` and names the wait;
    /// the same call is asked again under the id it first went with, so the
    /// host runs one invocation however often it is retried.
    #[test]
    fn a_busy_host_is_asked_again_under_the_same_idempotency_key() {
        let _key = keyed();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let api_server = test_http::Responder::handle(move |_request: &test_http::Request| {
            let first = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let reply = |status: u16, body: Value| {
                Some(
                    test_http::Reply::status(status)
                        .header("content-type", "application/json")
                        .body(body.to_string()),
                )
            };
            if first {
                reply(
                    429,
                    json!({"error": {"code": "rate_limited", "message": "busy",
                        "retry_after_ms": 1}}),
                )
            } else {
                reply(200, json!({"revoked": "grant-1"}))
            }
        });
        let url = api_server.url();
        keys(
            Reach {
                api: Some(&url),
                key_file: None,
                json: false,
            },
            None,
            KeysCmd::Revoke {
                grant: "grant-1".into(),
            },
        );
        let sent = api_server.drain_requests();
        assert_eq!(sent.len(), 2, "the busy answer is retried, not reported");
        assert_eq!(sent[1].target, "/v1/keys/grant-1");
        assert_eq!(
            sent[0].header("idempotency-key"),
            sent[1].header("idempotency-key")
        );
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
            serde_json::json!({"name": "Jane Doe", "count": 3, "ok": true,
                "edit": {"op": "set"}})
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
    fn a_workspace_line_names_its_id_and_where_it_lives() {
        let row = serde_json::json!({"id": "3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b",
            "name": "Applicant: Jane Doe", "host": "eu1.tilo.sh", "role": "owner"});
        assert_eq!(
            workspace_line(&row),
            "Applicant: Jane Doe (id 3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b, on eu1.tilo.sh, \
             owner)"
        );
    }

    #[test]
    fn an_action_answer_says_what_ran_and_where_it_is() {
        let done = serde_json::json!({"status": "ok", "path": "notes#open",
            "run": "run-1", "link": "https://tilo.sh/#/w/x/notes/Todo"});
        assert_eq!(
            action_text(&done),
            "ok notes#open\nhttps://tilo.sh/#/w/x/notes/Todo"
        );
        let pending = serde_json::json!({"status": "pending", "run": "run-2",
            "path": "pdf-viewer#import-pdf-content", "ask": {"url": "https://tilo.sh/approve/1"}});
        assert_eq!(
            action_text(&pending),
            "pending run-2: it waits for you, at https://tilo.sh/approve/1 \
             (pdf-viewer#import-pdf-content)"
        );
    }

    #[test]
    fn the_people_of_a_workspace_are_a_line_each() {
        let listed = serde_json::json!({
            "members": [{"name": "Ada", "role": "owner", "owner": true, "administrator": true},
                        {"name": "Rosa Ortiz", "role": "editor"}],
            "invitations": [{"id": "inv-1", "role": "viewer", "status": "pending"}],
        });
        assert_eq!(
            members_text(&listed),
            "members:\n  Ada — owner, administrator\n  Rosa Ortiz — editor\n\
             invitations:\n  viewer invitation pending, inv-1"
        );
        let alone = serde_json::json!({"members": [], "invitations": []});
        assert_eq!(
            members_text(&alone),
            "members:\n  nobody yet\ninvitations: none open"
        );
    }
}
