//! `app` — install, update, remove, and discover Tilo apps.

use std::io::{IsTerminal, Write};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use app_cli::{
    CatalogueApp,
    install::{DownloadProgress, InstallPhase},
};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;

mod local_tapp;
mod store_verbs;

use local_tapp::FileTrust;

#[derive(Debug, Parser)]
#[command(
    name = "app",
    about = "Install, update, remove, and discover Tilo apps",
    long_about = "Install, update, remove, and discover apps from the signed Tilo Store. `store` is an exact alias when installed by Tilo OS.",
    after_help = "Examples:\n  app search browser\n  app install browser\n  app u browser       Update Browser (`u` is short for `update`)\n  app update          Update every installed app\n  app repo list       Show configured repositories"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Generate shell completions on standard output.
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// List installed apps, the system image's included, and the apps
    /// available to install.
    #[command(visible_aliases = ["ls", "l"])]
    List {
        /// Print the apps available to install as JSON.
        #[arg(long)]
        json: bool,
        /// Only the installed apps, and where each came from: `image`,
        /// `user` (installed or updated since), or `held` (rolled back).
        #[arg(long)]
        installed: bool,
    },
    /// Open an app, or files and links in it; an app this device doesn't
    /// have yet is downloaded first.
    #[command(
        after_help = "Examples:\n  app open calculator\n  app open notes ~/Documents/plan.md\n  app open browser https://tilo.so"
    )]
    Open {
        /// The app: its name, a word of its name, or its id.
        app: String,
        /// Files or links to open in it.
        items: Vec<String>,
    },
    /// Search the app store.
    #[command(visible_aliases = ["s", "find"])]
    Search { query: String },
    /// Install an app by name, slug or package id, or from a package file
    /// or link. A file from a publisher you don't trust asks first.
    #[command(
        visible_aliases = ["i", "add"],
        after_help = "Examples:\n  app install browser\n  app install ./notes-1.2.0.tapp\n  app install https://example.org/notes.tapp --yes"
    )]
    Install {
        /// The app, or a .tapp file or https:// link.
        app: String,
        /// Install this version instead of the newest.
        #[arg(long)]
        version: Option<String>,
        #[command(flatten)]
        trust: FileTrust,
        /// Report the download on standard error as `progress downloading
        /// <received> <total>` lines (`-` for an unknown total), then
        /// `progress installing`, for a program showing its own progress.
        #[arg(long)]
        progress_lines: bool,
    },
    /// Install, list and remove the runtimes apps run on (an app's engine)
    /// and the environment images environments are stacked from.
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
    /// Update one app, or every installed app when no name is given.
    #[command(visible_aliases = ["u", "upgrade"])]
    Update {
        app: Option<String>,
        /// Update the app from this package file or link instead of its
        /// repository.
        #[arg(long, requires = "app", value_name = "TAPP")]
        from: Option<String>,
        #[command(flatten)]
        trust: FileTrust,
        /// As `install --progress-lines`, for one app; `progress up-to-date`
        /// when the store has no newer build of it this system can run.
        #[arg(long, requires = "app")]
        progress_lines: bool,
    },
    /// Go back to an older version of an installed app, keeping its saved
    /// state. An app the system image ships goes back to the image's version
    /// when that is the next older one.
    Rollback {
        app: String,
        /// The version to go back to (default: the newest older one).
        #[arg(long)]
        to: Option<String>,
    },
    /// Remove an installed app while keeping its saved state.
    #[command(visible_aliases = ["rm", "uninstall"])]
    Remove { app: String },
    /// Keep an app installed when space runs short: a pinned app is never
    /// evicted to make room.
    Pin { app: String },
    /// Let an app be evicted to make room again.
    Unpin { app: String },
    /// Check installed apps' files against their signed manifests: every
    /// app, or the one named.
    Verify { app: Option<String> },
    /// Show an installed app's manifest and how it was installed.
    Inspect { app: String },
    /// List the apps running now, with their memory, CPU time and tier.
    Running {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Remove app versions and runtime builds nothing uses any more.
    Gc {
        /// List what would be removed, removing nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Choose whose packages install without asking, and refuse publishers.
    Trust {
        #[command(subcommand)]
        cmd: store_verbs::TrustCmd,
    },
    /// Add, remove, or inspect app repositories.
    #[command(visible_alias = "registry")]
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
}

#[derive(Debug, Subcommand)]
enum RuntimeCmd {
    /// Fetch an exact signed runtime through the same verified install an app's
    /// own runtime takes.
    Install {
        /// Runtime reference, for example tilo-runtime/2026.10.4-chromium.
        runtime: String,
        /// Existing enabled repository name; its pinned authority is unchanged.
        /// Without it, the first enabled repository that offers the build.
        #[arg(long)]
        repo: Option<String>,
    },
    #[command(flatten)]
    Store(store_verbs::RuntimeStoreCmd),
}

#[derive(Debug, Subcommand)]
enum RepoCmd {
    List {
        /// Print the repositories as JSON: the list the App Store reads
        /// through the app host.
        #[arg(long)]
        json: bool,
    },
    Add {
        name: String,
        url: String,
        #[arg(long)]
        owner_key: String,
    },
    #[command(visible_alias = "rm")]
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    Channel {
        name: String,
        #[arg(value_parser = ["stable", "development"])]
        channel: String,
    },
}

fn main() -> ExitCode {
    match parse_cli().cmd {
        Cmd::Completions { shell } => {
            let mut generated = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "app", &mut generated);
            clap_complete::generate(shell, &mut Cli::command(), "store", &mut generated);
            generated.extend_from_slice(open_completions(shell).as_bytes());
            match std::io::stdout().write_all(&generated) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("app: could not write completions: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Cmd::List {
            json,
            installed: true,
        } => list_installed(json),
        Cmd::List {
            json,
            installed: false,
        } => list(json),
        Cmd::Open { app, items } => open(&app, &items),
        Cmd::Search { query } => search(&query),
        Cmd::Install {
            app,
            version,
            trust,
            progress_lines,
        } => {
            PROGRESS_LINES.store(progress_lines, Ordering::Relaxed);
            install(&app, version.as_deref(), &trust)
        }
        Cmd::Rollback { app, to } => rollback(&app, to.as_deref()),
        Cmd::Update {
            app,
            from,
            trust,
            progress_lines,
        } => {
            PROGRESS_LINES.store(progress_lines, Ordering::Relaxed);
            match (app, from) {
                (Some(app), Some(from)) => update_from(&app, &from, &trust),
                (app, None) => match trust.file_only_flag() {
                    Some(flag) => {
                        eprintln!("app: {flag} goes with --from <file>");
                        ExitCode::FAILURE
                    }
                    None => update(app.as_deref()),
                },
                (None, Some(_)) => {
                    eprintln!("app: --from needs the app it updates");
                    ExitCode::FAILURE
                }
            }
        }
        Cmd::Remove { app } => remove(&app),
        Cmd::Pin { app } => store_verbs::on_installed("pin", &app),
        Cmd::Unpin { app } => store_verbs::on_installed("unpin", &app),
        Cmd::Verify { app } => store_verbs::verify(app.as_deref()),
        Cmd::Inspect { app } => store_verbs::on_installed("inspect", &app),
        Cmd::Running { json } => store_verbs::running(json),
        Cmd::Gc { dry_run } => store_verbs::gc(dry_run),
        Cmd::Trust { cmd } => store_verbs::trust(cmd),
        Cmd::Repo { cmd } => repo(cmd),
        Cmd::Runtime {
            cmd: RuntimeCmd::Install { runtime, repo },
        } => install_runtime(&runtime, repo.as_deref()),
        Cmd::Runtime {
            cmd: RuntimeCmd::Store(cmd),
        } => store_verbs::runtime(cmd),
    }
}

/// App names after `app open`, asked of the desktop as they are typed
/// (`tilo complete apps`, which prints nothing without a desktop).
fn open_completions(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => {
            r#"
_app_open_dynamic() {
    if (( COMP_CWORD == 2 )) && [[ "${COMP_WORDS[1]}" == open ]]; then
        mapfile -t COMPREPLY < <(tilo complete apps "${COMP_WORDS[COMP_CWORD]}" 2>/dev/null)
        return
    fi
    _app "$@"
}
complete -F _app_open_dynamic -o bashdefault -o default app store
"#
        }
        Shell::Zsh => {
            r#"
_app_open_dynamic() {
    if (( CURRENT == 3 )) && [[ "${words[2]}" == open ]]; then
        local -a names
        names=("${(@f)$(tilo complete apps "${words[CURRENT]}" 2>/dev/null)}")
        compadd -U -a names
        return
    fi
    _app "$@"
}
compdef _app_open_dynamic app store
"#
        }
        Shell::Fish => {
            "\ncomplete -c app -n '__fish_seen_subcommand_from open; and test (count (commandline -opc)) -eq 2' -f -a '(tilo complete apps (commandline -ct))'\n"
        }
        _ => "",
    }
}

