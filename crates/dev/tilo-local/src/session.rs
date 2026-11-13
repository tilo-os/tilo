//! `cargo desktop` and `cargo headless`: the development session (`dist/dev/nested-session.sh`),
//! in a window on the desktop this runs on or with no window at all.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::time::{Duration, Instant};

use crate::{check, json, repo_root, seconds, state_home, target_dir};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Desktop,
    Headless,
}

const DESKTOP_HELP: &str = "\
cargo desktop: the Tilo desktop in a window on the desktop you are using.

Builds this checkout's session and apps (only what changed compiles), installs
the apps as signed packages in private state and opens the desktop in a window
of its own (a nested compositor). It has its own runtime directory, its own
session bus and its own state directory, and never reaches your session bus,
the system bus, systemd or logind. Close the window or press Ctrl-C to stop it.

Usage: cargo desktop [OPTIONS] [-- SESSION ARGUMENTS]

Options:
  --apps \"NAME ...\"   apps to install, directory names under apps/ (default:
                      every app the session builds; \"\" for none)
  --state DIR         state directory (default: $XDG_STATE_HOME/tilo-nested);
                      the desktop, identity and apps survive restarts there
  --fresh             forget workspaces and open windows; keep identity and apps
  --reset-identity    discard the throwaway identity and mint a new one
  --no-build          run the last build (target/nested-session) as it is
  --software          render on the CPU (lavapipe) and never open the GPU
  --ready FILE        write the session's control socket, runtime and state
                      directories to FILE as JSON once it answers
  --install-tapp FILE, --community SITE, --model-provider SPEC
                      passed to dist/dev/nested-session.sh (see its --help)
  --check             smoke test: start the desktop inside a throwaway headless
                      host desktop, with traps in place of the session bus, the
                      system bus and systemd, confirm its window opened there
                      and nothing reached a trap, then stop. CPU only, no
                      display needed; runs on a build machine.

Environment: TILO_WINIT_SIZE=WxH sizes the window (logical pixels, default
1280x800). Logs: STATE/logs/session.log.

Examples:
  cargo desktop --apps \"notes chat\"
  cargo desktop --no-build --fresh
  cargo desktop --check --apps notes
";

const HEADLESS_HELP: &str = "\
cargo headless: the Tilo session and its apps with no window, for agents,
scripts and tests.

Builds and starts the same session as `cargo desktop`, on an offscreen output
and rendering on the CPU (lavapipe), with its own runtime directory, session
bus and state. It runs in the foreground until Ctrl-C; once it answers it
prints how to reach it: tilo-control (snapshot, list-targets, click, type,
actions list and invoke) on its control socket, and the agent door.

Usage: cargo headless [OPTIONS] [-- SESSION ARGUMENTS]

Options:
  --apps \"NAME ...\"   apps to install, directory names under apps/ (default:
                      every app the session builds; \"\" for none)
  --state DIR         state directory (default: $XDG_STATE_HOME/tilo-headless)
  --fresh             forget workspaces and open windows; keep identity and apps
  --reset-identity    discard the throwaway identity and mint a new one
  --no-build          run the last build (target/nested-session) as it is
  --gpu               render on this machine's GPU instead of the CPU
  --ready FILE        write {control, runtime, state, bin} to FILE as JSON once
                      the session answers: a script waits for this file
  --install-tapp FILE, --community SITE, --model-provider SPEC
                      passed to dist/dev/nested-session.sh (see its --help)
  --check             smoke test: start it, list its actions through
                      tilo-control, take a snapshot, stop it; fails if it does
                      not answer or anything reached the host's buses

Environment: TILO_HEADLESS_SIZE=WxH sizes the output (default 1280x800).
Logs: STATE/logs/session.log.

Examples:
  cargo headless --apps notes --ready /tmp/tilo.json &   # then read the file
  cargo headless --check --apps notes
";

/// What the command line asked for.
pub struct Options {
    pub mode: Mode,
    pub state: PathBuf,
    pub build: bool,
    pub check: bool,
    pub ready: Option<PathBuf>,
    /// The apps asked for (`--apps`), which is also all the build builds.
    pub apps: Option<String>,
    /// Options for nested-session.sh, as it takes them.
    pub script: Vec<String>,
    /// Everything after `--`, for the session itself.
    pub session: Vec<String>,
}

