//! `tilolang`: build packages into Tilo apps and run them in a nested Tilo session; every other
//! command is the compiler's (`check`, `fmt`, `ids`).

use std::path::PathBuf;
use std::process::ExitCode;

use tilolang_cli::dev::{self, DevOptions};
use tilolang_cli::package::{self, BuildOptions};
use tilolang_cli::profile::{self, ProfileOptions};
use tilolang_cli::repl::{self, ReplOptions};
use tilolang_cli::run::{self, RunOptions};

const USAGE: &str = "\
usage:
  tilolang build [DIR] [--search DIR]... [--out DIR] [--identity FILE]
                 [--tilo-runtime tilo-runtime/<release>] [--version V]
      compile the package in DIR (default: .) and the packages it uses, and write each as a
      signed Tilo app: OUT/<name>.tapp (default OUT: DIR/target)
  tilolang run [DIR] [--search DIR]... [--headless] [--software] [--state DIR]
               [--repo DIR]
      build the package, install it into a locally built nested Tilo session
      (dist/dev/nested-session-build.sh target/nested-session) and open it there; from then on
      as `tilolang dev`. --software renders with lavapipe only and never opens this machine's
      GPU.
  tilolang dev [DIR] [--search DIR]... [--runtime BIN | --build-runtime] [--repo DIR]
  tilolang dev [DIR] --session SESSION [--search DIR]... [--runtime BIN | --build-runtime]
               [--runtime-identity FILE] [--repo DIR]
      develop the package as an ordinary window on this desktop: every saved .tl file is
      compiled, published to the running app and taken over with its state (the same window,
      its records, scroll and selection); a compile error is printed and the app keeps running.
      The app's records persist across runs. Without --runtime the tool builds tilolang-process
      from the checkout with cargo, and again on every change of its sources, restarting the
      window on it with its state.
      With --session, develop it in a running nested session instead (SESSION: its --ready
      file, runtime directory, control socket or state directory): every saved .tl file is compiled, installed
      and taken over by the app there, and --runtime BIN bakes the runtime binary into a signed
      runtime bundle installed in that session. See docs/tilolang-dev.md.
  tilolang profile [DIR] [--search DIR]... [--scenario NAME] [--seed N] [--size WxH] [--scale S]
                   [--root NAME] [--then INTERACTION]... [--times]
      count what the app in DIR (default: .) costs to open and to use, in the language's own
      terms: each unit of its code by source line, with the calls it ran, the fuel it spent and
      what it read from the host, sorted by fuel. --seed N opens a workspace of N items (as
      `tilolang bench` seeds one); --scenario stages that scenario of the app (the app's first
      by default); --then runs an interaction after the app is open and counts that alone:
      key:Tab, click:X,Y, type:TEXT, scroll:DX,DY, press:NAME (a click on the widget the window
      names so), drag:X0,Y0,X1,Y1 or hover:X,Y (the pointer moved there), in logical pixels,
      or resize:WxH (the window resized, as maximizing it does).
      --size is the window's size (default 1280x800), --scale the display's scale (default 1;
      1.25 or 2 as a laptop's panel), --root the layer of a package with more
      than one root. The counts are the same
      on any machine at any load; --times adds what each step took in the framework's named
      scopes, which depends on the machine.
  tilolang describe [DIR] [--search DIR]... [--fleet]
      print what the app in DIR publishes as tilolang: its resources' types, enums and actions,
      a native app (its committed surface.toml) and a tilolang package alike
      --fleet prints it as the catalogue fleet reads it instead (JSON): the id, name, category,
      tag line, other-language names and keywords, the kinds of record it declares, the actions
      it offers while closed and the surface.toml it ships, or null for a package that declares
      no app
  tilolang repl [--workspace NAME] [--key-file FILE] [--color always|never]
  tilolang repl --session [SESSION] [--workspace NAME] [--color always|never]
      tilolang typed in one of your workspaces, as you: the one this computer's Tilo login is
      in, or NAME, or the one you pick when your key reaches several. Sign in first with
      `tilo login` (or act as a key file). With --session, work in
      a nested session open on this computer instead (dist/dev/nested-session.sh): the
      only one, the one you pick, or SESSION (its state directory, --ready file, runtime
      directory or control socket). Each entry is a program
      run as one step: lets are kept, refs (n3) name what answers showed, `undo` takes a
      step back.
  tilolang layout-check [DIR]... [--search DIR]... [--allow FILE] [--strict] [--json] [--root NAME] [--state NAME=VALUE]... [--key KEY]... [--hover X,Y]... [--press LABEL]... [--dump|--dump-findings WxH [--shots DIR [--theme NAME]... [--bar-at-rest] [--shot-height H]]] [--locale TAG]
      lay out every view of each app headlessly, staged from each of its scenarios, at window
      sizes from 320x568 to 3440x1440, display scales 1 and 2 and text scales 1 and 1.5, and
      report overflow, clipped text, overlapping siblings, hit targets below the minimum and
      pages that scroll sideways, each at its view's node in the source. Exits 1 on a finding
      the allow-list (one finding id per line) does not name; --strict exits 1 on an allow-list
      line whose finding no longer shows and on a view no layout showed, too; --json prints the
      report as JSON;
      --dump prints each layout at that window size as the check reads it (boxes, clips and
      semantics of every widget) to stderr, for a run narrowed to the views it reads;
      --dump-findings prints only the layouts a finding was read from, which is what a check over
      every app asks for, and --shots writes the window at that size (display
      and text scale 1) as each stage leaves it, light and dark, to PNGs in DIR. --theme takes
      them in that shipped theme (default, glass, block, ...) instead of the stock pair, light and
      dark each. --bar-at-rest takes them as a maximized window stands at rest, its bar hidden
      until the person reaches for the top edge, instead of under the bar. --shot-height takes
      them in a window H tall at that width, so the lower part of a long page shows; the check
      itself still reads the window at its size. --root lays out the
      layer a package with more than one root names (the shell's `Shell`, the one with the bar
      and the dock), instead of the package's first root.
      --state stages a view state at a value the app's own code names (`place=Task.today`, a
      filter; `side=Side.right`, a case of its enum) before any window opens: the place a
      window stands on when nobody has picked one, as a scenario that laid it out would.
      --key sends a key press (`Tab`, `Down`, `Space`) to each window before it is read, in the
      order given, at the `--dump` size, so a shot and a dump show the layout a person reaches
      by keyboard. --hover rests the pointer at X,Y (logical pixels of the window) once the keys
      are sent, so a shot shows what pointing there shows. --locale lays every window out for a
      person reading in that language (`en-US`, `de-DE`; default en-US), whatever locale this
      machine runs in.
  tilolang check|fmt|ids [DIR] [--json] [--watch] [--write-ids] [--path DIR]... [--check] [--rename OLD NEW] [--help]
      the package's errors, its canonical form and its ids lock (check, fmt, ids); `use` also
      finds native apps and the platform's packages in --path";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Run in a checkout, the tool compiles against the checkout's standard library, as it does
    // against the packages beside the one it builds: an edit of it takes effect without
    // rebuilding the tool.
    let tree = std::env::current_dir()
        .ok()
        .and_then(|dir| tilolang::stdlib::in_tree(&dir));
    if let Some(Err(error)) = tree.map(|dir| tilolang::stdlib::load_from(&dir)) {
        eprintln!("tilolang: {error}");
        return ExitCode::FAILURE;
    }
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some("build") => build(rest),
        Some("run") => run_command(rest),
        Some("dev") => dev_command(rest),
        Some("describe") => describe_command(rest),
        Some("profile") => profile_command(rest),
        Some("repl") => repl_command(rest),
        Some("layout-check") => return layout_check_command(rest),
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        // A package may `use` a native app or the platform's (the device, Settings): their
        // published types are found as `build` and `layout-check` find them.
        Some(_) => {
            return tilolang::cli::main_with(&args, tilolang_tilo::catalogue::published_packages);
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

fn profile_command(args: &[String]) -> Result<(), String> {
    let mut dir = PathBuf::from(".");
    let mut options = ProfileOptions {
        dir: PathBuf::new(),
        search: Vec::new(),
        scenario: None,
        seed: None,
        size: (1280, 800),
        scale: 1.0,
        root: None,
        then: Vec::new(),
        times: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" => options.search.push(value(&mut iter, arg)?.into()),
            "--scenario" => options.scenario = Some(value(&mut iter, arg)?),
            "--seed" => {
                let n = value(&mut iter, arg)?;
                options.seed = Some(
                    n.parse()
                        .map_err(|_| format!("--seed {n}: not a number of items"))?,
                );
            }
            "--size" => {
                let size = value(&mut iter, arg)?;
                let (w, h) = size
                    .split_once(['x', 'X'])
                    .ok_or_else(|| format!("--size {size}: expected WxH"))?;
                let number = |text: &str| {
                    text.parse::<u32>()
                        .map_err(|_| format!("--size {size}: {text:?} is not a size"))
                };
                options.size = (number(w)?, number(h)?);
            }
            "--scale" => {
                let scale = value(&mut iter, arg)?;
                options.scale = scale
                    .parse::<f64>()
                    .ok()
                    .filter(|scale| *scale > 0.0)
                    .ok_or_else(|| format!("--scale {scale}: not a display scale"))?;
            }
            "--root" => options.root = Some(value(&mut iter, arg)?),
            "--then" => options.then.push(value(&mut iter, arg)?),
            "--times" => options.times = true,
            flag if flag.starts_with("--") => return Err(format!("profile: no flag {flag}")),
            dir_arg => dir = PathBuf::from(dir_arg),
        }
    }
    options.dir = dir;
    profile::profile(&options)
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
    let mut strict = false;
    let mut iter = args.iter();
    let parsed: Result<(), String> = (|| {
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--search" | "--path" => options.search.push(value(&mut iter, arg)?.into()),
                "--allow" => options.allow = Some(value(&mut iter, arg)?.into()),
                "--json" => json = true,
                "--strict" => strict = true,
                "--shots" => options.shots = Some(value(&mut iter, arg)?.into()),
                "--bar-at-rest" => options.bar_at_rest = true,
                "--shot-height" => {
                    let height = value(&mut iter, arg)?;
                    options.shot_height = Some(height.parse().map_err(|_| {
                        format!("--shot-height takes a height in logical pixels, not {height}")
                    })?);
                }
                "--theme" => options.themes.push(value(&mut iter, arg)?),
                "--root" => options.root = Some(value(&mut iter, arg)?),
                "--state" => options.states.push(value(&mut iter, arg)?),
                "--key" => options.keys.push(value(&mut iter, arg)?),
                "--hover" => options.hovers.push(value(&mut iter, arg)?),
                "--press" => options.presses.push(value(&mut iter, arg)?),
                "--locale" => options.locale = value(&mut iter, arg)?,
                "--dump" | "--dump-findings" => {
                    let size = value(&mut iter, arg)?;
                    let size = size
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .ok_or_else(|| format!("{arg} {size}: a window size such as 390x844"))?;
                    options.dump = Some(if arg == "--dump" {
                        tilolang_cli::layout_check::Dump::Every(size)
                    } else {
                        tilolang_cli::layout_check::Dump::Findings(size)
                    });
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
    if report.new_findings().next().is_some()
        || (strict && !(report.stale.is_empty() && report.unreached.is_empty()))
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn describe_command(args: &[String]) -> Result<(), String> {
    let mut dir = PathBuf::from(".");
    let mut search = Vec::new();
    let mut fleet = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" | "--path" => search.push(value(&mut iter, arg)?.into()),
            "--fleet" => fleet = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            other => dir = other.into(),
        }
    }
    if fleet {
        let entry = tilolang_cli::describe::fleet(&dir, &search)?;
        println!(
            "{}",
            serde_json::to_string(&entry).map_err(|error| error.to_string())?
        );
    } else {
        print!("{}", tilolang_cli::describe::describe(&dir, &search)?);
    }
    Ok(())
}

fn run_command(args: &[String]) -> Result<(), String> {
    let mut options = RunOptions {
        dir: PathBuf::from("."),
        search: Vec::new(),
        headless: false,
        software: false,
        state: None,
        repo: None,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" => options.search.push(value(&mut iter, arg)?.into()),
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
    let mut session: Option<PathBuf> = None;
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
    if runtime_identity.is_some() && session.is_none() {
        return Err(format!(
            "--runtime-identity signs a session's runtime bundle: give --session too\n{USAGE}"
        ));
    }
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
            "--workspace" | "-w" => options.workspace = Some(value(&mut iter, arg)?),
            "--key-file" => options.key_file = Some(PathBuf::from(value(&mut iter, arg)?)),
            "--session" => {
                let named = iter
                    .as_slice()
                    .first()
                    .filter(|next| !next.starts_with('-'));
                options.session = Some(named.map(PathBuf::from));
                if named.is_some() {
                    iter.next();
                }
            }
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
