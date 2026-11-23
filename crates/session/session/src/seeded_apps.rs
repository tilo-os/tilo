//! The tilolang apps a development session starts with (`tilo-session --seed-apps`, honoured by
//! development builds only): notes, tasks, chat, mail and the video app.
//!
//! Each is an **installed package**, as it is on a person's device: the app's `.tapp` is
//! installed into the session's own package store (`tilolang run` builds and installs them), the
//! session reads each app's payload from its store row, and the app runs through the launcher
//! like any other. Loading it here publishes the record it keeps: the app's root made and filled
//! by its first scenario (the seed data its shots and tests start from), published as the session
//! publishes a room's records ([`tilolang_tilo::resource_service::ResourceService`]), so every note,
//! task, channel and mailbox is a resource the session's `find` finds, a layout places and
//! an agent reads and changes through its actions.
//!
//! Each app's resources are the ones its windows open: once the session holds the workspace the
//! person is in, they are kept in the workspace's store ([`tilo_service::Service::keep_in_workspace`]),
//! filled with the seed data where the workspace holds none of the app's yet, so the app's window,
//! the REPL, an agent's programs and the canvas read and change one copy. Until then they are kept
//! in memory.
//!
//! There is no second way for an app to reach a session: an app that is not installed is not run,
//! and an app that is installed has the manifest the launcher and connector planning read. Its
//! scenario data is the only fixture here, and a person's session starts its apps empty.

use app_catalogue::apps::InstalledApp;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::notices::NOTICE_TAGS;
use tilolang_tilo::resource_service::ResourceService;

use crate::platform::notice::Toolkit;

/// Whether this build seeds the apps `--seed-apps` asks for: development builds only, the
/// optimised one with the developer overrides (`dev-overrides`) included, so a session whose
/// feel is measured on an optimised build shows the same scenario a debug one does.
pub const SEEDED_BY_THIS_BUILD: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// The tilolang apps a development session installs and seeds. Discussion is no app here: Tasks
/// and Notes are made with it, so the comments on a task or a note are kept in that app's own
/// record, as a package an app uses that is not installed keeps its records in it.
pub const APPS: [&str; 5] = [
    "so.tilo.notes",
    "so.tilo.tasks",
    "so.tilo.chat",
    "so.tilo.mail",
    "so.tilo.tube",
];

/// The records of the installed apps [`APPS`] names, each in a record of its own as its windows
/// keep it: the app's root made and filled by its first scenario, each step as its authorship
/// names it, published for them. Before them, the session's own document, which keeps the records of the packages the
/// session runs itself (the shell, [`tilo_service::Service::share`]).
///
/// # Errors
/// An app of [`APPS`] is not installed as a tilolang package, its payload does not read, or its
/// scenario does not run.
pub fn seeded(
    person: &str,
    catalog: &package_store::Catalog,
) -> Result<Vec<ResourceService>, String> {
    let installed = installed_apps(catalog)?;
    let mut services = vec![session_document(person)?];
    for app in &installed {
        services.push(told(seeded_app(person, app)?));
    }
    Ok(services)
}

/// [`seeded`] over the app store the environment names (`$TILO_APPS_DATA_DIR`, else the
/// person's `$XDG_DATA_HOME/tilo-apps`).
///
/// # Errors
/// The store cannot be located, or [`seeded`] fails.
pub fn seeded_from_env(person: &str) -> Result<Vec<ResourceService>, String> {
    let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    seeded(person, &catalog)
}

/// The session's own document, which keeps the records of the packages the session runs itself
/// (the shell, [`tilo_service::Service::share`]): what a session starts with whether or not it
/// seeds any app, so the package the shell actually shows — the store's installed
/// `so.tilo.shell`, else this build's stand-in (`docs/designs/app-language/shell.md` §6) — is one
/// a REPL's or an agent's program names, as it is on a device whose store holds the shell.
///
/// # Errors
/// The document's host does not open.
pub fn session_document(person: &str) -> Result<ResourceService, String> {
    let host = Host::open(
        tilolang_tilo::resources::ResourceStore::in_memory(),
        tilo_actions::Actor::from(tilo_actions::MemberId::person(person)),
        None,
        || None,
    )
    .map_err(|e| e.to_string())?;
    let mut service = ResourceService::session_document(host, person);
    service.set_words(std::rc::Rc::new(crate::i18n::SeededWords));
    Ok(told(service))
}

