//! Hosted workspaces and the account from a shell: `tilo do`, `tilo ls`, `tilo read`,
//! `tilo files` and `tilo run` with `-w`, and on `account/…` paths with none.
//!
//! Each command is a call of Tilo's actions ([`crate::api::Calls`]), as the
//! agent surface's tools make it: a workspace, its people and servers, the account's workspaces
//! and keys, programs and files are actions there. The caller is a key file `--key-file` (or
//! `TILO_KEY_FILE`) names, or, when this computer logged in (`tilo login`), its
//! own device: each command a call to this computer's daemon, which keeps that
//! device's one session, since a second session would cut it off.

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::api::{Calls, Reach};
use tilo_api::v1::lines;
use tilo_server_client::https::{HttpClient as _, Origin};

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

/// The id of the workspace `named` names, by its name or id; none for the account.
async fn workspace_of(client: &Calls, named: Option<&str>) -> Result<Option<String>> {
    Ok(match named {
        Some(named) => Some(client.workspace_id(named).await?),
        None => None,
    })
}

/// The id of the workspace `named` names, by its name or id, as `reach` knows it.
///
/// # Errors
/// It names no workspace the account reaches, or none could be asked.
pub fn workspace_id(reach: Reach<'_>, named: &str) -> Result<String> {
    runtime()?.block_on(async { open(reach).await?.workspace_id(named).await })
}

/// Whether `path` is one of the account's (`account/keys#create`): acted on or read with no
/// workspace.
pub fn on_account(path: &str) -> bool {
    path.split(['/', '#']).next() == Some("account")
}

