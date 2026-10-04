//! `app` — install, update, remove, and discover Tilo apps.

use std::io::{IsTerminal, Write};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use app_cli::{
    CatalogueApp,
    install::{DownloadProgress, InstallPhase},
};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;

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
    /// List apps available to install.
    #[command(visible_aliases = ["ls", "l"])]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Search the app store.
    #[command(visible_aliases = ["s", "find"])]
    Search { query: String },
    /// Install an app by name, slug, or package id.
    #[command(visible_aliases = ["i", "add"])]
    Install {
        app: String,
        /// Install this version instead of the newest.
        #[arg(long)]
        version: Option<String>,
    },
    /// Update one app, or every installed app when no name is given.
    #[command(visible_aliases = ["u", "upgrade"])]
    Update { app: Option<String> },
    /// Go back to an older version of an installed app, keeping its saved state.
    Rollback {
        app: String,
        /// The version to go back to (default: the newest older one).
        #[arg(long)]
        to: Option<String>,
    },
    /// Remove an installed app while keeping its saved state.
    #[command(visible_aliases = ["rm", "uninstall"])]
    Remove { app: String },
    /// Add, remove, or inspect app repositories.
    #[command(visible_alias = "registry")]
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
}

#[derive(Debug, Subcommand)]
enum RepoCmd {
    List,
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
            match std::io::stdout().write_all(&generated) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("app: could not write completions: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Cmd::List { json } => list(json),
        Cmd::Search { query } => search(&query),
        Cmd::Install { app, version: None } => change_one(&app, false),
        Cmd::Install {
            app,
            version: Some(version),
        } => install_version(&app, &version),
        Cmd::Rollback { app, to } => rollback(&app, to.as_deref()),
        Cmd::Update { app } => update(app.as_deref()),
        Cmd::Remove { app } => remove(&app),
        Cmd::Repo { cmd } => repo(cmd),
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
    let matches = Cli::command().bin_name(bin_name).get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

fn print_apps(apps: &[CatalogueApp]) {
    for app in apps {
        println!(
            "{}  {}  {}  ({})",
            app.package_id, app.display_name, app.version, app.registry
        );
    }
}

fn list(json: bool) -> ExitCode {
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

fn announce(message: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("{message}");
    }
}

fn progress_reporter() -> impl FnMut(DownloadProgress) {
    let terminal = std::io::stdout().is_terminal();
    let started = Instant::now();
    let mut last_report: Option<Instant> = None;
    let mut last_percent: Option<u64> = None;
    let mut finished = false;
    let mut spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        .into_iter()
        .cycle();
    move |progress| {
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
            if tilo_app_package::version::compare(&app.version, installed).is_le() =>
        {
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

fn update(query: Option<&str>) -> ExitCode {
    if let Some(query) = query {
        return change_one(query, true);
    }
    announce("Checking installed apps for updates…");
    let listing = app_cli::all_offers();
    let installed = match app_cli::installed_versions() {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("app: {error}");
            return ExitCode::FAILURE;
        }
    };
    update_from_listing(&listing, &installed)
}

fn update_from_listing(
    listing: &app_cli::CatalogueListing,
    installed: &std::collections::HashMap<String, String>,
) -> ExitCode {
    report_unreachable(&listing.unreachable);
    let mut failed = !listing.unreachable.is_empty();
    let candidates = listing
        .apps
        .iter()
        .filter_map(|app| installed.get(&app.package_id).map(|version| (app, version)))
        .collect::<Vec<_>>();
    let checked = candidates.len();
    for (index, (app, version)) in candidates.into_iter().enumerate() {
        println!(
            "Checking {} ({}/{checked})…",
            app.display_name,
            index.saturating_add(1)
        );
        if change_app(app, true, Some(version)) != ExitCode::SUCCESS {
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        println!("{}", bulk_update_summary(installed.len(), checked));
        ExitCode::SUCCESS
    }
}

fn bulk_update_summary(installed: usize, checked: usize) -> String {
    if installed == 0 {
        "no apps are installed yet".into()
    } else if checked == 0 {
        format!(
            "could not find update metadata for {installed} installed app{}",
            if installed == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "checked {checked} of {installed} installed app{}",
            if installed == 1 { "" } else { "s" }
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
    match Command::new(app_cli::install::tilo_install_bin())
        .args(["uninstall", &id, "--keep-state"])
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
        ),
        Err(error) => {
            eprintln!("app: could not run tilo-install: {error}");
            ExitCode::FAILURE
        }
    }
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
        RepoCmd::List => {
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
        // Phase `concise_update_parses`.
        {
            let cli = Cli::try_parse_from(["app", "u", "browser"]).expect("short update parses");
            assert!(matches!(cli.cmd, Cmd::Update { app: Some(ref app) } if app == "browser"));
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
            assert_eq!(bulk_update_summary(0, 0), "no apps are installed yet");
            assert_eq!(
                bulk_update_summary(1, 0),
                "could not find update metadata for 1 installed app"
            );
            assert_eq!(
                bulk_update_summary(3, 0),
                "could not find update metadata for 3 installed apps"
            );
            assert_eq!(bulk_update_summary(2, 2), "checked 2 of 2 installed apps");
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
