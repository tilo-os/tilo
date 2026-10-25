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
//!
//! Their accounts and contacts are held here too, in the records Settings
//! and Contacts keep them in: the apps the session shows open them here,
//! one copy in the process; the session's messaging changes the accounts
//! through their actions; and they sync with the sections to the person's
//! other devices. So are the models their provider settings offer for an
//! agent's run, which the session keeps from those settings for every app's
//! model picker.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use accounts::person::PersonAccounts;
use agent_provider_config::{OfferedModels, ProviderConfig};
use tilo_actions::remote::Pending;
use tilo_actions::{ActionAddress, ActionDescriptor, ActionInvoker, Resource, RiskLevel};
use tilo_app::framework::SettingsRecords;
use tilo_contact_directory::ContactDirectory;
use tilo_resource::SharedRecord;
use ui::record::person::PersonRecords;

use crate::shell::search::action_api::ActionError;

/// One piece of work on the person's records, run where they are held.
type Job = Box<dyn FnOnce(&SettingsRecords) + Send>;

/// The directory of the person's records, and the person.
type HeldFor = (PathBuf, String);

const UNAVAILABLE: &str = "the person's records are unavailable";

/// The person's records the session keeps — Settings' sections, their
/// accounts and contacts — held on the thread that shows the session's
/// apps, which open them there: one copy in the process. The records are
/// single-threaded; what another thread (the broker's) asks of them runs
/// when the holding thread next pumps them ([`pump`]), and their actions
/// are published for it to read without waiting. Clones share them.
#[derive(Clone)]
pub(crate) struct Holder(Arc<Port>);

/// How a holder reaches the thread holding the records.
struct Port {
    held_for: HeldFor,
    jobs: mpsc::Sender<Job>,
    /// Whether they need pumping: work was sent, or the broker sent
    /// something for them.
    pending: AtomicBool,
    /// The sections' actions, as the holding thread last published them.
    actions: Mutex<Arc<Actions>>,
}

/// The sections' actions at one version of the settings and of the
/// language they are worded in.
#[derive(Default)]
struct Actions {
    at: Option<Described>,
    resources: Vec<Resource>,
    descriptors: Vec<ActionDescriptor>,
}

