//! Hosted workspaces from a shell: `tilo keys`, `tilo workspace`,
//! `tilo do -w`, `tilo files` and `tilo run`.
//!
//! Each command is a call of Tilo's actions ([`crate::api::Calls`]), as the
//! agent surface's tools make it: a workspace and its people, keys, programs
//! and files are actions there. The caller is a key file `--key-file` (or
//! `TILO_KEY_FILE`) names, or, when this computer logged in (`tilo login`), its
//! own device: each command a call to this computer's daemon, which keeps that
//! device's one session, since a second session would cut it off.
//!
//! `tilo keys create` is the one command that asks the person: it waits at
//! the API server's own key page until they approve one in their Tilo.

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::api::{self, Calls, Reach};
use tilo_api::v1::lines;

mod key_api;

/// Where Tilo's SDKs read their key: what `tilo keys create` answers is set there.
const SDK_KEY_ENV: &str = "TILO_API_KEY";

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
        /// (a name or id), it reaches only that workspace.
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
    /// Revoke a key, by its name or grant as `tilo keys ls` lists it, and every key it minted.
    /// On a computer logged in as the person, any key of theirs, `tilo keys export`'s included.
    Revoke {
        /// The key's name, or its grant, as `tilo keys ls` lists them.
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
        /// (a name or id), it reaches only that workspace.
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
    /// Whether the server asked keeps all the content the workspace's
    /// records name (-w), or how much is still only on its members' devices.
    #[command(after_help = "Example:\n  tilo workspace servers content -w Tilo\n\n\
                      Before you switch off the last device that holds a copy of the workspace \
                      (a laptop, say), this says 0: the server keeps every file and attachment, \
                      and fetches what it lacks from the members' devices while they are on.")]
    Content,
    /// Have the server whose own line is HOST keep the workspace (-w) as well.
    #[command(
        after_help = "Example:\n  tilo workspace servers add \"$(cat web-host.json)\" -w Tilo\n\n\
                      HOST is that server's own web-host.json line, which its operator gives you: \
                      its certificate and where it is dialled. Both servers keep the workspace \
                      from then on: a change on either reaches the other, and its people reach it \
                      at either. Only its owners choose its servers, and the other server takes it \
                      only if it let you in and has capacity for it: sign in there first."
    )]
    Add {
        /// The other server's own `web-host.json` line.
        host: String,
    },
    /// Have a server keep the workspace (-w) no more.
    #[command(
        after_help = "Example:\n  tilo workspace servers remove https://tilo.sh -w Tilo\n\n\
                      SERVER is its name as `tilo workspace servers ls` shows it, or its device \
                      key. It is no longer named to anyone at once, and lets the workspace go when \
                      it hears so."
    )]
    Remove {
        /// The server's name in the list, or its device key.
        server: String,
    },
    /// Move the workspace (-w) from the server FROM to the server whose own
    /// line is HOST.
    #[command(
        after_help = "Example:\n  tilo workspace servers move tilo.sh \"$(cat web-host.json)\" -w Tilo\n\n\
                      HOST is the moving-to server's own web-host.json line, which its operator \
                      gives you. HOST keeps the workspace first, then FROM keeps it no more. If \
                      HOST refuses, the workspace stays where it was. FROM is its name as the \
                      server list shows it, or its device key."
    )]
    Move {
        /// The server it moves from: its name in the list, or its device key.
        from: String,
        /// The moving-to server's own `web-host.json` line.
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
                      tilo workspace servers remove tilo.sh -w Tilo")]
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
                      Whoever the workspace grants removing removes people, a moderator included."
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
    /// Your name from now on: what every workspace you are in, its people and
    /// the invitations you make say for you, and every device of yours shows.
    #[command(
        after_help = "Example:\n  tilo workspace people name \"Quinten Wolff\" -w Acme\n\n\
                      Any device or key of yours sets it, asked in any workspace of yours: the \
                      server keeping it keeps the name with your profile, which your browsers \
                      and other devices show, and names you by it in every workspace it keeps. \
                      Each workspace knows you by it before a browser of yours joins, and whoever \
                      you invite sees who asked them in."
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

/// The actions `reach` names: a key file's own session, else this computer's daemon, started
/// when none is up.
async fn open(reach: Reach<'_>) -> Result<Calls> {
    Calls::open_with(reach, crate::mcp::reach_daemon).await
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
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

/// `tilo do <path> name=value… -w <workspace>`: one action of a hosted
/// workspace, as `app/resource#action`, said as `tilo do` says an action
/// this desktop ran ([`tilo_control::front::say`]).
pub fn act(reach: Reach<'_>, workspace: &str, path: &str, args: &[String]) -> ExitCode {
    crate::log_to_stderr(tracing_subscriber::filter::LevelFilter::OFF);
    let acting = async {
        let client = open(reach).await?;
        let Some((resource, action)) = path.rsplit_once('#') else {
            bail!("{path:?} names no action: write it as app/resource#action, like notes#open");
        };
        let workspace = client.workspace_id(workspace).await?;
        let arguments =
            Value::Object(tilo_control::front::arguments(args).map_err(anyhow::Error::msg)?);
        client
            .act(&workspace, resource, action, &arguments, path)
            .await
    };
    let answered = runtime()
        .and_then(|runtime| runtime.block_on(acting))
        .map_err(|error| format!("tilo: {error:#}"));
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = tilo_control::front::say(
        answered,
        reach.json,
        &mut tilo_control::front::Io {
            out: &mut stdout.lock(),
            err: &mut stderr.lock(),
        },
    );
    ExitCode::from(code)
}

/// `tilo ls [path] -w <workspace>`: what the workspace holds, or what `path`
/// holds, as `tilo_ls` lists it: the host's own lines, so a person and an
/// agent read the same text, and `cursor` reads on from where a page ended.
pub fn ls(reach: Reach<'_>, workspace: &str, path: Option<&str>, cursor: Option<&str>) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let listed = client.list(&workspace, path, cursor).await?;
        Ok(show(reach.json, &listed, &lines::listing(&listed)))
    })
}

