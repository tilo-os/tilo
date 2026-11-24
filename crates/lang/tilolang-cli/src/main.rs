//! `tilolang`: the one command an app's author types, for an app written in tilolang (a package:
//! a directory of `.tl` files) and for one written in Rust (a Cargo package on the `tilo-app`
//! crate) alike.
//!
//! Two halves answer it. This file's answers what only a package has (`profile`, `repl`,
//! `layout-check`, the compiler's `fmt` and `ids`) and `describe`; [`cli`] answers what only a
//! Rust app has (`new`, `add`, `test`, `shot`, the platforms' builds, ...) and what every app shares
//! (`guide`, `keys`, `publish`, ...). A command both have (`build`, `package`, `run`, `dev`,
//! `check`) goes by the app it is given: a directory holding `.tl` files is a package, any other
//! is in a Rust app, which [`cli`] finds from there up to its `Cargo.toml`.

mod cli;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tilolang_cli::dev::{self, DevOptions};
use tilolang_cli::package::{self, BuildOptions};
use tilolang_cli::profile::{self, ProfileOptions};
use tilolang_cli::repl::{self, ReplOptions};
use tilolang_cli::run::{self, RunOptions};

/// What each command the tilolang half answers takes, as `tilolang VERB --help` prints it.
const USAGE: &[(&str, &str)] = &[
    (
        "build",
        "\
  tilolang build|package [DIR] [--search DIR]... [--out DIR] [--identity FILE]
                         [--tilo-runtime tilo-runtime/<release>] [--version V]
      compile the package in DIR (default: .) and the packages it uses, and write each as a
      signed Tilo app: OUT/<name>.tapp (default OUT: DIR/target)",
    ),
    (
        "run",
        "\
  tilolang run [DIR] [--search DIR]... [--headless] [--software] [--state DIR]
               [--repo DIR]
      build the package, install it into a locally built nested Tilo session
      (dist/dev/nested-session-build.sh target/nested-session) and open it there; from then on
      as `tilolang dev`. --software renders with lavapipe only and never opens this machine's
      GPU.",
    ),
    (
        "dev",
        "\
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
      runtime bundle installed in that session. See docs/tilolang-dev.md.",
    ),
    (
        "profile",
        "\
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
      scopes, which depends on the machine.",
    ),
    (
        "describe",
        "\
  tilolang describe [DIR] [--search DIR]... [--fleet]
      print what the app in DIR publishes as tilolang: its resources' types, enums and actions,
      a native app (its committed surface.toml) and a tilolang package alike
      --fleet prints it as the catalogue fleet reads it instead (JSON): the id, name, category,
      tag line, other-language names and keywords, the kinds of record it declares, the actions
      it offers while closed and the surface.toml it ships, or null for a package that declares
      no app",
    ),
    (
        "repl",
        "\
  tilolang repl [--workspace NAME] [--key-file FILE] [--color always|never]
  tilolang repl --session [SESSION] [--workspace NAME] [--color always|never]
      tilolang typed in one of your workspaces, as you: the one this computer's Tilo login is
      in, or NAME, or the one you pick when your key reaches several. Sign in first with
      `tilo login` (or act as a key file). With --session, work in
      a nested session open on this computer instead (dist/dev/nested-session.sh): the
      only one, the one you pick, or SESSION (its state directory, --ready file, runtime
      directory or control socket). Each entry is a program
      run as one step: lets are kept, refs (n3) name what answers showed, `undo` takes a
      step back.",
    ),
    (
        "layout-check",
        "\
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
      machine runs in.",
    ),
    (
        "check|fmt|ids",
        "\
  tilolang check|fmt|ids [DIR] [--json] [--watch] [--write-ids] [--path DIR]... [--check] [--rename OLD NEW] [--help]
      the package's errors, its canonical form and its ids lock (check, fmt, ids); `use` also
      finds native apps and the platform's packages in --path",
    ),
];

/// The commands only a package has: this half answers them whatever the directory holds.
const PACKAGE_COMMANDS: [&str; 6] = ["describe", "profile", "repl", "layout-check", "fmt", "ids"];

/// The commands a package and a Rust app both have, each its own way: the app named decides.
const SHARED_COMMANDS: [&str; 5] = ["build", "package", "run", "dev", "check"];

