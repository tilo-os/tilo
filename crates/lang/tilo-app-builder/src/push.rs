//! `tilolang push`: run a build of an app in a workspace that is running
//! now, for everyone in it.
//!
//! The workspace records which version of the app it runs (its `install`
//! governance action); every member is to run it, and a change
//! made with another version waits until its author has.
//!
//! That is the workspace's governance in its action API, so this command
//! calls it as the developer through the installed `tilo` command
//! ([`crate::tilo`], `tilo mcp --as-me`), as a key of theirs (a
//! `--key-file`, or the key `tilo login` kept here), invoking the actions on
//! the workspace's `workspace/apps` path exactly as `tilo do` does. The
//! workspace decides whether the key's person may.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};

use crate::tilo::{self, Refused, Tilo};

/// The path of a workspace's apps in its action API.
const APPS: &str = "workspace/apps";
/// The bytes of a package file one `upload` piece carries: the most
/// the workspace's server takes in one call.
const UPLOAD_PIECE: usize = 256 * 1024;
/// How long one call waits for its action before answering that it is
/// still running.
const WAIT_MS: u64 = 60_000;
/// How long a push follows an action that waits for someone's approval.
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// The pause between two looks at an action still running.
const PAUSE: Duration = Duration::from_secs(1);

/// What a person reads when nothing on this computer signs them in.
pub const NOT_SIGNED_IN: &str = "You're not signed in to Tilo on this computer. Sign in with \
     `tilo login`, or name a key of yours with --key-file, then push again.";

#[derive(clap::Args)]
pub struct PushArgs {
    /// App directory, or a file or directory inside it. Defaults to the
    /// current directory.
    path: Option<PathBuf>,
    /// The workspace to push to, by its id or the name it is listed under.
    #[arg(long = "workspace")]
    workspace: String,
    /// A key this computer has (`tilo do account/keys#create --out FILE`), instead of the key
    /// `tilo login` kept here.
    #[arg(long)]
    key_file: Option<PathBuf>,
    /// The `.tapp` to run; defaults to the newest in the app's `dist/`
    /// (`tilolang package`).
    #[arg(long)]
    package: Option<PathBuf>,
    /// A package reviewed and listed by a registry, rather than your own
    /// build. Every window of a dev build says whose it is.
    #[arg(long)]
    published: bool,
}

/// What `tilolang push` sends the workspace: the governance action and its
/// arguments.
#[derive(Debug, PartialEq)]
pub struct Push {
    pub action: &'static str,
    pub argument: Map<String, Value>,
    /// The package file the workspace's host keeps for its members to fetch,
    /// handed over before the action names it as `file`.
    pub upload: Option<PathBuf>,
}

pub fn run_push(args: PushArgs) -> Result<Value> {
    let app_dir = crate_app_dir(args.path.as_deref())?;
    let package = match args.package {
        Some(package) => package,
        None => newest_package(&app_dir.join("dist"))?,
    };
    let push = app_push(&app_dir, &package, !args.published)?;
    let started = Instant::now();
    let (mut tilo, workspace) = open(args.key_file.as_deref(), &args.workspace)?;
    let refused = |error: String| {
        anyhow::anyhow!("the workspace {} refused the push: {error}", args.workspace)
    };
    let mut argument = Value::Object(push.argument.clone());
    if let Some(path) = &push.upload {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let file = upload_app_file(&mut tilo, &bytes).map_err(refused)?;
        if let Some(fields) = argument.as_object_mut() {
            fields.insert("file".into(), Value::from(file));
        }
    }
    let title = format!("push {}", describe(&push));
    let answer = act(&mut tilo, push.action, &argument, &title).map_err(refused)?;
    eprintln!(
        "[tilolang] {} is in {workspace} ({} ms)",
        describe(&push),
        started.elapsed().as_millis()
    );
    Ok(json!({ "workspace": workspace, "action": push.action, "answer": answer }))
}

/// The installed `tilo`, acting as `key_file`'s key (else this computer's
/// login) in the workspace `named`, and that workspace's id.
fn open(key_file: Option<&Path>, named: &str) -> Result<(Tilo, String)> {
    let program = tilo::find().with_context(|| tilo::TILO_MISSING)?;
    let mut args: Vec<OsString> = Vec::new();
    if let Some(key_file) = key_file {
        args.extend(["--key-file".into(), key_file.into()]);
    }
    args.extend([
        "--workspace".into(),
        named.into(),
        "mcp".into(),
        "--as-me".into(),
    ]);
    let (tilo, served) = Tilo::start(&program, &args, &[]).map_err(|refused| match refused {
        Refused::NotSignedIn => anyhow::anyhow!(NOT_SIGNED_IN),
        Refused::Said(said) => anyhow::anyhow!("{said}"),
    })?;
    let Some((workspace, _)) = served else {
        bail!(
            "The `tilo` command at {} does not open the workspace {named}: update it with \
             `curl -fsSL https://tilo.sh | sh`.",
            program.display()
        );
    };
    Ok((tilo, workspace))
}

