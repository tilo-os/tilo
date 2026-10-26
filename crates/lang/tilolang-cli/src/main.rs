//! `tilolang`: build packages into Tilo apps and run them in a nested Tilo session; every other
//! command is the compiler's (`check`, `fmt`, `ids`).

use std::path::PathBuf;
use std::process::ExitCode;

use tilolang_cli::package::{self, BuildOptions};
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
      (dist/dev/nested-session-build.sh target/nested-session) and open it there; every .tl edit
      rebuilds and reinstalls it. --peers N runs N sessions in one workspace; --software
      renders with lavapipe only and never opens this machine's GPU.
  tilolang check|fmt|ids ...
      the compiler's commands (tilolangc --help)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some("build") => build(rest),
        Some("run") => run_command(rest),
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(_) => return tilolang::cli::main(&args),
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