/// How `tilolang --help` lists the commands: a heading, then each command with a line of what it
/// does. A command of [`cli`]'s that no group names is listed with the Rust apps, in its order,
/// with its own line; a line here is for a command [`cli`] does not have, or says what it does
/// for both kinds of app.
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Every app",
        &[
            ("guide", ""),
            ("examples", ""),
            (
                "check",
                "A package's errors; a Rust app's lints, structure, tests and audited scenarios.",
            ),
            (
                "dev",
                "Run the app and take every saved change into the running window, with its state.",
            ),
            (
                "run",
                "A package in a nested Tilo session; a Rust app natively or for a --target.",
            ),
            (
                "build",
                "A package into a signed .tapp; a Rust app into its artifact for a --target.",
            ),
            (
                "package",
                "Build, sign and verify the installable package (.tapp).",
            ),
            (
                "describe",
                "Print the resources' types, enums and actions an app publishes, as tilolang.",
            ),
            ("keys", ""),
            ("apply", ""),
            ("publish", ""),
            ("push", ""),
        ],
    ),
    (
        "tilolang packages",
        &[
            (
                "profile",
                "Count what the app costs to open and to use, by source line, calls and fuel.",
            ),
            (
                "layout-check",
                "Lay out every view at every window size and scale, and report what breaks.",
            ),
            ("repl", "Type tilolang in one of your workspaces, as you."),
            ("fmt", "Write the package in its canonical form."),
            ("ids", "Check or write the package's ids lock (app.ids)."),
        ],
    ),
    ("Rust apps", &[]),
];

fn main() -> ExitCode {
    // `dev` runs this binary again as the compiler's wrapper and as the linker (a Rust app's hot
    // reload): those runs are not command lines.
    tilo_app_builder::run_as_build_tool_if_requested();
    let argv: Vec<OsString> = std::env::args_os().collect();
    let verb = argv
        .get(1)
        .and_then(|verb| verb.to_str())
        .unwrap_or("--help");
    // What follows `--` is the app's own (`tilolang run -- --help`), never this command's.
    let asks_help = |args: &[OsString]| {
        args.iter()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "--help" || arg == "-h")
    };
    match verb {
        "-h" | "--help" => {
            print!("{}", overview());
            ExitCode::SUCCESS
        }
        // `tilolang help VERB` is `tilolang VERB --help`.
        "help" => match argv.get(2) {
            None => {
                print!("{}", overview());
                ExitCode::SUCCESS
            }
            Some(asked) => {
                let mut again = vec![argv.first().cloned().unwrap_or_default(), asked.clone()];
                again.push("--help".into());
                dispatch(asked.to_str().unwrap_or_default(), again, true)
            }
        },
        _ => {
            let help = asks_help(argv.get(2..).unwrap_or_default());
            dispatch(verb, argv.clone(), help)
        }
    }
}

/// Hand `argv` (`verb` its first argument after the program's name) to the half that answers it.
fn dispatch(verb: &str, argv: Vec<OsString>, help: bool) -> ExitCode {
    let shared = SHARED_COMMANDS.contains(&verb);
    if !(PACKAGE_COMMANDS.contains(&verb) || shared) {
        return cli::main(argv);
    }
    let words: Vec<String> = match argv
        .into_iter()
        .skip(1)
        .map(OsString::into_string)
        .collect()
    {
        Ok(words) => words,
        Err(arg) => {
            eprintln!("tilolang: {} is not UTF-8 text", arg.display());
            return ExitCode::from(2);
        }
    };
    if help && shared {
        // Both halves' flags: the package's first, then clap's own help for a Rust app.
        println!(
            "For a tilolang package (a directory of .tl files):\n{}\n\nFor a Rust app:",
            usage(verb)
        );
        let _ = std::io::stdout().flush();
        return cli::main(
            std::iter::once("tilolang".to_owned())
                .chain(words)
                .map(OsString::from)
                .collect(),
        );
    }
    if help && !matches!(verb, "fmt" | "ids") {
        println!("{}", usage(verb));
        return ExitCode::SUCCESS;
    }
    if shared && !names_package(words.get(1..).unwrap_or_default()) {
        return cli::main(
            std::iter::once("tilolang".to_owned())
                .chain(words)
                .map(OsString::from)
                .collect(),
        );
    }
    tilolang_command(&words)
}

/// Whether the app a command line of a shared command names is a package: its directory (the
/// first argument that is no flag nor a flag's value, else the working directory) holds `.tl`
/// files. A Rust app's command line reads the same way: no flag of its names a package.
fn names_package(args: &[String]) -> bool {
    const VALUE_FLAGS: [&str; 12] = [
        "--search",
        "--path",
        "--out",
        "--identity",
        "--tilo-runtime",
        "--version",
        "--state",
        "--repo",
        "--session",
        "--runtime",
        "--runtime-identity",
        "--allow",
    ];
    let mut dir = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--rename" {
            iter.nth(1);
        } else if VALUE_FLAGS.contains(&arg.as_str()) {
            iter.next();
        } else if !arg.starts_with('-') {
            dir = Some(arg.as_str());
            break;
        }
    }
    is_package(Path::new(dir.unwrap_or(".")))
}

