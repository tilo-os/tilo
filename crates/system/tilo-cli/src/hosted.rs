//! Hosted workspaces from a shell: `tilo keys`, `tilo workspace`,
//! `tilo do -w`, `tilo files` and `tilo run`.
//!
//! Each command is one call of Tilo's API (`/v1`, `docs/developers/api.md`),
//! made by [`crate::api::Client`]: a workspace and its people, keys, programs
//! and files are routes there, and an action is `POST
//! …/resources/{path}/actions/{action}` with its arguments as the body. The
//! key is `TILO_API_KEY` (at the server `--api` or `TILO_API_URL` names), a
//! key file `--key-file` names, or, when this computer logged in (`tilo
//! login`), its own device: each command a call to the process holding that
//! device's one session, which a second session would cut off.
//!
//! `tilo keys create` is the one command that asks the person: it waits at
//! the API server's own key page until they approve one in their Tilo.

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::api::{self, Client, Reach};

mod key_api;

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
    /// Make a key of its own for a program here, and write it as a key file.
    #[command(
        after_help = "Example:\n  tilo keys export \"Visa backend\" --out visa.json\n\n\
                      It asks for a new key, as `tilo keys create` does: this computer approves it \
                      when it holds your identity, and otherwise you approve it in your Tilo. The \
                      file holds that key, its certificate and the server this computer reaches: \
                      `tilo --key-file visa.json …` acts with it in process, with no API server \
                      between. It is never this computer's own key, so the program and this \
                      computer each keep their own session."
    )]
    Export {
        /// What will use it, as your grants page will name it. With -w
        /// (an id), it reaches only that workspace.
        name: String,
        /// Where to write it, readable by this account only.
        #[arg(long)]
        out: PathBuf,
        /// Seconds until it expires; without it, a key reaching all your
        /// workspaces lasts until revoked.
        #[arg(long)]
        expires_in: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ServersCmd {
    /// The servers keeping the workspace (-w): the one asked first.
    Ls,
    /// Have the server whose site is ORIGIN keep the workspace (-w) as well.
    #[command(
        after_help = "Example:\n  tilo workspace servers add https://tilo.sh -w Tilo\n\n\
                      Both servers keep the workspace from then on: a change on either reaches \
                      the other, and its people reach it at either. Only its owners choose its \
                      servers, and the other server takes it only if it let you in and has \
                      capacity for it: sign in there first."
    )]
    Add {
        /// The other server's site, like `https://tilo.sh`.
        origin: String,
    },
    /// Have a server keep the workspace (-w) no more.
    #[command(
        after_help = "Example:\n  tilo workspace servers remove https://tilo.sh -w Tilo\n\n\
                      SERVER is its site, or its device key as `tilo workspace servers ls` names \
                      it. It is no longer named to anyone at once, and lets the workspace go when \
                      it hears so."
    )]
    Remove {
        /// The server's site, or its device key.
        server: String,
    },
    /// Move the workspace (-w) from the server FROM to the server whose site
    /// is TO.
    #[command(
        after_help = "Example:\n  tilo workspace servers move https://old.example.com https://tilo.sh -w Tilo\n\n\
                      TO keeps the workspace first, then FROM keeps it no more. If TO refuses, \
                      the workspace stays where it was. FROM is a site, or a device key as \
                      `tilo workspace servers ls` names it; TO is a site."
    )]
    Move {
        /// The server it moves from: its site, or its device key.
        from: String,
        /// The site of the server it moves to, like `https://tilo.sh`.
        to: String,
    },
}

#[derive(Subcommand)]
pub enum WorkspaceCmd {
    /// Your workspaces.
    Ls {
        /// Only those labelled so, `NAME=VALUE`; several must all match.
        #[arg(long = "label", value_name = "NAME=VALUE", value_parser = label_of)]
        labels: Vec<(String, String)>,
    },
    /// Make a workspace; its server may refuse (quota, payment).
    Create {
        /// What to call it, as your workspaces list will name it.
        name: String,
        /// A label for your programs to find it by, `NAME=VALUE`
        /// (`customer_id=cus_8121`); repeat it for more. Only your keys read them.
        #[arg(long = "label", value_name = "NAME=VALUE", value_parser = label_of)]
        labels: Vec<(String, String)>,
    },
    /// Give a workspace another name, for every list and device of it.
    #[command(
        after_help = "Example:\n  tilo workspace rename Research \"Research 2027\"\n\n\
                      Its owners and moderators name it."
    )]
    Rename {
        /// The workspace, by its name or its id.
        workspace: String,
        /// What to call it from now on.
        name: String,
    },
    /// Delete a workspace of yours with everything in it, and revoke the keys
    /// made for it: final, there is no undo.
    Rm {
        /// The workspace, by its name or its id.
        workspace: String,
    },
    /// Make a workspace that starts with everything another holds and then
    /// goes its own way, with you as its only member.
    #[command(
        after_help = "Example:\n  tilo workspace branch template \"Acme\" --label customer_id=cus_8121\n\n\
                      Its records, with their history, its files and Terminal environments come \
                      along; its chat, its people and each person's own places stay behind, as do \
                      its labels. Nothing merges back."
    )]
    Branch {
        /// The workspace to branch, by its name or its id.
        source: String,
        /// What to call the branch.
        name: String,
        /// A label for your programs to find it by, `NAME=VALUE`
        /// (`customer_id=cus_8121`); repeat it for more. Only your keys read them.
        #[arg(long = "label", value_name = "NAME=VALUE", value_parser = label_of)]
        labels: Vec<(String, String)>,
    },
    /// Let the keys made for one workspace of yours alone run commands in its
    /// Linux machine without asking you each time, or ask again.
    #[command(after_help = "Example:\n  tilo workspace unattended template\n\n\
                      It asks you on your approving device first, and waits for your answer. \
                      Branches made from the workspace start with it on. A key for all your \
                      workspaces is still asked, and so is everything else that asks. \
                      `--off` asks again from the next command, without asking you.")]
    Unattended {
        /// The workspace, by its name or its id.
        workspace: String,
        /// Ask before each command again.
        #[arg(long)]
        off: bool,
    },
    /// The people in a workspace (-w): adding, removing, their roles, and
    /// inviting more.
    #[command(after_help = "Examples:\n  tilo workspace people ls -w Acme\n  \
                      tilo workspace people invite --role editor -w Acme\n  \
                      tilo workspace people add 3f9c…e1 --role editor -w Acme\n  \
                      tilo workspace people role 3f9c…e1 administrator -w Acme\n  \
                      tilo workspace people rm 3f9c…e1 -w Acme\n\n\
                      Without -w, the workspace `tilo login` entered.")]
    People {
        #[command(subcommand)]
        cmd: PeopleCmd,
    },
    /// The servers keeping a workspace (-w), and having another keep it as
    /// well: both keep it, a change on either reaches the other, and its people
    /// reach it at either.
    #[command(after_help = "Examples:\n  tilo workspace servers ls -w Tilo\n  \
                      tilo workspace servers add https://tilo.sh -w Tilo\n  \
                      tilo workspace servers remove https://tilo.sh -w Tilo")]
    Servers {
        #[command(subcommand)]
        cmd: ServersCmd,
    },
}

#[derive(Subcommand)]
pub enum PeopleCmd {
    /// The people in the workspace, with their roles and keys, and the
    /// invitations still open.
    Ls,
    /// A link that brings one more person into the workspace as --role.
    #[command(
        after_help = "Example:\n  tilo workspace people invite --role editor --email ops@example.com -w Acme\n\n\
                      Following the link signs them in and lands them in the workspace. You invite \
                      as far as your role in the workspace lets you, an administrator or owner \
                      where you may make one. No mail is sent: hand the link over yourself."
    )]
    Invite {
        /// viewer, commenter, editor, administrator or owner.
        #[arg(long, default_value = "viewer")]
        role: String,
        /// Their address, which the invitation names.
        #[arg(long)]
        email: Option<String>,
        /// The address on the wait list it is for (its entry, as `tilo
        /// workspace people wait-list` names it): mailed to it where the
        /// workspace mails invitations, and it leaves the list once they join.
        #[arg(long, value_name = "ENTRY", conflicts_with = "email")]
        waiting: Option<String>,
    },
    /// Let the person whose key you know into the workspace at once, as
    /// --role: no invitation to hand over.
    #[command(
        after_help = "Example:\n  tilo workspace people add 3f9c…e1 --role owner --name \"Ada Lovelace\" -w Lounge\n\n\
                      KEY is their user public key (`tilo login --new-identity --json` names it). \
                      The workspace lets them in from then on, and a key they create reaches it \
                      before any device of theirs has joined. You give at most what the workspace \
                      lets you: an administrator makes administrators, an owner makes owners."
    )]
    Add {
        /// Their user public key, 64 hex characters.
        key: String,
        /// viewer, commenter, editor, moderator, administrator or owner.
        #[arg(long)]
        role: String,
        /// How the workspace names them until they name themselves.
        #[arg(long)]
        name: Option<String>,
    },
    /// Move someone in the workspace to another role.
    #[command(
        after_help = "Example:\n  tilo workspace people role 3f9c…e1 administrator -w Acme\n\n\
                      You give at most what the workspace lets you."
    )]
    Role {
        /// Their user public key, as `tilo workspace people ls` names it.
        key: String,
        /// viewer, commenter, editor, moderator, administrator or owner.
        role: String,
    },
    /// Revoke an open invitation: its link lets nobody in from then on.
    #[command(
        after_help = "Example:\n  tilo workspace people revoke 7a41…c9 -w Acme\n\n\
                      INVITE is the invitation's id, as `tilo workspace people ls` lists it."
    )]
    Revoke {
        /// The invitation's id, as `tilo workspace people ls` names it.
        invite: String,
    },
    /// Remove someone from the workspace, with every device of theirs.
    #[command(
        after_help = "Example:\n  tilo workspace people rm 3f9c…e1 -w Acme\n\n\
                      Its owners remove people, and a moderator removes those ranked below them."
    )]
    Rm {
        /// Their user public key, as `tilo workspace people ls` names it.
        key: String,
    },
    /// The addresses people left at the workspace's entry while it takes
    /// people by invitation, to invite.
    #[command(
        name = "wait-list",
        after_help = "Example:\n  tilo workspace people wait-list -w Lounge\n  \
                      tilo workspace people invite --waiting 5d1e…0a --role editor -w Lounge"
    )]
    WaitList,
    /// Take an address off the workspace's wait list.
    Unlist {
        /// The address's entry, as `tilo workspace people wait-list` names it.
        entry: String,
    },
    /// How the workspace shows you from now on: the name its people and the
    /// invitations you make say for you.
    #[command(
        after_help = "Example:\n  tilo workspace people name \"Quinten Wolff\" -w Acme\n\n\
                      Any device or key of yours sets it: the workspace knows you by it before a \
                      browser of yours joins, and whoever you invite sees who asked them in."
    )]
    Name {
        /// The name to go by, as your teammates will read it.
        name: String,
    },
}

