//! The person's settings, kept by the session.
//!
//! Settings' sections (appearance, input, power, …) are records: each is
//! published with its field edits and verbs, and each change is checked by
//! the section's model and recorded in the settings document with what made
//! it and who. The session keeps them, so the palette, agents and scripts
//! change them through those same actions whether or not Settings is open,
//! and the Settings window is one view of them. Whether a change may run is
//! read from the published action, for the value it carries: one that
//! weakens the person's security asks them first, and an agent's needs
//! their own answer.

use std::sync::{Arc, Mutex, mpsc};

use tilo_actions::{ActionAddress, ActionDescriptor, ActionInvoker, RiskLevel};
use tilo_app::framework::SettingsRecords;

use crate::shell::search::action_api::ActionError;

/// One piece of work on the person's settings, run where they are held.
type Job = Box<dyn FnOnce(&SettingsRecords) + Send>;

/// What the thread holding the person's settings is asked.
enum Asked {
    Job(Job),
    /// The broker sent something for their records.
    Wake,
    /// Their keeper was let go.
    Stop,
}

/// Where the person's settings are held: on a thread of their own, since
/// the person's records they are kept in are single-threaded while the
/// palette and the broker change them from theirs. Held for one person and
/// one directory of their records; another opens them anew.
struct Keeper {
    held_for: (Option<std::path::PathBuf>, String),
    jobs: mpsc::Sender<Asked>,
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.jobs.send(Asked::Stop);
    }
}

static KEEPER: Mutex<Option<Keeper>> = Mutex::new(None);

/// Run `work` on the person's settings sections, where they are held.
fn with_records<T: Send + 'static>(
    work: impl FnOnce(&SettingsRecords) -> T + Send + 'static,
) -> Result<T, String> {
    let held_for = (person_dir(), crate::shell::state::local_user_id());
    let jobs = {
        let mut keeper = KEEPER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if keeper
            .as_ref()
            .is_none_or(|keeper| keeper.held_for != held_for)
        {
            *keeper = Some(hold(held_for));
        }
        keeper
            .as_ref()
            .map(|keeper| keeper.jobs.clone())
            .ok_or("the person's settings are unavailable")?
    };
    let (answer, answered) = mpsc::channel();
    jobs.send(Asked::Job(Box::new(move |records| {
        let _ = answer.send(work(records));
    })))
    .map_err(|_| "the person's settings are unavailable")?;
    answered
        .recv()
        .map_err(|_| "the person's settings are unavailable".to_owned())
}

/// Where Settings keeps the person's records on this device, which the
/// session keeps its sections in.
fn person_dir() -> Option<std::path::PathBuf> {
    launcher::appdir::host_data_home()
        .map(|home| launcher::appdir::host_person_dir(&home, settings_sections::APP_ID))
}

