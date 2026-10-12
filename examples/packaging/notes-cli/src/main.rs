//! `notes` — a small reference CLI app that writes a note into
//! the Tilo workspace (or the standalone fallback dir when the
//! FUSE isn't mounted).
//!
//! ### Shape
//!
//! ```text
//! notes "hello world"          # append a line to notes/inbox.md
//! notes list                   # print notes/inbox.md
//! notes path                   # print the resolved workspace path
//! ```
//!
//! ### Storage
//!
//! One flat file: `notes/inbox.md` under the workspace root
//! (or `~/.local/share/so.tilo.notes-cli/notes/inbox.md` when
//! standalone).
//!
//! ### Publishing
//!
//! Ships as `so.tilo.notes-cli` via the crate's
//! `manifest.toml`. The `[cli].commands = ["notes"]` block causes
//! `tilo-install install ...` to drop a `notes` shim into
//! `~/.local/share/tilo-apps/bin/`.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

// Thin build only: pull the shared runtime SDK dylib
// (`libruntime_dylib.so`) into the link graph. It statically contains
// the `cli` SDK (and its runtime + tokio graph) this app links, so with
// `-C prefer-dynamic` rustc serves `cli` and libstd from the dylib
// instead of baking them into `notes`. Naming the crate here is what
// keeps rustc from eliminating it as unused. Compiled out in normal
// (static) builds.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// `notes` — CLI notes for Tilo.
#[derive(Debug, Parser)]
#[command(
    name = "notes",
    version,
    about = "Write a note into the Tilo workspace."
)]
struct Cli {
    /// Words to append as a single note line. If no words are
    /// given, expects `list` / `path` / a stdin pipe.
    words: Vec<String>,
}

fn main() -> ExitCode {
    cli::run_main(|| {
        let args = Cli::parse();
        match args.words.first().map(String::as_str) {
            Some("list") => list(),
            Some("path") => print_path(),
            Some(_) | None => append(&args.words),
        }
    })
}

fn note_path() -> Result<std::path::PathBuf> {
    cli::resolve_workspace_path("notes/inbox.md").context("resolving workspace path notes/inbox.md")
}

fn append(words: &[String]) -> Result<()> {
    if words.is_empty() {
        anyhow::bail!("usage: notes \"your note text\"    (or `notes list`, `notes path`)");
    }
    let target = note_path()?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
        .with_context(|| format!("opening {}", target.display()))?;
    let line = words.join(" ");
    writeln!(f, "- {line}").with_context(|| format!("appending to {}", target.display()))?;
    let out = cli::output();
    let _ = out.hyperlink_path("wrote note →", &target);
    Ok(())
}

fn list() -> Result<()> {
    let target = note_path()?;
    if !target.exists() {
        println!("(no notes yet — try `notes \"your first note\"`)");
        return Ok(());
    }
    let text = std::fs::read_to_string(&target)
        .with_context(|| format!("reading {}", target.display()))?;
    print!("{text}");
    Ok(())
}

fn print_path() -> Result<()> {
    let target = note_path()?;
    println!("{}", target.display());
    Ok(())
}
