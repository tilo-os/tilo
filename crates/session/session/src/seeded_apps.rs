//! The tilolang apps a development session starts with (`tilo-session --seed-apps`, honoured by
//! development builds only): notes, tasks, chat and mail, loaded from `apps/tl` into one
//! workspace record, each filled by its first scenario (the seed data its shots and tests start
//! from), and published as the session publishes a room's records
//! ([`tilolang_tilo::records_service::RecordService`]): every note, task, channel and mailbox is
//! a resource the session's `find` finds, an arrangement places and an agent reads and changes
//! through its actions.
//!
//! This is a demo, chosen on the command line: a person's session starts its apps empty.

use tilolang_tilo::host::Host;
use tilolang_tilo::records_service::RecordService;
use tilolang_tilo::runtime::Principal;

/// Whether this build seeds the apps `--seed-apps` asks for: development builds only.
pub const SEEDED_BY_THIS_BUILD: bool = cfg!(debug_assertions);

/// Each app's source and the ids it was compiled with, by its id.
const APPS: [(&str, &str, &str); 4] = [
    (
        "so.tilo.lang.notes",
        include_str!("../../../../apps/tl/notes/app.tl"),
        include_str!("../../../../apps/tl/notes/app.ids"),
    ),
    (
        "so.tilo.lang.tasks",
        include_str!("../../../../apps/tl/tasks/app.tl"),
        include_str!("../../../../apps/tl/tasks/app.ids"),
    ),
    (
        "so.tilo.lang.chat",
        include_str!("../../../../apps/tl/chat/app.tl"),
        include_str!("../../../../apps/tl/chat/app.ids"),
    ),
    (
        "so.tilo.lang.mail",
        include_str!("../../../../apps/tl/mail/app.tl"),
        include_str!("../../../../apps/tl/mail/app.ids"),
    ),
];

/// Notes, tasks, chat and mail in one workspace record of `person`'s, each app's root made and
/// filled by its first scenario as `person`'s steps, published for them.
///
/// # Errors
/// An app does not compile or load, or its scenario does not run.
pub fn seeded(person: &str) -> Result<RecordService, String> {
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(person), || None).map_err(|e| e.to_string())?;
    let mut scenarios = Vec::new();
    for (app, source, ids) in APPS {
        let package = host
            .load_builtin(source, ids)
            .map_err(|error| format!("{app}: {error}"))?;
        scenarios.push((app, package.meta.scenarios.first().cloned()));
    }
    for (app, scenario) in scenarios {
        host.root_of(app)
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
    tilo_service::Service::settle(&mut service);
    Ok(service)
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
