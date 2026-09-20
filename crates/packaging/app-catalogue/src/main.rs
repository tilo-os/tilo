//! `tilo-catalogue` — list what the configured registries offer that
//! is not installed, and fetch one of them.
//!
//! The compositor shells out to this rather than linking the library:
//! listing and installing both mean HTTP, and neither belongs inside
//! the most privileged process on the machine. Same shape as
//! `tilo-launcher`, for the same reason.
//!
//! ```text
//! tilo-catalogue list [--json]
//! tilo-catalogue install <package-id>
//! ```
//!
//! `install` exits 0 when the package is installed and ready to
//! launch, and non-zero with a single plain-English line on stderr
//! otherwise. That line is what a caller shows a user, so it is
//! written to be read by one.
//!
//! ## What `install` says while it works
//!
//! An install is seconds long, and a caller with a person waiting on it
//! has to be able to say how far along it is. So it narrates on stdout,
//! one flushed line at a time:
//!
//! ```text
//! progress <received-bytes> <total-bytes|->
//! phase unpacking
//! installed <package-id> <version>
//! ```
//!
//! `-` for the total means the registry declared no `Content-Length`;
//! the caller shows an indeterminate indication rather than a fraction.
//! A reader that only cares about the exit status can ignore all of it.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::io::Write;
use std::process::ExitCode;

use app_catalogue::install::DownloadProgress;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-catalogue",
    about = "List and fetch apps the configured registries offer but this machine hasn't installed."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// List available-but-not-installed apps.
    List {
        /// Emit the machine-readable envelope instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Download and install one app by package id.
    #[command(alias = "add")]
    Install {
        /// Reverse-DNS package id, as printed by `list`.
        package_id: String,
    },
    /// Add, remove, or inspect app repositories.
    #[command(alias = "registry")]
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
}

#[derive(Debug, Subcommand)]
enum RepoCmd {
    /// List configured repositories in priority order.
    List,
    /// Add and enable a repository.
    Add {
        /// Short name used by list/remove/enable/disable.
        name: String,
        /// Base HTTP(S) URL serving `/entries`.
        url: String,
        /// Pinned Ed25519 registry-owner key (64 hex characters).
        #[arg(long)]
        owner_key: String,
    },
    /// Remove a repository from the configuration.
    #[command(alias = "rm")]
    Remove { name: String },
    /// Include a configured repository in catalogue refreshes.
    Enable { name: String },
    /// Keep a repository configured but stop querying it.
    Disable { name: String },
    /// Select stable releases or explicitly include development builds.
    Channel {
        name: String,
        #[arg(value_parser = ["stable", "development"])]
        channel: String,
    },
}

fn main() -> ExitCode {
    // Install the workspace-standard rustls ring provider explicitly. Failing
    // here would mean every https registry silently errored later.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List { json } => list(json),
        Cmd::Install { package_id } => install(&package_id),
        Cmd::Repo { cmd } => repo(cmd),
    }
}

fn repo(cmd: RepoCmd) -> ExitCode {
    use app_catalogue::config;

    let mut registries = config::load_registries();
    let confirmation = match cmd {
        RepoCmd::List => {
            if registries.registries.is_empty() {
                println!("no app repositories configured");
            }
            for registry in registries.registries {
                let state = if registry.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                let channel = match registry.channel {
                    pkg::version::ReleaseChannel::Stable => "stable",
                    pkg::version::ReleaseChannel::Development => "development",
                };
                println!(
                    "{}  {}  ({state}, {channel}, key pinned)",
                    registry.name, registry.base_url
                );
            }
            return ExitCode::SUCCESS;
        }
        RepoCmd::Add {
            name,
            url,
            owner_key,
        } => {
            if let Err(e) = config::add_registry(&mut registries, &name, &url, &owner_key) {
                eprintln!("could not add repository: {e}");
                return ExitCode::FAILURE;
            }
            format!("added repository '{name}' with a pinned owner key")
        }
        RepoCmd::Remove { name } => {
            if let Err(e) = config::remove_registry(&mut registries, &name) {
                eprintln!("could not remove repository: {e}");
                return ExitCode::FAILURE;
            }
            format!("removed repository '{name}'")
        }
        RepoCmd::Enable { name } => {
            if let Err(e) = config::set_registry_enabled(&mut registries, &name, true) {
                eprintln!("could not enable repository: {e}");
                return ExitCode::FAILURE;
            }
            format!("enabled repository '{name}'")
        }
        RepoCmd::Channel { name, channel } => {
            let Some(registry) = registries.registries.iter_mut().find(|r| r.name == name) else {
                eprintln!("registry '{name}' is not configured");
                return ExitCode::FAILURE;
            };
            registry.channel = if channel == "development" {
                pkg::version::ReleaseChannel::Development
            } else {
                pkg::version::ReleaseChannel::Stable
            };
            format!("selected {channel} releases for '{name}'")
        }
        RepoCmd::Disable { name } => {
            if let Err(e) = config::set_registry_enabled(&mut registries, &name, false) {
                eprintln!("could not disable repository: {e}");
                return ExitCode::FAILURE;
            }
            format!("disabled repository '{name}'")
        }
    };
    match config::save_registries(&registries) {
        Ok(()) => {
            println!("{confirmation}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not save repositories: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list(json: bool) -> ExitCode {
    let listing = app_catalogue::available();
    if json {
        return match serde_json::to_string(&listing) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("could not encode the catalogue: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if listing.apps.is_empty() {
        println!("nothing available to install");
    }
    for app in &listing.apps {
        println!(
            "{}  {}  {}  ({}, {})",
            app.package_id, app.display_name, app.version, app.registry, app.trust
        );
    }
    for bad in &listing.unreachable {
        eprintln!("registry {} unreachable: {}", bad.name, bad.detail);
    }
    ExitCode::SUCCESS
}

fn install(package_id: &str) -> ExitCode {
    // Flushed as it happens — the caller is a process watching a pipe, and a
    // fetch that only spoke when it finished is a fetch nobody can show. The
    // final `installed` line stays the last word either way, so a caller that
    // reads nothing but the exit status is unaffected.
    let mut announced_unpacking = false;
    let mut on_progress = |p: DownloadProgress| {
        match p.total {
            Some(total) => println!("progress {} {}", p.received, total),
            None => println!("progress {} -", p.received),
        }
        // The last report is the whole file being on disk, which is where
        // verification and extraction start.
        if !announced_unpacking && matches!(p.total, Some(t) if t == p.received) {
            announced_unpacking = true;
            println!("phase unpacking");
        }
        let _ = std::io::stdout().flush();
    };
    match app_catalogue::install::install_reporting(package_id, &mut on_progress) {
        Ok(app) => {
            println!("installed {} {}", app.package_id, app.version);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.user_message());
            ExitCode::FAILURE
        }
    }
}