/// Whether `dir` is a tilolang package: a directory with `.tl` files directly in it.
fn is_package(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext == tilolang::source::EXTENSION)
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
    })
}

/// `tilolang --help`: what the command is for, and every command of both halves by group.
fn overview() -> String {
    let rust: Vec<(String, String)> = cli::commands();
    let about = |name: &str| {
        rust.iter()
            .find(|(command, _)| command == name)
            .map(|(_, about)| about.clone())
            .unwrap_or_default()
    };
    let listed: Vec<&str> = GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter().map(|(name, _)| *name))
        .collect();
    let mut text = String::from(
        "tilolang: write a Tilo app, run it while you work on it, check, package and publish it.\n\n\
         An app is written in tilolang (a package: a directory of .tl files) or in Rust (a Cargo\n\
         package on the tilo-app crate). A command given an app reads which from its directory\n\
         (default: the working directory): one holding .tl files is a package; any other is in a\n\
         Rust app, found from there up to its Cargo.toml.\n\n\
         usage: tilolang [--json] <COMMAND> [ARGS]...\n",
    );
    for (heading, commands) in GROUPS {
        let mut rows: Vec<(String, String)> = commands
            .iter()
            .map(|(name, line)| {
                let line = if line.is_empty() {
                    about(name)
                } else {
                    (*line).to_owned()
                };
                ((*name).to_owned(), line)
            })
            .collect();
        if commands.is_empty() {
            rows = rust
                .iter()
                .filter(|(name, _)| !listed.contains(&name.as_str()))
                .cloned()
                .collect();
        }
        let _ = write!(text, "\n{heading}:\n");
        for (name, line) in rows {
            let _ = writeln!(text, "  {name:<13} {line}");
        }
    }
    text.push_str(
        "\n`tilolang <COMMAND> --help` tells what one command takes; `tilolang guide` lists short\n\
         workflows. --json (Rust apps' commands, and `guide`, `keys`, `publish`, ...) writes one JSON\n\
         document of the result to stdout.\n",
    );
    text
}

/// The usage of `verb`'s tilolang half: its section of [`USAGE`].
fn usage(verb: &str) -> &'static str {
    let verb = match verb {
        "package" => "build",
        "check" | "fmt" | "ids" => "check|fmt|ids",
        other => other,
    };
    USAGE
        .iter()
        .find(|(name, _)| *name == verb)
        .map_or("", |(_, text)| *text)
}

/// Run a command line of the tilolang half (`args`: the verb first).
fn tilolang_command(args: &[String]) -> ExitCode {
    if let Err(error) = tilolang_cli::load_checkout_stdlib() {
        eprintln!("tilolang: {error}");
        return ExitCode::FAILURE;
    }
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some("build" | "package") => build(rest),
        Some("run") => run_command(rest),
        Some("dev") => dev_command(rest),
        Some("describe") => describe_command(rest),
        Some("profile") => profile_command(rest),
        Some("repl") => repl_command(rest),
        Some("layout-check") => return tilolang_cli::layout_check::command::run(rest),
        // A package may `use` a native app or the platform's (the device, Settings): their
        // published types are found as `build` and `layout-check` find them.
        Some(_) | None => {
            return tilolang::cli::main_with(args, tilolang_tilo::catalogue::published_packages);
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
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag}\n{}", usage("build")));
            }
            dir => common.dir = dir.into(),
        }
    }
    let identity = match identity {
        Some(path) => tilolang_cli::identity_at(&path)?,
        None => tilolang_cli::publisher_identity()?,
    };
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

fn describe_command(args: &[String]) -> Result<(), String> {
    let mut dir = PathBuf::from(".");
    let mut search = Vec::new();
    let mut fleet = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--search" | "--path" => search.push(value(&mut iter, arg)?.into()),
            "--fleet" => fleet = true,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag}\n{}", usage("describe")));
            }
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
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag}\n{}", usage("run")));
            }
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
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag}\n{}", usage("dev")));
            }
            named => dir = named.into(),
        }
    }
    if runtime_identity.is_some() && session.is_none() {
        return Err(format!(
            "--runtime-identity signs a session's runtime bundle: give --session too\n{}",
            usage("dev")
        ));
    }
    if runtime.is_some() && build_runtime {
        return Err(format!(
            "--runtime names a binary built by hand, --build-runtime builds one: give one\n{}",
            usage("dev")
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
            other => return Err(format!("unknown option {other}\n{}", usage("repl"))),
        }
    }
    repl::repl(&options)
}
