//! `tilo-ush` — one binary, three roles, dispatched on `argv[0]`.
//!
//! - `ush` on the host: build a guest from an environment and run a shell in it.
//! - `pkg`, `perm`, `scan`, `guard`, `quarantine`, `restart` inside a guest:
//!   the runtime's own commands, symlinks to this binary on the guest's `PATH`.
//! - `tilo`, `app`, `store`, `system` inside a guest: brokered back to the
//!   logged-in host session so they update real app/system state.
//! - `tilo-ush serve`: the session supervisor an app asks for a guest.
//!
//! One binary because the guest has to run the *same* build that built its
//! confinement, and because `pkg` inside a guest and the supervisor outside it
//! are two halves of one protocol — splitting them would let the halves drift.

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

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use std::sync::Arc;

use ush::broker::{Request, Response};
use ush::images::Images;
use ush::store::Store;
use ush::supervisor::{Session, SessionOptions};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let prog_name = argv
        .first()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rest = argv.get(1..).unwrap_or(&[]);

    // Inside a guest the runtime is reached through its own name.
    let result = match prog_name.as_str() {
        "pkg" => guest::pkg(rest),
        "perm" => guest::perm(rest),
        "scan" => guest::scan(rest),
        "guard" => guest::guard(rest),
        "quarantine" => guest::quarantine(rest),
        "restart" => guest::restart(),
        "tilo" | "app" | "store" | "system" => guest::host_cli(&prog_name, rest),
        "ush" => host(rest),
        _ => dispatch(rest),
    };

    match result {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)),
        Err(e) => {
            eprintln!(
                "{}: {e:#}",
                if prog_name.is_empty() {
                    "ush"
                } else {
                    &prog_name
                }
            );
            ExitCode::from(1)
        }
    }
}

/// `tilo-ush <subcommand>` — the spelling used when the binary is invoked by
/// its own name rather than through a shim.
fn dispatch(args: &[String]) -> anyhow::Result<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("serve") => cmd_serve(rest),
        Some("guest-init") => guest::init(rest),
        Some("ush" | "run") => host(rest),
        Some("pkg") => guest::pkg(rest),
        Some("perm") => guest::perm(rest),
        Some("scan") => guest::scan(rest),
        Some("guard") => guest::guard(rest),
        Some("quarantine") => guest::quarantine(rest),
        Some("--help" | "-h") | None => {
            print!("{USAGE}");
            Ok(0)
        }
        Some(other) => anyhow::bail!("unknown command `{other}`; try --help"),
    }
}

const USAGE: &str = "\
tilo-ush — an application runtime that hosts a shell.

On the host:
  ush [-c COMMAND] [-v] [--environment RECORD]   open a shell in an environment
  tilo-ush serve [--socket PATH]                  hand guests to apps that ask

Inside a guest:
  tilo update                       update installed apps and the system image
  app update [APP]                  update apps (`store` is an alias)
  system update                     update the Tilo system image
  pkg install PKG…                 pkg remove PKG…    pkg list
  perm trust-dir DIR               perm list          perm revoke ID
  scan PATH
  guard status
  quarantine list | add PATH | restore PATH
  restart
";

// host side

fn host(args: &[String]) -> anyhow::Result<i32> {
    let mut opts = SessionOptions::default();
    let mut script: Option<String> = None;
    let mut i: usize = 0;
    while i < args.len() {
        let Some(arg) = args.get(i) else { break };
        let value = || {
            args.get(i.saturating_add(1))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{arg} needs a value"))
        };
        match arg.as_str() {
            "-c" => {
                // Matching `sh -c`: everything after it is one script, run by
                // the guest's shell, not split into an argv here.
                script = Some(value()?);
                i = i.saturating_add(2);
            }
            "-v" | "--verbose" => {
                opts.verbose = true;
                i = i.saturating_add(1);
            }
            "--environment" => {
                opts.environment = Some(value()?);
                i = i.saturating_add(2);
            }
            "--host-root" => {
                opts.host_root = PathBuf::from(value()?);
                i = i.saturating_add(2);
            }
            "--" => {
                // A program, as the supervisor passes one for a served session.
                opts.command = args.get(i.saturating_add(1)..).unwrap_or(&[]).to_vec();
                break;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(0);
            }
            other => anyhow::bail!("unknown option `{other}`; try --help"),
        }
    }

    init_tracing(opts.verbose);
    let store = Store::discover();
    let mut session = Session::new(store, opts, images(), builder());
    if let Some(script) = script {
        let resolved = session.resolve()?;
        let trees: Vec<PathBuf> = resolved.images.into_iter().map(|(_, p)| p).collect();
        session.opts.command = vec![
            ush::supervisor::guest_shell(&trees)
                .to_string_lossy()
                .into_owned(),
            "-c".into(),
            script,
        ];
    }
    session.run()
}

