//! `tilo-app push`: run a build of an app, or a bot's program, in a room
//! that is running now, for everyone in it.
//!
//! The room records which version of the app it runs (its `install-app`
//! governance action); every member is to run it, and a change
//! made with another version waits until its author has. A bot's program
//! goes to the room's host as the room's request for that bot
//! (`request-bot`), which the host runs at once, keeping the bot's
//! installation. This machine acts as one of the developer's devices,
//! enrolled like any other (`--identity`), and the room decides whether
//! that person may.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};
use tilo_relay_session::rooms::govern::{self, RoomLink};

#[derive(clap::Args)]
pub struct PushArgs {
    /// App directory, or a file or directory inside it. Defaults to the
    /// current directory.
    path: Option<PathBuf>,
    /// The room: `tilo-room:<relay>[@<ip:port>]/<workspace>/<room>`.
    #[arg(long)]
    room: String,
    /// This machine's Tilo identity: a device of yours, enrolled for the
    /// person the room knows (`tilo-enroll certify`). Defaults to
    /// `$TILO_IDENTITY`.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// The `.tapp` to run; defaults to the newest in the app's `dist/`
    /// (`tilo-app package`).
    #[arg(long, conflicts_with = "bot")]
    package: Option<PathBuf>,
    /// A package reviewed and listed by a registry, rather than your own
    /// build. Every window of a dev build says whose it is.
    #[arg(long, conflicts_with = "bot")]
    published: bool,
    /// Push this bot program (JSON) instead of the app.
    #[arg(long, requires_all = ["entry", "version"])]
    bot: Option<PathBuf>,
    /// The bot's directory entry, such as `dev.ada.greeter`.
    #[arg(long)]
    entry: Option<String>,
    /// The version the bot's program is pushed as.
    #[arg(long)]
    version: Option<String>,
}

/// What `tilo-app push` sends the room: the governance action and its
/// arguments.
#[derive(Debug, PartialEq)]
pub struct Push {
    pub action: &'static str,
    pub argument: Map<String, Value>,
}

pub fn run_push(args: PushArgs) -> Result<Value> {
    let link: RoomLink = args.room.parse().map_err(anyhow::Error::msg)?;
    let push = if let Some(program) = &args.bot {
        bot_push(
            program,
            args.entry.as_deref().unwrap_or_default(),
            args.version.as_deref().unwrap_or_default(),
        )?
    } else {
        let app_dir = crate_app_dir(args.path.as_deref())?;
        let package = match args.package {
            Some(package) => package,
            None => newest_package(&app_dir.join("dist"))?,
        };
        app_push(&app_dir, &package, !args.published)?
    };
    let identity = args
        .identity
        .or_else(|| std::env::var_os("TILO_IDENTITY").map(PathBuf::from))
        .context("name this machine's Tilo identity: --identity <dir> or $TILO_IDENTITY")?;
    let paths = tilo_user_identity::IdentityPaths::under(&identity);
    let key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
        .with_context(|| format!("read the device key {}", paths.device_privkey.display()))?;
    let cert =
        tilo_user_identity::persist::read_device_cert(&paths.device_cert).with_context(|| {
            format!(
                "read the device certificate {}",
                paths.device_cert.display()
            )
        })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let started = std::time::Instant::now();
    let answer = runtime.block_on(async {
        let client = govern::connect(&link, cert, key).await?;
        govern::invoke(
            &client,
            link.workspace_id,
            &link.room,
            push.action,
            push.argument.clone(),
        )
        .await
    });
    let answer = answer.map_err(|error| anyhow::anyhow!("{link} refused the push: {error}"))?;
    eprintln!(
        "[tilo-app] {} runs in {link} ({} ms)",
        describe(&push),
        started.elapsed().as_millis()
    );
    Ok(json!({ "room": link.to_string(), "action": push.action, "answer": answer }))
}

fn describe(push: &Push) -> String {
    let text = |key: &str| {
        push.argument
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("?")
    };
    if push.action == "request-bot" {
        format!("bot {} {}", text("entry"), text("version"))
    } else {
        format!("{} {}", text("app"), text("version"))
    }
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

/// The room's `install-app` for the package `package` built from the app in
/// `app_dir`: the surface its changes name (the BLAKE3 of the app's
/// `surface.toml`), the package's `tapp:` hash and the key that signed it.
///
/// # Errors
/// The package does not read as a signed package of an app, or the app has
/// no surface.
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
            "read {}: the room decides changes against it (`tilo-app surface`)",
            surface_file.display()
        )
    })?;
    let manifest = &tapp.meta.manifest.package;
    let argument = object([
        ("app", json!(manifest.id)),
        ("version", json!(manifest.version)),
        ("surface", json!(blake3::hash(&surface).to_hex().as_str())),
        ("package", json!(hex::encode(tapp.meta.id.0))),
        (
            "publisher",
            json!(hex::encode(tapp.meta.root.publisher_identity)),
        ),
        ("dev", json!(dev)),
    ]);
    Ok(Push {
        action: "install-app",
        argument,
    })
}

/// The room's `request-bot` carrying the program in `program`.
///
/// # Errors
/// The program cannot be read or does not parse.
pub fn bot_push(program: &Path, entry: &str, version: &str) -> Result<Push> {
    let text =
        std::fs::read_to_string(program).with_context(|| format!("read {}", program.display()))?;
    let shortcut = tilo_automation::program::parse_program(text.as_bytes())
        .map_err(|error| anyhow::anyhow!("{}: {error}", program.display()))?;
    let canonical = tilo_automation::program::canonical_bytes(&shortcut)
        .map_err(|error| anyhow::anyhow!("{}: {error}", program.display()))?;
    let argument = object([
        ("entry", json!(entry)),
        ("version", json!(version)),
        ("program", json!(String::from_utf8(canonical)?)),
    ]);
    Ok(Push {
        action: "request-bot",
        argument,
    })
}

/// An action's argument from its named fields.
fn object<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}