/// Run `action` on the workspace's apps with `argument`, kept in History
/// under `title`: its answer once it is done, followed while it waits for
/// someone's approval.
fn act(tilo: &mut Tilo, action: &str, argument: &Value, title: &str) -> Result<Value, String> {
    let invocation = json!({"id": uuid::Uuid::new_v4().simple().to_string(), "title": title});
    let call = json!({
        "path": format!("{APPS}#{action}"),
        "args": argument,
        "wait_ms": WAIT_MS,
        "invocation_id": uuid::Uuid::new_v4().to_string(),
    });
    let mut answer = tilo.call_in("tilo_do", call, Some(&invocation))?;
    let until = Instant::now().checked_add(ANSWER_DEADLINE);
    let mut told = false;
    while answer.get("status").and_then(Value::as_str) == Some("pending")
        && until.is_some_and(|until| Instant::now() < until)
    {
        if !told {
            told = true;
            if let Some(url) = answer.pointer("/ask/url").and_then(Value::as_str) {
                eprintln!("[tilolang] waiting for you: approve it at {url}");
            }
        }
        let Some(run) = answer
            .get("invocation_id")
            .or_else(|| answer.get("invocation"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            break;
        };
        std::thread::sleep(PAUSE);
        answer = tilo.call("tilo_status", json!({"invocation_id": run}))?;
    }
    // A failure is a refused call; what answers is done, cancelled or still waiting.
    match answer.get("status").and_then(Value::as_str) {
        Some("pending") => Err(answer
            .pointer("/ask/url")
            .and_then(Value::as_str)
            .map_or_else(
                || "it is still waiting for approval".to_owned(),
                |url| format!("it is still waiting for approval at {url}"),
            )),
        Some("cancelled") => Err("it was cancelled".to_owned()),
        _ => Ok(answer),
    }
}

/// Hand the package `bytes` to the workspace's server in the pieces it takes,
/// naming it by its BLAKE3 hash: what `install` names as the version's
/// `file`.
fn upload_app_file(tilo: &mut Tilo, bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("the package file is empty".to_owned());
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    let total = bytes.len() as u64;
    let mut offset = 0_u64;
    for piece in bytes.chunks(UPLOAD_PIECE) {
        let argument = json!({
            "file": hash,
            "offset": offset,
            "total": total,
            "data": hex::encode(piece),
        });
        act(
            tilo,
            "upload",
            &argument,
            "hand the workspace an app version's package",
        )?;
        offset = offset.saturating_add(piece.len() as u64);
    }
    Ok(hash)
}

fn describe(push: &Push) -> String {
    let text = |key: &str| {
        push.argument
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("?")
    };
    format!("{} {}", text("app"), text("version"))
}

fn crate_app_dir(path: Option<&Path>) -> Result<PathBuf> {
    let start = match path {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    };
    crate::resolve_app_dir(&start)
}

/// The newest `.tapp` in `dist`.
fn newest_package(dist: &Path) -> Result<PathBuf> {
    let mut packages: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dist)
        .with_context(|| format!("read {}; run `tilolang package` first", dist.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "tapp")
        })
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .collect();
    packages.sort();
    packages.pop().map(|(_, path)| path).with_context(|| {
        format!(
            "{} has no .tapp; run `tilolang package` first",
            dist.display()
        )
    })
}

/// The workspace's `install` for the package `package` built from the app in
/// `app_dir`: the surface its changes name (the BLAKE3 of the app's
/// `surface.toml` in the form it is kept by), the package's `tapp:` hash and
/// the key that signed it.
pub fn app_push(app_dir: &Path, package: &Path, dev: bool) -> Result<Push> {
    let file =
        std::fs::File::open(package).with_context(|| format!("open {}", package.display()))?;
    let tapp = tilo_app_package::tapp::read(
        std::io::BufReader::new(file),
        &mut tilo_app_package::blobs::DiscardBlobs,
    )
    .with_context(|| format!("read {}", package.display()))?;
    if tapp.meta.root.signature.is_none() {
        bail!(
            "{} is not signed; `tilolang package` signs it",
            package.display()
        );
    }
    let surface_file = app_dir.join("surface.toml");
    let surface = std::fs::read(&surface_file).with_context(|| {
        format!(
            "read {}: the workspace decides changes against it (`tilolang surface`)",
            surface_file.display()
        )
    })?;
    let (_, kept) = collab_types::surface::SurfaceFile::kept(&surface)
        .map_err(|error| anyhow::anyhow!("{}: {error}", surface_file.display()))?;
    let manifest = &tapp.meta.manifest.package;
    let argument = object([
        ("app", json!(manifest.id)),
        ("version", json!(manifest.version)),
        (
            "surface",
            json!(blake3::hash(kept.as_bytes()).to_hex().as_str()),
        ),
        ("package", json!(hex::encode(tapp.meta.id.0))),
        (
            "publisher",
            json!(hex::encode(tapp.meta.root.publisher_identity)),
        ),
        ("dev", json!(dev)),
    ]);
    Ok(Push {
        action: "install",
        argument,
        upload: Some(package.to_path_buf()),
    })
}

fn object<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}