/// Hold the person's settings sections for `person`, in their records in
/// `dir`, on a thread that runs every job sent until its keeper is let go,
/// and keeps their records in step with the person's other devices over
/// their routes.
fn hold((dir, person): (Option<std::path::PathBuf>, String)) -> Keeper {
    let (jobs, asked) = mpsc::channel::<Asked>();
    let waker = jobs.clone();
    let held_for = (dir.clone(), person.clone());
    let spawned = std::thread::Builder::new()
        .name("settings-records".into())
        .spawn(move || {
            // Without a root identity there is no person room: the records
            // are this device's alone.
            let room = collab_types::person_room_of(&person);
            let records = open(dir.as_deref(), person);
            let mut routes = room.and_then(|room| {
                ui::app::record_routes::RecordRouteParticipant::connect(
                    crate::shell::in_process::session_connector(
                        launcher::services::collab_socket(),
                        launcher::services::identity_socket(),
                        settings_sections::APP_ID,
                        &room.to_string(),
                    ),
                    &records.history_owners(),
                    move || {
                        let _ = waker.send(Asked::Wake);
                    },
                )
            });
            for asked in asked {
                match asked {
                    Asked::Job(job) => job(&records),
                    Asked::Wake => {}
                    Asked::Stop => break,
                }
                if let Some(routes) = &mut routes {
                    routes.pump(&records.history_owners());
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "the person's settings could not be held");
    }
    Keeper { held_for, jobs }
}

/// The person's settings sections, kept in their records in `dir` as
/// Settings' window keeps them, so each change is a step of theirs they
/// undo and redo; without records, as stored.
fn open(dir: Option<&std::path::Path>, person: String) -> SettingsRecords {
    let records = dir.and_then(|dir| {
        std::fs::create_dir_all(dir)
            .map_err(
                |error| tracing::warn!(%error, "the person's settings records are unavailable"),
            )
            .ok()?;
        let mut owners = ui::record::RecordOwners::with_store(std::rc::Rc::new(
            app_state::FsStore::new(dir.to_path_buf()),
        ));
        ui::record::person::PersonRecords::open(
            &mut owners,
            settings_sections::APP_ID,
            ui::permissions::UserId(person),
        )
        .map_err(|error| tracing::warn!(%error, "the person's settings records are unavailable"))
        .ok()
    });
    match records {
        Some(records) => SettingsRecords::of_person(
            settings_sections::APP_ID,
            settings_sections::APP_TITLE,
            &[settings_sections::declare],
            records,
            this_device(),
        ),
        None => SettingsRecords::new(settings_sections::APP_ID, settings_sections::declare),
    }
}

/// This device, as the session's apps know it: a section kept per device
/// is kept in a record of this device's.
fn this_device() -> Option<String> {
    let root = cli::data_home()
        .join(crate::shell::SHELL_RECORDS_PACKAGE)
        .join("apps");
    std::fs::create_dir_all(&root).ok()?;
    ui::application::NativeProfile::open(&root, None)
        .map(|profile| profile.device_id().to_owned())
        .map_err(|error| tracing::warn!(%error, "this device's identity is unavailable"))
        .ok()
}

/// The action `action` of the section `kind` (`settings.power`).
pub(crate) fn address(kind: &str, action: &str) -> ActionAddress {
    ActionAddress::new(format!("settings:{kind}"), action)
}

/// The arguments of a field edit setting `field` to `value`.
pub(crate) fn set_field(
    field: &str,
    value: &serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        tilo_actions::contract::FIELD_EDIT_PARAM.to_owned(),
        serde_json::json!({ "path": [{ "field": field }], "op": "set", "value": value }),
    );
    arguments
}

/// Run `address` with `arguments` as the person at this seat, on behalf of
/// the action `invoked_by` (the palette's `appearance.theme.dark`), which
/// the change records beside the section action it runs. A change whose
/// published action asks first runs only once they `confirmed` it.
///
/// # Errors
/// [`ActionError::ConfirmationRequired`] for a change they have yet to
/// confirm; otherwise why the section refused it.
pub(crate) fn change(
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    confirmed: bool,
    invoked_by: &settings_store::InvokingAction,
) -> Result<(), ActionError> {
    let person = crate::shell::state::local_user_id();
    let (address, arguments, invoked_by) = (address.clone(), arguments.clone(), invoked_by.clone());
    with_records(move |records| {
        records
            .invoke(
                &address,
                &arguments,
                confirmed,
                &ActionInvoker::actor(person),
                Some(&invoked_by),
            )
            .map(drop)
    })
    .map_err(ActionError::Refused)?
    .map_err(|error| match error {
        tilo_actions::DoorError::ConfirmationRequired => ActionError::ConfirmationRequired,
        error => ActionError::Refused(error.to_string()),
    })
}

/// This session's door to the person's settings, for the broker.
pub(crate) fn broker_door() -> Arc<dyn tilo_action_broker::services::HostStepServices> {
    Arc::new(BrokerDoor::default())
}

/// What the broker offers: every section's actions, described once per
/// version of the settings and of the language they are worded in.
#[derive(Default)]
struct BrokerDoor {
    described: Mutex<Option<(Described, Vec<ActionDescriptor>)>>,
}

type Described = (ui::storage::SettingsRevision, u64);

fn described_at() -> Described {
    (
        ui::storage::SettingsDocument::Session.revision(),
        ui::i18n::I18nManager::revision(),
    )
}

/// The typed arguments an argument slot carries.
fn arguments_of(argument: Option<&str>) -> Option<serde_json::Map<String, serde_json::Value>> {
    let json = argument?.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)?;
    serde_json::from_str(json).ok()
}

