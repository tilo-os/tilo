//! `tilolang`: build packages into Tilo apps and run them in a nested Tilo session; every other
//! command is the compiler's (`check`, `fmt`, `ids`).

use std::path::PathBuf;
use std::process::ExitCode;

use tilolang_cli::dev::{self, DevOptions};
use tilolang_cli::package::{self, BuildOptions};
use tilolang_cli::repl::{self, ReplOptions};
use tilolang_cli::run::{self, RunOptions};

const USAGE: &str = "\
usage:
  tilolang build [DIR] [--search DIR]... [--out DIR] [--identity FILE]
                 [--tilo-runtime tilo-runtime/<release>] [--version V]
      compile the package in DIR (default: .) and the packages it uses, and write each as a
      signed Tilo app: OUT/<name>.tapp (default OUT: DIR/target)
  tilolang run [DIR] [--search DIR]... [--peers N] [--headless] [--software] [--state DIR]
               [--repo DIR]
      build the package, install it into a locally built nested Tilo session
      (dist/dev/nested-session-build.sh target/nested-session) and open it there; from then on
      as `tilolang dev`. --peers N runs N sessions in one workspace; --software renders with
      lavapipe only and never opens this machine's GPU.
  tilolang dev [DIR] --session SESSION [--search DIR]... [--runtime BIN | --build-runtime]
               [--runtime-identity FILE] [--repo DIR]
      develop the package in a running nested session (SESSION: its --ready file, runtime
      directory or control socket): every saved .tl file is compiled, installed and taken over
      by the running app with its state; a compile error is printed and the app keeps running.
      --runtime BIN runs the apps on a tilolang runtime binary of your own build, and a new
      build of it restarts them on it; --build-runtime builds it from the checkout and again
      on every change of its sources with cargo. See docs/tilolang-dev.md.
  tilolang describe [DIR] [--search DIR]...
      print what the app in DIR publishes as tilolang: its resources' types, enums and actions,
      a native app (its committed surface.toml) and a tilolang package alike
  tilolang repl [--session SESSION | --door SOCKET | --real] [--color always|never]
      tilolang typed against a running session through its agent door: the nested session
      started last by default (SESSION: its --ready file, runtime directory or control socket,
      where the REPL is admitted as the person's agent, its answers theirs), another session's
      door, or the person's own session (--real), where the person admits it. Each entry is a
      program for the door's `tilo` tool, run as the REPL's task: lets are kept, refs (n3) name
      what answers showed, a bulk change previews until `confirm`, `undo` takes a step back.
  tilolang layout-check [DIR]... [--search DIR]... [--allow FILE] [--json] [--dump WxH [--shots DIR]]
      lay out every view of each app headlessly, staged from each of its scenarios, at window
      sizes from 320x568 to 3440x1440, display scales 1 and 2 and text scales 1 and 1.5, and
      report overflow, clipped text, overlapping siblings, hit targets below the minimum and
      pages that scroll sideways, each at its view's node in the source. Exits 1 on a finding
      the allow-list (one finding id per line) does not name; --json prints the report as JSON;
      --dump prints each layout at that window size as the check reads it (boxes, clips and
      semantics of every widget) to stderr, and --shots writes the window at that size (display
      and text scale 1) as each stage leaves it, light and dark, to PNGs in DIR.
  tilolang check|fmt|ids ...
      the compiler's commands (tilolangc --help); `use` also finds native apps in --path";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some("build") => build(rest),
        Some("run") => run_command(rest),
        Some("dev") => dev_command(rest),
        Some("describe") => describe_command(rest),
        Some("repl") => repl_command(rest),
        Some("layout-check") => return layout_check_command(rest),
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        // A package may `use` a native app: its published types are found among --path too.
        Some(_) => {
            return tilolang::cli::main_with(&args, tilolang_tilo::catalogue::native_apps);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilolang: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The flags both `build` and `run` take.
struct Common {
    dir: PathBuf,
    search: Vec<PathBuf>,
}

fn value(args: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String, String> {
    args.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn build(args: &[String]) -> Result<(), String> {
    let mut common = Common {
        dir: PathBuf::from("."),
        search: Vec::new(),
    };
    let mut out = None;
    let mut identity = None;
    let mut tilo_runtime = package::DEFAULT_TILO_RUNTIME.to_owned();
    let mut version = "0.1.0".to_owned();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" => common.search.push(value(&mut iter, arg)?.into()),
            "--out" => out = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--identity" => identity = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--tilo-runtime" => tilo_runtime = value(&mut iter, arg)?,
            "--version" => version = value(&mut iter, arg)?,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            dir => common.dir = dir.into(),
        }
    }
    let identity_path =
        identity.unwrap_or_else(|| tilolang_cli::state_dir().join("publisher.json"));
    let identity = tilolang_cli::identity_at(&identity_path)?;
    let options = BuildOptions {
        out: out.unwrap_or_else(|| common.dir.join("target")),
        dir: common.dir,
        search: common.search,
        tilo_runtime,
        tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
        version,
    };
    for app in package::build(&options, &identity)? {
        println!("{} ({}): {}", app.app, app.title, app.tapp.display());
    }
    Ok(())
}

fn layout_check_command(args: &[String]) -> ExitCode {
    let mut options = tilolang_cli::layout_check::Options::default();
    let mut json = false;
    let mut iter = args.iter();
    let parsed: Result<(), String> = (|| {
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--search" | "--path" => options.search.push(value(&mut iter, arg)?.into()),
                "--allow" => options.allow = Some(value(&mut iter, arg)?.into()),
                "--json" => json = true,
                "--shots" => options.shots = Some(value(&mut iter, arg)?.into()),
                "--dump" => {
                    let size = value(&mut iter, arg)?;
                    options.dump = size
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .map(Some)
                        .ok_or_else(|| format!("--dump {size}: a window size such as 390x844"))?;
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown option {flag}\n{USAGE}"));
                }
                dir => options.dirs.push(dir.into()),
            }
        }
        Ok(())
    })();
    if let Err(error) = parsed {
        eprintln!("tilolang: {error}");
        return ExitCode::from(2);
    }
    if options.dirs.is_empty() {
        options.dirs.push(PathBuf::from("."));
    }
    let report = match tilolang_cli::layout_check::check(&options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("tilolang layout-check: {error}");
            return ExitCode::from(2);
        }
    };
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("tilolang layout-check: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", tilolang_cli::layout_check::render(&report));
    }
    if report.new_findings().next().is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn describe_command(args: &[String]) -> Result<(), String> {
    let mut dir = PathBuf::from(".");
    let mut search = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" | "--path" => search.push(value(&mut iter, arg)?.into()),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            other => dir = other.into(),
        }
    }
    print!("{}", tilolang_cli::describe::describe(&dir, &search)?);
    Ok(())
}

