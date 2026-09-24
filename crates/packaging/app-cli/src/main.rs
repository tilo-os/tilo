//! `app` — install, update, remove, and discover Tilo apps.

use std::io::Write;
use std::process::{Command, ExitCode};

use app_cli::{CatalogueApp, install::DownloadProgress};
use clap::{Parser, Subcommand};

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
    /// List apps available to install.
    #[command(alias = "ls", alias = "l")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Search the app store.
    #[command(alias = "s", alias = "find")]
    Search { query: String },
    /// Install an app by name, slug, or package id.
    #[command(alias = "i", alias = "add")]
    Install { app: String },
    /// Update one app, or every installed app when no name is given.
    #[command(alias = "u", alias = "upgrade")]
    Update { app: Option<String> },
    /// Remove an installed app while keeping its saved state.
    #[command(alias = "rm", alias = "uninstall")]
    Remove { app: String },
    /// Add, remove, or inspect app repositories.
    #[command(alias = "registry")]
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
    #[command(alias = "rm")]
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
    match Cli::parse().cmd {
        Cmd::List { json } => list(json),
        Cmd::Search { query } => search(&query),
        Cmd::Install { app } => change_one(&app, false),
        Cmd::Update { app } => update(app.as_deref()),
        Cmd::Remove { app } => remove(&app),
        Cmd::Repo { cmd } => repo(cmd),
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

fn list(json: bool) -> ExitCode {
    let listing = app_cli::available();
    if json {
        return match serde_json::to_string(&listing) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("app: could not encode the store listing: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if listing.apps.is_empty() {
        println!("nothing available to install");
    }
    print_apps(&listing.apps);
    report_unreachable(&listing.unreachable);
    ExitCode::SUCCESS
}

fn search(query: &str) -> ExitCode {
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
        eprintln!("app: no apps matched '{query}'");
    }
    ExitCode::SUCCESS
}

fn progress_reporter() -> impl FnMut(DownloadProgress) {
    let mut announced = false;
    move |progress| {
        match progress.total {
            Some(total) => println!("progress {} {}", progress.received, total),
            None => println!("progress {} -", progress.received),
        }
        if !announced && matches!(progress.total, Some(total) if total == progress.received) {
            announced = true;
            println!("phase unpacking");
        }
        let _ = std::io::stdout().flush();
    }
}

fn change_one(query: &str, update: bool) -> ExitCode {
    let listing = app_cli::all_offers();
    report_unreachable(&listing.unreachable);
    let app = match app_cli::resolve_offer(&listing.apps, query) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("app: {error}");
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
    let mut checked = 0_usize;
    for app in &listing.apps {
        let Some(version) = installed.get(&app.package_id) else {
            continue;
        };
        checked = checked.saturating_add(1);
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
            "no updates are available for {installed} installed app{}",
            if installed == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "checked {checked} installed app{}",
            if checked == 1 { "" } else { "s" }
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
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn concise_update_parses() {
        let cli = Cli::try_parse_from(["app", "u", "browser"]).expect("short update parses");
        assert!(matches!(cli.cmd, Cmd::Update { app: Some(ref app) } if app == "browser"));
    }

    #[test]
    fn bulk_update_always_reports_what_it_checked() {
        assert_eq!(bulk_update_summary(0, 0), "no apps are installed yet");
        assert_eq!(
            bulk_update_summary(1, 0),
            "no updates are available for 1 installed app"
        );
        assert_eq!(
            bulk_update_summary(3, 0),
            "no updates are available for 3 installed apps"
        );
        assert_eq!(bulk_update_summary(2, 2), "checked 2 installed apps");
    }

    #[test]
    fn bulk_update_fails_when_a_configured_repository_is_unreachable() {
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

    #[test]
    fn install_refuses_an_already_installed_offer() {
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