#[derive(Subcommand)]
pub enum FilesCmd {
    /// Upload files, or whole folders, into a workspace's file tree: each
    /// file is written where the workspace keeps its files, and prints its
    /// path there.
    #[command(
        after_help = "A folder goes up whole, keeping its structure: `tilo files put notes/` \
                      keeps `notes/2026/plan.md` called `notes/2026/plan.md`, and `--to docs` \
                      puts it at `docs/notes/2026/plan.md`. A file larger than one request \
                      carries goes up in pieces, and a connection that fails resumes where it \
                      stopped; putting it again over a file of the same name replaces that \
                      file's content. `--store` puts the bytes only, with no file."
    )]
    Put {
        /// The files and folders to upload, by their paths here.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// The folder in the workspace to put them in, default its root.
        #[arg(long)]
        to: Option<String>,
        /// Put the bytes only: print each `tilo.content:` reference, and
        /// write no file.
        #[arg(long)]
        store: bool,
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
///
/// The API in process logs as `RUST_LOG` asks, and says nothing without it: a
/// call's phases (`tilo_mcp::timing=debug`) show where its time goes.
pub(crate) fn run(future: impl std::future::Future<Output = Result<ExitCode>>) -> ExitCode {
    crate::log_to_stderr(tracing_subscriber::filter::LevelFilter::OFF);
    match runtime().and_then(|runtime| runtime.block_on(future)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tilo: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

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

/// A label as `--label` takes it: `NAME=VALUE`.
fn label_of(label: &str) -> Result<(String, String), String> {
    label
        .split_once('=')
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .ok_or_else(|| format!("`{label}` is no label: write NAME=VALUE, as customer_id=cus_8121"))
}

fn workspace_line(row: &Value) -> String {
    let mut line = format!("{} (id {}", text(row, "name"), text(row, "id"));
    if let Some(host) = row.get("host").and_then(Value::as_str) {
        let _ = write!(line, ", on {host}");
    }
    if let Some(role) = row.get("role").and_then(Value::as_str) {
        let _ = write!(line, ", {role}");
    }
    for (name, value) in row
        .get("labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let _ = write!(line, ", {name}={}", value.as_str().unwrap_or_default());
    }
    if row.get("unattended").and_then(Value::as_bool) == Some(true) {
        line.push_str(", runs commands unattended");
    }
    line.push(')');
    line
}

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
        let workspace = client.workspace_id(workspace).await?;
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
            let mut said = format!("pending {}: it waits for you", text(answer, "invocation"));
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
    // What the action answered it made or changed, as the workspace names it:
    // the path the next command can take as it is.
    for resource in answer
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let _ = write!(said, "\n-> {resource}");
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

/// `tilo ls [path] -w <workspace>`: what the workspace holds, or what `path`
/// holds, as `tilo_ls` lists it: the host's own lines, so a person and an
/// agent read the same text, and `cursor` reads on from where a page ended.
pub fn ls(reach: Reach<'_>, workspace: &str, path: Option<&str>, cursor: Option<&str>) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        let mut query = Vec::new();
        if let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) {
            query.push(format!("path={}", api::encode_query(path)));
        }
        if let Some(cursor) = cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) {
            query.push(format!("cursor={}", api::encode_query(cursor)));
        }
        let mut route = format!("/v1/workspaces/{workspace}/resources");
        if !query.is_empty() {
            let _ = write!(route, "?{}", query.join("&"));
        }
        let listed = client.get(&route).await?;
        Ok(show(reach.json, &listed, text(&listed, "text")))
    })
}

/// `tilo read <path> -w <workspace>`: one resource, as `tilo_read` reads it.
pub fn read(reach: Reach<'_>, workspace: &str, path: &str, cursor: Option<&str>) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        let mut route = format!(
            "/v1/workspaces/{workspace}/resources/{}",
            api::encode_path(path.trim())
        );
        if let Some(cursor) = cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) {
            let _ = write!(route, "?cursor={}", api::encode_query(cursor));
        }
        let read = client.get(&route).await?;
        // A read the tool has no lines for (an app's surface) prints the
        // typed answer as it came, rather than nothing.
        let said = match read
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            Some(text) => text.to_owned(),
            None => serde_json::to_string_pretty(&read).unwrap_or_default(),
        };
        Ok(show(reach.json, &read, &said))
    })
}

/// `tilo invocations -w <workspace>`: the workspace's invocations in records
/// the key may read, newest first, as `tilo_read workspace/invocations` lists
/// them: `limit` to a page, and the `cursor` a page ended with for the older
/// ones.
pub fn invocations(
    reach: Reach<'_>,
    workspace: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        let mut query = Vec::new();
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        if let Some(cursor) = cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) {
            query.push(format!("cursor={}", api::encode_query(cursor)));
        }
        let mut route = format!("/v1/workspaces/{workspace}/invocations");
        if !query.is_empty() {
            let _ = write!(route, "?{}", query.join("&"));
        }
        let listed = client.get(&route).await?;
        Ok(show(reach.json, &listed, &invocations_text(&listed)))
    })
}

/// A workspace's invocations as a person reads them: the lines the host
/// wrote, and the command that reads on.
fn invocations_text(listed: &Value) -> String {
    let mut said = text(listed, "text").to_owned();
    if said.is_empty() {
        let total = listed.get("total").and_then(Value::as_u64).unwrap_or(0);
        said = format!("{total} invocations");
    }
    if let Some(cursor) = listed.get("next_cursor").and_then(Value::as_str) {
        let _ = write!(said, "\nolder: tilo invocations --cursor {cursor}");
    }
    said
}

/// `tilo view <path> -w <workspace>`: what the app's window shows for the
/// resource, as `tilo_view` reads it, and a picture of it when asked.
pub fn view(
    reach: Reach<'_>,
    workspace: &str,
    path: &str,
    picture: Option<&std::path::Path>,
) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        let mut route = format!(
            "/v1/workspaces/{workspace}/views?path={}",
            api::encode_query(path.trim())
        );
        if picture.is_some() {
            route.push_str("&picture=true");
        }
        let view = client.get(&route).await?;
        let mut said = text(&view, "text").to_owned();
        if let (Some(file), Some(content)) = (picture, view.get("picture").and_then(Value::as_str))
        {
            let mut written = std::fs::File::create(file).context("making the picture's file")?;
            client
                .download(&workspace, content, &mut written, &mut |_, _| {})
                .await?;
            let _ = write!(said, "\npicture: {}", file.display());
            said = said.trim_start().to_owned();
        }
        Ok(show(reach.json, &view, &said))
    })
}

/// `tilo run <file.tl> -w <workspace>`: the program as one task, one step
/// for undo.
pub fn program(reach: Reach<'_>, workspace: &str, file: &Path) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let source =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let workspace = client.workspace_id(workspace).await?;
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
    let workspace = client.workspace_id(workspace).await?;
    let mut apps = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(cursor) => format!(
                "/v1/workspaces/{workspace}/resources?cursor={}",
                percent_encoding::utf8_percent_encode(cursor, percent_encoding::NON_ALPHANUMERIC)
            ),
            None => format!("/v1/workspaces/{workspace}/resources"),
        };
        let listing = client.get(&path).await?;
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
            let read = match client
                .get(&format!(
                    "/v1/workspaces/{workspace}/resources/{}",
                    api::encode_path(path)
                ))
                .await
            {
                Ok(read) => read,
                Err(error) => {
                    // One app the workspace could not answer for is named, and
                    // the others' types are still written.
                    eprintln!("tilo: {path}: {error:#}");
                    continue;
                }
            };
            let Some(surface) = read.get("surface").and_then(Value::as_str) else {
                eprintln!(
                    "tilo: {path} publishes no surface in that workspace: it is not installed there"
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
        match listing.get("next_cursor").and_then(Value::as_str) {
            Some(next) if !next.is_empty() => cursor = Some(next.to_owned()),
            _ => break,
        }
    }
    Ok(apps)
}

/// `tilo workspace …`: the person's workspaces, and with `-w` (`workspace`)
/// the people and servers of one.
pub fn workspace(reach: Reach<'_>, workspace: Option<&str>, cmd: WorkspaceCmd) -> ExitCode {
    let named = || {
        workspace.ok_or_else(|| {
            eprintln!("tilo: name the workspace: -w <name or id>");
            ExitCode::from(2)
        })
    };
    match cmd {
        WorkspaceCmd::People { cmd } => {
            named().map_or_else(|code| code, |workspace| members(reach, workspace, cmd))
        }
        WorkspaceCmd::Servers { cmd } => {
            named().map_or_else(|code| code, |workspace| servers(reach, workspace, cmd))
        }
        cmd => workspaces(reach, cmd),
    }
}