/// Where images come from on this device: the package store, or fixture
/// trees under `TILO_USH_IMAGES` in tests.
fn images() -> Arc<dyn Images> {
    ush::images::on_this_device()
}

/// How `pkg install` builds tool images on this device.
fn builder() -> Arc<dyn tilo_environment::ToolBuilder> {
    Arc::new(tilo_environment::NoToolBuilder)
}

fn cmd_serve(args: &[String]) -> anyhow::Result<i32> {
    let mut socket = default_service_socket();
    let mut host_root = PathBuf::from("/");
    let mut i: usize = 0;
    while i < args.len() {
        let Some(arg) = args.get(i) else { break };
        match arg.as_str() {
            "--socket" => {
                socket = PathBuf::from(
                    args.get(i.saturating_add(1))
                        .ok_or_else(|| anyhow::anyhow!("--socket needs a path"))?,
                );
                i = i.saturating_add(2);
            }
            "--host-root" => {
                host_root = PathBuf::from(
                    args.get(i.saturating_add(1))
                        .ok_or_else(|| anyhow::anyhow!("--host-root needs a path"))?,
                );
                i = i.saturating_add(2);
            }
            other => anyhow::bail!("unknown option `{other}`"),
        }
    }
    init_tracing(true);
    ush::supervisor::serve(&socket, &Store::discover(), &host_root)?;
    Ok(0)
}

/// `$XDG_RUNTIME_DIR/tilo/services/ush.sock` — the path `tilo-launcher` looks in
/// for a `[sandbox.services]` entry named `ush`.
fn default_service_socket() -> PathBuf {
    let runtime = xdg_paths::xdg::runtime_dir()
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
    runtime.join("tilo/services/ush.sock")
}

fn init_tracing(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .without_time()
        .try_init();
}

mod guest {
    use super::{Path, PathBuf, Request, Response};

    fn broker_socket() -> PathBuf {
        std::env::var_os("TILO_USH_BROKER")
            .map_or_else(|| PathBuf::from(ush::GUEST_BROKER_SOCK), PathBuf::from)
    }