impl Port {
    fn wake(&self) {
        self.pending.store(true, Ordering::Release);
        let wake = WAKE.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    fn published(&self) -> Arc<Actions> {
        Arc::clone(&self.actions.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Wakes the thread holding the session's records to pump them
/// ([`pump`]), once it bound it ([`wake_with`]).
static WAKE: Mutex<Option<crate::session_runtime::Wake>> = Mutex::new(None);

/// Wake the thread holding the session's records with `wake` whenever they
/// need it to pump them.
pub(crate) fn wake_with(wake: crate::session_runtime::Wake) {
    *WAKE.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
}

/// Run what other threads asked of the session's records, keep them in
/// step with the person's other devices and publish their actions: on the
/// thread holding them, each time it is woken, locked or not.
pub(crate) fn pump() {
    if let Ok(holder) = Holder::session()
        && let Some(held) = holder.here()
    {
        if holder.0.pending.swap(false, Ordering::AcqRel) {
            held.pump();
        } else {
            held.publish();
        }
    }
}

/// The person's records, on the thread holding them.
struct Held {
    port: Arc<Port>,
    records: SettingsRecords,
    /// Where they are kept, which the apps shown on this thread open the
    /// person's records of other apps in.
    person: PersonRecords,
    jobs: mpsc::Receiver<Job>,
    /// Their routes to the person's other devices, when they have a room.
    routes: RefCell<Option<ui::app::record_routes::RecordRouteParticipant>>,
}

impl Held {
    /// Run what other threads asked, then publish what changed and merge
    /// what the person's other devices sent.
    fn pump(&self) {
        for job in self.jobs.try_iter() {
            job(&self.records);
        }
        if let Ok(mut routes) = self.routes.try_borrow_mut()
            && let Some(routes) = routes.as_mut()
        {
            routes.pump(&self.records.history_owners());
        }
        self.publish();
    }

    /// Publish the sections' actions, when the settings or the language
    /// changed since they last were.
    fn publish(&self) {
        let at = described_at();
        if self.port.published().at == Some(at) {
            return;
        }
        let resources = self.records.resources();
        let descriptors = tilo_actions::door::survey(&resources);
        *self
            .port
            .actions
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(Actions {
            at: Some(at),
            resources,
            descriptors,
        });
    }
}

thread_local! {
    /// The person's records this thread holds, one holding per person
    /// and directory of their records.
    static HELD: RefCell<Vec<Rc<Held>>> = const { RefCell::new(Vec::new()) };
}

/// The session's holder.
static HOLDER: Mutex<Option<Holder>> = Mutex::new(None);

impl Holder {
    /// The person's records as the session keeps them, held on this thread.
    pub(crate) fn of(person: &str) -> Result<Self, String> {
        let dir = person_dir().ok_or(UNAVAILABLE)?;
        Self::held((dir, person.to_owned()), &session_apps())
    }

    /// The person's records in `dir`, held for `person` on this thread —
    /// opened here unless it holds them already, taking in once what the
    /// session kept of them under `former` — and the session's from now on.
    pub(crate) fn held(held_for: HeldFor, former: &Path) -> Result<Self, String> {
        let here = HELD.with(|held| {
            held.borrow()
                .iter()
                .find(|held| held.port.held_for == held_for)
                .map(|held| Self(Arc::clone(&held.port)))
        });
        let holder = match here {
            Some(holder) => holder,
            None => hold(held_for, former)?,
        };
        *HOLDER.lock().unwrap_or_else(PoisonError::into_inner) = Some(holder.clone());
        Ok(holder)
    }

    /// The session's holder, once the thread showing its apps holds the
    /// person's records.
    fn session() -> Result<Self, String> {
        HOLDER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| UNAVAILABLE.to_owned())
    }

    /// The records, when this thread holds them.
    fn here(&self) -> Option<Rc<Held>> {
        HELD.with(|held| {
            held.borrow()
                .iter()
                .find(|held| Arc::ptr_eq(&held.port, &self.0))
                .cloned()
        })
    }

    /// Run `work` on the person's records, on the thread holding them.
    pub(crate) fn run<T>(&self, work: impl FnOnce(&SettingsRecords) -> T) -> Result<T, String> {
        let held = self.here().ok_or(UNAVAILABLE)?;
        let answer = work(&held.records);
        held.pump();
        Ok(answer)
    }

    /// Run `work` on the person's records from any thread: at once on the
    /// thread holding them, else when it next pumps them, never waiting on
    /// it. Abandoned when they are held no more.
    fn ask<T: Send + 'static>(
        &self,
        work: impl FnOnce(&SettingsRecords) -> T + Send + 'static,
    ) -> Pending<T> {
        if let Some(held) = self.here() {
            let answer = work(&held.records);
            held.pump();
            return Pending::ready(answer);
        }
        let (answer, answered) = tilo_actions::remote::pending();
        if self
            .0
            .jobs
            .send(Box::new(move |records| answer.complete(work(records))))
            .is_ok()
        {
            self.0.wake();
        }
        answered
    }

    /// Where the records are kept, on the thread holding them: the apps it
    /// shows open the person's records of other apps there.
    pub(crate) fn person_records(&self) -> Result<PersonRecords, String> {
        Ok(self.here().ok_or(UNAVAILABLE)?.person.clone())
    }

    /// Read the person's shared record `P`.
    pub(crate) fn read<P: SharedRecord, T>(
        &self,
        read: impl FnOnce(&P::State) -> T,
    ) -> Result<T, String> {
        self.run(|records| {
            records
                .shared::<P>()
                .map(|record| record.with(read))
                .ok_or_else(|| UNAVAILABLE.to_owned())
        })?
    }

    /// Change the person's accounts by the action `invoke` names, as the
    /// person, if `admitted` lets it run when it does, and store the
    /// change.
    pub(crate) fn change_accounts(
        &self,
        invoke: tilo_resource::Invoke<PersonAccounts>,
        admitted: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        self.run(move |records| {
            let record = records.shared::<PersonAccounts>().ok_or(UNAVAILABLE)?;
            admitted()?;
            record.invoke(&invoke).map_err(|error| error.to_string())?;
            record.record().flush();
            Ok(())
        })?
    }
}

/// Run `work` on the person's settings sections, held on this thread: the
/// session's loop, where the palette asks.
fn with_records<T>(work: impl FnOnce(&SettingsRecords) -> T) -> Result<T, String> {
    Holder::of(&crate::shell::state::local_user_id())?.run(work)
}

/// The person's provider settings changed: offer the models they offer now.
pub(crate) fn provider_settings_changed() {
    if let Err(error) = with_records(offer_models) {
        tracing::warn!(%error, "the offered models were not updated");
    }
}

/// Record the models the provider settings offer for an agent's run in the
/// person's record of them, when they changed: what apps' model pickers
/// list.
fn offer_models(records: &SettingsRecords) {
    let Some(record) = records.shared::<OfferedModels>() else {
        return;
    };
    let models: Vec<_> = ProviderConfig::load().offered_models().collect();
    if record.with(|offered| offered.models == models) {
        return;
    }
    match record.invoke(&OfferedModels::OFFER.with(models)) {
        Ok(()) => record.record().flush(),
        Err(error) => tracing::warn!(%error, "the offered models were not recorded"),
    }
}

/// Where Settings keeps the person's records on this device, which the
/// session keeps its sections and their accounts and contacts in.
pub(crate) fn person_dir() -> Option<PathBuf> {
    launcher::appdir::host_data_home()
        .map(|home| launcher::appdir::host_person_dir(&home, settings_sections::APP_ID))
}

/// Settings' committed surface: the one its package ships, which the
/// session's shell carries because it keeps Settings' records itself.
fn settings_surface() -> &'static [u8] {
    static SURFACE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/settings/surface.toml"
    ));
    SURFACE
}