/// The person's workspaces: listing, making, naming, branching and deleting
/// them.
fn workspaces(reach: Reach<'_>, cmd: WorkspaceCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        match cmd {
            WorkspaceCmd::Ls { labels } => list_workspaces(&client, reach.json, &labels).await,
            WorkspaceCmd::Create { name, labels } => {
                let labels: serde_json::Map<String, Value> = labels
                    .into_iter()
                    .map(|(name, value)| (name, Value::String(value)))
                    .collect();
                let made = client
                    .post(
                        "/v1/workspaces",
                        &serde_json::json!({"name": name, "labels": labels}),
                    )
                    .await?;
                Ok(show(reach.json, &made, &workspace_line(&made)))
            }
            WorkspaceCmd::Rename {
                workspace: named,
                name,
            } => {
                let id = client.workspace_id(&named).await?;
                let renamed = client
                    .put(
                        &format!("/v1/workspaces/{id}/name"),
                        &serde_json::json!({ "name": name }),
                    )
                    .await?;
                Ok(show(reach.json, &renamed, &workspace_line(&renamed)))
            }
            WorkspaceCmd::Branch {
                source,
                name,
                labels,
            } => {
                let id = client.workspace_id(&source).await?;
                let labels: serde_json::Map<String, Value> = labels
                    .into_iter()
                    .map(|(name, value)| (name, Value::String(value)))
                    .collect();
                let made = client
                    .post(
                        &format!("/v1/workspaces/{id}/branches"),
                        &serde_json::json!({"name": name, "labels": labels}),
                    )
                    .await?;
                Ok(show(reach.json, &made, &workspace_line(&made)))
            }
            WorkspaceCmd::Unattended {
                workspace: named,
                off,
            } => unattended(&client, reach.json, &named, off).await,
            WorkspaceCmd::Rm { workspace: named } => {
                let id = client.workspace_id(&named).await?;
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
            WorkspaceCmd::People { .. } | WorkspaceCmd::Servers { .. } => {
                bail!("`tilo workspace people` and `servers` act in one workspace (-w)")
            }
        }
    })
}

/// `tilo workspace ls [--label NAME=VALUE]`: the workspaces the key reaches,
/// those labelled so when labels are named.
async fn list_workspaces(
    client: &Client,
    json: bool,
    labels: &[(String, String)],
) -> Result<ExitCode> {
    let query: Vec<String> = labels
        .iter()
        .map(|(name, value)| {
            let encode = |text: &str| {
                percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
            };
            format!("label%5B{}%5D={}", encode(name), encode(value))
        })
        .collect();
    let path = if query.is_empty() {
        "/v1/workspaces".to_owned()
    } else {
        format!("/v1/workspaces?{}", query.join("&"))
    };
    let listed = client.get(&path).await?;
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
    Ok(show(json, &listed, &said))
}

/// `tilo workspace unattended <workspace> [--off]`: held while the
/// person decides on their approving device, and said as it stands then.
async fn unattended(client: &Client, json: bool, named: &str, off: bool) -> Result<ExitCode> {
    let id = client.workspace_id(named).await?;
    let answer = client
        .put_asking(
            &format!("/v1/workspaces/{id}/unattended"),
            &serde_json::json!({"on": !off}),
        )
        .await?;
    let said = match (
        text(&answer, "status"),
        answer.pointer("/result/unattended"),
    ) {
        ("pending", _) => format!(
            "pending {}: it waits for you on your approving device",
            text(&answer, "invocation")
        ),
        (_, Some(Value::Bool(true))) => format!(
            "{named}: keys made for it alone run commands in its Linux machine without asking you"
        ),
        _ => format!("{named}: every command asks you first"),
    };
    Ok(show(json, &answer, &said))
}

/// `tilo files …`.
pub fn files(reach: Reach<'_>, workspace: &str, cmd: FilesCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        match cmd {
            FilesCmd::Put { paths, to, store } => {
                put_files(
                    &client,
                    &workspace,
                    &paths,
                    to.as_deref(),
                    store,
                    reach.json,
                )
                .await
            }
            FilesCmd::Get { content, output } => {
                get_content(&client, &workspace, &content, output.as_deref(), reach.json).await
            }
        }
    })
}

/// One file to upload: where it is here, and the name it keeps in the
/// workspace.
pub(crate) struct Uploading {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
}