fn run_command(args: &[String]) -> Result<(), String> {
    let mut options = RunOptions {
        dir: PathBuf::from("."),
        search: Vec::new(),
        peers: 1,
        headless: false,
        software: false,
        state: None,
        repo: None,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" => options.search.push(value(&mut iter, arg)?.into()),
            "--peers" => {
                let peers = value(&mut iter, arg)?;
                options.peers = peers
                    .parse()
                    .ok()
                    .filter(|peers| (1..=26).contains(peers))
                    .ok_or_else(|| format!("--peers {peers}: a number of sessions from 1 to 26"))?;
            }
            "--headless" => options.headless = true,
            "--software" => options.software = true,
            "--state" => options.state = Some(value(&mut iter, arg)?.into()),
            "--repo" => options.repo = Some(value(&mut iter, arg)?.into()),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            dir => options.dir = dir.into(),
        }
    }
    run::run(&options)
}

fn dev_command(args: &[String]) -> Result<(), String> {
    let mut dir = PathBuf::from(".");
    let mut search = Vec::new();
    let mut session = None;
    let mut runtime = None;
    let mut build_runtime = false;
    let mut runtime_identity = None;
    let mut repo = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" => search.push(value(&mut iter, arg)?.into()),
            "--session" => session = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--runtime" => runtime = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--build-runtime" => build_runtime = true,
            "--runtime-identity" => runtime_identity = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--repo" => repo = Some(PathBuf::from(value(&mut iter, arg)?)),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            named => dir = named.into(),
        }
    }
    let session = session.ok_or_else(|| {
        format!("tilolang dev needs --session (or start one with `tilolang run`)\n{USAGE}")
    })?;
    if runtime.is_some() && build_runtime {
        return Err(format!(
            "--runtime names a binary built by hand, --build-runtime builds one: give one\n{USAGE}"
        ));
    }
    dev::dev(&DevOptions {
        dir,
        search,
        session,
        runtime,
        build_runtime,
        runtime_identity,
        repo,
    })
}

fn repl_command(args: &[String]) -> Result<(), String> {
    let mut options = ReplOptions::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--session" => options.session = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--door" => options.door = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--real" => options.real = true,
            "--color" => {
                options.color = match value(&mut iter, arg)?.as_str() {
                    "always" => Some(true),
                    "never" => Some(false),
                    "auto" => None,
                    other => return Err(format!("--color is always, never or auto, not {other}")),
                }
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    repl::repl(&options)
}