/// `tilo do <path> name=value… [-w <workspace>] [--out FILE]`: one action of a hosted
/// workspace, or of the account with none, as `app/resource#action`, said as `tilo do` says an
/// action this desktop ran ([`tilo_control::front::say`]). An argument the action takes as a
/// workspace is named by the workspace's name or id. With `--out`, what the action answers as
/// a secret is written to FILE, readable by this account only, and never printed.
pub fn act(reach: Reach<'_>, workspace: Option<&str>, path: &str, args: &[String]) -> ExitCode {
    crate::log_to_stderr(tracing_subscriber::filter::LevelFilter::OFF);
    let mut args = args.to_vec();
    let out = args.iter().position(|arg| arg == "--out").map(|at| {
        let file = args.get(at.saturating_add(1)).cloned();
        args.drain(at..(at.saturating_add(2)).min(args.len()));
        file
    });
    let acting = async {
        let out = match out {
            Some(Some(file)) => Some(PathBuf::from(file)),
            Some(None) => bail!("--out names the file the answer's secret is written to"),
            None => None,
        };
        // A key asked for on a computer that is no device of the person's: it has no daemon to
        // ask them on their behalf, and no key of theirs to certify with. Their site asks them.
        if path == "account/keys#create"
            && reach.key_file.is_none()
            && !crate::login::logged_in()
            && !crate::identity::holds_root()
        {
            return site_key(&args, out.as_deref()).await;
        }
        let client = open(reach).await?;
        let Some((resource, action)) = path.rsplit_once('#') else {
            bail!("{path:?} names no action: write it as app/resource#action, like notes#open");
        };
        let workspace = workspace_of(&client, workspace).await?;
        let mut arguments = tilo_control::front::arguments(&args).map_err(anyhow::Error::msg)?;
        let described = client
            .list(workspace.as_deref(), Some(path), None)
            .await
            .ok()
            .and_then(|listed| listed.pointer("/actions/0").cloned())
            .unwrap_or_default();
        for name in workspace_params(&described) {
            if let Some(Value::String(named)) = arguments.get(&name) {
                let id = client.workspace_id(named).await?;
                arguments.insert(name, Value::String(id));
            }
        }
        let arguments = Value::Object(arguments);
        let started = client
            .start(workspace.as_deref(), resource, action, &arguments, path)
            .await?;
        // This command is a program of the person's, never their own act: what it asks waits for
        // their answer on their session or another device of theirs.
        let mut answer = client
            .follow_asked(workspace.as_deref(), started, async |record: &str| {
                eprintln!(
                    "tilo: waiting for your answer on a device of yours: it waits in {record}"
                );
                Ok(())
            })
            .await?;
        if let Some(out) = out {
            write_out(&out, &secret_fields(&described), &mut answer)?;
        }
        Ok(answer)
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

/// Write the answer's secret (one of `secrets`) to `out`, readable by this account only, and say
/// in its place that it was written.
fn write_out(out: &Path, secrets: &[String], answer: &mut Value) -> Result<()> {
    let Some(result) = answer.get_mut("result").and_then(Value::as_object_mut) else {
        bail!("the answer named nothing to write to {}", out.display());
    };
    let Some((name, Value::String(secret))) = secrets
        .iter()
        .find_map(|name| Some((name.clone(), result.remove(name)?)))
    else {
        bail!("the answer named no secret to write to {}", out.display());
    };
    write_secret(out, secret.as_bytes())?;
    result.insert(name, json!(format!("written to {}", out.display())));
    Ok(())
}

/// `account/keys#create` from a computer that is no device of the person's: it has no daemon to
/// ask them and no root of theirs to certify the key, so their site asks them, and this terminal
/// waits there for the key they approve. It reads no device key.
async fn site_key(args: &[String], out: Option<&Path>) -> Result<Value> {
    let http = tilo_net::Client::new();
    let site = std::env::var("TILO_SITE").unwrap_or_else(|_| "https://tilo.sh".to_owned());
    let (_, entry) = Origin::new(&http, &site)
        .get("/entry.json")
        .await
        .map_err(anyhow::Error::msg)?;
    let api = serde_json::from_slice::<Value>(&entry)?
        .get("api")
        .and_then(Value::as_str)
        .context(format!("{site} names no API server keys are asked for at"))?
        .to_owned();
    let api = Origin::new(&http, &api);
    let new = Value::Object(tilo_control::front::arguments(args).map_err(anyhow::Error::msg)?);
    let (status, asked) = json_at(&api, tilo_api::keys::NEW_PATH, new).await?;
    if status != 201 {
        bail!("the site refused the key: {}", refusal(&asked));
    }
    let field = |name: &str| asked.get(name).and_then(Value::as_str).unwrap_or_default();
    eprintln!(
        "tilo: waiting for you: approve it at {}",
        field("approve_url")
    );
    let poll = json!({"poll": field("poll")});
    let interval = asked.get("interval").and_then(Value::as_u64).unwrap_or(2);
    let pause = Duration::from_secs(interval.max(1));
    loop {
        tokio::time::sleep(pause).await;
        let (status, answer) = json_at(&api, tilo_api::keys::POLL_PATH, poll.clone()).await?;
        match status {
            202 => {}
            200 => {
                let mut answer = json!({"result": answer});
                if let Some(out) = out {
                    write_out(out, &["key".to_owned()], &mut answer)?;
                }
                return Ok(answer);
            }
            _ => bail!("the site made no key: {}", refusal(&answer)),
        }
    }
}

/// `api`'s `path`, asked `body`: its status, and its answer as JSON.
async fn json_at(api: &Origin, path: &str, body: Value) -> Result<(u16, Value)> {
    let answer = api
        .post_answer(path, body.to_string().into_bytes())
        .await
        .map_err(anyhow::Error::msg)?;
    Ok((answer.status, serde_json::from_slice(&answer.body)?))
}

/// Why the site refused, in its words, or nothing.
fn refusal(answer: &Value) -> &str {
    answer
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The parameters an action, as `tilo_ls path#action` describes it, takes as a workspace: those
/// typed as a workspace reference, else the one named `workspace` when none is typed.
fn workspace_params(described: &Value) -> Vec<String> {
    let params = described
        .get("params")
        .and_then(Value::as_array)
        .map_or(&[] as &[Value], Vec::as_slice);
    let typed: Vec<String> = params
        .iter()
        .filter(|param| {
            param
                .pointer("/schema/resource_kind")
                .and_then(Value::as_str)
                == Some("tilo.workspace")
        })
        .filter_map(|param| Some(param.get("name")?.as_str()?.to_owned()))
        .collect();
    if typed.is_empty() {
        return vec!["workspace".to_owned()];
    }
    typed
}

/// The fields of an action's result its declared result schema types as a secret
/// (`"format": "password"`).
fn secret_fields(described: &Value) -> Vec<String> {
    described
        .pointer("/result/properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, schema)| schema.get("format").and_then(Value::as_str) == Some("password"))
        .map(|(name, _)| name.clone())
        .collect()
}

/// `tilo ls [path] -w <workspace>`: what the workspace holds, or what `path`
/// holds (the account's with no workspace), as `tilo_ls` lists it: the host's own lines, so a
/// person and an agent read the same text, and `cursor` reads on from where a page ended.
pub fn ls(
    reach: Reach<'_>,
    workspace: Option<&str>,
    path: Option<&str>,
    cursor: Option<&str>,
) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = workspace_of(&client, workspace).await?;
        let listed = client.list(workspace.as_deref(), path, cursor).await?;
        Ok(show(reach.json, &listed, &lines::listing(&listed)))
    })
}

/// `tilo read <path> -w <workspace>`: one resource (the account's with no workspace), as
/// `tilo_read` reads it.
pub fn read(
    reach: Reach<'_>,
    workspace: Option<&str>,
    path: &str,
    cursor: Option<&str>,
) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = workspace_of(&client, workspace).await?;
        let read = client
            .read(workspace.as_deref(), path.trim(), cursor)
            .await?;
        Ok(show(reach.json, &read, &lines::resource(&read)))
    })
}