/// `tilo read <path> -w <workspace>`: one resource, as `tilo_read` reads it.
pub fn read(reach: Reach<'_>, workspace: &str, path: &str, cursor: Option<&str>) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let read = client.read(&workspace, path.trim(), cursor).await?;
        Ok(show(reach.json, &read, &lines::resource(&read)))
    })
}

/// `tilo invocations -w <workspace>`: the workspace's invocations in records
/// the key may read, newest first, as `invocations#list` answers them: `limit`
/// to a page, and the `cursor` a page ended with for the older ones.
pub fn invocations(
    reach: Reach<'_>,
    workspace: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let mut arguments = serde_json::Map::new();
        if let Some(limit) = limit {
            arguments.insert("limit".into(), limit.into());
        }
        if let Some(cursor) = cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) {
            arguments.insert("cursor".into(), cursor.into());
        }
        let answered = client
            .act(
                &workspace,
                "invocations",
                "list",
                &Value::Object(arguments),
                "invocations#list",
            )
            .await?;
        let listed = answered.get("result").cloned().unwrap_or(Value::Null);
        Ok(show(reach.json, &listed, &invocations_text(&listed)))
    })
}

/// A workspace's invocations as a person reads them: how many, one line per
/// invocation (who made it, how it stands, what it changed), and the command
/// that reads on.
fn invocations_text(listed: &Value) -> String {
    let count = |name: &str| listed.get(name).and_then(Value::as_u64).unwrap_or(0);
    let plural = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut said = format!(
        "{}, newest first",
        plural(count("total"), "invocation", "invocations")
    );
    if count("waiting") > 0 {
        let _ = write!(said, "; {} waiting for the person", count("waiting"));
    }
    for listed in listed
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |name: &str| listed.get(name).and_then(Value::as_str);
        let number = |name: &str| listed.get(name).and_then(Value::as_u64).unwrap_or(0);
        let by = |name: &str| {
            listed
                .pointer(&format!("/by/{name}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let id = field("id").or_else(|| field("step")).unwrap_or_default();
        let _ = write!(
            said,
            "\n{id} {} by {} ({})",
            field("status").unwrap_or_default(),
            by("name"),
            by("kind")
        );
        if let Some(title) = field("title") {
            let _ = write!(said, " \"{title}\"");
        }
        let _ = write!(said, ": {}", plural(number("changes"), "change", "changes"));
        if number("undone") > 0 {
            let _ = write!(said, ", {} undone", number("undone"));
        }
        if let Some(about) = field("summary").or_else(|| field("action")) {
            let _ = write!(said, "; {about}");
        }
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
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let view = client
            .view(&workspace, path.trim(), picture.is_some())
            .await?;
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
        let client = open(reach).await?;
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
    runtime()?.block_on(async { read_surfaces(&open(reach).await?, workspace).await })
}

async fn read_surfaces(client: &Calls, workspace: &str) -> Result<Vec<crate::sdk::App>> {
    let workspace = client.workspace_id(workspace).await?;
    let mut apps = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let listing = client.list(&workspace, None, cursor.as_deref()).await?;
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
            let read = match client.read(&workspace, path, None).await {
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
        let client = open(reach).await?;
        match cmd {
            WorkspaceCmd::Ls { labels } => list_workspaces(&client, reach.json, &labels).await,
            WorkspaceCmd::Create { name, labels } => {
                let labels: serde_json::Map<String, Value> = labels
                    .into_iter()
                    .map(|(name, value)| (name, Value::String(value)))
                    .collect();
                let made = client
                    .account(
                        "account/workspaces",
                        "create",
                        &serde_json::json!({"name": name, "labels": labels}),
                    )
                    .await?;
                Ok(show(
                    reach.json,
                    &made,
                    &lines::workspace(made.get("workspace").unwrap_or(&Value::Null)),
                ))
            }
            WorkspaceCmd::Rename {
                workspace: named,
                name,
            } => {
                let id = client.workspace_id(&named).await?;
                let renamed = client
                    .account(
                        "account/workspaces",
                        "rename",
                        &serde_json::json!({"workspace": id, "name": name}),
                    )
                    .await?;
                Ok(show(
                    reach.json,
                    &renamed,
                    &lines::workspace(renamed.get("workspace").unwrap_or(&Value::Null)),
                ))
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
                    .account(
                        "account/workspaces",
                        "branch",
                        &serde_json::json!({"workspace": id, "name": name, "labels": labels}),
                    )
                    .await?;
                Ok(show(
                    reach.json,
                    &made,
                    &lines::workspace(made.get("workspace").unwrap_or(&Value::Null)),
                ))
            }
            WorkspaceCmd::Unattended {
                workspace: named,
                off,
            } => unattended(&client, reach.json, &named, off).await,
            WorkspaceCmd::Rm { workspace: named } => {
                let id = client.workspace_id(&named).await?;
                let deleted = client
                    .account(
                        "account/workspaces",
                        "delete",
                        &serde_json::json!({ "workspace": id }),
                    )
                    .await?;
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
    client: &Calls,
    json: bool,
    labels: &[(String, String)],
) -> Result<ExitCode> {
    let labels: serde_json::Map<String, Value> = labels
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    let listed = client
        .account(
            "account/workspaces",
            "list",
            &serde_json::json!({ "labels": labels }),
        )
        .await?;
    Ok(show(json, &listed, &lines::workspaces(&listed)))
}

/// `tilo workspace unattended <workspace> [--off]`: held while the
/// person decides on their approving device, and said as it stands then.
async fn unattended(client: &Calls, json: bool, named: &str, off: bool) -> Result<ExitCode> {
    let id = client.workspace_id(named).await?;
    let answer = client
        .act(
            &id,
            "account/workspaces",
            "unattended",
            &serde_json::json!({"workspace": id, "on": !off}),
            "account/workspaces#unattended",
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
        let client = open(reach).await?;
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
    client: &Calls,
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

/// How many files one action puts into the workspace's tree: a folder larger
/// than this goes in more runs, each answered within the peer's half minute,
/// whatever a loaded disk makes a node cost.
const TREE_BATCH_FILES: usize = 2_000;
/// How many bytes of file entries one change carries: under the 64 MiB one
/// upload carries, so a large folder goes in a few changes rather than one
/// no request carries.
const TREE_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// How long one follow of a put Files is still saving waits, and how many
/// follows in a row: a huge folder saved slowly is followed for a few minutes,
/// and one that is never saved is a failure the command reports.
const TREE_SAVE_WAIT_SECONDS: u64 = 10;
/// How many follows of one put's save, at most.
const TREE_SAVE_FOLLOWS: usize = 30;

/// `file`'s length, as the disk says it.
fn file_size(file: &Uploading) -> u64 {
    std::fs::metadata(&file.path).map_or(0, |meta| meta.len())
}

/// Upload `files`, sending none the workspace already holds ([`Calls::put_files`]). Tells a
/// person at a terminal how far it is, and answers each with the `tilo.content:` reference it is
/// kept by, in name order.
pub(crate) async fn upload_all(
    client: &Calls,
    workspace: &str,
    files: Vec<Uploading>,
) -> Result<Vec<(Uploading, String)>> {
    let total: u64 = files.iter().map(file_size).sum();
    let name = match files.as_slice() {
        [one] => one.name.clone(),
        _ => format!("{} files", files.len()),
    };
    let mut progress = Progress::new(&name);
    progress.told(0, total);
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    let mut done = 0_u64;
    let contents = client
        .put_files(workspace, &paths, &mut |bytes| {
            done = done.saturating_add(bytes);
            progress.told(done.min(total), total);
        })
        .await
        .context("storing the files")?;
    progress.done();
    Ok(files.into_iter().zip(contents).collect())
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
    client: &Calls,
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
    // A pending answer is a change Files has not saved yet: the command follows it and only
    // then says where each file is, so a put that failed is a failure here rather than a path
    // printed over nothing.
    let answer = followed(client, workspace, answer).await?;
    // The open answers how many files it put, not each one: the paths are
    // where the tree keeps what was asked for.
    let counted = answer.pointer("/result/count").and_then(Value::as_u64);
    if counted != Some(files.len() as u64) {
        bail!(
            "Files put {} of the {} files: {answer}",
            counted.map_or_else(|| "none".to_owned(), |count| count.to_string()),
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

/// `answer` of a put, held while it is still saving what it wrote: the invocation followed
/// until it is done, refused or answered as failed. A put that never finishes is a failure, not
/// a success: the files it names are not in the workspace.
async fn followed(client: &Calls, workspace: &str, mut answer: Value) -> Result<Value> {
    for _ in 0..TREE_SAVE_FOLLOWS {
        if answer.get("status").and_then(Value::as_str) != Some("pending") {
            return Ok(answer);
        }
        let Some(invocation) = answer.get("invocation").and_then(Value::as_str) else {
            bail!("Files is still saving this folder and named no invocation to follow: {answer}");
        };
        answer = client
            .invocation(workspace, invocation, TREE_SAVE_WAIT_SECONDS)
            .await?;
    }
    if answer.get("status").and_then(Value::as_str) == Some("pending") {
        let waited = TREE_SAVE_WAIT_SECONDS.saturating_mul(TREE_SAVE_FOLLOWS as u64);
        bail!("Files is still saving this folder after {waited}s: {answer}");
    }
    Ok(answer)
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
    client: &Calls,
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

/// The workspace's people, as a path names them.
const PEOPLE: &str = "workspace/members";

/// `tilo workspace people … -w <workspace>`: the workspace's people, its invitations and its
/// wait list, as the workspace's own `workspace/members`, `workspace/invitations` and
/// `workspace/waiting` actions.
fn members(reach: Reach<'_>, workspace: &str, cmd: PeopleCmd) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let act = |path: String, action: &'static str, args: Value| {
            let (client, workspace) = (&client, &workspace);
            async move {
                let answer = client
                    .act(workspace, &path, action, &args, &format!("{path}#{action}"))
                    .await?;
                anyhow::Ok(answer.get("result").cloned().unwrap_or_default())
            }
        };
        let (answer, said) = match cmd {
            PeopleCmd::Ls => {
                let members = act(PEOPLE.into(), "list", json!({})).await?;
                let invitations = act("workspace/invitations".into(), "list", json!({})).await?;
                let listed = json!({"members": members, "invitations": invitations});
                let said = people_text(&listed);
                (listed, said)
            }
            PeopleCmd::Invite {
                role,
                email,
                waiting,
            } => {
                let mut args = json!({"role": role});
                if let Some(fields) = args.as_object_mut() {
                    if let Some(email) = &email {
                        fields.insert("invitee_name".into(), Value::String(email.clone()));
                    }
                    if let Some(waiting) = waiting {
                        fields.insert("wait_list_entry".into(), Value::String(waiting));
                    }
                }
                let invited = act("workspace/invitations".into(), "invite", args).await?;
                let mut said = text(&invited, "link").to_owned();
                if let Some(next) = invited.get("next").and_then(Value::as_str) {
                    let _ = write!(said, "\n{next}");
                } else if let (Some(email), None) = (email, invited.get("sent")) {
                    let _ = write!(said, "\nTilo sends no email: give {email} this link");
                }
                (invited, said)
            }
            PeopleCmd::Add { key, role, name } => {
                let mut args = json!({"person": key, "role": role});
                if let (Some(fields), Some(name)) = (args.as_object_mut(), name) {
                    fields.insert("name".into(), Value::String(name));
                }
                let listed = json!({"members": act(PEOPLE.into(), "add", args).await?});
                let said = people_text(&listed);
                (listed, said)
            }
            PeopleCmd::Role { key, role } => {
                let moved = act(format!("{PEOPLE}/{key}"), "role", json!({ "role": role }));
                let listed = json!({"members": moved.await?});
                let said = people_text(&listed);
                (listed, said)
            }
            PeopleCmd::Rm { key } => {
                let removed = act(format!("{PEOPLE}/{key}"), "remove", json!({}));
                let listed = json!({"members": removed.await?});
                let said = people_text(&listed);
                (listed, said)
            }
            PeopleCmd::Revoke { invite } => {
                let args = json!({ "invite": invite });
                let answer = act("workspace/invitations".into(), "cancel", args).await?;
                (answer, format!("revoked {invite}: its link lets nobody in"))
            }
            PeopleCmd::Name { name } => {
                let answer = act(PEOPLE.into(), "name", json!({ "name": name })).await?;
                (answer, format!("the workspace calls you {name}"))
            }
            PeopleCmd::WaitList => {
                let listed = act("workspace/waiting".into(), "list", json!({})).await?;
                let said = waiting_text(&listed);
                (listed, said)
            }
            PeopleCmd::Unlist { entry } => {
                let args = json!({ "entry": entry });
                let listed = act("workspace/waiting".into(), "unlist", args).await?;
                let said = waiting_text(&listed);
                (listed, said)
            }
        };
        Ok(show(reach.json, &answer, &said))
    })
}

/// A workspace's people and its open invitations as a person reads them: one line each, a
/// person by the one place they hold and their key, an invitation by what redeeming it makes
/// and its id.
fn people_text(listed: &Value) -> String {
    let rows = |name: &str| {
        listed
            .get(name)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let place = |row: &Value| {
        Some(text(row, "place"))
            .filter(|place| !place.is_empty())
            .unwrap_or_else(|| text(row, "role"))
            .to_owned()
    };
    let members = rows("members");
    let mut said = String::from("members:");
    for member in &members {
        let _ = write!(said, "\n  {} — {}", text(member, "name"), place(member));
        if let Some(key) = member.pointer("/member/person").and_then(Value::as_str) {
            let _ = write!(said, "  {key}");
        }
    }
    if members.is_empty() {
        said.push_str("\n  nobody yet");
    }
    let invitations = rows("invitations");
    if invitations.is_empty() {
        said.push_str("\ninvitations: none open");
        return said;
    }
    said.push_str("\ninvitations:");
    for invitation in &invitations {
        let _ = write!(said, "\n  {} invitation", place(invitation));
        let named = text(invitation, "invitee_name");
        if !named.is_empty() {
            let _ = write!(said, " for {named}");
        }
        let _ = write!(said, ", {}", text(invitation, "invite"));
    }
    said
}

/// A workspace's wait list as a person reads it: each address with its entry, and whether it
/// was invited.
fn waiting_text(listed: &Value) -> String {
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

/// `tilo workspace servers … -w <workspace>`: the servers keeping the
/// workspace, and choosing them, as the workspace's own `workspace/servers`
/// actions.
fn servers(reach: Reach<'_>, workspace: &str, cmd: ServersCmd) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let (action, args) = match cmd {
            ServersCmd::Ls => ("list", json!({})),
            ServersCmd::Content => ("content", json!({})),
            ServersCmd::Add { host } => ("add", json!({ "host": host })),
            ServersCmd::Remove { server } => ("remove", json!({ "server": server })),
            ServersCmd::Move { from, to } => ("move", json!({ "server": from, "host": to })),
        };
        let answer = client
            .act(
                &workspace,
                "workspace/servers",
                action,
                &args,
                &format!("workspace/servers#{action}"),
            )
            .await?;
        let listed = answer.get("result").cloned().unwrap_or_default();
        let said = if action == "content" {
            content_text(&listed)
        } else {
            servers_text(&listed)
        };
        Ok(show(reach.json, &answer, &said))
    })
}

/// How much of a workspace's content its server keeps, as a person reads it.
fn content_text(held: &Value) -> String {
    match held.get("lacking").and_then(Value::as_u64) {
        Some(0) => "All of the workspace's files and attachments are on its server.".to_owned(),
        Some(1) => "1 piece of the workspace's content is still only on its members' devices: \
                    keep one of them on until the server has fetched it."
            .to_owned(),
        Some(lacking) => format!(
            "{lacking} pieces of the workspace's content are still only on its members' \
             devices: keep one of them on until the server has fetched them."
        ),
        None => "The server did not say how much of the workspace's content it keeps.".to_owned(),
    }
}

/// The servers keeping a workspace as a person reads them: each by its
/// device key, the one asked marked.
fn servers_text(listed: &Value) -> String {
    let mut said = String::from("kept by:");
    for server in listed
        .get("servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
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

/// The id of the workspace a new key is to reach, named as every command names one: by its name
/// or its id. The API server's key page takes the id alone.
async fn key_workspace(reach: Reach<'_>, workspace: Option<&str>) -> Result<Option<String>> {
    match workspace {
        None => Ok(None),
        Some(id) if api::is_uuid(id) => Ok(Some(id.to_owned())),
        Some(named) => Ok(Some(open(reach).await?.workspace_id(named).await?)),
    }
}

/// `tilo keys …`.
pub fn keys(reach: Reach<'_>, workspace: Option<&str>, cmd: KeysCmd) -> ExitCode {
    run(async {
        let new_key = matches!(cmd, KeysCmd::Create { .. } | KeysCmd::Export { .. });
        let key_reaches = if new_key {
            key_workspace(reach, workspace).await?
        } else {
            None
        };
        let workspace_id = key_reaches.as_deref();
        match cmd {
            KeysCmd::Create {
                name,
                expires_in,
                no_browser_pages,
            } => {
                let asked = KeyAsk {
                    name: &name,
                    workspace: workspace_id,
                    expires_in,
                    browser_pages: !no_browser_pages,
                };
                let answer = ask_key(&asked).await?;
                if reach.json {
                    println!("{answer}");
                } else {
                    eprintln!("Approved. Your key (shown once):");
                    println!("export {SDK_KEY_ENV}={}", text(&answer, "key"));
                }
                Ok(ExitCode::SUCCESS)
            }
            KeysCmd::Export {
                name,
                out,
                expires_in,
            } => {
                let asked = KeyAsk {
                    name: &name,
                    workspace: workspace_id,
                    expires_in,
                    browser_pages: true,
                };
                export(&asked, &out, reach.json).await
            }
            KeysCmd::Ls => {
                let listed = open(reach)
                    .await?
                    .account("account/keys", "list", &serde_json::json!({}))
                    .await?;
                Ok(show(reach.json, &listed, &lines::keys(&listed)))
            }
            KeysCmd::Mint { name, expires_in } => {
                let Some(named) = workspace else {
                    bail!("name the workspace: -w <name or id>");
                };
                let client = open(reach).await?;
                let id = client.workspace_id(named).await?;
                let mut body = serde_json::json!({"workspace": id});
                if let (Some(fields), Some(name)) = (body.as_object_mut(), name) {
                    fields.insert("name".into(), Value::String(name));
                }
                if let (Some(fields), Some(expires_in)) = (body.as_object_mut(), expires_in) {
                    fields.insert("expires_in".into(), serde_json::json!(expires_in));
                }
                let minted = client.account("account/keys", "mint", &body).await?;
                let said = lines::minted_key(&minted);
                Ok(show(reach.json, &minted, &said))
            }
            KeysCmd::Revoke { grant } => revoke_key(reach, &grant).await,
        }
    })
}

/// `tilo keys revoke KEY`: the key `named` (its grant, or its name as `tilo keys ls` lists it)
/// revoked, with every key it minted.
async fn revoke_key(reach: Reach<'_>, named: &str) -> Result<ExitCode> {
    let client = open(reach).await?;
    let listed = client
        .account("account/keys", "list", &serde_json::json!({}))
        .await?;
    let grant = key_api::key_named(&listed, named)?;
    let revoked = client
        .account(
            "account/keys",
            "revoke",
            &serde_json::json!({ "grant": grant }),
        )
        .await?;
    Ok(show(
        reach.json,
        &revoked,
        &format!("revoked {}", text(&revoked, "revoked")),
    ))
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
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(file)?;
    match tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600) {
        Ok(()) => Ok(()),
        // The server already approved this key; once the renamed file is in
        // place, a refused directory sync must not fail the command and prompt
        // the person to ask for a second key.
        #[cfg(any(unix, windows))]
        Err(error)
            if tilo_files::native::atomic::publication_state(&error)
                == Some(
                    tilo_files::native::atomic::PublicationState::PublishedDurabilityUnknown,
                ) =>
        {
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("writing {}", path.display())),
    }
}

/// A key asked of the person: what `tilo keys create` and `tilo keys export` ask for.
struct KeyAsk<'a> {
    /// What will use it, as the grants page names it.
    name: &'a str,
    /// The id of the one workspace it reaches, when not all of them.
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
        name,
        workspace,
        expires_in,
        browser_pages,
    } = *asked;
    let api = key_api::key_api().await?;
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
        eprintln!("Approved here, as {}.", person.who());
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

#[cfg(test)]
mod tests {

    use super::*;
    use std::sync::Arc;

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
            device_label: "Claude Code on build-box".into(),
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
            agent: "Claude Code".into(),
            cert: cert.clone(),
            server_cert: cert.clone(),
            server_address: None,
        };
        let mut env = tilo_test_env::ScopedEnv::new();
        crate::login::keep_for_test(&mut env, dir.path(), &key, &device);

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

        // A key file is someone else's key: it names no workspace of its own.
        assert_eq!(
            api::entered_workspace(Reach {
                key_file: Some(Path::new("visa.json")),
                json: false,
            })
            .unwrap(),
            None
        );

        exports_a_key_of_its_own(dir.path(), &root, &device, &mut env);
    }

    /// `tilo keys export` asks for a new key, as `tilo keys create` does, at the API server the
    /// login's site names, and writes that one, never the device's own (as the login of `device`
    /// under `dir` keeps it): the script it is for and this computer each have a key
    /// alone.
    fn exports_a_key_of_its_own(
        dir: &Path,
        root: &ed25519_dalek::SigningKey,
        device: &crate::login::Device,
        env: &mut tilo_test_env::ScopedEnv,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        let cert = &device.cert;
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
        let site = Arc::new(std::sync::OnceLock::<String>::new());
        let api_server = test_http::Responder::handle({
            let (asked, site) = (Arc::clone(&asked), Arc::clone(&site));
            move |request| {
                if request.target == "/entry.json" {
                    let named = serde_json::json!({"api": site.get()});
                    return Some(
                        test_http::Reply::status(200)
                            .header("content-type", "application/json")
                            .body(named.to_string()),
                    );
                }
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
        env.set("XDG_CONFIG_HOME", nobody.path());
        let out = dir.join("visa.json");
        let url = api_server.url();
        site.set(url.clone()).unwrap();
        // The login's site names the API server keys are asked of.
        let at_site = crate::login::Device {
            site: url,
            ..device.clone()
        };
        let login = crate::login::Place::here(crate::identity::paths()).unwrap();
        crate::login::store(&login, &at_site).unwrap();
        let file = &api::login_key(None).unwrap().expect("still logged in");
        let exported = crate::hosted::keys(
            Reach::default(),
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
        // It acts as itself; a key file naming the device's own key acts as this computer.
        let as_file = |path: &Path| {
            Reach {
                key_file: Some(path),
                json: false,
            }
            .as_device()
            .unwrap()
        };
        assert!(!as_file(&out));
        let own = dir.join("own.json");
        std::fs::write(&own, serde_json::to_vec(file).unwrap()).unwrap();
        assert!(as_file(&own));
    }

    tilo_test_env::scenario! {
        fn hosted_answers_read_as_a_person_wrote_them;

        phase a_listing_and_a_read_print_their_typed_rows;
        {
            // `tilo ls` and `tilo read` print the typed answer as lines: the path and its type,
            // a row each, and a note's markdown under its title. `--json` keeps the typed answer.
            let listed = serde_json::json!({"path": "notes", "kind": "notes.notebook",
                "rows": [{"name": "Notebook", "kind": "notes.notebook", "has_children": true}]});
            assert_eq!(
                lines::listing(&listed),
                "notes  (notes.notebook)\nNotebook/  notes.notebook"
            );
            let read = serde_json::json!({"path": "notes/Notebook/Todo",
                "fields": {"title": "Todo", "markdown": "- milk"}});
            assert_eq!(lines::resource(&read), "# Todo\n- milk");
        }
    }

    #[cfg(target_os = "linux")]
    fn sample_key(name: &str, fill: &str) -> tilo_api::direct::KeyFile {
        tilo_api::direct::KeyFile {
            host: "https://eu1.tilo.test".into(),
            site: Some("https://tilo.sh".into()),
            name: name.into(),
            key: fill.repeat(32),
            cert: "aa".repeat(32),
            reached_at: None,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn key_file_is_private_under_zero_umask_and_keeps_old_key_on_failed_replace() {
        tilo_test_env::in_own_process(
            module_path!(),
            "key_file_is_private_under_zero_umask_and_keeps_old_key_on_failed_replace",
            || {
                use std::os::unix::fs::PermissionsExt as _;
                use std::sync::atomic::{AtomicBool, Ordering};
                use tilo_files::native::atomic::test_support::{
                    Checkpoint, with_failure, with_observer,
                };
                use tilo_test_env::file_mode::{on_a_thread_without_chmod, with_zero_umask};

                let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
                with_zero_umask(|| {
                    let dir = tempfile::tempdir().unwrap();
                    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                    let path = dir.path().join("visa.json");
                    let first = sample_key("first", "11");
                    let second = sample_key("second", "22");

                    // With umask 0 and chmod/fchmod neutralized, neither the writer nor the
                    // atomic backend's post-open fchmod can narrow the inode: the stage can
                    // only be 0600 if openat created it with 0600.
                    let observed = Arc::new(std::sync::Mutex::new(None));
                    let seen = Arc::clone(&observed);
                    let existed_at_stage = Arc::new(AtomicBool::new(false));
                    let flag = Arc::clone(&existed_at_stage);
                    let target = path.clone();
                    on_a_thread_without_chmod(|| {
                        with_observer(
                            move |event| {
                                if event.checkpoint == Checkpoint::StageCreated {
                                    let m =
                                        event.stage.as_ref().unwrap().metadata.permissions().mode()
                                            & 0o777;
                                    *seen.lock().unwrap() = Some(m);
                                    flag.store(target.exists(), Ordering::SeqCst);
                                }
                            },
                            || write_key_file(&path, &first).unwrap(),
                        );
                    });
                    assert_eq!(*observed.lock().unwrap(), Some(0o600));
                    assert!(!existed_at_stage.load(Ordering::SeqCst));
                    assert_eq!(mode(&path), 0o600);
                    assert_eq!(mode(dir.path()), 0o755);
                    let before = std::fs::read(&path).unwrap();

                    let failed =
                        with_failure(Checkpoint::BeforePublish, || write_key_file(&path, &second));
                    assert!(failed.is_err());
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

                    // Once the replacement is renamed into place, a refused directory sync
                    // must not fail the export command.
                    with_failure(Checkpoint::BeforeCommit, || write_key_file(&path, &second))
                        .unwrap();
                    assert_eq!(
                        std::fs::read(&path).unwrap(),
                        serde_json::to_vec_pretty(&second).unwrap()
                    );
                    assert_eq!(mode(&path), 0o600);
                    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

                    let nested = dir.path().join("nested/visa.json");
                    on_a_thread_without_chmod(|| write_key_file(&nested, &second).unwrap());
                    assert_eq!(mode(&nested), 0o600);
                    assert_eq!(mode(dir.path()), 0o755);
                });
            },
        );
    }
}
