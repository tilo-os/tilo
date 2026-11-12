//! `tilo-local`: one command for each way to run Tilo from a checkout. The workspace's Cargo
//! aliases (`.cargo/config.toml`) are the way in: `cargo desktop`, `cargo headless`, `cargo web`.
//!
//! Each command drives the path that already exists for it and adds nothing beside it: the
//! development session (`dist/dev/nested-session.sh` over `dist/dev/nested-session-build.sh`) for
//! the desktop and the headless runtime, and the web bundle's build (`build.sh`) and the local
//! hosting stack (`os/containers/tilo-sh/stack.sh`) for the web desktop. What this adds is the
//! choice of options, a line saying where the running thing is and how to reach it, and `--check`:
//! a smoke test that starts the thing, asks it something, and stops it.

mod check;
mod json;
mod session;
mod web;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = "\
Run Tilo from this checkout, one command each.

  cargo desktop [OPTIONS]    the Tilo desktop in a window on the desktop you are using
  cargo headless [OPTIONS]   the session and its apps with no window, for agents and tests
  cargo web [OPTIONS]        the web desktop, built and served by the local tilo.sh stack

Each command takes --help for its options, and --check to start it, ask it
something, stop it and say whether it worked (a smoke test, run by CI too).

Nothing here touches the session you are logged in to: the desktop and the
headless runtime get their own runtime directory, state directory and session
bus, and reach neither your session bus, the system bus, systemd nor logind.

A single app in its own window needs none of this: cargo run --locked -p notes
(the package name from apps/<app>/Cargo.toml).
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let command = args.remove(0);
    let ran = match command.as_str() {
        "desktop" => session::main(session::Mode::Desktop, args),
        "headless" => session::main(session::Mode::Headless, args),
        "web" => web::main(&args),
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!(
            "no command {other}: the commands are desktop, headless and web (cargo desktop --help)"
        )),
    };
    ran.unwrap_or_else(|error| {
        eprintln!("tilo-local: {error}");
        ExitCode::FAILURE
    })
}

/// The checkout this runs in: the nearest directory above the working directory holding the
/// development session's script, else the one this launcher was built from.
fn repo_root() -> Result<PathBuf, String> {
    let marker = Path::new("dist/dev/nested-session.sh");
    if let Ok(cwd) = std::env::current_dir() {
        for dir in cwd.ancestors() {
            if dir.join(marker).is_file() {
                return Ok(dir.to_path_buf());
            }
        }
    }
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    if built.join(marker).is_file() {
        return built
            .canonicalize()
            .map_err(|error| format!("{}: {error}", built.display()));
    }
    Err("run this inside a Tilo checkout (no dist/dev/nested-session.sh above here)".into())
}

/// Cargo's target directory for this checkout, as the build scripts pick it.
fn target_dir(repo: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map_or_else(|| repo.join("target"), |dir| repo.join(dir))
}

/// `$XDG_STATE_HOME/NAME`, where the development session keeps its state by default.
fn state_home(name: &str) -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    base.join(name)
}

/// "12.3s".
fn seconds(elapsed: std::time::Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}