/// Name Settings' surface in this process as its built-in app's, unless a
/// launched package of Settings already named its own: the person-record
/// kinds it declares are what the broker admits its routes to the person's
/// room for, and the session connects them.
pub(crate) fn register_surface() {
    ui::app_surface::register_built_in(settings_sections::APP_ID, settings_surface());
}

/// Where the session keeps the records of the apps it shows itself.
pub(crate) fn session_apps() -> PathBuf {
    cli::data_home()
        .join(crate::shell::SHELL_RECORDS_PACKAGE)
        .join("apps")
}

/// Hold the person's records for `person`, in `dir`, on this thread, kept
/// in step with the person's other devices over their routes. What the
/// session kept of them under `former` is taken in first.
fn hold((dir, person): HeldFor, former: &Path) -> Result<Holder, String> {
    // Before their routes connect: the broker admits each to the person's
    // room by the kinds this surface declares.
    register_surface();
    for taken in [
        take_in::<PersonAccounts>(former, &dir, &person),
        take_in::<ContactDirectory>(former, &dir, &person),
    ] {
        if let Err(error) = taken {
            tracing::warn!(%error, "the person's records kept by the session were not taken in");
        }
    }
    let (jobs, asked) = mpsc::channel();
    let port = Arc::new(Port {
        held_for: (dir.clone(), person.clone()),
        jobs,
        pending: AtomicBool::new(false),
        actions: Mutex::default(),
    });
    // Without a root identity there is no person room: the records are
    // this device's alone.
    let room = collab_types::person_room_of(&person);
    let (records, person) = open(&dir, person)?;
    // Held from the start, so what the person's other devices change
    // reaches them.
    drop(records.shared::<PersonAccounts>());
    drop(records.shared::<ContactDirectory>());
    offer_models(&records);
    let waking = Arc::downgrade(&port);
    let routes = room.and_then(|room| {
        ui::app::record_routes::RecordRouteParticipant::connect(
            crate::shell::in_process::session_connector(
                launcher::services::collab_socket(),
                launcher::services::identity_socket(),
                settings_sections::APP_ID,
                &room.to_string(),
            ),
            &records.history_owners(),
            move || {
                if let Some(port) = waking.upgrade() {
                    port.wake();
                }
            },
        )
    });
    let held = Rc::new(Held {
        port: Arc::clone(&port),
        records,
        person,
        jobs: asked,
        routes: RefCell::new(routes),
    });
    held.publish();
    HELD.with(|here| here.borrow_mut().push(held));
    Ok(Holder(port))
}