fn parse_cli() -> Cli {
    let forwarded_name = std::env::var("TILO_APP_DISPLAY_NAME").ok();
    let invoked_as_store = std::env::args_os()
        .next()
        .as_deref()
        .and_then(|arg| std::path::Path::new(arg).file_stem())
        .is_some_and(|name| name == "store");
    let bin_name = match forwarded_name.as_deref() {
        Some("tilo app") => "tilo app",
        Some("tilo store") => "tilo store",
        _ if invoked_as_store => "store",
        _ => "app",
    };
    let mut command = Cli::command().bin_name(bin_name);
    if !tilo_control::client::desktop_present(&tilo_control::client::doors_from_env()) {
        // Opening needs a desktop session; where there is none it is not
        // offered, and typed anyway says so in one sentence.
        command = command.mut_subcommands(|sub| {
            if sub.get_name() == "open" {
                sub.hide(true)
            } else {
                sub
            }
        });
    }
    let matches = command.get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

/// `app open`: the same as `tilo open`.
fn open(app: &str, items: &[String]) -> ExitCode {
    let doors = tilo_control::client::doors_from_env();
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let code = tilo_control::front::open(
        &doors,
        app,
        items,
        &cwd,
        &tilo_control::front::Spelling::standalone(),
        &mut tilo_control::front::Io {
            out: &mut out,
            err: &mut err,
        },
    );
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code)
    }
}