/// `tilo invocations -w <workspace>`: the workspace's invocations in records the key may read,
/// newest first, read through the record API: `limit` to a page, and the `cursor` a page ended
/// with for the older ones.
pub fn invocations(
    reach: Reach<'_>,
    workspace: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> ExitCode {
    run(async {
        let client = open(reach).await?;
        let workspace = client.workspace_id(workspace).await?;
        let read = client
            .read(Some(&workspace), "record:tilo.invocations:", None)
            .await?;
        let fields = read.get("fields").cloned().unwrap_or(Value::Null);
        // The record is read whole: the page is cut from it here, by where the cursor left off.
        let offset = cursor
            .map(str::trim)
            .filter(|cursor| !cursor.is_empty())
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or(0);
        let page = usize::try_from(limit.unwrap_or(50))
            .unwrap_or(50)
            .clamp(1, 200);
        let items = fields
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let shown: Vec<Value> = items.iter().skip(offset).take(page).cloned().collect();
        let end = offset.saturating_add(shown.len());
        let mut listed = fields;
        if let Some(fields) = listed.as_object_mut() {
            fields.insert("items".to_owned(), Value::Array(shown));
            match (end < items.len()).then(|| end.to_string()) {
                Some(next) => {
                    fields.insert("next_cursor".to_owned(), Value::String(next));
                }
                None => {
                    fields.remove("next_cursor");
                }
            }
        }
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
        // What a model call it made cost, where one did.
        if let Some(spend) = listed.get("spend") {
            let tokens = |name: &str| spend.get(name).and_then(Value::as_u64).unwrap_or_default();
            let _ = write!(
                said,
                "; {} in + {} out tokens",
                tokens("input_tokens"),
                tokens("output_tokens")
            );
            if let Some(model) = spend.get("model").and_then(Value::as_str) {
                let _ = write!(said, " on {model}");
            }
            let micros = tokens("cost_micros");
            if micros > 0 {
                let _ = write!(
                    said,
                    ", costing {}.{:06}",
                    micros / 1_000_000,
                    micros % 1_000_000
                );
            }
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
        let listing = client
            .list(Some(&workspace), None, cursor.as_deref())
            .await?;
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
            let read = match client.read(Some(&workspace), path, None).await {
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
        .act(Some(workspace), "files", "open", &arguments, &title)
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
            .invocation(Some(workspace), invocation, TREE_SAVE_WAIT_SECONDS)
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

/// Write `bytes` (a secret an action answered) at `path`, readable by this account only, in
/// place of what was there.
fn write_secret(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    match tilo_files::native::atomic::write_with_mode(path, bytes, 0o600) {
        Ok(()) => Ok(()),
        // The action already answered; once the renamed file is in place, a refused directory
        // sync must not fail the command and prompt the person to ask a second time.
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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::api;
    use std::sync::Arc;

    /// A key `tilo do account/keys#create --out FILE` wrote (the key alone, as the SDKs take it)
    /// reaches the server this computer's login reaches, through a key file of its own: it is
    /// never the device's key, which stays with the daemon.
    #[test]
    fn a_script_key_reaches_the_server_this_computers_login_reaches() {
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
        let _taken = crate::login::keep_for_test(&mut env, dir.path(), &key, &device);

        // The login's device is where its keys reach: a script's key file is built on it.
        let home = crate::login::home().expect("a logged-in computer");
        assert_eq!(home.host_origin, "https://eu1.tilo.test");
        assert_eq!(
            home.site, "https://tilo.sh/",
            "the device keeps the site it logged in with: links are built there"
        );

        // A key file is someone else's key: it names no workspace of its own.
        assert_eq!(
            api::entered_workspace(Reach {
                key_file: Some(Path::new("visa.json")),
                json: false,
            })
            .unwrap(),
            None
        );

        exports_a_key_of_its_own(dir.path(), &root, &device, &env);
    }

    /// A key `tilo do account/keys#create --out FILE` wrote (the key alone, as the SDKs take it)
    /// is a key file of its own: it reaches the server the login of `device` under `dir`
    /// reaches, and is never the device's own key.
    fn exports_a_key_of_its_own(
        dir: &Path,
        root: &ed25519_dalek::SigningKey,
        device: &crate::login::Device,
        env: &tilo_test_env::ScopedEnv,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = env;
        let now = tilo_clock::unix_now_ns();
        let script = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let mut script_cert = device.cert.clone();
        script_cert.device_public_key = script.verifying_key().to_bytes();
        script_cert.device_label = "Visa backend".into();
        tilo_user_identity::sign_device_cert(&mut script_cert, root).unwrap();
        let token = tilo_api::store::key_token(
            "Visa backend",
            &tilo_api::grant_key::GrantKey::held(script.clone(), script_cert, now).unwrap(),
        )
        .unwrap();
        let out = dir.join("visa.key");
        write_secret(&out, token.as_bytes()).unwrap();
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600,
            "only this account may read a key"
        );
        let written = tilo_api::direct::KeyFile::read(&out).unwrap();
        assert_eq!(written.key, hex::encode(script.to_bytes()));
        assert_eq!(written.name, "Visa backend");
        assert_eq!(
            written.host, device.host_origin,
            "it reaches the login's server"
        );
        assert_eq!(
            written.site.as_deref(),
            Some(device.site.trim_end_matches('/')),
            "it keeps the login's site"
        );
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
        // The device's own key file, as a person's own device names it: a program that writes
        // one is that computer.
        let own = tilo_api::direct::KeyFile::new(
            &device.host_origin,
            Some(&device.site),
            &device.cert.device_label,
            &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            &device.cert,
        );
        let own_path = dir.join("own.json");
        std::fs::write(&own_path, serde_json::to_vec(&own).unwrap()).unwrap();
        tilo_api::direct::Direct::new(&own).unwrap();
        assert!(as_file(&own_path));
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
                            || {
                                write_secret(&path, &serde_json::to_vec_pretty(&first).unwrap())
                                    .unwrap();
                            },
                        );
                    });
                    assert_eq!(*observed.lock().unwrap(), Some(0o600));
                    assert!(!existed_at_stage.load(Ordering::SeqCst));
                    assert_eq!(mode(&path), 0o600);
                    assert_eq!(mode(dir.path()), 0o755);
                    let before = std::fs::read(&path).unwrap();

                    let failed = with_failure(Checkpoint::BeforePublish, || {
                        write_secret(&path, &serde_json::to_vec_pretty(&second).unwrap())
                    });
                    assert!(failed.is_err());
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

                    // Once the replacement is renamed into place, a refused directory sync
                    // must not fail the command.
                    with_failure(Checkpoint::BeforeCommit, || {
                        write_secret(&path, &serde_json::to_vec_pretty(&second).unwrap())
                    })
                    .unwrap();
                    assert_eq!(
                        std::fs::read(&path).unwrap(),
                        serde_json::to_vec_pretty(&second).unwrap()
                    );
                    assert_eq!(mode(&path), 0o600);
                    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

                    let nested = dir.path().join("nested/visa.json");
                    on_a_thread_without_chmod(|| {
                        write_secret(&nested, &serde_json::to_vec_pretty(&second).unwrap())
                            .unwrap();
                    });
                    assert_eq!(mode(&nested), 0o600);
                    assert_eq!(mode(dir.path()), 0o755);
                });
            },
        );
    }
}