/// Take into the person's records in `dir` their shared record `P` the
/// session kept in its own records under `former`: the whole record, its
/// history with it, merged into the one kept in `dir` and stored, then
/// removed from `former`. Whether there was one to take in; once taken,
/// there is none.
fn take_in<P: SharedRecord>(former: &Path, dir: &Path, person: &str) -> Result<bool, String> {
    let kind = ui::record::RecordKind::new(P::KIND);
    let author = ui::permissions::UserId(person.to_owned());
    let kept = Rc::new(app_state::FsStore::new(former.to_path_buf()));
    let key = durable_store::StorageKey::record(
        P::OWNER,
        ui::record::person::scope_of(&author),
        ui::record::person::record_id(&kind).storage_key(),
    );
    let Some(stored) = durable_store::DurableStore::local_path(kept.as_ref(), &key)
        .and_then(|document| document.parent().map(Path::to_path_buf))
        .filter(|stored| stored.exists())
    else {
        return Ok(false);
    };
    let document = {
        let mut owners = ui::record::RecordOwners::with_store(kept);
        let records = PersonRecords::open(&mut owners, P::OWNER, author.clone())
            .map_err(|error| error.to_string())?;
        let Some(record) = records
            .open_existing_record(kind.clone())
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        record.document().ok_or("the record kept is unreadable")?
    };
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let mut owners =
        ui::record::RecordOwners::with_store(Rc::new(app_state::FsStore::new(dir.to_path_buf())));
    let records =
        PersonRecords::open(&mut owners, P::OWNER, author).map_err(|error| error.to_string())?;
    // The record itself, never a copy following another window's.
    records
        .open_original_record(kind, |doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        })
        .map_err(|error| error.to_string())?
        .merge_peer_document(document)?;
    records.flush()?;
    std::fs::remove_dir_all(&stored).map_err(|error| error.to_string())?;
    Ok(true)
}

/// The person's settings sections, kept in their records in `dir` as
/// Settings' window keeps them, so each change is a step of theirs they
/// undo and redo, with those records.
fn open(dir: &Path, person: String) -> Result<(SettingsRecords, PersonRecords), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let mut owners =
        ui::record::RecordOwners::with_store(Rc::new(app_state::FsStore::new(dir.to_path_buf())));
    let records = PersonRecords::open(
        &mut owners,
        settings_sections::APP_ID,
        ui::permissions::UserId(person),
    )
    .map_err(|error| error.to_string())?;
    Ok((
        SettingsRecords::of_person(
            settings_sections::APP_ID,
            settings_sections::APP_TITLE,
            &[settings_sections::declare],
            records.clone(),
            this_device(),
        ),
        records,
    ))
}

/// This device, as the session's apps know it: a section kept per device
/// is kept in a record of this device's.
fn this_device() -> Option<String> {
    let root = session_apps();
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
    set_path([field], value)
}

/// The arguments of a field edit setting the field at `path` (its names,
/// outermost first) to `value`.
pub(crate) fn set_path<'a>(
    path: impl IntoIterator<Item = &'a str>,
    value: &serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let path: Vec<serde_json::Value> = path
        .into_iter()
        .map(|field| serde_json::json!({ "field": field }))
        .collect();
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        tilo_actions::contract::FIELD_EDIT_PARAM.to_owned(),
        serde_json::json!({ "path": path, "op": "set", "value": value }),
    );
    arguments
}