    fn ask(req: &Request) -> anyhow::Result<Response> {
        use std::io::IsTerminal as _;

        let show_progress = std::io::stderr().is_terminal();
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        let mut response = ush::broker::ask_streaming_with_progress(
            &broker_socket(),
            req,
            &mut stdout,
            &mut stderr,
            show_progress,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "the broker is not reachable at {} ({e}). These commands only \
                 work inside a guest — run `ush` first.",
                broker_socket().display()
            )
        })?;
        // Command output was already copied live. Keep `broker::ask`'s fully
        // buffered response compatible for library callers, but do not replay
        // it in the guest CLI.
        if let Response::Command { stdout, stderr, .. } = &mut response {
            stdout.clear();
            stderr.clear();
        }
        Ok(response)
    }

    fn report(resp: Response) -> i32 {
        match resp {
            Response::Ok => 0,
            Response::NeedsRestart { detail } => {
                println!("{detail}");
                0
            }
            Response::Denied { reason } => {
                eprintln!("refused: {reason}");
                1
            }
            other => {
                println!("{other:?}");
                0
            }
        }
    }

    const APP_HELP: &str = "\
Manage Tilo apps through the signed-in host session.\n\n\
Usage:\n  app update [APP]       Update every installed app, or one app\n  app list [--json]      List apps available to install\n  app search QUERY       Search the Tilo Store\n  app repo list          Show configured repositories\n\n\
Aliases:\n  `store` is the same command; `u`, `upgrade`, `ls`, `l`, `s`, and `find` work too.\n\n\
Examples:\n  app u browser\n  store search notes\n  tilo app update\n\n\
Installing, removing, and changing repositories are not exposed from this sandbox.\n";

    const APP_REPO_HELP: &str = "\
Inspect the host's Tilo app repositories.\n\n\
Usage:\n  app repo list\n\n\
Repository changes are not exposed from this sandbox.\n";

    const SYSTEM_HELP: &str = "\
Manage the Tilo OS image through the signed-in host session.\n\n\
Usage:\n  system status [--json]  Show the booted and staged system images\n  system update [-y]      Fetch and stage the newest signed image\n\n\
`system` is present only when the host is Tilo OS. Updates ask for confirmation\n\
unless `--yes` or `-y` is supplied.\n";

    const TILO_HELP: &str = "\
Manage Tilo apps and the system beneath them through the signed-in host session.\n\n\
Usage:\n  tilo app ...             App discovery and updates (`store` is an alias)\n  tilo system ...          Tilo OS image status and updates\n  tilo status [--json]     Show system image status\n  tilo update [-y]         Update apps and every available system provider\n\n\
Examples:\n  tilo app u browser\n  tilo system status\n  tilo update\n\n\
Unavailable providers are skipped by `tilo update`; explicit provider commands\n\
explain when the host does not offer them.\n";

    pub fn host_cli(program: &str, args: &[String]) -> anyhow::Result<i32> {
        if let Some(help) = management_help(program, args) {
            print!("{help}");
            return Ok(0);
        }
        let request = management_request(program, args)?;
        if requires_confirmation(&request) && !has_yes(args) {
            use std::io::Write as _;
            eprint!("{} [y/N] ", confirmation_prompt(&request));
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
                eprintln!("cancelled");
                return Ok(1);
            }
        }
        match ask(&request)? {
            Response::Command {
                code,
                stdout,
                stderr,
            } => {
                use std::io::Write as _;
                std::io::stdout().write_all(stdout.as_bytes())?;
                std::io::stderr().write_all(stderr.as_bytes())?;
                Ok(code)
            }
            Response::Denied { reason } => anyhow::bail!("{reason}"),
            other => anyhow::bail!("the broker returned an unexpected response: {other:?}"),
        }
    }

    fn requires_confirmation(request: &Request) -> bool {
        matches!(request, Request::SystemUpdate | Request::AllUpdates)
    }

    fn confirmation_prompt(request: &Request) -> &'static str {
        match request {
            Request::AllUpdates => "Update installed apps and every available system provider?",
            Request::SystemUpdate => "Update the host system image?",
            _ => "Continue?",
        }
    }

    fn management_help(program: &str, args: &[String]) -> Option<&'static str> {
        let help_requested = args.is_empty()
            || args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--help" | "-h"));
        if !help_requested {
            return None;
        }
        match program {
            "app" | "store" => {
                if matches!(args.first().map(String::as_str), Some("repo" | "registry")) {
                    Some(APP_REPO_HELP)
                } else {
                    Some(APP_HELP)
                }
            }
            "system" => Some(SYSTEM_HELP),
            "tilo" => match args.first().map(String::as_str) {
                Some("app" | "store") => management_help("app", args.get(1..).unwrap_or(&[])),
                Some("system") => management_help("system", args.get(1..).unwrap_or(&[])),
                _ => Some(TILO_HELP),
            },
            _ => None,
        }
    }

    fn has_yes(args: &[String]) -> bool {
        args.iter()
            .any(|arg| matches!(arg.as_str(), "--yes" | "-y"))
    }

    fn optional_yes(args: &[String], command: &str) -> anyhow::Result<()> {
        match args {
            [] => Ok(()),
            [flag] if matches!(flag.as_str(), "--yes" | "-y") => Ok(()),
            _ => anyhow::bail!("{command} accepts only --yes (or -y)"),
        }
    }

    fn optional_json(args: &[String], command: &str) -> anyhow::Result<bool> {
        match args {
            [] => Ok(false),
            [flag] if flag == "--json" => Ok(true),
            _ => anyhow::bail!("{command} accepts only --json"),
        }
    }

    fn management_request(program: &str, args: &[String]) -> anyhow::Result<Request> {
        let (verb, tail): (&str, &[String]) = args
            .split_first()
            .map_or(("", &[]), |(verb, tail)| (verb.as_str(), tail));
        let package = |tail: &[String]| -> anyhow::Result<Option<String>> {
            anyhow::ensure!(tail.len() <= 1, "update accepts at most one app name");
            Ok(tail.first().cloned())
        };
        match (program, verb) {
            ("app" | "store", "update" | "u" | "upgrade") => Ok(Request::AppUpdate {
                package: package(tail)?,
            }),
            ("app" | "store", "list" | "ls" | "l") => Ok(Request::AppList {
                json: optional_json(tail, "app list")?,
            }),
            ("app" | "store", "search" | "s" | "find") => match tail {
                [query] => Ok(Request::AppSearch {
                    query: query.clone(),
                }),
                _ => anyhow::bail!(
                    "that host command is unavailable from ush; app discovery/update and system status/update are supported"
                ),
            },
            ("app" | "store", "repo") if matches!(tail, [command] if command == "list") => {
                Ok(Request::AppRepoList)
            }
            ("system", "update") => {
                optional_yes(tail, "system update")?;
                Ok(Request::SystemUpdate)
            }
            ("system", "status") => Ok(Request::SystemStatus {
                json: optional_json(tail, "system status")?,
            }),
            ("tilo", "update") => {
                optional_yes(tail, "tilo update")?;
                Ok(Request::AllUpdates)
            }
            ("tilo", "status") => Ok(Request::SystemStatus {
                json: optional_json(tail, "tilo status")?,
            }),
            ("tilo", "app" | "store") => management_request("app", tail),
            ("tilo", "system") => management_request("system", tail),
            _ => anyhow::bail!("{}", unavailable_management_command(program, args)),
        }
    }

    fn unavailable_management_command(program: &str, args: &[String]) -> String {
        let command = std::iter::once(program)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let suggestion = args.first().and_then(|verb| {
            let candidates: &[&str] = match program {
                "app" | "store" => &["update", "list", "search", "repo"],
                "system" => &["update", "status"],
                "tilo" => &["update", "status", "app", "store", "system"],
                _ => &[],
            };
            candidates
                .iter()
                .copied()
                .map(|candidate| (strsim::levenshtein(verb, candidate), candidate))
                .min_by_key(|(distance, _)| *distance)
                .filter(|(distance, _)| *distance <= 2)
                .map(|(_, candidate)| candidate)
        });
        let hint = suggestion.map_or_else(
            || format!("try `{program} --help`"),
            |suggestion| format!("did you mean `{program} {suggestion}`?"),
        );
        format!(
            "`{command}` is not available from this sandbox. App discovery and signed \
             app/system updates use typed host requests here; {hint}"
        )
    }

    /// The guest's first process: installs Landlock inside the namespace, where
    /// the rules name paths the guest will actually use, then becomes the shell.
    pub fn init(args: &[String]) -> anyhow::Result<i32> {
        use std::os::unix::process::CommandExt;
        let rest: Vec<String> = args
            .iter()
            .skip_while(|a| *a != "--")
            .skip(1)
            .cloned()
            .collect();
        anyhow::ensure!(!rest.is_empty(), "guest-init needs a command after `--`");

        // Everything the guest may write, in here: scratch, its own `/var`,
        // home, and whatever was granted (named again so a later remount can't
        // widen it). `/usr` and `/etc` are read-only images and get no rule.
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let mut rw: Vec<PathBuf> = ["/tmp", "/var", "/run", "/dev", "/proc"]
            .iter()
            .map(PathBuf::from)
            .collect();
        rw.push(home);
        // The root is readable, not writable — without a rule for it the ruleset
        // denies `stat("/")`, and `ls /` fails looking like a broken system.
        let mut ro: Vec<PathBuf> = vec![PathBuf::from("/")];
        if let Ok(Response::Status { status }) = ask(&Request::Status) {
            rw.extend(status.trusted_dirs.iter().map(PathBuf::from));
            ro.extend(status.readable_dirs.iter().map(PathBuf::from));
        } else {
            // No broker means no record to read, which means no grants — the
            // restriction is *tighter*, not skipped.
            tracing::debug!("no broker; restricting to the base set only");
        }
        let outcome = ush::guard::restrict_self(&rw, &ro);
        tracing::debug!("landlock: {}", ush::guard::describe(outcome));

        let cmd = rest
            .first()
            .ok_or_else(|| anyhow::anyhow!("guest-init needs a command after `--`"))?;
        Err(std::process::Command::new(cmd)
            .args(rest.get(1..).unwrap_or(&[]))
            .exec()
            .into())
    }

    pub fn pkg(args: &[String]) -> anyhow::Result<i32> {
        let rest = args.get(1..).unwrap_or(&[]);
        let names = || -> Vec<String> {
            rest.iter()
                .filter(|a| !a.starts_with('-'))
                .cloned()
                .collect()
        };
        match args.first().map(String::as_str) {
            Some("install") => {
                let packages = names();
                anyhow::ensure!(!packages.is_empty(), "pkg install needs a package name");
                Ok(report(ask(&Request::AddTool { packages })?))
            }
            Some("remove") => {
                let packages = names();
                anyhow::ensure!(!packages.is_empty(), "pkg remove needs a package name");
                Ok(report(ask(&Request::RemoveTool { packages })?))
            }
            Some("list") => pkg_list(),
            _ => {
                eprintln!("pkg install PKG… | remove PKG… | list");
                Ok(2)
            }
        }
    }

    fn pkg_list() -> anyhow::Result<i32> {
        let Response::Tools { runtime, tools } = ask(&Request::ListTools)? else {
            anyhow::bail!("the broker did not answer with the environment's tools");
        };
        println!("runtime  {runtime}");
        if tools.is_empty() {
            println!("no tools; `pkg install PKG` adds one.");
        }
        for tool in &tools {
            println!("tool     {:<40} {}", tool.packages.join(" "), tool.id);
        }
        Ok(0)
    }

    pub fn perm(args: &[String]) -> anyhow::Result<i32> {
        match args.first().map(String::as_str) {
            Some("trust-dir") => {
                let dir = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("perm trust-dir needs a directory"))?;
                Ok(report(ask(&Request::TrustDir {
                    path: PathBuf::from(dir),
                })?))
            }
            Some("revoke") => {
                let id = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("perm revoke needs a capability id"))?;
                Ok(report(ask(&Request::Revoke { id: id.clone() })?))
            }
            Some("list") | None => {
                let Response::Perms { entries } = ask(&Request::ListPerms)? else {
                    anyhow::bail!("the broker did not answer with a permission list");
                };
                if entries.is_empty() {
                    println!(
                        "nothing is granted. Your home directory in here is this \
                         environment's, not yours; `perm trust-dir DIR` is how a real \
                         directory appears."
                    );
                }
                for e in &entries {
                    println!("{:<24} {:<10} {:<10} {}", e.id, e.allows, e.expiry, e.scope);
                }
                Ok(0)
            }
            Some(other) => anyhow::bail!("unknown `perm {other}`; try trust-dir, list, revoke"),
        }
    }

    pub fn scan(args: &[String]) -> anyhow::Result<i32> {
        let path = args
            .first()
            .ok_or_else(|| anyhow::anyhow!("scan needs a path"))?;
        let trusted = match ask(&Request::Status) {
            Ok(Response::Status { status }) => {
                status.trusted_dirs.iter().map(PathBuf::from).collect()
            }
            _ => Vec::new(),
        };
        let image = match ask(&Request::Provenance {
            path: PathBuf::from(path),
        }) {
            Ok(Response::Provenance { image }) => image,
            _ => None,
        };
        let r = ush::guard::scan(Path::new(path), &trusted, image);
        println!("{}", r.path.display());
        if !r.exists {
            for n in &r.notes {
                println!("  {n}");
            }
            return Ok(1);
        }
        println!("  size       : {} bytes", r.bytes);
        println!(
            "  kind       : {}",
            if r.is_dir {
                "directory"
            } else if r.executable {
                "executable file"
            } else {
                "file"
            }
        );
        println!(
            "  image      : {}",
            r.image.as_deref().unwrap_or("none — not from an image")
        );
        for n in &r.notes {
            println!("  note       : {n}");
        }
        Ok(0)
    }

    pub fn guard(args: &[String]) -> anyhow::Result<i32> {
        match args.first().map(String::as_str) {
            Some("status") | None => {
                let Response::Status { status } = ask(&Request::Status)? else {
                    anyhow::bail!("the broker did not answer with a status");
                };
                println!("session     : {}", status.session);
                println!("environment : {} ({})", status.environment, status.record);
                println!("runtime     : {}", status.runtime);
                for tool in &status.tools {
                    println!("tool        : {tool}");
                }
                println!("seccomp     : {}", status.seccomp);
                println!("landlock    : {}", landlock_now());
                println!(
                    "trusted     : {}",
                    if status.trusted_dirs.is_empty() {
                        "nothing — your files are not in here".to_string()
                    } else {
                        status.trusted_dirs.join(", ")
                    }
                );
                if !status.readable_dirs.is_empty() {
                    println!("readable    : {}", status.readable_dirs.join(", "));
                }
                println!("audit       : {} records", status.audit_records);
                Ok(0)
            }
            Some(other) => anyhow::bail!("unknown `guard {other}`; try status"),
        }
    }

    /// Report Landlock by testing it, not repeating what was asked for.
    /// `guard status` answers "is this actually confined" — a line that said
    /// "installed" because the install code ran would pass exactly when it
    /// should fail, so the answer comes from an operation the ruleset forbids.
    fn landlock_now() -> String {
        let probed = ush::guard::describe(ush::guard::landlock_probe());
        let reported = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("Landlock:").map(|v| v.trim().to_string()))
            });
        match reported {
            Some(v) => format!("{probed} [/proc/self/status: {v}]"),
            None => format!("{probed} [this kernel does not report it in /proc/self/status]"),
        }
    }

    pub fn quarantine(args: &[String]) -> anyhow::Result<i32> {
        let dir = PathBuf::from(ush::guard::QUARANTINE_DIR);
        match args.first().map(String::as_str) {
            Some("add") => {
                let path = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("quarantine add needs a path"))?;
                let stored = ush::guard::quarantine_add(Path::new(path), &dir)?;
                println!("quarantined {path} → {} (not executable)", stored.display());
                Ok(0)
            }
            Some("restore") => {
                let path = args
                    .get(1)
                    .ok_or_else(|| anyhow::anyhow!("quarantine restore needs a path"))?;
                let wanted = PathBuf::from(path);
                let entry = ush::guard::quarantine_list(&dir)
                    .into_iter()
                    .find(|e| e.original == wanted)
                    .ok_or_else(|| anyhow::anyhow!("{path} is not in quarantine"))?;
                ush::guard::quarantine_restore(&entry)?;
                println!("restored {path}");
                Ok(0)
            }
            Some("list") | None => {
                let entries = ush::guard::quarantine_list(&dir);
                if entries.is_empty() {
                    println!(
                        "quarantine is empty. Nothing is put here automatically: \
                         this runtime ships no signature database, so the \
                         judgement is yours. `quarantine add PATH` moves a file \
                         here and clears its executable bits."
                    );
                    return Ok(0);
                }
                for e in &entries {
                    println!("{:>10}  {}", e.bytes, e.original.display());
                }
                Ok(0)
            }
            Some(other) => {
                anyhow::bail!("unknown `quarantine {other}`; try list, add, restore")
            }
        }
    }

    /// Tear the guest down so the supervisor can build it again. There is no
    /// way to remount a namespace from inside it, so a restart is literally a
    /// new guest: the broker records the request, this ends the current one,
    /// and the supervisor loops — signalling PID 1 (the sandbox's own init
    /// inside the guest's pid namespace) takes every process in it with it.
    pub fn restart() -> anyhow::Result<i32> {
        ask(&Request::Restart)?;
        println!("restarting the guest…");
        // SAFETY: plain `kill(2)`. Inside the guest's pid namespace pid 1 is
        // the sandbox's own init; outside one this fails with EPERM and the
        // fallback below applies.
        if unsafe { libc::kill(1, libc::SIGTERM) } == 0 {
            return Ok(0);
        }
        // SAFETY: `getppid` cannot fail; the parent is the shell that ran this.
        unsafe { libc::kill(libc::getppid(), libc::SIGHUP) };
        Ok(0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn argv(parts: &[&str]) -> Vec<String> {
            parts.iter().map(|part| (*part).to_string()).collect()
        }

        #[test]
        fn management_commands_parse_into_typed_requests() {
            // Aliases and read-only operations are typed.
            let cases = [
                (
                    "app",
                    argv(&["u", "browser"]),
                    Request::AppUpdate {
                        package: Some("browser".into()),
                    },
                ),
                (
                    "tilo",
                    argv(&["app", "u", "browser"]),
                    Request::AppUpdate {
                        package: Some("browser".into()),
                    },
                ),
                ("store", argv(&["repo", "list"]), Request::AppRepoList),
                (
                    "app",
                    argv(&["list", "--json"]),
                    Request::AppList { json: true },
                ),
                (
                    "app",
                    argv(&["search", "browser"]),
                    Request::AppSearch {
                        query: "browser".into(),
                    },
                ),
            ];
            for (program, args, want) in cases {
                assert_eq!(
                    management_request(program, &args).unwrap(),
                    want,
                    "case {program} {args:?}"
                );
            }

            // System updates accept both yes spellings and require confirmation otherwise.
            for flag in ["--yes", "-y"] {
                let request = management_request("system", &argv(&["update", flag])).unwrap();
                assert_eq!(request, Request::SystemUpdate, "case {flag}");
                assert!(requires_confirmation(&request), "case {flag}");
                assert!(has_yes(&argv(&["update", flag])), "case {flag}");
            }
            let request = management_request("tilo", &argv(&["update"])).unwrap();
            assert_eq!(request, Request::AllUpdates);
            assert!(requires_confirmation(&request));
            assert!(!has_yes(&argv(&["update"])));
        }

        #[test]
        fn help_and_confirmation_text_need_no_broker_round_trip() {
            for (program, args, want) in [
                ("tilo", argv(&[]), "tilo update"),
                ("tilo", argv(&["app", "--help"]), "app u browser"),
                ("store", argv(&["repo", "--help"]), "app repo list"),
                ("system", argv(&["update", "-h"]), "system update"),
            ] {
                assert!(
                    management_help(program, &args).unwrap().contains(want),
                    "case {program} {args:?}"
                );
            }
            assert!(management_help("app", &argv(&["u", "browser"])).is_none());

            // Confirmation names the state that will change.
            assert_eq!(
                confirmation_prompt(&Request::AllUpdates),
                "Update installed apps and every available system provider?"
            );
            assert_eq!(
                confirmation_prompt(&Request::SystemUpdate),
                "Update the host system image?"
            );
        }

        #[test]
        fn management_parser_rejects_extra_arguments_and_unsupported_mutations() {
            // phase: management_parser_rejects_ignored_extra_arguments
            {
                for (program, args) in [
                    ("app", argv(&["list", "--json", "extra"])),
                    ("system", argv(&["status", "--json", "extra"])),
                    ("system", argv(&["update", "--yes", "extra"])),
                    ("tilo", argv(&["update", "unexpected"])),
                    ("tilo", argv(&["status", "--yes"])),
                ] {
                    assert!(
                        management_request(program, &args).is_err(),
                        "{program} {args:?} must not silently discard arguments"
                    );
                }
            }
            // phase: unsupported_mutations_never_become_broker_requests
            {
                for args in [
                    argv(&["install", "browser"]),
                    argv(&["remove", "browser"]),
                    argv(&["rollback"]),
                    argv(&["boot", "old-image"]),
                ] {
                    assert!(management_request("tilo", &args).is_err());
                }

                let error = management_request("app", &argv(&["install", "browser"]))
                    .unwrap_err()
                    .to_string();
                assert!(error.contains("`app install browser`"));
                assert!(error.contains("app --help"));

                let typo = management_request("app", &argv(&["udpate", "browser"]))
                    .unwrap_err()
                    .to_string();
                assert!(typo.contains("did you mean `app update`?"), "{typo}");
            }
        }
    }
}
