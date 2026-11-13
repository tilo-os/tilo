//! The tilolang apps a development session starts with (`tilo-session --seed-apps`, honoured by
//! development builds only): notes, tasks, chat, mail and the video app.
//!
//! Each is an **installed package**, as it is on a person's device: the app's `.tapp` is
//! installed into the session's own package store (`tilolang run` builds and installs them), the
//! session reads each app's payload from its store row, and the app runs through the launcher
//! like any other. Loading it here publishes the records it keeps: the app's root made
//! and filled by its first scenario (the seed data its shots and tests start from), published as
//! the session publishes a room's records ([`tilolang_tilo::records_service::RecordService`]), so
//! every note, task, channel and mailbox is a resource the session's `find` finds, an
//! arrangement places and an agent reads and changes through its actions.
//!
//! There is no second way for an app to reach a session: an app that is not installed is not run,
//! and an app that is installed has the manifest the launcher and connector planning read. Its
//! scenario data is the only fixture here, and a person's session starts its apps empty.

use app_catalogue::apps::InstalledApp;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::notices::NOTICE_TAGS;
use tilolang_tilo::records_service::RecordService;
use tilolang_tilo::runtime::Principal;

use crate::platform::notice::Toolkit;

/// Whether this build seeds the apps `--seed-apps` asks for: development builds only.
pub const SEEDED_BY_THIS_BUILD: bool = cfg!(debug_assertions);

/// The tilolang apps a development session installs and seeds, in the order a session reads
/// them: the packages they use meet each other as [`seeded`] loads them. Discussion is no app
/// here: Tasks and Notes are made with it, so the comments on a task or a note are kept in that
/// app's own record, as a package an app uses that is not installed keeps its records in it.
pub const APPS: [&str; 5] = [
    "so.tilo.lang.notes",
    "so.tilo.lang.tasks",
    "so.tilo.lang.chat",
    "so.tilo.lang.mail",
    "so.tilo.lang.tube",
];

/// The records of the installed apps [`APPS`] names, in one workspace record of `person`'s:
/// each app's root made and filled by its first scenario as `person`'s steps, published for
/// them.
///
/// # Errors
/// An app of [`APPS`] is not installed as a tilolang package, its payload does not read, or its
/// scenario does not run.
pub fn seeded(person: &str, catalog: &package_store::Catalog) -> Result<RecordService, String> {
    let installed = installed_apps(catalog)?;
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(person), || None).map_err(|e| e.to_string())?;
    host.set_words(crate::i18n::SeededWords);
    // A package's imported modules ride in its own bundle, so any order loads; the packages a
    // session reads first are loaded first, so a package that another one shows is the one it
    // was compiled against.
    let mut scenarios = Vec::new();
    for app in &installed {
        let bundle =
            Bundle::read(&app.payload_root).map_err(|error| format!("{}: {error}", app.app_id))?;
        let package = host
            .load_bundle(&bundle)
            .map_err(|error| format!("{}: {error}", app.app_id))?;
        scenarios.push((
            package.meta.app.clone(),
            package.meta.scenarios.first().cloned(),
        ));
    }
    for (app, scenario) in scenarios {
        host.root_of(&app)
            .map_err(|error| format!("{app}'s root: {error}"))?;
        commit(&host, &mut record, person)?;
        let Some(scenario) = scenario else {
            continue;
        };
        host.runtime()
            .invoke_action(&Principal::new(person), (scenario.action.as_str(), vec![]))
            .map_err(|error| format!("{app} {:?}: {error}", scenario.name))?;
        commit(&host, &mut record, person)?;
    }
    let mut service = RecordService::new(record, host, person);
    // What an app tells reaches the person through the platform's notifier this session serves,
    // as any app's notification does.
    service.set_tell(Toolkit::new(
        ui::notify::system(Some("so.tilo.session")),
        NOTICE_TAGS,
    ));
    tilo_service::Service::settle(&mut service);
    Ok(service)
}

/// [`seeded`] over the app store the environment names (`$TILO_APPS_DATA_DIR`, else the
/// person's `$XDG_DATA_HOME/tilo-apps`).
///
/// # Errors
/// The store cannot be located, or [`seeded`] fails.
pub fn seeded_from_env(person: &str) -> Result<RecordService, String> {
    let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    seeded(person, &catalog)
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

/// Commit what `host` committed to `record`, each as a step of `person`'s.
fn commit(host: &Host, record: &mut tilo_resource::Doc, person: &str) -> Result<(), String> {
    for committed in host.take_committed() {
        let action = &committed.action;
        let encoded = action.encode().map_err(|error| error.to_string())?;
        let context = ui::tilo_actions::ActionContext::for_resource(
            action.resources().first().copied().unwrap_or("room"),
            &action.kind,
            &action.kind,
        )
        .with_invoker(ui::tilo_actions::ActionInvoker::actor(person.to_owned()));
        host.with_schemas(|| {
            ui::tilo_actions::apply_local_with_context(
                record,
                action,
                encoded,
                Some(person.as_bytes()),
                context,
            )
        })
        .map_err(|error| error.to_string())?;
        record.commit();
        host.sync_from(record);
    }
    Ok(())
}
