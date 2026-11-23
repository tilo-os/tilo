//! The `app` verbs on the package store and the running apps, said with app
//! names: pin, unpin, verify, inspect, gc, trust, runtimes and what is
//! running. A change is the package service's action; a read runs the
//! program that owns it (`tilo-install`, the launcher). So there is one
//! implementation and `app` is only the way in.

use std::path::Path;
use std::process::{Command, ExitCode, ExitStatus};

use app_cli::install::tilo_install_bin;
use clap::Subcommand;
use package_service_client::person;
use package_service_client::wire::{Change, Uploaded};

/// `app trust …`: whose packages this person accepts without asking.
#[derive(Debug, Subcommand)]
pub enum TrustCmd {
    /// List the attestors you trust.
    #[command(visible_alias = "ls")]
    List,
    /// Trust an attestor: packages it vouches for install without asking.
    Add {
        /// Attestor id, as packages name it.
        #[arg(long)]
        id: String,
        /// The attestor's Ed25519 public key (64 hex characters).
        #[arg(long)]
        pubkey: String,
        /// What it may vouch for: `all`, `mime:<a>[,<b>…]` or `prefix:<a>[,<b>…]`.
        #[arg(long, default_value = "all")]
        scope: String,
    },
    /// Stop trusting an attestor.
    #[command(visible_alias = "rm")]
    Remove { id: String },
    /// Refuse every package a publisher key signed.
    Revoke {
        /// The publisher's Ed25519 public key (64 hex characters).
        pubkey: String,
        /// Why, kept next to the key.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// List the publisher keys you revoked.
    Revoked,
}

/// `app runtime …` beyond `install`: the runtime builds and environment
/// images apps run on.
#[derive(Debug, Subcommand)]
pub enum RuntimeStoreCmd {
    /// List the installed runtime builds.
    #[command(visible_alias = "ls")]
    List,
    /// Remove a runtime build. One a running app uses goes once it closes.
    #[command(visible_alias = "rm")]
    Remove { version: String },
    /// Keep a runtime build even when no installed app runs on it.
    Pin { version: String },
    /// Let collection remove a runtime build again once nothing needs it.
    Unpin { version: String },
    /// The environment images an environment's system is stacked from.
    Image {
        #[command(subcommand)]
        cmd: ImageCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum ImageCmd {
    /// List the environment images this device holds.
    #[command(visible_alias = "ls")]
    List,
    /// Add an environment image from a package file.
    Add {
        /// Path to the .tapp file.
        tapp: std::path::PathBuf,
        /// Also trust this publisher (64 hex characters) for this image.
        #[arg(long)]
        publisher: Option<String>,
    },
    /// Remove an environment image (`tapp:<hex>`) from your store.
    #[command(visible_alias = "rm")]
    Remove { id: String },
}

/// The exit code of a program `app` ran for the person, its output already
/// theirs.
pub fn exit_code(status: std::io::Result<ExitStatus>) -> ExitCode {
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
        ),
        Err(error) => {
            eprintln!("app: could not run it: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Run `tilo-install` with `args`, its output the person's: the reads it answers.
pub fn installer<I, S>(args: I) -> ExitCode
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    exit_code(Command::new(tilo_install_bin()).args(args).status())
}

/// Make `change` through the package service's action, uploading `file` first, and show what it did.
pub fn change(change: Change, file: Option<&Path>) -> ExitCode {
    match person::change(change, file) {
        Ok(report) => {
            person::show(&report, "app");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("app: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Make the change `make` builds for the installed app `query` names.
fn on_installed(query: &str, make: impl FnOnce(String) -> Change) -> ExitCode {
    match crate::resolve_installed(query) {
        Ok(id) => change(make(id), None),
        Err(error) => {
            eprintln!("app: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `app pin` and `app unpin`.
pub fn pin(query: &str, pinned: bool) -> ExitCode {
    on_installed(query, |app| Change::SetPinned { app, pinned })
}

/// Run `tilo-install <verb> <id>` for the installed app `query` names: the reads it answers.
pub fn read_installed(verb: &str, query: &str) -> ExitCode {
    match crate::resolve_installed(query) {
        Ok(id) => installer([verb, id.as_str()]),
        Err(error) => {
            eprintln!("app: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `app verify [app]`: every installed app's tree against its signed
/// manifest, or the one named.
pub fn verify(query: Option<&str>) -> ExitCode {
    match query {
        Some(query) => read_installed("verify", query),
        None => installer(["verify"]),
    }
}

pub fn gc(dry_run: bool) -> ExitCode {
    change(
        Change::Collect {
            dry_run,
            grace_secs: runtime_bundle::gc::DEFAULT_GRACE_SECS,
            max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
        },
        None,
    )
}

pub fn trust(cmd: TrustCmd) -> ExitCode {
    match cmd {
        TrustCmd::List => installer(["trust", "list"]),
        TrustCmd::Add { id, pubkey, scope } => {
            change(Change::AddAttestor { id, pubkey, scope }, None)
        }
        TrustCmd::Remove { id } => change(Change::RemoveAttestor { id }, None),
        TrustCmd::Revoke { pubkey, reason } => change(Change::Revoke { pubkey, reason }, None),
        TrustCmd::Revoked => installer(["revoked", "list"]),
    }
}

pub fn runtime(cmd: RuntimeStoreCmd) -> ExitCode {
    match cmd {
        RuntimeStoreCmd::List => installer(["runtime", "list"]),
        RuntimeStoreCmd::Remove { version } => change(Change::RemoveRuntime { version }, None),
        RuntimeStoreCmd::Pin { version } => change(
            Change::PinRuntime {
                version,
                pinned: true,
            },
            None,
        ),
        RuntimeStoreCmd::Unpin { version } => change(
            Change::PinRuntime {
                version,
                pinned: false,
            },
            None,
        ),
        RuntimeStoreCmd::Image {
            cmd: ImageCmd::List,
        } => installer(["image", "list"]),
        RuntimeStoreCmd::Image {
            cmd: ImageCmd::Add { tapp, publisher },
        } => change(
            Change::AddImage {
                file: Uploaded {
                    source: tapp.display().to_string(),
                },
                publisher,
                payload_from: None,
            },
            Some(&tapp),
        ),
        RuntimeStoreCmd::Image {
            cmd: ImageCmd::Remove { id },
        } => match package_service_client::image_id(&id) {
            Ok(id) => change(Change::RemoveImage { id: *id.as_bytes() }, None),
            Err(error) => {
                eprintln!("app: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

/// The launcher, which knows what runs: beside this program in a
/// development build, Tilo OS's own, else the one on `PATH`.
fn launcher_bin() -> std::path::PathBuf {
    const INSTALLED: &str = "/usr/libexec/tilo/tilo-launcher";
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-launcher")))
        .filter(|sibling| sibling.is_file())
        .or_else(|| {
            let installed = std::path::PathBuf::from(INSTALLED);
            installed.is_file().then_some(installed)
        })
        .unwrap_or_else(|| "tilo-launcher".into())
}

/// `app running`: the apps running in this session, with their memory,
/// CPU time and tier.
pub fn running(json: bool) -> ExitCode {
    let mut command = Command::new(launcher_bin());
    command.arg("apps");
    if json {
        command.arg("--json");
    }
    exit_code(command.status())
}