/// The record of the installed app `app`: its root made and filled by its first scenario as
/// steps its authorship names, in a record of its own.
fn seeded_app(person: &str, app: &InstalledApp) -> Result<ResourceService, String> {
    let host = Host::open(
        tilolang_tilo::resources::ResourceStore::in_memory(),
        tilo_actions::Actor::from(tilo_actions::MemberId::person(person)),
        None,
        || None,
    )
    .map_err(|e| e.to_string())?;
    // The packages the app is made with ride in its own bundle, and keep their records in its
    // record.
    let bundle =
        Bundle::read(&app.payload_root).map_err(|error| format!("{}: {error}", app.app_id))?;
    let package = host
        .load_bundle(&bundle)
        .map_err(|error| format!("{}: {error}", app.app_id))?;
    let name = package.meta.app.clone();
    let scenario = package
        .meta
        .scenarios
        .first()
        .map(|scenario| scenario.action.clone());
    let seed = {
        let (name, person) = (name.clone(), person.to_owned());
        move |host: &Host| -> Result<(), String> {
            host.root_of(&name)
                .map_err(|error| format!("{name}'s root: {error}"))?;
            if let Some(action) = &scenario {
                host.runtime()
                    .invoke_action(
                        &tilo_actions::Actor::from(tilo_actions::MemberId::person(&person)),
                        (action.as_str(), vec![]),
                    )
                    .map_err(|error| format!("{name} {action}: {error}"))?;
            }
            // A workspace of many items, to feel the app as a person with that many does.
            if let Some(items) = seed_items() {
                tilolang_tilo::seed::seed(host, &name, items, &tilolang_tilo::host::system_time())?;
            }
            Ok(())
        }
    };
    seed(&host)?;
    let mut service = ResourceService::of_app(name, host, person);
    service.set_words(std::rc::Rc::new(crate::i18n::SeededWords));
    service.set_seed(seed);
    Ok(service)
}

/// How many items beyond its scenario's each seeded app starts with: `$TILO_SEED_ITEMS`, none
/// without it.
fn seed_items() -> Option<usize> {
    tilo_failure::ignored!(
        "a seed count that is not a number asks for no seeded items",
        std::env::var("TILO_SEED_ITEMS").ok()?.parse().ok()
    )
    .filter(|items| *items > 0)
}

/// `service`, what its apps tell reaching the person through the platform's notifier this
/// session serves, as any app's notification does, and its records published.
fn told(mut service: ResourceService) -> ResourceService {
    service.set_tell(Toolkit::new(
        ui::notify::system(Some("so.tilo.session")),
        NOTICE_TAGS,
    ));
    tilo_service::Service::settle(&mut service);
    service
}

/// The installed packages [`APPS`] names, each an app of the language: a payload that carries
/// the compiled module (a Rust app's payload does not).
///
/// # Errors
/// One of [`APPS`] is not installed, or is installed as a package of another kind.
fn installed_apps(catalog: &package_store::Catalog) -> Result<Vec<InstalledApp>, String> {
    let installed = app_catalogue::apps::read(catalog).map_err(|error| error.to_string())?;
    let mut apps = Vec::new();
    for app in APPS {
        let found = installed
            .iter()
            .find(|installed| installed.app_id == app)
            .ok_or_else(|| format!("{app} is not installed in this session's app store"))?;
        if !found
            .payload_root
            .join(tilolang_tilo::bundle::WASM_FILE)
            .is_file()
        {
            return Err(format!(
                "{app} is installed as a package that is not a tilolang app"
            ));
        }
        apps.push(found.clone());
    }
    Ok(apps)
}