impl tilo_action_broker::services::HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        tilo_actions::remote::settings_records::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        let at = described_at();
        let mut described = self
            .described
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((when, descriptors)) = described.as_ref()
            && *when == at
        {
            return descriptors.clone();
        }
        let Ok(descriptors) =
            with_records(|records| tilo_actions::door::survey(&records.resources()))
        else {
            return Vec::new();
        };
        *described = Some((at, descriptors.clone()));
        descriptors
    }

    fn invocation(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let (address, argument) = (address.clone(), argument.map(str::to_owned));
        with_records(move |records| {
            let resources = records.resources();
            let spec = tilo_actions::door::resolve(&resources, &address)?;
            let argument = argument.as_deref();
            let need = spec.need_for(arguments_of(argument).as_ref(), argument);
            let mut descriptor = tilo_actions::door::survey(&resources)
                .into_iter()
                .find(|descriptor| descriptor.address == address)?;
            descriptor.contract.kind = need.kind;
            if need.privileged {
                descriptor.contract.risk = RiskLevel::Privileged;
            }
            Some(descriptor)
        })
        .ok()
        .flatten()
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<tilo_action_broker::services::HostStepOutcome>, String>
    {
        let arguments = arguments_of(request.argument).ok_or("the change carries no arguments")?;
        // The broker admitted it, its confirmation included.
        let (address, actor) = (request.address.clone(), request.actor.id().to_owned());
        let output = with_records(move |records| {
            records.invoke(
                &address,
                &arguments,
                true,
                &ActionInvoker::actor(actor),
                None,
            )
        })?
        .map_err(|error| error.to_string())?;
        Ok(tilo_actions::remote::Pending::ready(Ok(output)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tilo_action_broker::test_support::{Grants as _, KindSet, Subject};
    use tilo_actions::remote::{
        ActionHostError, Caller, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
        settings_records::PROVIDER,
    };

    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    fn lock_after(minutes: u32, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.into(),
                address: super::address("settings.power", "field-edit"),
            },
            argument: InvokeArgument::Json(super::set_field(
                "lock_minutes",
                &serde_json::json!(minutes),
            )),
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    /// With no window of Settings, an agent changes the person's settings
    /// through the session: it locks sooner on its own, but never makes the
    /// lock later or turns it off without the person's own answer.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its broker alive through its assertions"
    )]
    #[test]
    #[serial_test::serial]
    fn with_settings_closed_an_agent_weakens_the_lock_only_with_the_persons_answer() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths).unwrap());
        let settings = crate::shell::test_support::install_settings(session.host_view().unwrap());
        let mut workspace = Workspace::new(WorkspaceId(7), "Settings");
        workspace.set_owner("owner");
        workspace.admit_agent("scribe", KindSet::READ);
        workspace.allow_app(
            Subject::agent("scribe"),
            settings_sections::APP_ID,
            KindSet::ALL,
        );
        let room = workspace.room().to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let broker =
            tilo_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
                Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                    tree.shared(),
                )),
                tilo_action_broker::services::HostServices {
                    host_steps: vec![super::broker_door()],
                    ..Default::default()
                },
            );
        let agent = Caller::local_agent("scribe").in_room(room);
        let invoke = |request: &InvokeRequest| {
            broker
                .state()
                .action_invoke(&agent, None, None, request)
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("admission answers at once")
        };

        let offered = broker
            .state()
            .action_query(
                &agent,
                &tilo_actions::remote::CatalogueQuery {
                    provider: Some(PROVIDER.into()),
                    max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                    ..Default::default()
                },
            )
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the catalogue answers")
            .expect("the agent may look");
        assert!(
            offered
                .rows
                .iter()
                .any(|row| row.action.as_ref().is_some_and(|action| {
                    action.address == super::address("settings.power", "field-edit")
                })),
            "the session offers the lock's field edit"
        );

        invoke(&lock_after(1, None)).expect("locking sooner asks nothing");
        assert_eq!(settings_store::power::LOCK_MINUTES.get(), 1);
        for minutes in [10, 0] {
            for confirmation in [None, Some(Confirmation::Confirmed)] {
                assert!(
                    matches!(
                        invoke(&lock_after(minutes, confirmation)),
                        Err(ActionHostError::ConfirmationRequired | ActionHostError::NotPermitted)
                    ),
                    "an agent's own confirmation is no answer of the person's"
                );
            }
        }
        assert_eq!(
            settings_store::power::LOCK_MINUTES.get(),
            1,
            "nothing weakened"
        );
        drop(settings);
    }

    /// A change the person makes through the session is a step of theirs,
    /// kept in their records: the session publishes its undo, and undoing it
    /// stores the value before it again.
    #[test]
    #[serial_test::serial]
    fn the_persons_change_through_the_session_is_a_step_they_undo() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths).unwrap());
        let settings = crate::shell::test_support::install_settings(session.host_view().unwrap());
        let palette = settings_store::InvokingAction::action("power.lock");
        let before = settings_store::power::LOCK_MINUTES.get();
        let sooner = if before == 1 { 2 } else { 1 };
        super::change(
            &super::address("settings.power", "field-edit"),
            &super::set_field("lock_minutes", &serde_json::json!(sooner)),
            false,
            &palette,
        )
        .unwrap();
        assert_eq!(settings_store::power::LOCK_MINUTES.get(), sooner);
        let undo = super::address("settings.power", "undo");
        assert!(
            super::with_records(|records| tilo_actions::door::survey(&records.resources()))
                .unwrap()
                .iter()
                .any(|descriptor| descriptor.address == undo),
            "the session publishes the person's undo"
        );
        super::change(&undo, &serde_json::Map::new(), false, &palette).unwrap();
        assert_eq!(settings_store::power::LOCK_MINUTES.get(), before);
        drop(settings);
    }
}