/// Whether each of the yes-or-no settings `switches` is on as stored now,
/// read where the person's settings are held in one go; `None` for no
/// switch, or a section the session does not keep.
pub(crate) fn switches_on(
    switches: impl IntoIterator<Item = Option<settings_store::index::Switch>>,
) -> Vec<Option<bool>> {
    let switches: Vec<_> = switches.into_iter().collect();
    let asked = switches.len();
    with_records(move |records| {
        switches
            .into_iter()
            .map(|switch| switch.and_then(|switch| on_in(records, switch)))
            .collect()
    })
    .unwrap_or_else(|_| vec![None; asked])
}

/// Whether the yes-or-no setting `switch` is on as stored now; `None` for
/// a section the session does not keep.
pub(crate) fn switch_is_on(switch: settings_store::index::Switch) -> Option<bool> {
    switches_on([Some(switch)]).pop().flatten()
}

/// Whether `switch` is on as `records` hold it.
fn on_in(records: &SettingsRecords, switch: settings_store::index::Switch) -> Option<bool> {
    let stored = records.stored(switch.section)?;
    switch
        .path()
        .try_fold(&stored, |value, field| value.get(field))?
        .as_bool()
}

/// Flip the yes-or-no setting `switch` through its section's field edit,
/// on behalf of the action `invoked_by`: on when it is off, off when it is
/// on, as stored at this moment. A flip whose field edit asks first runs
/// only once the person `confirmed` it.
///
/// # Errors
/// [`ActionError::ConfirmationRequired`] for a flip they have yet to
/// confirm; otherwise the session keeps no such section, or the section
/// refused the change.
pub(crate) fn flip(
    switch: settings_store::index::Switch,
    confirmed: bool,
    invoked_by: &settings_store::InvokingAction,
) -> Result<bool, ActionError> {
    let on = !switch_is_on(switch).ok_or_else(|| {
        ActionError::Refused(format!("{} is not a setting kept here", switch.section))
    })?;
    change(
        &address(switch.section, "field-edit"),
        &set_path(switch.path(), &on.into()),
        confirmed,
        invoked_by,
    )?;
    Ok(on)
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
    Arc::new(BrokerDoor)
}

/// What the broker offers: every section's actions, as the session last
/// published them, and changes run where the settings are held.
struct BrokerDoor;

type Described = (ui::storage::SettingsRevision, u64);

fn described_at() -> Described {
    (
        ui::storage::SettingsDocument::Session.revision(),
        ui::i18n::I18nManager::revision(),
    )
}

