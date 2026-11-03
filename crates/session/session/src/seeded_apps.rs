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
use tilolang_tilo::tilolang::{IdsLock, PackageSource, compile_package_source};

/// Whether this build seeds the apps `--seed-apps` asks for: development builds only.
pub const SEEDED_BY_THIS_BUILD: bool = cfg!(debug_assertions);

/// An app started here: its id, its source, the ids it was compiled with, and the catalogues it
/// ships its words in, as an installed app carries them in its module.
struct Seed {
    app: &'static str,
    source: &'static str,
    ids: &'static str,
    locales: &'static [(&'static str, &'static str)],
}

const APPS: [Seed; 4] = [
    Seed {
        app: "so.tilo.lang.notes",
        source: include_str!("../../../../apps/tl/notes/app.tl"),
        ids: include_str!("../../../../apps/tl/notes/app.ids"),
        locales: &[
            (
                "en-US",
                include_str!("../../../../apps/tl/notes/locales/en-US.ftl"),
            ),
            (
                "de-DE",
                include_str!("../../../../apps/tl/notes/locales/de-DE.ftl"),
            ),
        ],
    },
    Seed {
        app: "so.tilo.lang.tasks",
        source: include_str!("../../../../apps/tl/tasks/app.tl"),
        ids: include_str!("../../../../apps/tl/tasks/app.ids"),
        locales: &[(
            "en-US",
            include_str!("../../../../apps/tl/tasks/locales/en-US.ftl"),
        )],
    },
    Seed {
        app: "so.tilo.lang.chat",
        source: include_str!("../../../../apps/tl/chat/app.tl"),
        ids: include_str!("../../../../apps/tl/chat/app.ids"),
        locales: &[(
            "en-US",
            include_str!("../../../../apps/tl/chat/locales/en-US.ftl"),
        )],
    },
    Seed {
        app: "so.tilo.lang.mail",
        source: include_str!("../../../../apps/tl/mail/app.tl"),
        ids: include_str!("../../../../apps/tl/mail/app.ids"),
        locales: &[(
            "en-US",
            include_str!("../../../../apps/tl/mail/locales/en-US.ftl"),
        )],
    },
];

/// Notes, tasks, chat and mail in one workspace record of `person`'s, each app's root made and
/// filled by its first scenario as `person`'s steps, published for them.
///
/// # Errors
/// An app does not compile or load, or its scenario does not run.
pub fn seeded(person: &str) -> Result<RecordService, String> {
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(person), || None).map_err(|e| e.to_string())?;
    host.set_words(crate::i18n::PackageWords);
    let mut scenarios = Vec::new();
    for seed in &APPS {
        let source = PackageSource::single(seed.source).with_locales(
            seed.locales
                .iter()
                .map(|(locale, text)| ((*locale).to_owned(), (*text).to_owned())),
        );
        let ids = IdsLock::parse(seed.ids).map_err(|error| format!("{}: {error}", seed.app))?;
        let compiled = compile_package_source(&source, &ids, &[])
            .map_err(|diagnostics| format!("{}: {diagnostics:?}", seed.app))?;
        let package = host
            .load_compiled(&compiled)
            .map_err(|error| format!("{}: {error}", seed.app))?;
        scenarios.push((seed.app, package.meta.scenarios.first().cloned()));
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