/// Upload `paths` into `workspace`'s file tree, in the folder `to` when one
/// is named, and print where each file is there.
async fn put_files(
    client: &Client,
    workspace: &str,
    paths: &[PathBuf],
    to: Option<&str>,
    store: bool,
    json: bool,
) -> Result<ExitCode> {
    let mut files = Vec::new();
    for path in paths {
        files.extend(files_under(path)?);
    }
    if files.is_empty() {
        bail!("nothing to upload: those paths hold no files");
    }
    // Every byte goes up before any of it is imported: an app that cannot
    // take one file does not stop the rest of the folder. Many files go up
    // side by side, a few at a time.
    let uploaded = upload_all(client, workspace, files).await?;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    // The tree takes a folder in as few changes as its entries fit: one run
    // and one History entry for a folder a request carries whole.
    let parts = batches(&uploaded, to);
    let count = parts.len();
    for (part, batch) in parts.into_iter().enumerate() {
        let paths = if store {
            vec![None; batch.len()]
        } else {
            match put_in_tree(
                client,
                workspace,
                &batch,
                to,
                (part.saturating_add(1), count),
            )
            .await
            {
                Ok(paths) => paths,
                Err(why) => {
                    for (file, _) in batch.iter().copied() {
                        failures.push(format!("{}: {why:#}", file.name));
                    }
                    vec![None; batch.len()]
                }
            }
        };
        for ((file, content), path) in batch.iter().copied().zip(paths) {
            rows.push(json!({
                "name": file.name,
                "content": content,
                "path": path.as_deref(),
            }));
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    } else {
        for row in &rows {
            let name = text(row, "name");
            match (row.get("path").and_then(Value::as_str), store) {
                (Some(path), _) => println!("{path}  {name}"),
                (None, true) => println!("{}  {name}", text(row, "content")),
                (None, false) => println!(
                    "{}  {name}  (no file was written: its bytes are in the workspace)",
                    text(row, "content")
                ),
            }
        }
    }
    if !failures.is_empty() {
        bail!(
            "{} of the {} files went up but are not in the workspace's files:\n  {}",
            failures.len(),
            rows.len(),
            failures.join("\n  ")
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Every file `path` names, with the names they keep in the workspace: a
/// file is itself, and a folder is every file under it, named by its path
/// from the folder's own place (`notes/2026/plan.md` from `notes/`).
fn files_under(path: &Path) -> Result<Vec<Uploading>> {
    let named = |path: &Path, name: String| Uploading {
        path: path.to_owned(),
        name,
    };
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if meta.is_file() {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        return Ok(vec![named(path, name)]);
    }
    if !meta.is_dir() {
        bail!("{} is not a file or a folder", path.display());
    }
    let root = path.parent().unwrap_or_else(|| Path::new(""));
    let mut found = Vec::new();
    let mut walking = vec![path.to_owned()];
    while let Some(dir) = walking.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading the folder {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("reading the folder {}", dir.display()))?;
            let kind = entry
                .file_type()
                .with_context(|| format!("reading {}", entry.path().display()))?;
            if kind.is_dir() {
                walking.push(entry.path());
            } else if kind.is_file() {
                let name = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(&entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(named(&entry.path(), name));
            }
            // A link is left alone: it may name what is outside the folder,
            // or name the folder again.
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// How many files go up at once.
const UPLOADS_AT_ONCE: usize = 32;
/// How many files one action puts into the workspace's tree: a folder larger
/// than this goes in more runs, each answered within the peer's half minute,
/// whatever a loaded disk makes a node cost.
const TREE_BATCH_FILES: usize = 2_000;
/// How many bytes of file entries one change carries: under the 64 MiB one
/// upload carries, so a large folder goes in a few changes rather than one
/// no request carries.
const TREE_BATCH_BYTES: usize = 32 * 1024 * 1024;

/// The most raw bytes one many-files call carries: under the 1 MiB request
/// and session frame bounds once base64 and hex grow them.
const MANY_BYTES: usize = 512 * 1024;
/// How many many-files calls go at once. A workspace answers them one after
/// another, so more in flight only queue: the oldest waits a step for every
/// call ahead of it, and the peer gives up on one that waits half a minute.
const CONTENTS_AT_ONCE: usize = 4;
/// How many content hashes one call asks the workspace about: a small body,
/// and the session asks the store in [`MAX_HAVE_HASHES`]-sized steps.
const HAVE_HASHES: usize = 2_000;

/// `file`'s length, as the disk says it.
fn file_size(file: &Uploading) -> u64 {
    std::fs::metadata(&file.path).map_or(0, |meta| meta.len())
}

/// The address `file`'s bytes keep in the workspace, lowercase BLAKE3 hex: what
/// the upload would answer, read here so content already kept is not sent again.
fn hash_of(file: &Uploading) -> Result<String> {
    use std::io::Read as _;
    let mut opened = std::fs::File::open(&file.path)
        .with_context(|| format!("reading {}", file.path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut piece = vec![0_u8; 256 * 1024];
    loop {
        let read = match opened.read(&mut piece) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", file.path.display()));
            }
        };
        hasher.update(piece.get(..read).unwrap_or_default());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Upload `files`: what the workspace already holds is named without going up
/// again, small files go many to a call, and a file larger than one call goes
/// in pieces a few at a time. Tells a person at a terminal how far it is, and
/// answers each with the `tilo.content:` reference it is kept by, in name
/// order.
pub(crate) async fn upload_all(
    client: &Client,
    workspace: &str,
    files: Vec<Uploading>,
) -> Result<Vec<(Uploading, String)>> {
    use futures_util::StreamExt as _;
    let count = files.len();
    let total: u64 = files.iter().map(file_size).sum();
    let name = match files.as_slice() {
        [one] => one.name.clone(),
        _ => format!("{count} files"),
    };
    let mut progress = Progress::new(&name);
    progress.told(0, total);
    // What the workspace already holds needs no upload.
    let hashes: Vec<String> = files.iter().map(hash_of).collect::<Result<_>>()?;
    let held = held_of(client, workspace, &hashes).await?;
    let mut contents: Vec<Option<String>> = (0..count).map(|_| None).collect();
    let (mut done, smalls, larges) = plan(&files, &held, &hashes, &mut contents);
    progress.told(done.min(total), total);
    // Small files go many to a call, a few calls at a time.
    let mut sending = futures_util::stream::iter(smalls)
        .map(|group| async {
            let mut batch = Vec::with_capacity(group.len());
            for index in &group {
                let file = files
                    .get(*index)
                    .ok_or_else(|| anyhow!("a file went missing"))?;
                batch.push(
                    std::fs::read(&file.path)
                        .with_context(|| format!("reading {}", file.path.display()))?,
                );
            }
            let refs = client
                .put_contents(workspace, &batch)
                .await
                .context("storing the files")?;
            anyhow::Ok((group, refs))
        })
        .buffer_unordered(CONTENTS_AT_ONCE);
    while let Some(sent) = sending.next().await {
        let (group, refs) = sent?;
        for (index, content) in group.into_iter().zip(refs) {
            if let Some(kept) = contents.get_mut(index) {
                *kept = Some(content);
            }
            if let Some(file) = files.get(index) {
                done = done.saturating_add(file_size(file));
            }
            progress.told(done.min(total), total);
        }
    }
    // A file larger than one many-files call goes up in pieces, a few at a time.
    let listing = &files;
    let mut big = futures_util::stream::iter(larges)
        .map(|index| {
            let files = listing;
            async move {
                let file = files
                    .get(index)
                    .ok_or_else(|| anyhow!("a file went missing"))?;
                let put = client
                    .upload_file(workspace, &file.path, &mut |_, _| {})
                    .await
                    .with_context(|| format!("uploading {}", file.name))?;
                anyhow::Ok((index, text(&put, "content").to_owned(), file_size(file)))
            }
        })
        .buffer_unordered(UPLOADS_AT_ONCE);
    while let Some(sent) = big.next().await {
        let (index, content, len) = sent?;
        if let Some(kept) = contents.get_mut(index) {
            *kept = Some(content);
        }
        done = done.saturating_add(len);
        progress.told(done.min(total), total);
    }
    progress.done();
    drop(sending);
    drop(big);
    files
        .into_iter()
        .zip(contents)
        .map(|(file, content)| {
            let content = content
                .ok_or_else(|| anyhow!("no content reference was answered for {}", file.name))?;
            Ok((file, content))
        })
        .collect()
}

/// What of `files` goes up how: the bytes the workspace holds already, whose
/// references are put in `contents`; the small files grouped many to a call;
/// and the files too large for one.
fn plan(
    files: &[Uploading],
    held: &[bool],
    hashes: &[String],
    contents: &mut [Option<String>],
) -> (u64, Vec<Vec<usize>>, Vec<usize>) {
    let mut done = 0_u64;
    let mut smalls: Vec<Vec<usize>> = Vec::new();
    let mut larges: Vec<usize> = Vec::new();
    let mut group: Vec<usize> = Vec::new();
    let mut group_bytes = 0_usize;
    for (index, file) in files.iter().enumerate() {
        if held.get(index).copied().unwrap_or(false) {
            if let (Some(kept), Some(hash)) = (contents.get_mut(index), hashes.get(index)) {
                *kept = Some(format!("tilo.content:{hash}"));
            }
            done = done.saturating_add(file_size(file));
            continue;
        }
        let len = usize::try_from(file_size(file)).unwrap_or(usize::MAX);
        if len > MANY_BYTES {
            larges.push(index);
        } else {
            if group_bytes.saturating_add(len) > MANY_BYTES && !group.is_empty() {
                smalls.push(std::mem::take(&mut group));
                group_bytes = 0;
            }
            group_bytes = group_bytes.saturating_add(len);
            group.push(index);
        }
    }
    if !group.is_empty() {
        smalls.push(group);
    }
    (done, smalls, larges)
}

/// Which of the files hashed `hashes` the workspace holds already, in order.
async fn held_of(client: &Client, workspace: &str, hashes: &[String]) -> Result<Vec<bool>> {
    let mut held = vec![false; hashes.len()];
    for (at, asks) in hashes.chunks(HAVE_HASHES).enumerate() {
        let answered = client
            .have_contents(workspace, asks)
            .await
            .context("asking which files the workspace already holds")?;
        for (offset, present) in answered.into_iter().enumerate() {
            let index = at.saturating_mul(HAVE_HASHES).saturating_add(offset);
            if let Some(held_here) = held.get_mut(index) {
                *held_here = present;
            }
        }
    }
    Ok(held)
}

/// `uploaded` in as few batches as the workspace takes: each a whole run and
/// one History entry, under [`TREE_BATCH_FILES`] files and
/// [`TREE_BATCH_BYTES`] bytes of entries.
fn batches<'a>(
    uploaded: &'a [(Uploading, String)],
    to: Option<&str>,
) -> Vec<Vec<&'a (Uploading, String)>> {
    let size = |(file, content): &(Uploading, String)| {
        content
            .len()
            .saturating_add(file.name.len())
            .saturating_add(to.map_or(0, str::len))
            .saturating_add(40)
    };
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut bytes = 0_usize;
    for entry in uploaded {
        let next = size(entry);
        let full = batch.len() >= TREE_BATCH_FILES || bytes.saturating_add(next) > TREE_BATCH_BYTES;
        if full && !batch.is_empty() {
            batches.push(batch);
            batch = Vec::new();
            bytes = 0;
        }
        batch.push(entry);
        bytes = bytes.saturating_add(next);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// The folder `file` goes into under `to`, as the tree keeps it: the parts of
/// both, no empty ones, joined by `/`.
fn folder_of(file: &Uploading, to: Option<&str>) -> String {
    let parent = parent_of(&file.name);
    let mut kept: Vec<&str> = Vec::new();
    for part in to.into_iter().chain([parent.as_str()]) {
        for part in part.split('/') {
            let part = part.trim();
            if !part.is_empty() {
                kept.push(part);
            }
        }
    }
    kept.join("/")
}

/// Write `files` into `workspace`'s file tree in one change, in the folder `to`
/// when one is named, and answer each one's path there.
///
/// Their content is opened in Files (`files#open`), which is the workspace's
/// folder: the bytes go where the workspace keeps its files, and the nodes
/// name them, so every member's folder shows the files. A folder's list goes
/// up as content of its own, so the open carries one reference however many
/// files it puts in the tree: one invocation, one History entry.
async fn put_in_tree(
    client: &Client,
    workspace: &str,
    files: &[&(Uploading, String)],
    to: Option<&str>,
    (part, parts): (usize, usize),
) -> Result<Vec<Option<String>>> {
    let listed: Vec<Value> = files
        .iter()
        .map(|(file, content)| {
            json!({"content": content, "name": file_base(&file.name),
                   "dir": folder_of(file, to)})
        })
        .collect();
    let title = if files.len() == 1 {
        files.first().map_or_else(
            || "Upload a file".to_owned(),
            |(file, _)| format!("Upload {}", file.name),
        )
    } else if parts > 1 {
        format!("Upload {} files ({part} of {parts})", files.len())
    } else {
        format!("Upload {} files", files.len())
    };
    let arguments = if files.len() == 1 {
        // One file's entry is smaller than the reference that would name it.
        json!({"files": listed})
    } else {
        let list = serde_json::to_vec(&listed)?;
        let put = client.upload(workspace, list).await?;
        json!({"files": text(&put, "content")})
    };
    let answer = client
        .act(workspace, "files", "open", &arguments, &title)
        .await?;
    // The open answers how many files it put, not each one: the paths are
    // where the tree keeps what was asked for.
    let counted = answer.pointer("/result/count").and_then(Value::as_u64);
    if counted.is_some_and(|count| count != files.len() as u64) {
        bail!(
            "Files put {counted:?} of the {} files: {answer}",
            files.len()
        );
    }
    Ok(files
        .iter()
        .map(|(file, _)| {
            let dir = folder_of(file, to);
            let name = file_base(&file.name);
            Some(if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            })
        })
        .collect())
}

/// The folder part of an uploaded file's name in the workspace
/// (`notes/2026` of `notes/2026/plan.md`), empty for one at the root.
fn parent_of(name: &str) -> String {
    name.rsplit_once('/')
        .map(|(folder, _)| folder.to_owned())
        .unwrap_or_default()
}

/// The last part of an uploaded file's name (`plan.md`).
fn file_base(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_owned()
}

/// An upload's progress, as a person at a terminal sees it: the file's name,
/// how much of it is there and how much of the whole that is. Everywhere
/// else it says nothing, so a script's output stays what it asked for.
struct Progress {
    line: Option<String>,
    said_at: Option<std::time::Instant>,
}

impl Progress {
    fn new(name: &str) -> Self {
        Self {
            line: std::io::IsTerminal::is_terminal(&std::io::stderr()).then(|| name.to_owned()),
            said_at: None,
        }
    }

    fn told(&mut self, done: u64, size: u64) {
        let Some(name) = &self.line else {
            return;
        };
        // Throttled: a 300 MB file would otherwise repaint thousands of times.
        let now = std::time::Instant::now();
        let finished = done >= size;
        if !finished
            && self
                .said_at
                .is_some_and(|said| now.duration_since(said).as_millis() < 100)
        {
            return;
        }
        self.said_at = Some(now);
        let percent = if size == 0 {
            100
        } else {
            done.saturating_mul(100).checked_div(size).unwrap_or(0)
        };
        eprint!("\r{name}  {} of {}  ({percent}%)", bytes(done), bytes(size));
        if finished {
            eprintln!();
        }
    }

    fn done(&mut self) {
        self.line = None;
    }
}

/// `bytes` as a person reads a size: "512 B", "3.4 MB".
fn bytes(count: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut size = count;
    let mut unit: usize = 0;
    while size.checked_div(1024).is_some_and(|smaller| smaller > 0)
        && unit.saturating_add(1) < units.len()
    {
        size = size.checked_div(1024).unwrap_or(0);
        unit = unit.saturating_add(1);
    }
    if unit == 0 {
        return format!("{count} B");
    }
    let tenths = count
        .saturating_mul(10)
        .checked_div(1024_u64.pow(u32::try_from(unit).unwrap_or(0)));
    let tenths = tenths.unwrap_or(0);
    let whole = tenths.checked_div(10).unwrap_or(0);
    let part = tenths.checked_rem(10).unwrap_or(0);
    format!("{whole}.{part} {}", units.get(unit).copied().unwrap_or("B"))
}

/// `tilo files get`: the bytes written where they were asked for, as they
/// arrive.
#[expect(
    clippy::future_not_send,
    reason = "the file or the terminal is this command's own, written on its one thread"
)]
async fn get_content(
    client: &Client,
    workspace: &str,
    content: &str,
    output: Option<&Path>,
    json: bool,
) -> Result<ExitCode> {
    let mut progress = Progress::new("downloading");
    let written = if let Some(path) = output {
        let mut file =
            std::fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
        let written = client
            .download(workspace, content, &mut file, &mut |done, total| {
                progress.told(done, total.unwrap_or(done));
            })
            .await?;
        drop(file);
        if json {
            println!(
                "{}",
                json!({"content": content, "size": written, "file": path})
            );
        }
        written
    } else {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        client
            .download(workspace, content, &mut out, &mut |done, total| {
                progress.told(done, total.unwrap_or(done));
            })
            .await?
    };
    progress.done();
    Ok(if written == 0 {
        bail!("{content} is empty or the workspace no longer holds it")
    } else {
        ExitCode::SUCCESS
    })
}

/// `tilo workspace people … -w <workspace>`: the workspace's people, and inviting one
/// more.
#[expect(
    clippy::too_many_lines,
    reason = "one match over the people commands, each a few lines"
)]
fn members(reach: Reach<'_>, workspace: &str, cmd: PeopleCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        match cmd {
            PeopleCmd::Ls => {
                let listed = client
                    .get(&format!("/v1/workspaces/{workspace}/members"))
                    .await?;
                Ok(show(reach.json, &listed, &members_text(&listed)))
            }
            PeopleCmd::Invite {
                role,
                email,
                waiting,
            } => {
                let mut body = serde_json::json!({"role": role});
                if let Some(fields) = body.as_object_mut() {
                    if let Some(email) = email {
                        fields.insert("email".into(), Value::String(email));
                    }
                    if let Some(waiting) = waiting {
                        fields.insert("waiting".into(), Value::String(waiting));
                    }
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
            PeopleCmd::Role { key, role } => {
                let moved = client
                    .put(
                        &format!(
                            "/v1/workspaces/{workspace}/members/{}/role",
                            api::encode_path(&key)
                        ),
                        &serde_json::json!({ "role": role }),
                    )
                    .await?;
                Ok(show(reach.json, &moved, &members_text(&moved)))
            }
            PeopleCmd::Revoke { invite } => {
                let left = client
                    .delete(&format!(
                        "/v1/workspaces/{workspace}/members/invitations/{}",
                        api::encode_path(&invite)
                    ))
                    .await?;
                Ok(show(reach.json, &left, &members_text(&left)))
            }
            PeopleCmd::Rm { key } => {
                let left = client
                    .delete(&format!(
                        "/v1/workspaces/{workspace}/members/{}",
                        api::encode_path(&key)
                    ))
                    .await?;
                Ok(show(reach.json, &left, &members_text(&left)))
            }
            PeopleCmd::Add { key, role, name } => {
                let mut body = serde_json::json!({"person": key, "role": role});
                if let (Some(fields), Some(name)) = (body.as_object_mut(), name) {
                    fields.insert("name".into(), Value::String(name));
                }
                let added = client
                    .post(&format!("/v1/workspaces/{workspace}/members"), &body)
                    .await?;
                Ok(show(reach.json, &added, &members_text(&added)))
            }
            PeopleCmd::WaitList => {
                let listed = client
                    .get(&format!("/v1/workspaces/{workspace}/members/wait-list"))
                    .await?;
                Ok(show(reach.json, &listed, &wait_list_text(&listed)))
            }
            PeopleCmd::Unlist { entry } => {
                let listed = client
                    .delete(&format!(
                        "/v1/workspaces/{workspace}/members/wait-list/{entry}"
                    ))
                    .await?;
                Ok(show(reach.json, &listed, &wait_list_text(&listed)))
            }
            PeopleCmd::Name { name } => {
                let named = client
                    .post(
                        &format!("/v1/workspaces/{workspace}/members/name"),
                        &serde_json::json!({"name": name}),
                    )
                    .await?;
                Ok(show(reach.json, &named, &members_text(&named)))
            }
        }
    })
}

/// `tilo workspace servers … -w <workspace>`: the servers keeping the
/// workspace, and choosing them.
fn servers(reach: Reach<'_>, workspace: &str, cmd: ServersCmd) -> ExitCode {
    run(async {
        let client = Client::open(reach)?;
        let workspace = client.workspace_id(workspace).await?;
        let path = format!("/v1/workspaces/{workspace}/servers");
        let listed = match cmd {
            ServersCmd::Ls => client.get(&path).await?,
            ServersCmd::Add { origin } => {
                client
                    .post(&path, &serde_json::json!({ "origin": origin }))
                    .await?
            }
            ServersCmd::Remove { server } => {
                let server = percent_encoding::utf8_percent_encode(
                    &server,
                    percent_encoding::NON_ALPHANUMERIC,
                );
                client.delete(&format!("{path}/{server}")).await?
            }
            ServersCmd::Move { from, to } => {
                let from = percent_encoding::utf8_percent_encode(
                    &from,
                    percent_encoding::NON_ALPHANUMERIC,
                );
                client
                    .post(
                        &format!("{path}/{from}/move"),
                        &serde_json::json!({ "origin": to }),
                    )
                    .await?
            }
        };
        Ok(show(reach.json, &listed, &servers_text(&listed)))
    })
}

/// A workspace's servers, as lines: each by its device key, the one asked
/// marked.
fn servers_text(listed: &Value) -> String {
    let servers = listed
        .get("servers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut said = String::from("kept by:");
    for server in &servers {
        let here = if server.get("here").and_then(Value::as_bool) == Some(true) {
            "  (asked here)"
        } else {
            ""
        };
        let _ = write!(said, "\n  {}{here}", text(server, "device"));
    }
    if listed.get("confirmed").and_then(Value::as_bool) == Some(false) {
        said.push_str(
            "\nthe server that was let go was not reached: it is no longer named, and lets the \
             workspace go once it hears so",
        );
    }
    said
}

/// A workspace's wait list, as lines: each address with its entry, and
/// whether it was invited.
fn wait_list_text(listed: &Value) -> String {
    let entries = listed
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        return "the wait list is empty".to_owned();
    }
    let mut said = String::from("wait list:");
    for entry in &entries {
        let invited = if entry.get("invited_at_ns").is_some_and(|at| !at.is_null()) {
            ", invited"
        } else {
            ""
        };
        let _ = write!(
            said,
            "\n  {}{invited} — {}",
            text(entry, "contact"),
            text(entry, "entry")
        );
    }
    said
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
        // One place each: an owner is an administrator with a role beneath, named by the first.
        let what = Some(text(member, "place"))
            .filter(|place| !place.is_empty())
            .unwrap_or_else(|| text(member, "role"));
        let _ = write!(said, "\n  {} — {what}", text(member, "name"));
        // A person is named by their key, which `role` and `rm` take.
        if let Some(key) = member.pointer("/member/person").and_then(Value::as_str) {
            let _ = write!(said, "  {key}");
        }
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
        // Named by what redeeming it makes them: an owner's or an administrator's invitation
        // joins someone as an Editor and makes them more.
        let what = Some(text(invitation, "place"))
            .filter(|place| !place.is_empty())
            .unwrap_or_else(|| text(invitation, "role"));
        // Who it is for, when the inviter named them, and its id, which
        // cancelling it takes.
        let named = text(invitation, "invitee_name");
        let _ = write!(said, "\n  {what} invitation");
        if !named.is_empty() {
            let _ = write!(said, " for {named}");
        }
        let _ = write!(said, ", {}", text(invitation, "invite"));
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
                let asked = KeyAsk {
                    api: reach.api,
                    name: &name,
                    workspace,
                    expires_in,
                    browser_pages: !no_browser_pages,
                };
                let answer = ask_key(&asked).await?;
                if reach.json {
                    println!("{answer}");
                } else {
                    eprintln!("Approved. Your key (shown once):");
                    println!("export {}={}", api::KEY_ENV, text(&answer, "key"));
                }
                Ok(ExitCode::SUCCESS)
            }
            KeysCmd::Export {
                name,
                out,
                expires_in,
            } => {
                let asked = KeyAsk {
                    api: reach.api,
                    name: &name,
                    workspace,
                    expires_in,
                    browser_pages: true,
                };
                export(&asked, &out, reach.json).await
            }
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
                let id = client.workspace_id(named).await?;
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

/// `tilo keys export NAME --out <path>`: a new key for a program here, asked
/// for and approved as `tilo keys create`'s is, written as the key file the
/// in-process API reads, readable by this account only. It reaches the server
/// this computer's login reaches, and is never the device's own key: one key
/// has one holder, and a program sharing the device's would cut the
/// computer's session off.
async fn export(asked: &KeyAsk<'_>, path: &Path, json: bool) -> Result<ExitCode> {
    if api::login_key(None)?.is_none() {
        bail!(
            "this computer is not logged in: `tilo login` makes it one of your devices, and the \
             key this writes reaches the server its login reaches"
        );
    }
    let answer = ask_key(asked).await?;
    let file = api::script_key(text(&answer, "key"))?
        .ok_or_else(|| anyhow!("this computer's login went away while the key was approved"))?;
    write_key_file(path, &file)?;
    let said = format!("{}: {}, reaching {}", path.display(), file.name, file.host);
    let typed = serde_json::json!({"file": path, "name": file.name, "host": file.host});
    Ok(show(json, &typed, &said))
}

/// Write `file` at `path`, readable by this account only, in place of what was
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

/// A key asked of the person: what `tilo keys create` and `tilo keys export` ask for.
struct KeyAsk<'a> {
    /// The API server, like `--api`.
    api: Option<&'a str>,
    /// What will use it, as the grants page names it.
    name: &'a str,
    /// The one workspace it reaches, when not all of them.
    workspace: Option<&'a str>,
    expires_in: Option<u64>,
    browser_pages: bool,
}

/// Ask the person for a key and wait until they answer: the approved key's
/// answer (`key`, the token, and its grant). This computer approves it itself
/// when it holds the person's root; otherwise the person approves at the link
/// it prints.
async fn ask_key(asked: &KeyAsk<'_>) -> Result<Value> {
    let KeyAsk {
        api,
        name,
        workspace,
        expires_in,
        browser_pages,
    } = *asked;
    let api = key_api::key_api(api).await?;
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
    // A computer that holds the person's root approves as their Tilo would.
    if let Some(person) = crate::identity::held()? {
        crate::identity::approve(&person, field("approve_url")).await?;
        eprintln!("Approved here, as {}.", person.name);
    } else {
        eprintln!(
            "Approve the key in your Tilo:\n\n  {}\n\nWaiting for you…",
            field("approve_url")
        );
    }
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
            200 => return Ok(answer),
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
    use std::sync::Arc;

    const WORKSPACE: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";
    const BRANCH: &str = "0b5e7c1d-2e3f-4a5b-8c6d-7e8f9a0b1c2d";

    /// This computer's own key, as `tilo login` kept it, is a key file the API
    /// in process reads: its certificate in the canonical bytes `Direct`
    /// decodes, which is not the postcard the login itself keeps it as.
    /// `tilo keys export` writes a key of its own for a script, never this one.
    #[test]
    fn the_login_key_is_a_key_file_the_in_process_api_reads() {
        let dir = tempfile::tempdir().unwrap();
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let now = tilo_clock::unix_now_ns();
        let month = 30 * 24 * 3600 * 1_000_000_000_u64;
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            // A minute of slack, as the login's own certificates have: the
            // clock that verifies it is not the clock that issued it.
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(month)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let device = crate::login::Device {
            site: "https://tilo.sh/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".into(),
            workspace_name: Some("Studio".into()),
            user: hex::encode(root.verifying_key().to_bytes()),
            agent: "Claude Code".into(),
            label: "Claude Code on build-box".into(),
            cert: hex::encode(postcard::to_allocvec(&cert).unwrap()),
            host_cert: "00".into(),
            server_address: None,
        };
        crate::login::store(dir.path(), &key, &device).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_DEVICE_DIR", dir.path());

        let file = api::login_key(None)
            .unwrap()
            .expect("a logged-in computer has a key");
        assert_eq!(file.host, "https://eu1.tilo.test");
        assert_eq!(
            file.site.as_deref(),
            Some("https://tilo.sh"),
            "the device keeps the site it logged in with: links are built there"
        );
        assert_eq!(file.name, "Claude Code on build-box");
        assert_eq!(file.key, hex::encode(key.to_bytes()));
        assert_eq!(
            hex::decode(&file.cert).unwrap(),
            tilo_user_identity::canonical::canonical_bytes(&cert).unwrap(),
            "the certificate is the canonical encoding the in-process API reads"
        );
        // It is a key the API in process takes, as it stands.
        tilo_api::direct::Direct::new(&file).unwrap();

        // The workspace the device entered is the one a command acts in when
        // `-w` names none: its id, and the name the device keeps for it.
        assert_eq!(
            api::entered_workspace(Reach {
                api: None,
                key_file: None,
                json: false,
            })
            .unwrap(),
            Some((
                "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".to_owned(),
                "Studio".to_owned()
            ))
        );
        // A key file is someone else's key: it names no workspace of its own.
        assert_eq!(
            api::entered_workspace(Reach {
                api: None,
                key_file: Some(Path::new("visa.json")),
                json: false,
            })
            .unwrap(),
            None
        );

        exports_a_key_of_its_own(dir.path(), &root, &cert, &file, &mut env);
    }

    /// `tilo keys export` asks for a new key, as `tilo keys create` does, and writes that one,
    /// never the device's own (`file`, as the login under `dir` keeps it): the script it is for
    /// and this computer each hold a key alone.
    fn exports_a_key_of_its_own(
        dir: &Path,
        root: &ed25519_dalek::SigningKey,
        cert: &tilo_user_identity::DeviceCert,
        file: &tilo_api::direct::KeyFile,
        env: &mut tilo_test_env::ScopedEnv,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        let now = tilo_clock::unix_now_ns();
        let script = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let mut script_cert = cert.clone();
        script_cert.device_public_key = script.verifying_key().to_bytes();
        script_cert.device_label = "Visa backend".into();
        tilo_user_identity::sign_device_cert(&mut script_cert, root).unwrap();
        let token = tilo_api::store::key_token(
            "Visa backend",
            &tilo_api::grant_key::GrantKey::held(script.clone(), script_cert, now).unwrap(),
        )
        .unwrap();
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let api_server = test_http::Responder::handle({
            let asked = Arc::clone(&asked);
            move |request| {
                asked.lock().unwrap().push(request.target.clone());
                let body = match request.target.as_str() {
                    "/keys/new" => (
                        201,
                        serde_json::json!({"poll": "p", "interval": 1,
                            "approve_url": "https://mcp.tilo.test/keys/approve/a"}),
                    ),
                    "/keys/poll" => (200, serde_json::json!({"key": token, "grant": "g"})),
                    _ => (404, serde_json::json!({})),
                };
                Some(
                    test_http::Reply::status(body.0)
                        .header("content-type", "application/json")
                        .body(body.1.to_string()),
                )
            }
        });
        let nobody = tempfile::tempdir().unwrap();
        env.set(crate::identity::DIR_ENV, nobody.path());
        let out = dir.join("visa.json");
        let api_url = api_server.url();
        let reach = Reach {
            api: Some(&api_url),
            key_file: None,
            json: false,
        };
        let exported = crate::hosted::keys(
            reach,
            None,
            crate::hosted::KeysCmd::Export {
                name: "Visa backend".into(),
                out: out.clone(),
                expires_in: None,
            },
        );
        assert_eq!(exported, ExitCode::SUCCESS);
        assert_eq!(
            *asked.lock().unwrap(),
            ["/keys/new", "/keys/poll"],
            "the key was asked for and approved, as `tilo keys create` asks"
        );
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600,
            "only this account may read a key"
        );
        let written = tilo_api::direct::KeyFile::read(&out).unwrap();
        assert_ne!(written.key, file.key, "never the device's own key");
        assert_eq!(written.key, hex::encode(script.to_bytes()));
        assert_eq!(written.name, "Visa backend");
        assert_eq!(written.host, file.host, "it reaches the login's server");
        assert_eq!(written.site, file.site, "an exported key keeps its site");
        tilo_api::direct::Direct::new(&written).unwrap();
        // It acts as itself; a key file naming the device's own key goes to the held session.
        let as_file = |path: &Path| {
            api::held_api_for(Reach {
                api: None,
                key_file: Some(path),
                json: false,
            })
            .unwrap()
        };
        assert_eq!(as_file(&out), None);
        let own = dir.join("own.json");
        std::fs::write(&own, serde_json::to_vec(file).unwrap()).unwrap();
        assert!(as_file(&own).is_some());
    }

    /// A folder goes up whole, each file named by its path from the folder's
    /// own place, in one order however the tree is walked; a file is itself.
    #[test]
    fn a_folder_keeps_its_structure_and_a_file_is_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("contracts");
        std::fs::create_dir_all(root.join("2026/signed")).unwrap();
        std::fs::write(root.join("nda.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(root.join("2026/plan.md"), b"# plan").unwrap();
        std::fs::write(root.join("2026/signed/nda.pdf"), b"%PDF-1.7").unwrap();
        let names: Vec<String> = files_under(&root)
            .unwrap()
            .into_iter()
            .map(|file| file.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "contracts/2026/plan.md",
                "contracts/2026/signed/nda.pdf",
                "contracts/nda.pdf"
            ]
        );
        let one = files_under(&root.join("nda.pdf")).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "nda.pdf");
    }

    /// Sizes read as a person says them.
    #[test]
    fn a_size_reads_in_the_unit_a_person_would_say() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(300 * 1024 * 1024), "300.0 MB");
    }

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
            ("PUT", route) if route.ends_with("/unattended") => json_reply(
                200,
                json!({"status": "ok", "path": "workspace/unattended#set",
                    "result": {"workspace": WORKSPACE, "unattended": true}}),
            ),
            ("POST", route) if route.ends_with("/branches") => json_reply(
                201,
                json!({"id": BRANCH, "name": "Applicant: Jane Doe, second try",
                    "host": "eu1.tilo.sh", "role": "owner",
                    "labels": {"customer_id": "cus_8121"}}),
            ),
            ("POST", route) if route.ends_with("/contents/have") => {
                let asked: Value = serde_json::from_slice(&request.body).unwrap_or_default();
                let count = asked
                    .get("hashes")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                json_reply(200, json!({"have": vec![false; count]}))
            }
            ("PUT", route) if route.ends_with("/contents") => {
                let asked: Value = serde_json::from_slice(&request.body).unwrap_or_default();
                let count = asked
                    .get("files")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                json_reply(200, json!({"contents": vec!["tilo.content:abc"; count]}))
            }
            ("POST", route) if route.ends_with("/uploads") => json_reply(
                201,
                json!({"content": "tilo.content:abc", "size": request.body.len()}),
            ),
            ("POST", route) if route.ends_with("/actions/open") => json_reply(
                200,
                json!({"status": "ok", "path": "files#open", "invocation": "inv-1",
                    "result": {}}),
            ),
            ("GET", route) if route.contains("/content/") => {
                Some(test_http::Reply::status(200).body(b"hello".to_vec()))
            }
            ("GET", route) if route.contains("/invocations") => json_reply(
                200,
                json!({"items": [{"id": "run-2", "status": "waiting",
                    "by": {"name": "Jane Doe's agent", "kind": "agent"}}],
                    "total": 2, "waiting": 1, "next_cursor": "17:s1",
                    "text": "2 invocations, newest first; 1 waiting for the person\n\
                             run-2 waiting by Jane Doe's agent (agent): 0 changes"}),
            ),
            ("GET", route) if route.contains("/resources?path=") => json_reply(
                200,
                json!({"path": "notes", "rows": [], "text": "notes  (notes.notebook)"}),
            ),
            ("GET", route) if route.contains("/resources/") => json_reply(
                200,
                json!({"path": "notes/Notebook/Todo", "text": "# Todo"}),
            ),
            _ => json_reply(
                404,
                json!({"error": {"code": "not_found", "message": "no such route"}}),
            ),
        }
    }

    /// was sent.
    fn fake_api() -> test_http::Responder {
        test_http::Responder::handle(answer)
    }

    /// `Reach` names.
    fn keyed() -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(api::KEY_ENV, "tilo_sk_test");
        env
    }

    tilo_test_env::scenario! {
        fn the_hosted_commands_call_the_api;

        phase the_hosted_commands_are_the_apis_routes;
        // `docs/developers/api.md`, with the headers it takes.
        {
            let _key = keyed();
            let api_server = fake_api();
            let url = api_server.url();
            let reach = Reach {
                api: Some(&url),
                key_file: None,
                json: false,
            };

            // A workspace is made as `POST /v1/workspaces`, once per idempotency key.
            workspace(
                reach,
                None,
                WorkspaceCmd::Create {
                    name: "Applicant: Jane Doe".into(),
                    labels: vec![("customer_id".into(), "cus_8121".into())],
                },
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
                json!({"name": "Applicant: Jane Doe", "labels": {"customer_id": "cus_8121"}})
            );
            // A branch is `POST /v1/workspaces/{source}/branches`, its source
            // named by a read of the list, its labels as create takes them.
            workspace(
                reach,
                None,
                WorkspaceCmd::Branch {
                    source: "Applicant: Jane Doe".into(),
                    name: "Applicant: Jane Doe, second try".into(),
                    labels: vec![("customer_id".into(), "cus_8121".into())],
                },
            );
            let branched = api_server.drain_requests();
            assert_eq!(branched.len(), 2, "the source is named by the list first");
            let branch = branched.last().expect("the branch");
            assert_eq!(branch.method, "POST");
            assert_eq!(branch.target, format!("/v1/workspaces/{WORKSPACE}/branches"));
            assert!(
                branch.header("idempotency-key").is_some(),
                "a call that makes something names its invocation"
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&branch.body).unwrap(),
                json!({"name": "Applicant: Jane Doe, second try",
                    "labels": {"customer_id": "cus_8121"}})
            );
            // Running commands unattended is `PUT /v1/workspaces/{id}/unattended`,
            // held while the person decides.
            workspace(
                reach,
                None,
                WorkspaceCmd::Unattended {
                    workspace: "Applicant: Jane Doe".into(),
                    off: false,
                },
            );
            let unattended = api_server.drain_requests();
            let asked = unattended.last().expect("the setting");
            assert_eq!(asked.method, "PUT");
            assert_eq!(asked.target, format!("/v1/workspaces/{WORKSPACE}/unattended"));
            assert_eq!(asked.header("prefer"), Some("wait=60"));
            assert_eq!(
                serde_json::from_slice::<Value>(&asked.body).unwrap(),
                json!({"on": true})
            );
            // Listing by a label is `GET /v1/workspaces?label[name]=value`.
            workspace(
                reach,
                None,
                WorkspaceCmd::Ls {
                    labels: vec![("customer_id".into(), "cus_8121".into())],
                },
            );
            let listed = api_server.drain_requests();
            assert_eq!(
                listed.last().map(|request| request.target.as_str()),
                Some("/v1/workspaces?label%5Bcustomer%5Fid%5D=cus%5F8121")
            );

            // A file goes in as one upload, and the reference it answers is what
            // the command prints.
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("passport.png");
            std::fs::write(&file, b"hello").unwrap();
            files(
                reach,
                "applicant-jane-doe",
                FilesCmd::Put {
                    paths: vec![file],
                    to: None,
                    store: false,
                },
            );
            let put = api_server.drain_requests();
            // Content the workspace already holds is not sent again: the
            // command asks about the file's address first.
            let asked = put
                .iter()
                .find(|request| request.target.ends_with("/contents/have"))
                .expect("the file's address is asked about");
            assert_eq!(asked.method, "POST");
            let asked: Value = serde_json::from_slice(&asked.body).unwrap();
            let hashes = asked["hashes"].as_array().expect("an address each");
            assert_eq!(hashes.len(), 1);
            assert!(hashes[0].as_str().is_some_and(|hash| hash.len() == 64));
            // The file goes up whole in one call, base64 as the route takes it.
            let upload = put
                .iter()
                .find(|request| request.target.ends_with("/contents"))
                .expect("the file goes up");
            assert_eq!(upload.target, format!("/v1/workspaces/{WORKSPACE}/contents"));
            assert_eq!(upload.method, "PUT");
            assert_eq!(
                serde_json::from_slice::<Value>(&upload.body).unwrap(),
                json!({"files": ["aGVsbG8="]})
            );
            assert!(
                put.iter()
                    .all(|request| !request.target.ends_with("/uploads")),
                "the file does not go up one request to a call"
            );
            // The bytes are then written into the workspace's files: a Files
            // open names the contents and the names they take, and answers
            // the paths they are at.
            let placed = put
                .iter()
                .find(|request| request.target.ends_with("/actions/open"))
                .expect("the file is put into the workspace");
            assert_eq!(
                placed.target,
                format!("/v1/workspaces/{WORKSPACE}/resources/files/actions/open")
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&placed.body).unwrap(),
                json!({"files": [{"content": "tilo.content:abc", "name": "passport.png",
                                  "dir": ""}]})
            );

            // A folder goes up as one open, however many files it holds: many
            // uploads, one change of the workspace's tree.
            let folder = dir.path().join("contracts");
            std::fs::create_dir_all(folder.join("2026")).unwrap();
            for name in ["nda.pdf", "2026/plan.md", "2026/signed.pdf"] {
                std::fs::write(folder.join(name), b"hello").unwrap();
            }
            files(
                reach,
                "applicant-jane-doe",
                FilesCmd::Put {
                    paths: vec![folder],
                    to: Some("docs".into()),
                    store: false,
                },
            );
            let folder_put = api_server.drain_requests();
            let uploads: Vec<&test_http::Request> = folder_put
                .iter()
                .filter(|request| request.target.ends_with("/uploads"))
                .collect();
            assert_eq!(uploads.len(), 1, "the list goes up as content, once");
            let sent: Vec<&test_http::Request> = folder_put
                .iter()
                .filter(|request| request.target.ends_with("/contents"))
                .collect();
            assert_eq!(sent.len(), 1, "the folder's files go up in one call");
            let sent: Value = serde_json::from_slice(&sent[0].body).unwrap();
            assert_eq!(
                sent["files"].as_array().map(Vec::len),
                Some(3),
                "every file is in that call"
            );
            let listed = uploads
                .iter()
                .find(|request| request.body.starts_with(b"["))
                .expect("the list of files goes up as content");
            assert_eq!(
                serde_json::from_slice::<Value>(&listed.body).unwrap(),
                json!([
                    {"content": "tilo.content:abc", "name": "plan.md", "dir": "docs/contracts/2026"},
                    {"content": "tilo.content:abc", "name": "signed.pdf", "dir": "docs/contracts/2026"},
                    {"content": "tilo.content:abc", "name": "nda.pdf", "dir": "docs/contracts"},
                ]),
                "each file keeps its place under the folder it came from"
            );
            let opened: Vec<&test_http::Request> = folder_put
                .iter()
                .filter(|request| request.target.ends_with("/actions/open"))
                .collect();
            assert_eq!(opened.len(), 1, "the folder is one change of the tree");
            assert_eq!(
                serde_json::from_slice::<Value>(&opened[0].body).unwrap(),
                json!({"files": "tilo.content:abc"}),
                "the open names the list, however many files it holds"
            );

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
            assert_eq!(action.header("tilo-invocation-title"), Some("pdf-viewer#open"));
            assert!(
                action
                    .header("tilo-invocation")
                    .is_some_and(|id| id == action.header("idempotency-key").unwrap_or_default()),
                "one invocation, one idempotency key"
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&action.body).unwrap(),
                json!({})
            );

            // A listing and a read are the routes `tilo_ls` and `tilo_read`
            // answer: the same call, in the words a person's command takes.
            ls(reach, "applicant-jane-doe", Some("notes/Todo"), None);
            let listed = api_server.drain_requests();
            let listing = listed.last().expect("the listing");
            assert_eq!(listing.method, "GET");
            assert_eq!(
                listing.target,
                format!("/v1/workspaces/{WORKSPACE}/resources?path=notes%2FTodo")
            );
            // A listing past its first page names the cursor it ended with.
            ls(reach, "applicant-jane-doe", Some("notes/Todo"), Some("12"));
            let paged = api_server.drain_requests();
            assert_eq!(
                paged.last().expect("the listing").target,
                format!("/v1/workspaces/{WORKSPACE}/resources?path=notes%2FTodo&cursor=12")
            );
            read(
                reach,
                "applicant-jane-doe",
                "notes/Notebook/Todo",
                Some("12"),
            );
            let read_requests = api_server.drain_requests();
            assert_eq!(
                read_requests.last().expect("the read").target,
                format!(
                    "/v1/workspaces/{WORKSPACE}/resources/notes/Notebook/Todo?cursor=12"
                )
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

            // A fellow administrator is invited by the role the API reads.
            members(
                reach,
                "applicant-jane-doe",
                PeopleCmd::Invite {
                    role: "administrator".into(),
                    email: Some("ops@example.com".into()),
                    waiting: None,
                },
            );
            let invited = api_server.drain_requests();
            let invitation = invited.last().expect("the invitation");
            assert_eq!(invitation.method, "POST");
            assert_eq!(
                invitation.target,
                format!("/v1/workspaces/{WORKSPACE}/members/invitations")
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&invitation.body).unwrap(),
                json!({"role": "administrator", "email": "ops@example.com"})
            );

            // Someone in is moved by their key, and removed by it.
            let ada = "ab".repeat(32);
            members(
                reach,
                "applicant-jane-doe",
                PeopleCmd::Role {
                    key: ada.clone(),
                    role: "editor".into(),
                },
            );
            let moved = api_server.drain_requests();
            let moved = moved.last().expect("the move");
            assert_eq!(moved.method, "PUT");
            assert_eq!(
                moved.target,
                format!("/v1/workspaces/{WORKSPACE}/members/{ada}/role")
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&moved.body).unwrap(),
                json!({"role": "editor"})
            );
            members(reach, "applicant-jane-doe", PeopleCmd::Rm { key: ada.clone() });
            let removed = api_server.drain_requests();
            let removed = removed.last().expect("the removal");
            assert_eq!(removed.method, "DELETE");
            assert_eq!(
                removed.target,
                format!("/v1/workspaces/{WORKSPACE}/members/{ada}")
            );

            // A workspace is named again with `PUT …/name`.
            workspace(
                reach,
                None,
                WorkspaceCmd::Rename {
                    workspace: "applicant-jane-doe".into(),
                    name: "Jane Doe".into(),
                },
            );
            let renamed = api_server.drain_requests();
            let renamed = renamed.last().expect("the rename");
            assert_eq!(renamed.method, "PUT");
            assert_eq!(renamed.target, format!("/v1/workspaces/{WORKSPACE}/name"));
            assert_eq!(
                serde_json::from_slice::<Value>(&renamed.body).unwrap(),
                json!({"name": "Jane Doe"})
            );
        }

        phase a_workspaces_invocations_are_listed_page_by_page;
        // `tilo invocations` is `GET …/invocations`, its limit and cursor the
        // query's, and says how to read on.
        {
            let _key = keyed();
            let api_server = fake_api();
            let url = api_server.url();
            let reach = Reach {
                api: Some(&url),
                key_file: None,
                json: false,
            };
            assert_eq!(
                invocations(reach, "applicant-jane-doe", Some(1), Some("17:s0")),
                ExitCode::SUCCESS
            );
            let listed = api_server.drain_requests();
            let listing = listed.last().expect("the listing");
            assert_eq!(listing.method, "GET");
            assert_eq!(
                listing.target,
                format!("/v1/workspaces/{WORKSPACE}/invocations?limit=1&cursor=17%3As0")
            );
            let said = invocations_text(&json!({
                "items": [], "total": 2, "waiting": 1, "next_cursor": "17:s1",
                "text": "2 invocations, newest first; 1 waiting for the person",
            }));
            assert!(
                said.starts_with("2 invocations, newest first; 1 waiting"),
                "{said}"
            );
            assert!(
                said.ends_with("\nolder: tilo invocations --cursor 17:s1"),
                "{said}"
            );
        }

        phase a_server_that_did_not_answer_is_asked_again_under_the_same_idempotency_key;
        // A server that says it is briefly unavailable answers `503`; the same
        // call is asked again under the id it first went with, so the host
        // runs one invocation however often it is retried.
        {
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
                        503,
                        json!({"error": {"code": "unavailable", "message": "still running"}}),
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
            assert_eq!(
                sent.len(),
                2,
                "the unavailable answer is retried, not reported"
            );
            assert_eq!(sent[1].target, "/v1/keys/grant-1");
            assert_eq!(
                sent[0].header("idempotency-key"),
                sent[1].header("idempotency-key")
            );
        }
    }

    tilo_test_env::scenario! {
        fn hosted_arguments_and_answers_read_as_a_person_wrote_them;

        phase options_are_json_where_they_parse;
        {
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

        phase a_workspace_is_named_by_its_words;
        {
            assert_eq!(api::slug("Applicant: Jane Doe"), "applicant-jane-doe");
            assert_eq!(api::slug("applicant-jane-doe"), "applicant-jane-doe");
            assert!(api::is_uuid("3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b"));
            assert!(!api::is_uuid("jane-doe"));
        }

        phase a_workspace_line_names_its_id_and_where_it_lives;
        {
            let row = serde_json::json!({"id": "3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b",
                "name": "Applicant: Jane Doe", "host": "eu1.tilo.sh", "role": "owner"});
            assert_eq!(
                workspace_line(&row),
                "Applicant: Jane Doe (id 3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b, on eu1.tilo.sh, \
             owner)"
            );
        }

        phase an_action_answer_says_what_ran_and_where_it_is;
        {
            let done = serde_json::json!({"status": "ok", "path": "notes#open",
                "invocation": "inv-1", "link": "https://tilo.sh/#/w/x/notes/Todo"});
            assert_eq!(
                action_text(&done),
                "ok notes#open\nhttps://tilo.sh/#/w/x/notes/Todo"
            );
            // What it made or changed, as a path the next command takes as it
            // is: the answer an agent's create prints is not lost.
            let created = serde_json::json!({"status": "ok", "path": "tasks#create",
                "resources": ["tasks.tracker/issues/7"]});
            assert_eq!(
                action_text(&created),
                "ok tasks#create\n-> tasks.tracker/issues/7"
            );
            let pending = serde_json::json!({"status": "pending", "invocation": "inv-2",
                "path": "pdf-viewer#import-pdf-content", "ask": {"url": "https://tilo.sh/approve/1"}});
            assert_eq!(
                action_text(&pending),
                "pending inv-2: it waits for you, at https://tilo.sh/approve/1 \
             (pdf-viewer#import-pdf-content)"
            );
        }

        phase a_listing_and_a_read_print_what_the_host_said;
        {
            // The host's own text, unrendered: `tilo ls` and `tilo read` print
            // exactly the lines `tilo_ls` and `tilo_read` answer an agent, so
            // a person and an agent read one listing. `--json` keeps the typed
            // answer beside it.
            let listed = serde_json::json!({"path": "notes", "kind": "notes.notebook",
                "rows": [{"path": "notes/Notebook", "label": "Notebook", "kind": "notes.notebook"}],
                "text": "notes  (notes.notebook)\nNotebook  notes.notebook"});
            assert_eq!(
                text(&listed, "text"),
                "notes  (notes.notebook)\nNotebook  notes.notebook"
            );
            let read = serde_json::json!({"path": "notes/Notebook/Todo", "text": "# Todo\n"});
            assert_eq!(text(&read, "text"), "# Todo\n");
        }

        phase the_people_of_a_workspace_are_a_line_each;
        {
            // One place each, as the workspace names it: an owner is an administrator and an
            // editor beneath, and reads as an owner alone.
            let listed = serde_json::json!({
                "members": [{"name": "Ada", "role": "editor", "owner": true, "administrator": true,
                             "place": "owner"},
                            {"name": "Rosa Ortiz", "role": "editor", "place": "editor"}],
                "invitations": [{"invite": "inv-1", "role": "viewer", "invitee_name": "Bo",
                                 "place": "viewer"},
                                {"invite": "inv-2", "role": "editor", "administrator": true,
                                 "owner": true, "place": "owner"}],
            });
            assert_eq!(
                members_text(&listed),
                "members:\n  Ada — owner\n  Rosa Ortiz — editor\n\
             invitations:\n  viewer invitation for Bo, inv-1\n  owner invitation, inv-2"
            );
            let alone = serde_json::json!({"members": [], "invitations": []});
            assert_eq!(
                members_text(&alone),
                "members:\n  nobody yet\ninvitations: none open"
            );
        }
    }
}