/// The sections' actions as last published, once the session holds them.
fn published() -> Option<Arc<Actions>> {
    Some(Holder::session().ok()?.0.published())
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
        published().map_or_else(Vec::new, |actions| actions.descriptors.clone())
    }

    fn invocation(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let actions = published()?;
        let spec = tilo_actions::door::resolve(&actions.resources, address)?;
        let need = spec.need_for(arguments_of(argument).as_ref(), argument);
        let mut descriptor = actions
            .descriptors
            .iter()
            .find(|descriptor| &descriptor.address == address)?
            .clone();
        descriptor.contract.kind = need.kind;
        if need.privileged {
            descriptor.contract.risk = RiskLevel::Privileged;
        }
        Some(descriptor)
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::HostStepRequest<'_>,
    ) -> Result<Pending<tilo_action_broker::services::HostStepOutcome>, String> {
        let arguments = arguments_of(request.argument).ok_or("the change carries no arguments")?;
        // The broker admitted it, its confirmation included.
        let (address, actor) = (request.address.clone(), request.actor.id().to_owned());
        Ok(Holder::session()?.ask(move |records| {
            records
                .invoke(
                    &address,
                    &arguments,
                    true,
                    &ActionInvoker::actor(actor),
                    None,
                )
                .map_err(|error| {
                    Box::new(tilo_actions::ActionFailure {
                        code: "settings_refused".into(),
                        message: error.to_string(),
                        details: None,
                        retryable: false,
                    })
                })
        }))
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
        let _held = super::Holder::of(&crate::shell::state::local_user_id()).unwrap();
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

        // The broker asks from its own thread, never waiting on the one
        // holding the settings: the change runs when that one next pumps.
        let before = settings_store::power::LOCK_MINUTES.get();
        std::thread::scope(|scope| scope.spawn(|| invoke(&lock_after(1, None))).join())
            .unwrap()
            .expect("locking sooner asks nothing");
        assert_eq!(settings_store::power::LOCK_MINUTES.get(), before);
        super::pump();
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

    /// The person's records the session holds are Settings' own, whose
    /// routes to the person's room the broker admits only for the kinds
    /// Settings' surface declares: the session names that surface before it
    /// connects them, so their first join is not refused.
    #[test]
    #[serial_test::serial]
    fn the_session_holds_settings_records_with_the_surface_settings_ships() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        super::register_surface();
        let held = super::Holder::of(&crate::shell::state::local_user_id()).unwrap();
        assert!(
            ui::app_surface::keeps_person_record(settings_sections::APP_ID, "tilo.accounts"),
            "the session admits Settings' accounts route to the person's room"
        );
        drop(held);
    }

    /// The accounts the session kept in its own records are taken into the
    /// person's records Settings keeps, history and all, synced with the
    /// sections, and kept by the session no more: taking them in again
    /// finds none.
    #[test]
    fn the_accounts_the_session_kept_are_taken_in_once_with_their_history() {
        use accounts::person::{ACCOUNTS_OWNER, PersonAccounts};
        use tilo_actions::Action as _;
        use tilo_resource::Resource as _;
        use ui::automerge::ReadDoc as _;

        let root = tempfile::tempdir().unwrap();
        let (former, dir) = (root.path().join("apps"), root.path().join("person"));
        let person = hex::encode([7_u8; 32]);
        let kind = ui::record::RecordKind::new(PersonAccounts::KIND);
        let records_in = |root: &std::path::Path, package: &str| {
            let mut owners = ui::record::RecordOwners::with_store(std::rc::Rc::new(
                app_state::FsStore::new(root.to_path_buf()),
            ));
            ui::record::person::PersonRecords::open(
                &mut owners,
                package,
                ui::permissions::UserId(person.clone()),
            )
            .unwrap()
        };
        let kept = {
            let records = records_in(&former, ACCOUNTS_OWNER);
            let record = records
                .open_record(kind.clone(), &|doc| {
                    PersonAccounts::initialize(doc).map_err(|error| error.to_string())
                })
                .unwrap();
            let save = PersonAccounts::SAVE_ACCOUNT
                .with((
                    "mail".into(),
                    "email".into(),
                    "Mail".into(),
                    serde_json::json!({}),
                    true,
                    0,
                ))
                .build(tilo_resource::origin_now_ms())
                .unwrap();
            record
                .edit(true, |doc| {
                    save.apply(doc).map_err(|error| error.to_string())
                })
                .unwrap();
            records.flush().unwrap();
            record.heads()
        };

        assert!(super::take_in::<PersonAccounts>(&former, &dir, &person).unwrap());
        assert!(
            !super::take_in::<PersonAccounts>(&former, &dir, &person).unwrap(),
            "once taken in, none is left to take"
        );
        assert!(
            records_in(&former, ACCOUNTS_OWNER)
                .open_existing_record(kind)
                .unwrap()
                .is_none(),
            "the session keeps them no more"
        );
        let records = tilo_app::framework::SettingsRecords::of_person(
            settings_sections::APP_ID,
            settings_sections::APP_TITLE,
            &[],
            records_in(&dir, settings_sections::APP_ID),
            None,
        );
        let accounts = records.shared::<PersonAccounts>().unwrap();
        assert!(accounts.with(|state| {
            state
                .accounts
                .get(&tilo_resource::Id::from_raw("mail"))
                .is_some()
        }));
        assert!(
            accounts
                .record()
                .read(|doc| kept
                    .iter()
                    .all(|head| doc.get_change_by_hash(head).is_some()))
                .unwrap(),
            "with their history"
        );
        assert_eq!(
            records.history_owners().len(),
            1,
            "synced over their route with the sections"
        );
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
