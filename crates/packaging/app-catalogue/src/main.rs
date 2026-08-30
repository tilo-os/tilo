//! `qos-catalogue` — list what the configured registries offer that
//! is not installed, and fetch one of them.
//!
//! The compositor shells out to this rather than linking the library:
//! listing and installing both mean HTTP, and neither belongs inside
//! the most privileged process on the machine. Same shape as
//! `qos-launcher`, for the same reason.
//!
//! ```text
//! qos-catalogue list [--json]
//! qos-catalogue install <package-id>
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

use std::io::Write;

use app_catalogue::install::DownloadProgress;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "qos-catalogue",
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
    Install {
        /// Reverse-DNS package id, as printed by `list`.
        package_id: String,
    },
}

fn main() {
    // rustls needs an explicit provider: the tree carries both ring
    // and aws-lc-rs, and 0.23 will not pick one on its own. Failing
    // here would mean every https registry silently errored later.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List { json } => list(json),
        Cmd::Install { package_id } => install(&package_id),
    }
}

fn list(json: bool) {
    let listing = app_catalogue::available();
    if json {
        match serde_json::to_string(&listing) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("could not encode the catalogue: {e}");
                std::process::exit(1);
            }
        }
        return;
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
}

fn install(package_id: &str) {
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
        }
        Err(e) => {
            eprintln!("{}", e.user_message());
            std::process::exit(1);
        }
    }
}
