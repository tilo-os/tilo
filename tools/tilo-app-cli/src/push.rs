//! `tilo-app push`: run a build of an app in a workspace that is running
//! now, for everyone in it.
//!
//! The workspace records which version of the app it runs (its `install`
//! governance action); every member is to run it, and a change
//! made with another version waits until its author has.
//!
//! That is the workspace's governance in its action API, so this command
//! calls them as a key of the developer's (a `--key-file`, or the key
//! `tilo login` kept here) through [`tilo_api::calls`], invoking them on the
//! workspace's `workspace/apps` path exactly as `tilo do` does. The workspace
//! decides whether the key's person may.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};
use tilo_api::calls::{Calls, Reach};

/// The path of a workspace's apps in its action API.
const APPS: &str = "workspace/apps";
/// The bytes of a package file one `upload` piece carries: the most
/// the workspace's server takes in one call.
const UPLOAD_PIECE: usize = 256 * 1024;

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
    /// (`tilo-app package`).
    #[arg(long)]
    package: Option<PathBuf>,
    /// A package reviewed and listed by a registry, rather than your own
    /// build. Every window of a dev build says whose it is.
    #[arg(long)]
    published: bool,
}

/// What `tilo-app push` sends the workspace: the governance action and its
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let started = std::time::Instant::now();
    let answer = runtime.block_on(async {
        let calls = Box::pin(Calls::open(Reach {
            key_file: args.key_file.as_deref(),
            json: false,
        }))
        .await
        .context("reach the workspace's action API")?;
        let workspace = Box::pin(calls.workspace_id(&args.workspace)).await?;
        let mut argument = Value::Object(push.argument.clone());
        if let Some(path) = &push.upload {
            let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let file = Box::pin(upload_app_file(&calls, &workspace, &bytes)).await?;
            if let Some(fields) = argument.as_object_mut() {
                fields.insert("file".into(), Value::from(file));
            }
        }
        let title = format!("push {}", describe(&push));
        let answer = Box::pin(calls.act(&workspace, APPS, push.action, &argument, &title)).await?;
        Ok::<_, anyhow::Error>((workspace, answer))
    });
    let (workspace, answer) = answer.map_err(|error| {
        anyhow::anyhow!("the workspace {} refused the push: {error}", args.workspace)
    })?;
    eprintln!(
        "[tilo-app] {} is in {workspace} ({} ms)",
        describe(&push),
        started.elapsed().as_millis()
    );
    Ok(json!({ "workspace": workspace, "action": push.action, "answer": answer }))
}

/// Hand the package `bytes` to `workspace`'s server in the pieces it takes,
/// naming it by its BLAKE3 hash: what `install` names as the version's
/// `file`.
async fn upload_app_file(calls: &Calls, workspace: &str, bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() {
        bail!("the package file is empty");
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
        Box::pin(calls.act(
            workspace,
            APPS,
            "upload",
            &argument,
            "hand the workspace an app version's package",
        ))
        .await?;
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
        .with_context(|| format!("read {}; run `tilo-app package` first", dist.display()))?
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
            "{} has no .tapp; run `tilo-app package` first",
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
            "{} is not signed; `tilo-app package` signs it",
            package.display()
        );
    }
    let surface_file = app_dir.join("surface.toml");
    let surface = std::fs::read(&surface_file).with_context(|| {
        format!(
            "read {}: the workspace decides changes against it (`tilo-app surface`)",
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
