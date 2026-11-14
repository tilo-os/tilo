//! The tilolang apps a development session starts with (`tilo-session --seed-apps`, honoured by
//! development builds only): notes, tasks, chat, mail and the video app.
//!
//! Each is an **installed package**, as it is on a person's device: the app's `.tapp` is
//! installed into the session's own package store (`tilolang run` builds and installs them), the
//! session reads each app's payload from its store row, and the app runs through the launcher
//! like any other. Loading it here publishes the record it keeps: the app's root made and filled
//! by its first scenario (the seed data its shots and tests start from), published as the session
//! publishes a room's records ([`tilolang_tilo::records_service::RecordService`]), so every note,
//! task, channel and mailbox is a resource the session's `find` finds, an arrangement places and
//! an agent reads and changes through its actions.
//!
//! Each app's record is the one its windows open: once the session holds the workspace the
//! person is in, it keeps the record there ([`tilo_service::Service::workspace_record`]), so the
//! seed data is written into the workspace's record of the app, and the app's window, the REPL,
//! an agent's programs and the canvas read and change that one record. A record the workspace
//! holds already takes the place of the seed data.
//!
//! There is no second way for an app to reach a session: an app that is not installed is not run,
//! and an app that is installed has the manifest the launcher and connector planning read. Its
//! scenario data is the only fixture here, and a person's session starts its apps empty.

use std::cell::RefCell;
use std::rc::Rc;

use app_catalogue::apps::InstalledApp;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::notices::NOTICE_TAGS;
use tilolang_tilo::records_service::RecordService;
use tilolang_tilo::runtime::Principal;

use crate::platform::notice::Toolkit;

/// Whether this build seeds the apps `--seed-apps` asks for: development builds only, the
/// optimised one with the developer overrides (`dev-overrides`) included, so a session whose
/// feel is measured on an optimised build shows the same scenario a debug one does.
pub const SEEDED_BY_THIS_BUILD: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// The tilolang apps a development session installs and seeds. Discussion is no app here: Tasks
/// and Notes are made with it, so the comments on a task or a note are kept in that app's own
/// record, as a package an app uses that is not installed keeps its records in it.
pub const APPS: [&str; 5] = [
    "so.tilo.lang.notes",
    "so.tilo.lang.tasks",
    "so.tilo.lang.chat",
    "so.tilo.lang.mail",
    "so.tilo.lang.tube",
];

/// The records of the installed apps [`APPS`] names, each in a record of its own as its windows
/// keep it: the app's root made and filled by its first scenario as `person`'s steps, published
/// for them. Before them, the session's own document, which keeps the records of the packages the
/// session runs itself (the shell, [`tilo_service::Service::share`]).
///
/// # Errors
/// An app of [`APPS`] is not installed as a tilolang package, its payload does not read, or its
/// scenario does not run.
pub fn seeded(
    person: &str,
    catalog: &package_store::Catalog,
) -> Result<Vec<RecordService>, String> {
    let installed = installed_apps(catalog)?;
    let own = tilo_resource::Doc::new();
    let host = Host::new(&own, Principal::new(person), || None).map_err(|e| e.to_string())?;
    host.set_words(crate::i18n::SeededWords);
    let mut services = vec![told(RecordService::new(own, host, person))];
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
pub fn seeded_from_env(person: &str) -> Result<Vec<RecordService>, String> {
    let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    seeded(person, &catalog)
}

/// The record of the installed app `app`: its root made and filled by its first scenario as
/// `person`'s steps, in a record of its own.
fn seeded_app(person: &str, app: &InstalledApp) -> Result<RecordService, String> {
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(person), || None).map_err(|e| e.to_string())?;
    host.set_words(crate::i18n::SeededWords);
    // The packages the app is made with ride in its own bundle, and keep their records in its
    // record.
    let bundle =
        Bundle::read(&app.payload_root).map_err(|error| format!("{}: {error}", app.package_id))?;
    let package = host
        .load_bundle(&bundle)
        .map_err(|error| format!("{}: {error}", app.package_id))?;
    let name = package.meta.app.clone();
    host.root_of(&name)
        .map_err(|error| format!("{name}'s root: {error}"))?;
    commit(&host, &mut record, person)?;
    if let Some(scenario) = package.meta.scenarios.first() {
        host.runtime()
            .invoke_action(&Principal::new(person), (scenario.action.as_str(), vec![]))
            .map_err(|error| format!("{name} {:?}: {error}", scenario.name))?;
        commit(&host, &mut record, person)?;
    }
    Ok(RecordService::of_app(
        name,
        Rc::new(RefCell::new(record)),
        host,
        person,
    ))
}

/// `service`, what its apps tell reaching the person through the platform's notifier this
/// session serves, as any app's notification does, and its records published.
fn told(mut service: RecordService) -> RecordService {
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
            .find(|installed| installed.package_id == app)
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