fn parse(mode: Mode, args: Vec<String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        mode,
        state: state_home(match mode {
            Mode::Desktop => "tilo-nested",
            Mode::Headless => "tilo-headless",
        }),
        build: true,
        check: false,
        ready: None,
        apps: None,
        script: Vec::new(),
        session: Vec::new(),
    };
    let mut software = mode == Mode::Headless;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value (see --help)"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!(
                    "{}",
                    match mode {
                        Mode::Desktop => DESKTOP_HELP,
                        Mode::Headless => HEADLESS_HELP,
                    }
                );
                return Ok(None);
            }
            "--state" => options.state = PathBuf::from(value("--state")?),
            "--ready" => options.ready = Some(PathBuf::from(value("--ready")?)),
            "--no-build" => options.build = false,
            "--check" => options.check = true,
            "--software" => software = true,
            "--gpu" if mode == Mode::Headless => software = false,
            "--fresh" => options.script.push("--fresh-desktop".into()),
            "--reset-identity" | "--fresh-desktop" => options.script.push(arg),
            "--apps" => {
                let given = value("--apps")?;
                options.apps = Some(given.clone());
                options.script.extend([arg, given]);
            }
            "--install-tapp" | "--community" | "--model-provider" => {
                let given = value(&arg)?;
                options.script.extend([arg, given]);
            }
            "--" => {
                options.session.extend(args.by_ref());
            }
            other => {
                return Err(format!(
                    "unknown option {other} (cargo {} --help lists them)",
                    match mode {
                        Mode::Desktop => "desktop",
                        Mode::Headless => "headless",
                    }
                ));
            }
        }
    }
    if software {
        options.script.push("--software".into());
    }
    if mode == Mode::Headless {
        options.script.push("--headless".into());
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    options.state = cwd.join(&options.state);
    Ok(Some(options))
}

pub fn main(mode: Mode, args: Vec<String>) -> Result<ExitCode, String> {
    let Some(options) = parse(mode, args)? else {
        return Ok(ExitCode::SUCCESS);
    };
    let repo = repo_root()?;
    if options.check {
        return check::session(&repo, &options);
    }
    run(&repo, &options)
}

/// Where the session's build lands, as nested-session.sh reads it.
pub fn build_dir(repo: &Path) -> PathBuf {
    std::env::var_os("TILO_NESTED_SESSION_BUILD")
        .map_or_else(|| target_dir(repo).join("nested-session"), PathBuf::from)
}

/// nested-session.sh with `options`, writing its readiness to `ready`.
pub fn script(repo: &Path, options: &Options, ready: &Path, build: bool) -> Command {
    let mut command = Command::new("bash");
    command
        .arg(repo.join("dist/dev/nested-session.sh"))
        .arg("--state")
        .arg(&options.state)
        .arg("--ready")
        .arg(ready)
        .args(&options.script);
    if !build {
        command.arg("--no-build");
    }
    apps_to_build(&mut command, options);
    if !options.session.is_empty() {
        command.arg("--").args(&options.session);
    }
    command
}

/// Build only the apps asked for: the build takes them from `TILO_NESTED_APPS`, as directory names.
pub fn apps_to_build(command: &mut Command, options: &Options) {
    if let Some(apps) = &options.apps
        && std::env::var_os("TILO_NESTED_APPS").is_none()
    {
        let names: Vec<&str> = apps
            .split_whitespace()
            .map(|app| if app == "files" { "file-explorer" } else { app })
            .collect();
        command.env("TILO_NESTED_APPS", names.join(" "));
    }
}

/// Wait until `ready` exists, or the session ended or took longer than `limit`.
pub fn wait_ready(child: &mut Child, ready: &Path, limit: Duration) -> Result<String, String> {
    let began = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(ready) {
            return Ok(text);
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("the session ended before it answered ({status})"));
        }
        if began.elapsed() > limit {
            return Err(format!(
                "the session did not answer within {}",
                seconds(limit)
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn run(repo: &Path, options: &Options) -> Result<ExitCode, String> {
    let began = Instant::now();
    let ready = options
        .ready
        .clone()
        .unwrap_or_else(|| options.state.join("ready.json"));
    let _ = std::fs::remove_file(&ready);
    let mut child = script(repo, options, &ready, options.build)
        .spawn()
        .map_err(|error| format!("could not start nested-session.sh: {error}"))?;
    // A build and first install take minutes; a session that never answers says why in its log.
    match wait_ready(&mut child, &ready, Duration::from_secs(3 * 3600)) {
        Ok(text) => eprint!("{}", reach(options, &text, began.elapsed())),
        Err(error) => eprintln!("tilo-local: {error}"),
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from))
}

/// How to reach the running session, from what it wrote once it answered.
fn reach(options: &Options, ready: &str, took: Duration) -> String {
    let field = |key| json::string_field(ready, key).unwrap_or_default();
    let (control, runtime, bin) = (field("control"), field("runtime"), field("bin"));
    let what = match options.mode {
        Mode::Desktop => "The Tilo desktop is up in its window",
        Mode::Headless => "Tilo is running headless",
    };
    format!(
        "\n==> {what} ({} to start). Ctrl-C stops it.\n\
         \x20   state:      {state}\n\
         \x20   logs:       {state}/logs/session.log\n\
         \x20   drive it:   TILO_TEST_CONTROL={control} {bin}/tilo-control actions list\n\
         \x20               (also: windows, list-targets, click TARGET, type TEXT; headless: snapshot FILE.png)\n\
         \x20   agent door: {runtime}/tilo-session/agent.sock (MCP over newline-delimited JSON-RPC)\n\
         \x20   where:      {runtime}/session.json\n\n",
        seconds(took),
        state = options.state.display(),
    )
}