fn print_apps(apps: &[CatalogueApp]) {
    for app in apps {
        println!(
            "{}  {}  {}  ({})",
            app.package_id, app.display_name, app.version, app.registry
        );
    }
}

fn list_installed(json: bool) -> ExitCode {
    let apps = match app_cli::installed_apps() {
        Ok(mut apps) => {
            let host = app_cli::host_apps(&apps);
            apps.extend(host);
            apps
        }
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        return match serde_json::to_string(&apps) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("app: could not encode the installed apps: {error}");
                ExitCode::FAILURE
            }
        };
    }
    print_installed(&apps);
    ExitCode::SUCCESS
}

fn print_installed(apps: &[app_cli::InstalledApp]) {
    for app in apps {
        if app.missing {
            println!(
                "{}  {}  ({}, file not found)",
                app.package_id, app.name, app.from
            );
        } else if app.version.is_empty() {
            println!("{}  {}  ({})", app.package_id, app.name, app.from);
        } else {
            println!("{}  {}  ({})", app.package_id, app.version, app.from);
        }
    }
}

fn list(json: bool) -> ExitCode {
    if !json {
        match app_cli::installed_apps() {
            Ok(mut apps) => {
                let host = app_cli::host_apps(&apps);
                apps.extend(host);
                println!("Installed:");
                print_installed(&apps);
                println!("Available:");
            }
            Err(error) => {
                eprintln!("app: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    announce("Loading the Tilo Store…");
    let listing = app_cli::available();
    if json {
        return match serde_json::to_string(&listing) {
            Ok(value) => {
                println!("{value}");
                if listing.unreachable.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(error) => {
                eprintln!("app: could not encode the store listing: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if listing.apps.is_empty() && listing.unreachable.is_empty() {
        println!("nothing available to install");
    }
    print_apps(&listing.apps);
    report_unreachable(&listing.unreachable);
    if listing.unreachable.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("app: the listing is incomplete; try again when the repositories are reachable");
        ExitCode::FAILURE
    }
}

fn search(query: &str) -> ExitCode {
    announce(&format!("Searching the Tilo Store for '{query}'…"));
    let listing = app_cli::all_offers();
    let needle = query.to_ascii_lowercase();
    let matches = listing
        .apps
        .into_iter()
        .filter(|app| {
            app.package_id.to_ascii_lowercase().contains(&needle)
                || app.display_name.to_ascii_lowercase().contains(&needle)
                || app.description.to_ascii_lowercase().contains(&needle)
        })
        .collect::<Vec<_>>();
    print_apps(&matches);
    report_unreachable(&listing.unreachable);
    if matches.is_empty() {
        if listing.unreachable.is_empty() {
            eprintln!("app: no apps matched '{query}'");
        } else {
            eprintln!(
                "app: no reachable repository matched '{query}', but the search is incomplete"
            );
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Say what is about to take a while. On stderr, so `--json` output stays
/// clean, and whether or not a terminal is watching: the Terminal's shell
/// forwards host commands through pipes, and silence there reads as a hang.
fn announce(message: &str) {
    eprintln!("{message}");
}

/// Set by `install --progress-lines` and `update --progress-lines`.
static PROGRESS_LINES: AtomicBool = AtomicBool::new(false);

/// What `update --progress-lines` says when there is no newer build to take.
const UP_TO_DATE_LINE: &str = "progress up-to-date";

/// How much of a download of unknown size arrives between two lines.
const PROGRESS_LINE_STEP_BYTES: u64 = 256 * 1024;

/// What `--progress-lines` prints for `progress`, if anything: a line per
/// percent of a known size (or per [`PROGRESS_LINE_STEP_BYTES`] of an
/// unknown one), the last byte, and the switch to installing once.
/// `last` is the received count of the previous line (`u64::MAX` after
/// `installing`), `None` before any.
fn progress_line(progress: DownloadProgress, last: &mut Option<u64>) -> Option<String> {
    if let InstallPhase::Installing { .. } = progress.phase {
        return (last.replace(u64::MAX) != Some(u64::MAX)).then(|| "progress installing".into());
    }
    let step = progress
        .total
        .map_or(PROGRESS_LINE_STEP_BYTES, |total| (total / 100).max(1));
    // A second download (a runtime, then the app) starts again from zero.
    let due = match *last {
        None | Some(u64::MAX) => true,
        Some(before) => {
            progress.received < before
                || progress.received >= before.saturating_add(step)
                || progress.total == Some(progress.received) && before != progress.received
        }
    };
    if !due {
        return None;
    }
    *last = Some(progress.received);
    let total = progress
        .total
        .map_or_else(|| "-".to_owned(), |total| total.to_string());
    Some(format!(
        "progress downloading {} {total}",
        progress.received
    ))
}

fn progress_reporter() -> impl FnMut(DownloadProgress) {
    let lines = PROGRESS_LINES.load(Ordering::Relaxed);
    let mut last_line: Option<u64> = None;
    let terminal = std::io::stdout().is_terminal();
    let started = Instant::now();
    let mut last_report: Option<Instant> = None;
    let mut last_percent: Option<u64> = None;
    let mut finished = false;
    let mut spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        .into_iter()
        .cycle();
    move |progress| {
        if lines {
            if let Some(line) = progress_line(progress, &mut last_line) {
                eprintln!("{line}");
            }
            return;
        }
        if let InstallPhase::Installing { elapsed } = progress.phase {
            let should_report = terminal
                || last_report.is_none()
                || last_report.is_some_and(|last| last.elapsed() >= Duration::from_secs(10));
            if should_report {
                if terminal {
                    let frame = spinner.next().unwrap_or("•");
                    print!(
                        "\r\x1b[2K{} Verifying and installing… ({})",
                        frame,
                        human_duration(elapsed)
                    );
                } else {
                    println!(
                        "Still verifying and installing… ({})",
                        human_duration(elapsed)
                    );
                }
                last_report = Some(Instant::now());
                let _ = std::io::stdout().flush();
            }
            return;
        }
        let complete =
            matches!(progress.total, Some(total) if total > 0 && total == progress.received);
        let percent = progress.total.filter(|total| *total > 0).map(|total| {
            progress
                .received
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(0)
        });
        let should_report = terminal
            || last_report.is_none()
            || complete
            || percent
                .zip(last_percent)
                .is_some_and(|(now, last)| now >= last.saturating_add(10))
            || last_report.is_some_and(|last| last.elapsed() >= Duration::from_secs(10));
        if !should_report {
            return;
        }

        let elapsed = started.elapsed();
        let message = match (percent, progress.total) {
            (Some(percent), Some(total)) => format!(
                "Downloading… {percent:>3}% ({}/{}, {})",
                human_bytes(progress.received),
                human_bytes(total),
                human_duration(elapsed)
            ),
            _ => format!(
                "Downloading… {} ({})",
                human_bytes(progress.received),
                human_duration(elapsed)
            ),
        };
        if terminal {
            print!("\r\x1b[2K{message}");
        } else {
            println!("{message}");
        }
        if complete && !finished {
            if terminal {
                println!();
            }
            println!("Verifying and installing…");
            finished = true;
        }
        last_percent = percent;
        last_report = Some(Instant::now());
        let _ = std::io::stdout().flush();
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut whole = bytes;
    let mut remainder = 0;
    let mut unit: usize = 0;
    while whole >= 1024 && unit < 3 {
        remainder = whole % 1024;
        whole /= 1024;
        unit = unit.saturating_add(1);
    }
    let unit_name = UNITS.get(unit).copied().unwrap_or("GiB");
    if unit == 0 {
        format!("{bytes} {unit_name}")
    } else {
        let decimal = remainder.saturating_mul(10) / 1024;
        format!("{whole}.{decimal} {unit_name}")
    }
}

fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn install_runtime(runtime: &str, registry: Option<&str>) -> ExitCode {
    let mut progress = progress_reporter();
    let installed = match registry {
        Some(registry) => {
            app_cli::install::install_runtime_reporting(runtime, registry, &mut progress).map(
                |offer| {
                    format!(
                        "installed runtime {runtime} {} {}",
                        offer.package_id, offer.version
                    )
                },
            )
        }
        None => app_cli::install::install_runtime_from_enabled(runtime, &mut progress)
            .map(|()| format!("runtime {runtime} is installed")),
    };
    match installed {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("app: {}", error.user_message());
            ExitCode::FAILURE
        }
    }
}

fn change_one(query: &str, update: bool) -> ExitCode {
    announce(if update {
        "Checking the Tilo Store for an update…"
    } else {
        "Finding the app in the Tilo Store…"
    });
    let listing = app_cli::all_offers();
    report_unreachable(&listing.unreachable);
    let app = match app_cli::resolve_offer(&listing.apps, query) {
        Ok(app) => app,
        // Every repository answered and none offers a build of it this
        // system can run: nothing to update to, which is not a failure.
        Err(_)
            if update
                && listing.unreachable.is_empty()
                && PROGRESS_LINES.load(Ordering::Relaxed)
                && matches!(app_cli::installed_version(query), Ok(Some(_))) =>
        {
            eprintln!("{UP_TO_DATE_LINE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("app: {error}");
            if !listing.unreachable.is_empty() {
                eprintln!(
                    "app: that result is inconclusive because {} {} unreachable",
                    listing.unreachable.len(),
                    if listing.unreachable.len() == 1 {
                        "repository was"
                    } else {
                        "repositories were"
                    }
                );
            }
            return ExitCode::FAILURE;
        }
    };
    let installed = match app_cli::installed_version(&app.package_id) {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Opening an app for the first time takes the newest version this device
    // can run without fetching a runtime; `app update` moves it on later.
    let first_open = (!update && installed.is_none()).then(|| app_cli::first_open_offer(app));
    let app = first_open.as_ref().unwrap_or(app);
    let result = change_app(app, update, installed.as_deref());
    if update && !listing.unreachable.is_empty() {
        ExitCode::FAILURE
    } else {
        result
    }
}

fn change_app(app: &CatalogueApp, update: bool, installed: Option<&str>) -> ExitCode {
    match (update, installed) {
        (true, None) => {
            eprintln!("app: '{}' is not installed", app.display_name);
            return ExitCode::FAILURE;
        }
        (true, Some(installed))
            if tilo_app_package::version::compare(&app.version, installed).is_le()
                && app_cli::installed_current(&app.package_id) =>
        {
            if PROGRESS_LINES.load(Ordering::Relaxed) {
                eprintln!("{UP_TO_DATE_LINE}");
            }
            println!("up to date {} {}", app.package_id, installed);
            return ExitCode::SUCCESS;
        }
        (false, Some(_)) => {
            eprintln!(
                "app: '{}' is already installed; use 'app update {}'",
                app.display_name, app.package_id
            );
            return ExitCode::FAILURE;
        }
        _ => {}
    }
    let mut progress = progress_reporter();
    let result = if update {
        app_cli::install::update_app_reporting(app, &mut progress)
    } else {
        app_cli::install::install_app_reporting(app, &mut progress)
    };
    report_change(app, update, result)
}

/// Say how installing or updating `app` ended.
fn report_change(
    app: &CatalogueApp,
    update: bool,
    result: Result<(), app_cli::install::InstallError>,
) -> ExitCode {
    if std::io::stdout().is_terminal() {
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();
    }
    match result {
        Ok(()) => {
            println!(
                "{} {} {}",
                if update { "updated" } else { "installed" },
                app.package_id,
                app.version
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("app: {}", error.user_message());
            ExitCode::FAILURE
        }
    }
}

/// The offer for `query` and every version its registry has for this machine.
fn offered_versions(query: &str) -> Result<(CatalogueApp, Vec<CatalogueApp>), String> {
    let listing = app_cli::all_offers();
    report_unreachable(&listing.unreachable);
    let app = app_cli::resolve_offer(&listing.apps, query)?.clone();
    let versions = app_cli::version_offers(&app.package_id, &app.registry)?;
    Ok((app, versions))
}

fn install_version(query: &str, version: &str) -> ExitCode {
    announce("Finding the app in the Tilo Store…");
    let (app, versions) = match offered_versions(query) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(offer) = versions.iter().find(|offer| offer.version == version) else {
        eprintln!(
            "app: '{}' has no version {version} for this machine",
            app.display_name
        );
        return ExitCode::FAILURE;
    };
    match app_cli::installed_version(&app.package_id) {
        Ok(None) => change_app(offer, false, None),
        Ok(Some(installed))
            if !tilo_app_package::version::compare(&offer.version, &installed).is_lt() =>
        {
            change_app(offer, true, Some(&installed))
        }
        Ok(Some(installed)) => {
            eprintln!(
                "app: '{}' {installed} is already installed; use 'app rollback {} --to {version}' or 'app update {}'",
                app.display_name, app.package_id, app.package_id
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("app: {error}");
            ExitCode::FAILURE
        }
    }
}

fn rollback(query: &str, to: Option<&str>) -> ExitCode {
    // An app the system image ships can go back to the image's version
    // without the store, by removing the user's copy above it.
    if let Ok(id) = resolve_installed(query)
        && let Some(code) = rollback_to_image(&id, to)
    {
        return code;
    }
    announce("Finding older versions in the Tilo Store…");
    let (app, versions) = match offered_versions(query) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    let installed = match app_cli::installed_version(&app.package_id) {
        Ok(Some(installed)) => installed,
        Ok(None) => {
            eprintln!("app: '{}' is not installed", app.display_name);
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Newest first, so the first older one is the previous release.
    let older = |offer: &&CatalogueApp| {
        tilo_app_package::version::compare(&offer.version, &installed).is_lt()
    };
    let target = match to {
        Some(version) => versions
            .iter()
            .filter(older)
            .find(|offer| offer.version == version),
        None => versions.iter().find(older),
    };
    let Some(target) = target else {
        eprintln!(
            "app: '{}' has no {} older than {installed} for this machine",
            app.display_name,
            to.map_or_else(
                || "version".to_owned(),
                |version| format!("version {version}")
            )
        );
        return ExitCode::FAILURE;
    };
    let mut progress = progress_reporter();
    let result = app_cli::install::rollback_app_reporting(target, &mut progress);
    if std::io::stdout().is_terminal() {
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();
    }
    match result {
        Ok(()) => {
            println!(
                "rolled back {} {installed} -> {}",
                target.package_id, target.version
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("app: {}", error.user_message());
            ExitCode::FAILURE
        }
    }
}

/// Go back to the system image's version of `id` when that is the version
/// asked for, or the next older one when none is named; `None` when the
/// store decides instead.
fn rollback_to_image(id: &str, to: Option<&str>) -> Option<ExitCode> {
    let image = app_cli::image_version_beneath(id).ok()??;
    let installed = app_cli::installed_version(id).ok()??;
    if !tilo_app_package::version::compare(&image, &installed).is_lt() {
        return None;
    }
    match to {
        Some(version) if version != image => return None,
        Some(_) => {}
        None => {
            // Prefer the image when no published version lies between it and
            // the installed one; an unreachable store leaves the image.
            let newer_in_store = offered_versions(id).ok().is_some_and(|(_, versions)| {
                versions.iter().any(|offer| {
                    tilo_app_package::version::compare(&offer.version, &installed).is_lt()
                        && tilo_app_package::version::compare(&offer.version, &image).is_gt()
                })
            });
            if newer_in_store {
                return None;
            }
        }
    }
    let status = Command::new(app_cli::install::tilo_install_bin())
        .args(["uninstall", id, "--keep-state"])
        .stdout(std::process::Stdio::null())
        .status();
    Some(match status {
        Ok(status) if status.success() => {
            println!("rolled back {id} {installed} -> {image} (the system image's)");
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("app: could not remove the updated copy of '{id}'");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("app: could not run tilo-install: {error}");
            ExitCode::FAILURE
        }
    })
}

fn resolve_installed(query: &str) -> Result<String, String> {
    let ids = app_cli::installed_ids();
    if let Some(id) = ids.iter().find(|id| id.eq_ignore_ascii_case(query)) {
        return Ok(id.clone());
    }
    let matches = ids
        .into_iter()
        .filter(|id| {
            id.rsplit('.')
                .next()
                .is_some_and(|slug| slug.eq_ignore_ascii_case(query))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(format!("'{query}' is not installed")),
        _ => Err(format!("'{query}' is ambiguous; use the full package id")),
    }
}

/// `app install`: from a package file or link, or from the repositories.
fn install(app: &str, version: Option<&str>, trust: &FileTrust) -> ExitCode {
    if local_tapp::is_package_source(app) {
        if version.is_some() {
            eprintln!("app: --version picks from a repository; a package file is its own version");
            return ExitCode::FAILURE;
        }
        let mut progress = progress_reporter();
        return local_tapp::apply(app, &local_tapp::Change::Install, trust, &mut progress);
    }
    if let Some(flag) = trust.file_only_flag() {
        eprintln!("app: {flag} is for a package file or link; '{app}' comes from a repository");
        return ExitCode::FAILURE;
    }
    match version {
        None => change_one(app, false),
        Some(version) => install_version(app, version),
    }
}

/// `app update <app> --from <file>`.
fn update_from(query: &str, from: &str, trust: &FileTrust) -> ExitCode {
    let id = match resolve_installed(query) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut progress = progress_reporter();
    local_tapp::apply(from, &local_tapp::Change::Update(&id), trust, &mut progress)
}

fn update(query: Option<&str>) -> ExitCode {
    if let Some(query) = query {
        return change_one(query, true);
    }
    // Updating everything can take minutes; say what is happening from the
    // first moment, whether or not a terminal is watching.
    let installed = match app_cli::installed_versions() {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    let repositories = app_cli::config::load_registries()
        .enabled()
        .map(|registry| registry.name.clone())
        .collect::<Vec<_>>();
    println!(
        "Checking {} installed app{} for updates in {}…",
        installed.len(),
        if installed.len() == 1 { "" } else { "s" },
        if repositories.is_empty() {
            "no repositories".to_owned()
        } else {
            repositories.join(", ")
        }
    );
    let _ = std::io::stdout().flush();
    let listing = app_cli::all_offers();
    update_from_listing(&listing, &installed)
}

/// The offers newer than what is installed, with the installed version.
fn outdated<'a>(
    listing: &'a app_cli::CatalogueListing,
    installed: &'a std::collections::HashMap<String, String>,
) -> (Vec<(&'a CatalogueApp, &'a str)>, usize) {
    let mut checked = 0usize;
    let mut outdated = Vec::new();
    for app in &listing.apps {
        let Some(version) = installed.get(&app.package_id) else {
            continue;
        };
        checked = checked.saturating_add(1);
        if tilo_app_package::version::compare(&app.version, version).is_gt()
            || !app_cli::installed_current(&app.package_id)
        {
            outdated.push((app, version.as_str()));
        }
    }
    (outdated, checked)
}

fn update_from_listing(
    listing: &app_cli::CatalogueListing,
    installed: &std::collections::HashMap<String, String>,
) -> ExitCode {
    report_unreachable(&listing.unreachable);
    let mut failed = !listing.unreachable.is_empty();
    let (outdated, checked) = outdated(listing, installed);
    if outdated.is_empty() {
        println!("{}", bulk_update_summary(installed.len(), checked, 0));
        return if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    println!(
        "{} update{} available: {}",
        outdated.len(),
        if outdated.len() == 1 { "" } else { "s" },
        outdated
            .iter()
            .map(|(app, _)| app.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let total = outdated.len();
    let mut updated = 0usize;
    let mut done = 0usize;
    // Every update downloads at once; each installs as soon as it is ready.
    let apps: Vec<&CatalogueApp> = outdated.iter().map(|(app, _)| *app).collect();
    app_cli::install::stage_each(&apps, |index, staged| {
        let Some((app, version)) = outdated.get(index) else {
            return;
        };
        done = done.saturating_add(1);
        println!(
            "Updating {} {version} -> {} ({done}/{total})…",
            app.display_name, app.version,
        );
        let _ = std::io::stdout().flush();
        let mut progress = progress_reporter();
        let result = staged.and_then(|staged| {
            app_cli::install::update_staged_reporting(app, staged, &mut progress)
        });
        if report_change(app, true, result) == ExitCode::SUCCESS {
            updated = updated.saturating_add(1);
        } else {
            failed = true;
        }
    });
    println!("{}", bulk_update_summary(installed.len(), checked, updated));
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn bulk_update_summary(installed: usize, checked: usize, updated: usize) -> String {
    let apps = |n: usize| format!("{n} installed app{}", if n == 1 { "" } else { "s" });
    if installed == 0 {
        "no apps are installed yet".into()
    } else if checked == 0 {
        format!("could not find update metadata for {}", apps(installed))
    } else if updated == 0 {
        format!("checked {checked} of {}; all up to date", apps(installed))
    } else {
        format!(
            "checked {checked} of {}; updated {updated}",
            apps(installed)
        )
    }
}

fn remove(query: &str) -> ExitCode {
    let id = match resolve_installed(query) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    store_verbs::installer(["uninstall", &id, "--keep-state"])
}

fn report_unreachable(items: &[app_cli::UnreachableRegistry]) {
    for item in items {
        eprintln!("app: repository {} unreachable: {}", item.name, item.detail);
    }
}

fn repo(cmd: RepoCmd) -> ExitCode {
    use app_cli::config;
    let mut registries = config::load_registries();
    let confirmation = match cmd {
        RepoCmd::List { json: true } => {
            return match serde_json::to_string(&registries) {
                Ok(value) => {
                    println!("{value}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("app: could not encode the repositories: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        RepoCmd::List { json: false } => {
            for registry in registries.registries {
                println!(
                    "{}  {}  ({}, {}, key pinned)",
                    registry.name,
                    registry.base_url,
                    if registry.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    match registry.channel {
                        tilo_app_package::version::ReleaseChannel::Stable => "stable",
                        tilo_app_package::version::ReleaseChannel::Development => "development",
                    },
                );
            }
            return ExitCode::SUCCESS;
        }
        RepoCmd::Add {
            name,
            url,
            owner_key,
        } => match config::add_registry(&mut registries, &name, &url, &owner_key) {
            Ok(()) => format!("added repository '{name}'"),
            Err(error) => {
                eprintln!("app: {error}");
                return ExitCode::FAILURE;
            }
        },
        RepoCmd::Remove { name } => match config::remove_registry(&mut registries, &name) {
            Ok(()) => format!("removed repository '{name}'"),
            Err(error) => {
                eprintln!("app: {error}");
                return ExitCode::FAILURE;
            }
        },
        RepoCmd::Enable { name } => {
            match config::set_registry_enabled(&mut registries, &name, true) {
                Ok(()) => format!("enabled repository '{name}'"),
                Err(error) => {
                    eprintln!("app: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        RepoCmd::Disable { name } => {
            match config::set_registry_enabled(&mut registries, &name, false) {
                Ok(()) => format!("disabled repository '{name}'"),
                Err(error) => {
                    eprintln!("app: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        RepoCmd::Channel { name, channel } => {
            let Some(registry) = registries
                .registries
                .iter_mut()
                .find(|registry| registry.name == name)
            else {
                eprintln!("app: repository '{name}' is not configured");
                return ExitCode::FAILURE;
            };
            registry.channel = if channel == "development" {
                tilo_app_package::version::ReleaseChannel::Development
            } else {
                tilo_app_package::version::ReleaseChannel::Stable
            };
            format!("selected {channel} releases for '{name}'")
        }
    };
    match config::save_registries(&registries) {
        Ok(()) => {
            println!("{confirmation}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("app: could not save repositories: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn runtime_install_takes_an_optional_configured_repository() {
        let cli = Cli::try_parse_from([
            "app",
            "runtime",
            "install",
            "tilo-runtime/2026.10.4-chromium",
            "--repo",
            "Tilo main",
        ])
        .unwrap();
        assert!(
            matches!(cli.cmd, Cmd::Runtime { cmd: RuntimeCmd::Install { runtime, repo } }
            if runtime == "tilo-runtime/2026.10.4-chromium"
                && repo.as_deref() == Some("Tilo main"))
        );
        let cli = Cli::try_parse_from([
            "app",
            "runtime",
            "install",
            "tilo-runtime/2026.10.4-chromium",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Runtime {
                cmd: RuntimeCmd::Install { repo: None, .. }
            }
        ));
    }

    #[test]
    fn progress_lines_follow_the_bytes_without_flooding() {
        let downloading = |received, total| DownloadProgress {
            received,
            total,
            phase: InstallPhase::Downloading,
        };
        let mut last = None;
        let mut lines = Vec::new();
        for received in (0..=1_000_000).step_by(1000) {
            lines.extend(progress_line(
                downloading(received, Some(1_000_000)),
                &mut last,
            ));
        }
        assert_eq!(lines.first().unwrap(), "progress downloading 0 1000000");
        assert_eq!(
            lines.last().unwrap(),
            "progress downloading 1000000 1000000"
        );
        assert_eq!(lines.len(), 101, "a line per percent: {lines:?}");
        let installing = DownloadProgress {
            received: 1_000_000,
            total: Some(1_000_000),
            phase: InstallPhase::Installing {
                elapsed: Duration::from_secs(1),
            },
        };
        assert_eq!(
            progress_line(installing, &mut last).as_deref(),
            Some("progress installing")
        );
        assert_eq!(progress_line(installing, &mut last), None, "said once");
        assert_eq!(
            progress_line(downloading(0, None), &mut last).as_deref(),
            Some("progress downloading 0 -"),
            "a later download starts again"
        );
    }

    #[test]
    fn the_cli_surface_parses_and_names_both_public_commands() {
        // Phase `command_tree_is_valid`.
        {
            Cli::command().debug_assert();
        }
        // Phase `store_alias_help_names_the_command_the_user_typed`.
        {
            let help = Cli::command().bin_name("store").render_help().to_string();
            assert!(help.contains("Usage: store"), "{help}");
            let help = Cli::command()
                .bin_name("tilo app")
                .render_help()
                .to_string();
            assert!(help.contains("Usage: tilo app"), "{help}");
        }
        // Phase `completion_scripts_cover_both_public_names`.
        {
            let mut output = Vec::new();
            clap_complete::generate(Shell::Bash, &mut Cli::command(), "app", &mut output);
            clap_complete::generate(Shell::Bash, &mut Cli::command(), "store", &mut output);
            let script = String::from_utf8(output).expect("completion is UTF-8");
            assert!(script.contains("_app()"), "{script}");
            assert!(script.contains("default app"), "{script}");
            assert!(script.contains("_store()"), "{script}");
            assert!(script.contains("default store"), "{script}");
        }
        // Phase `runtime_install_names_the_exact_runtime`.
        {
            let cli = Cli::try_parse_from([
                "app",
                "runtime",
                "install",
                "tilo-runtime/2026.10.9-chromium",
            ])
            .expect("runtime install parses");
            assert!(matches!(
                cli.cmd,
                Cmd::Runtime { cmd: RuntimeCmd::Install { ref runtime, repo: None } }
                    if runtime == "tilo-runtime/2026.10.9-chromium"
            ));
        }
        // Phase `concise_update_parses`.
        {
            let cli = Cli::try_parse_from(["app", "u", "browser"]).expect("short update parses");
            assert!(matches!(cli.cmd, Cmd::Update { app: Some(ref app), .. } if app == "browser"));
        }
    }

    #[test]
    fn updates_and_installs_report_what_they_checked_or_refused() {
        // Phase `progress_units_are_readable`.
        {
            assert_eq!(human_bytes(999), "999 B");
            assert_eq!(human_bytes(1536), "1.5 KiB");
            assert_eq!(human_duration(Duration::from_secs(65)), "1m 05s");
        }
        // Phase `bulk_update_always_reports_what_it_checked`.
        {
            assert_eq!(bulk_update_summary(0, 0, 0), "no apps are installed yet");
            assert_eq!(
                bulk_update_summary(1, 0, 0),
                "could not find update metadata for 1 installed app"
            );
            assert_eq!(
                bulk_update_summary(3, 0, 0),
                "could not find update metadata for 3 installed apps"
            );
            assert_eq!(
                bulk_update_summary(2, 2, 0),
                "checked 2 of 2 installed apps; all up to date"
            );
            assert_eq!(
                bulk_update_summary(3, 2, 1),
                "checked 2 of 3 installed apps; updated 1"
            );
        }
        // Phase `bulk_update_fails_when_a_configured_repository_is_unreachable`.
        {
            let listing = app_cli::CatalogueListing {
                apps: Vec::new(),
                unreachable: vec![app_cli::UnreachableRegistry {
                    name: "main".into(),
                    detail: "connection refused".into(),
                }],
            };
            assert_eq!(
                update_from_listing(&listing, &std::collections::HashMap::new()),
                ExitCode::FAILURE
            );
        }
        // Phase `install_refuses_an_already_installed_offer`.
        {
            let offer = CatalogueApp {
                package_id: "so.tilo.browser".into(),
                display_name: "Browser".into(),
                description: String::new(),
                tagline: String::new(),
                images: Vec::new(),
                supported_languages: vec!["en".into()],
                category: None,
                version: "1.0.0".into(),
                registry: "main".into(),
                download_url: "https://apps.tilo.so/browser.tapp".into(),
                tapp_root_hash_hex: "ab".repeat(32),
                tapp_size_bytes: 1,
                tapp_blake3_hex: "cd".repeat(32),
                architecture: std::env::consts::ARCH.into(),
                isa: std::env::consts::ARCH.into(),
                runtime: None,
                publisher_pubkey_hex: "ef".repeat(32),
                trust: "verified".into(),
            };
            assert_eq!(change_app(&offer, false, Some("2.0.0")), ExitCode::FAILURE);
        }
    }
}
