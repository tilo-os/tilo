//! The device's one record reader: every read of a record, by whoever asks, decided by who
//! asks.
//!
//! A host is a device that stores more, and it answers every workspace it holds alike: one it
//! founded, the person's own, one it hosts. Every read is decided by the workspace the record is
//! in alone, through its governance as this device holds it ([`StatePeer`]): the caller as the
//! actor it is there (a person, an app acting for one, an agent by its key's grant), with the
//! smaller of the two grants where it acts for someone, and a device the workspace admitted by
//! its membership besides. The workspace a caller's connection was admitted to widens nothing.
//! A change of a record is decided the same way, with the write action kind: an app changes its
//! own records, and another app's only where its grants allow a change there.
//! A read of a workspace this device does not hold goes whole to the app action server this host
//! was given, when it was given one.
//!
//! Who is in a workspace ([`PRESENCE`]) is read here too, as the resource it is: what the
//! workspace's roster names, with the devices each is present on as presence on this device hears
//! them. It is never stored: a held read of it wakes when presence moves
//! ([`super::presence_changed`]) or the roster does.
//!
//! Each key a workspace admitted ([`KEY_GRANTS`]) is read here as well, a resource per grant: the
//! grant as the workspace's governance keeps it, woken when the governance moves. A key reads its
//! own grant and the grants it minted, a device of a person every grant of theirs, and whoever the
//! workspace lets revoke its grants every one.
//! Its members (`record:tilo.workspace:workspace/members`) read as the store answers them on the
//! device, decided as any record: every member reads them, anyone else is refused as for a
//! workspace this device does not keep.

use std::sync::Arc;

use collab_types::store::wire::{Command, Reply};
use collab_types::workspace_grant::WorkspaceGrant;
use tilo_action_types::ActionKind;
use tilo_actions::remote::{
    ActionCallError, Caller, DEVICE_JOBS, KEY_GRANTS, PRESENCE, RESOURCE_READ_REFUSED,
    RESOURCE_WRITE_REFUSED, ReadingAnswer, ReadsAnswer, RemoteActionMessage, ResourceAddress,
    ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake, WORKSPACE_GOVERNANCE_PROVIDER,
    parse_device_record_space,
};
use tilo_peer_state::StatePeer;
use tilo_server_protocol::workspaces::MEMBERSHIP_APP;

use super::{WeakWorkspaces, Workspaces};

#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
use super::app_invocations::{AppInvocations, WeakAppInvocations, may_act_at, questions_reading};

/// The engine jobs a device's daemon runs ([`DEVICE_JOBS`]): kept in the device's own record
/// space, read and followed through the one record API like any record. The daemon owns them, so
/// this is what it is handed to answers a read of them, and [`Self::decided_by`] is what the
/// reader above it decides by: the app's own read rule, not a rule of the jobs'.
pub trait DeviceJobReads: Send + Sync {
    /// The device whose jobs these are.
    fn device(&self) -> [u8; 32];

    /// The app whose read rule decides who reads job `job`, and where: the app's record of the
    /// workspace it runs in. `None` when the device runs no such job.
    fn decided_by(&self, job: &str) -> Option<ResourceAddress>;

    /// Job `address` as [`ResourceReads::read`] answers it.
    ///
    /// # Errors
    ///
    /// As [`ResourceReads::read`].
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError>;

    /// Whether `caller` may read job `address`, decided as [`Self::read`] is.
    ///
    /// # Errors
    ///
    /// As [`ResourceReads::read`].
    fn may_read(&self, caller: &Caller, address: &ResourceAddress) -> Result<(), ActionCallError>;

    /// The jobs of `workspace` `caller` may read.
    ///
    /// # Errors
    ///
    /// As [`ResourceReads::list`].
    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<tilo_actions::remote::ListedResource>, ActionCallError>;

    /// Wake `wake` when job `address` moves.
    ///
    /// # Errors
    ///
    /// As [`ResourceReads::watch`].
    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError>;

    /// Answer `read` of a job, as [`ResourceReads::answer_read`] does.
    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> ReadingAnswer;
}

/// Where another device's jobs are read, by the device record space its address names: the one
/// record API carried to that device ([`ResourceReads::answer_read`]), so a held read waits
/// there.
pub type OtherDeviceReads = Arc<dyn Fn(&str) -> Option<Arc<dyn ResourceReads>> + Send + Sync>;

/// The device's record reader.
pub struct RecordReads {
    /// The workspaces this device holds, read for another device's certificate there.
    workspaces: Option<WeakWorkspaces>,
    /// The apps this device serves itself, which decide a caller inside it by its grants.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    broker: Option<WeakAppInvocations>,
    /// The app action server this host was given, which reads for every other caller.
    served: Option<Arc<dyn ResourceReads>>,
    /// What admits a caller to the invocations of a dispatch's workspace: its admitted run.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    admitted: Option<Admitted>,
    /// The runtime the reader was made on, which a held read's wait for the next write and the
    /// routing of a read run on where whoever asks drives none.
    runtime: Option<tokio::runtime::Handle>,
    /// The engine jobs this device's daemon runs, when it published them: what reads and follows
    /// `devices/<this device>/jobs/<job>`.
    jobs: Option<Arc<dyn DeviceJobReads>>,
    /// Where another device's jobs are read, by the device space it names: the one record API
    /// carried to that device, forwarded whole so a held read waits there.
    other_devices: Option<OtherDeviceReads>,
}

/// Whether a caller reads the invocations of a workspace through a dispatch: the dispatch's own
/// workspace, as the run it admitted, still valid now.
#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub type Admitted = Arc<dyn Fn(&Caller, &str) -> Result<(), ActionCallError> + Send + Sync>;

/// Who answers a read of an engine job.
enum JobsRoute {
    /// This device's daemon, which runs the jobs.
    Own(Arc<dyn DeviceJobReads>),
    /// Another device, which runs the jobs its space names.
    Other(Arc<dyn ResourceReads>),
}

/// Who reads a record for its caller.
enum Route {
    /// A workspace this device holds, the read decided by its governance already.
    Held(Workspaces),
    /// The app action server this host was given.
    Served(Arc<dyn ResourceReads>),
}

/// The kind of the resource `address` names when the workspace builds it from its members and roles
/// rather than keeps it ([`super::resources::built_kind`]).
fn built_kind(address: &ResourceAddress) -> Option<&'static str> {
    super::resources::built_kind(&address.record)
        .filter(|_| address.app == WORKSPACE_GOVERNANCE_PROVIDER)
}

fn refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: RESOURCE_READ_REFUSED.into(),
    }
}

fn write_refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: RESOURCE_WRITE_REFUSED.into(),
    }
}

/// What a caller that may not read a device's own settings is refused with: who may.
fn device_refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: tilo_actions::remote::DEVICE_SETTINGS_REFUSED.into(),
    }
}

/// What an app reaches of a record by what its signed manifest declares, whatever its person's
/// grants there allow ([`app_reach`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppReach {
    /// Its own records, the workspace's (a record of no app) and a record every app of the
    /// workspace opens: it reads and changes them as its caller's grants decide.
    Changes,
    /// Another app's record of a kind it declares it reads (`reads` in its manifest), or a record
    /// of the person's its manifest declares: read, live, and never changed. It changes another
    /// app's resources only by invoking that app's actions.
    Reads,
}

/// What `app` reaches of `address` by what its manifest declares, in every workspace it is
/// installed in alike: its own records (its windows' among them), a record of no app and one every
/// app of the workspace opens ([`collab_types::WORKSPACE_DOCUMENTS`]) to read and change; another
/// app's record, read-only, where it declares that it reads that app's kinds, but never the view a
/// device keeps of one of its windows. The person's own workspace (`own_workspace`) holds their
/// records besides: an app reaches one only where its manifest declares its kind, and one of the
/// workspace's records only where it is the record's app or reads that app's kinds. Anything else
/// is refused, saying why.
pub fn app_reach(
    app: &str,
    address: &ResourceAddress,
    own_workspace: bool,
) -> Result<AppReach, String> {
    let own = address.app.is_empty() || address.app == app;
    let reads = || collab_types::app_surface::reads_app(app, &address.app);
    let workspace =
        collab_types::shared_workspace_document(&address.app, &address.record).is_some();
    if own_workspace
        && !workspace
        && !collab_types::document::DocumentId::is_window_record(&address.record)
        && let Some(kind) = collab_types::person_resource_type(&address.record)
    {
        if !collab_types::app_surface::keeps_person_resource(app, kind) {
            return Err(format!(
                "{app} declares no record of the person's of kind {kind}"
            ));
        }
        return Ok(if own {
            AppReach::Changes
        } else {
            AppReach::Reads
        });
    }
    if own {
        return Ok(AppReach::Changes);
    }
    // What a device keeps of another app's window is that window's, in any workspace.
    if collab_types::document::DocumentId::is_window_view(&address.record) {
        return Err(format!(
            "{app} reaches no view of {}'s windows",
            address.app
        ));
    }
    // A workspace record is changed from any app's window of its workspace; the workspace's
    // grants on its owner decide each change. The person's own are the apps' that read them.
    if workspace && (!own_workspace || reads()) {
        return Ok(AppReach::Changes);
    }
    if !reads() {
        return Err(format!(
            "{app} declares no read of {}'s resources; it changes another app's resources only \
             by invoking its actions",
            address.app
        ));
    }
    Ok(AppReach::Reads)
}

impl RecordReads {
    /// What an app caller reaches of `address` by what its app declares ([`app_reach`]): every
    /// caller that acts through no app, and a resource that is no app's record (who is here, a
    /// workspace's keys, its invocations), reach it as their grants decide.
    fn declared(caller: &Caller, address: &ResourceAddress) -> Result<AppReach, ActionCallError> {
        let Some(app) = caller.acting().app() else {
            return Ok(AppReach::Changes);
        };
        if [
            PRESENCE,
            KEY_GRANTS,
            tilo_action_services::workspace_invocations::PROVIDER,
        ]
        .contains(&address.app.as_str())
            || built_kind(address).is_some()
        {
            return Ok(AppReach::Changes);
        }
        let own_workspace = caller.user_public_key().is_some_and(|person| {
            tilo_server_protocol::servers::person_workspace_id(person) == address.workspace
        });
        app_reach(app.as_str(), address, own_workspace)
            .map_err(|reason| ActionCallError::Refused { reason })
    }

    /// The reader of a device's action service: the workspaces it holds, and the app action
    /// server it was given.
    pub fn of_device(
        workspaces: Option<WeakWorkspaces>,
        served: Option<Arc<dyn ResourceReads>>,
    ) -> Self {
        Self {
            workspaces,
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            broker: None,
            served,
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            admitted: None,
            runtime: tokio::runtime::Handle::try_current().ok(),
            jobs: None,
            other_devices: None,
        }
    }

    /// The same reader, answering the workspace's invocations from `apps`' record: the
    /// invocations this device's app calls were asked for, read over a device link.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    #[must_use]
    pub(crate) fn with_invocations(mut self, apps: Option<WeakAppInvocations>) -> Self {
        self.broker = apps;
        self
    }

    /// The reader of a daemon's dispatch that also runs engine jobs (`jobs`), reads another
    /// device's jobs through `other_devices`, and answers the apps `broker` serves.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub fn of_daemon(
        workspaces: &Workspaces,
        broker: &Arc<impl AppInvocations + 'static>,
        admitted: Option<Admitted>,
        jobs: Arc<dyn DeviceJobReads>,
        other_devices: OtherDeviceReads,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Some(WeakWorkspaces(Arc::downgrade(&workspaces.inner))),
            broker: Some(Arc::downgrade(broker) as WeakAppInvocations),
            served: None,
            admitted,
            runtime: tokio::runtime::Handle::try_current().ok(),
            jobs: Some(jobs),
            other_devices: Some(other_devices),
        })
    }

    /// Who answers a read of engine `address`, when it names one: this device's daemon for its own
    /// jobs space, and the device whose space it names for another's.
    fn jobs_route(&self, address: &ResourceAddress) -> Option<Result<JobsRoute, ActionCallError>> {
        if address.app != DEVICE_JOBS {
            return None;
        }
        let device = parse_device_record_space(&address.workspace)?;
        if let Some(jobs) = self.jobs.as_ref().filter(|jobs| jobs.device() == device) {
            return Some(Ok(JobsRoute::Own(Arc::clone(jobs))));
        }
        Some(
            self.other_devices
                .as_ref()
                .and_then(|other| other(&address.workspace))
                .map_or_else(|| Err(refused()), |reads| Ok(JobsRoute::Other(reads))),
        )
    }

    /// The reader of a device's dispatch: the workspaces it holds, for a caller whose
    /// certificate a workspace keeps, and the apps `broker` serves, for every caller inside it;
    /// its invocations for the callers `admitted` admits, every caller's where it admits none.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub fn with_apps(
        workspaces: &Workspaces,
        broker: &Arc<impl AppInvocations + 'static>,
        admitted: Option<Admitted>,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Some(WeakWorkspaces(Arc::downgrade(&workspaces.inner))),
            broker: Some(Arc::downgrade(broker) as WeakAppInvocations),
            served: None,
            admitted,
            runtime: tokio::runtime::Handle::try_current().ok(),
            jobs: None,
            other_devices: None,
        })
    }

    /// Whether `caller` reads `workspace`'s invocations: as the run a dispatch admitted, in its
    /// own workspace, where this reader is a dispatch's.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    fn may_read_invocations(
        &self,
        caller: &Caller,
        workspace: &str,
    ) -> Result<(), ActionCallError> {
        self.admitted
            .as_ref()
            .map_or(Ok(()), |admitted| admitted(caller, workspace))
    }

    /// The workspace's invocations ([`tilo_action_services::workspace_invocations::PROVIDER`]),
    /// whole or one by its id: the calls the dispatch here keeps a record of, each as the
    /// caller's grants let it read the call, with the calls that wait for a person's answer.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    async fn invocations(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        self.may_read_invocations(caller, &address.workspace)?;
        let broker = self.kept_invocations(&address.workspace).await;
        let asked = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .and_then(|workspaces| workspaces.asked_store())
            .map(|store| store.waiting_in(&address.workspace))
            .unwrap_or_default();
        if let Some(broker) = broker {
            broker.read_invocations(caller, &address.workspace, &address.record, asked)
        } else {
            // A host that runs no apps, or does not hold the workspace, keeps no invocations of
            // it: what it reads is the calls that wait here for the caller's own person, and
            // nobody else's.
            let person = caller.user_public_key().map(hex::encode);
            let asked = asked
                .into_iter()
                .filter(|waiting| Some(&waiting.person) == person.as_ref())
                .collect();
            questions_reading(asked, &address.record)
        }
    }

    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    fn broker(&self) -> Option<Arc<dyn AppInvocations>> {
        self.broker.as_ref().and_then(std::sync::Weak::upgrade)
    }

    /// What keeps `workspace`'s invocations here: the apps this reader serves, else the
    /// device's own record of the calls it was asked for, which its registry of providers holds
    /// (what the person's shell reads their runs through, a shortcut's that a trigger started
    /// among them). None for a workspace the device does not hold: it was asked for none of its
    /// calls and cannot decide a read of them by its grants.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    async fn kept_invocations(&self, workspace: &str) -> Option<Arc<dyn AppInvocations>> {
        let Some(workspaces) = self.workspaces.as_ref().and_then(WeakWorkspaces::upgrade) else {
            return self.broker();
        };
        let broker = self.broker().or_else(|| {
            workspaces
                .providers()
                .invocations()
                .and_then(|apps| apps.upgrade())
        })?;
        workspaces.state(workspace).await.map(|_| broker)
    }

    /// What answers `address` when it is a resource this device publishes of its own
    /// ([`Workspaces::publish_own`]): a resource of the person's own workspace, refused in every
    /// other. This device's own person, acting as themselves on this device, reads it. Where it is
    /// published for the workspace's readers ([`super::OwnReaders::Workspace`]), every other
    /// caller reads it as that workspace's read rules decide ([`Self::decides`]), exactly as they
    /// read any record of it: the person at a page of theirs, a member's view, an agent; otherwise
    /// nobody else does. Where it is published as its person's own records
    /// ([`super::OwnReaders::OwnRecords`]), it is the one policy decision at the record's node
    /// (a device's settings): its person reads it from any of their devices, and whoever they
    /// granted that setting reads it, in their own workspace.
    async fn own(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Option<Result<Arc<dyn ResourceReads>, ActionCallError>> {
        let workspaces = self.workspaces.as_ref()?.upgrade()?;
        let (readers, reads) = workspaces.own_reading(&address.app)?;
        let device = workspaces.device_cert();
        // A device's own space (`devices/<device hex>`) is read by the store that publishes it,
        // which decides at each of its records; no workspace's governance reaches it.
        if address.workspace == tilo_actions::remote::device_record_space(&device.device_public_key)
        {
            return Some(Ok(reads));
        }
        if address.workspace
            != tilo_server_protocol::servers::person_workspace_id(&device.user_public_key)
        {
            return Some(Err(refused()));
        }
        // The device its person sits at is this service's own: one key is the device's, so the
        // caller this device answers as is its person acting on it.
        let sitting = workspaces.signing_device().device();
        let mine = caller.user_public_key() == Some(&device.user_public_key)
            && caller.device_public_key() == Some(&sitting)
            && *caller.acting()
                == tilo_member_types::Acting::person(hex::encode(device.user_public_key));
        if mine {
            return Some(Ok(reads));
        }
        match readers {
            super::OwnReaders::ThisDevice => Some(Err(refused())),
            super::OwnReaders::Workspace => {
                let admitted = workspaces
                    .state(&address.workspace)
                    .await
                    .ok_or_else(refused)
                    .and_then(|state| self.decides(&state, caller, address, ActionKind::Read));
                Some(admitted.map(|()| reads))
            }
            // A device's settings, and any other record published as its workspace's own: the
            // one policy decision at the record's node decides for every caller.
            super::OwnReaders::OwnRecords => {
                let Some(workspace) = tilo_server_protocol::WorkspaceId::parse(&address.workspace)
                else {
                    return Some(Err(device_refused()));
                };
                let Some(copy) = workspaces.copy_now(workspace) else {
                    return Some(Err(device_refused()));
                };
                let Some(described) = super::describe(workspace, "", &copy.peer) else {
                    return Some(Err(device_refused()));
                };
                let node = super::server_controls::settings_node(
                    &address.workspace,
                    &device.device_public_key,
                    &address.record,
                );
                Some(
                    if super::server_controls::caller_may(
                        &described.policy,
                        caller,
                        &node,
                        ActionKind::Read,
                    ) {
                        Ok(reads)
                    } else {
                        Err(device_refused())
                    },
                )
            }
        }
    }

    /// Whether `caller` may take `kind` at `address` now, decided by the workspace the address
    /// names alone, through its governance as this device holds it (`state`). Its grants find the
    /// key the caller reads as: one revoked, expired or scoped to another workspace reads
    /// nothing. A device the workspace admitted acts as its membership lets it, within its
    /// certificate's scope, and every caller as the actor it is there, the smaller grant where it
    /// acts for someone. Every member sees who else is there, and a key is read as
    /// [`Self::reads_key`] decides.
    fn decides(
        &self,
        state: &StatePeer,
        caller: &Caller,
        address: &ResourceAddress,
        kind: ActionKind,
    ) -> Result<(), ActionCallError> {
        let now = tilo_clock::wall();
        let device = caller.device_public_key().copied();
        // A placed device's caller reads the records its certificate names in its one workspace,
        // decided by the certificate as its state link is, and nothing else: never as its person,
        // and never as whoever is signed in here.
        let placed = device.and_then(|device| state.placed_device(&device, now));
        if caller.placed_records().is_some() || placed.is_some() {
            let (Some(device), Some(Ok(placed))) = (device, placed) else {
                return Err(refused());
            };
            let channel = crate::environment::document_channel(&address.app, &address.record);
            let own_record = placed.records.reaches(&channel)
                && caller
                    .placed_records()
                    .is_none_or(|named| named.reaches(&channel));
            let own_workspace = state.workspace_id() == address.workspace
                && caller
                    .workspace()
                    .is_none_or(|admitted| admitted == address.workspace);
            if kind != ActionKind::Read
                || address.app == KEY_GRANTS
                || address.app == PRESENCE
                || !own_record
                || !own_workspace
            {
                return Err(refused());
            }
            return state
                .check_document_read(device, &address.app, &address.record, now)
                .map_err(|_| refused());
        }
        if let Some(device) = device {
            Self::key_reaches(state, &device, &address.workspace, now)?;
        }
        if address.app == KEY_GRANTS {
            return Self::reads_key(state, caller, &address.record, now).map_err(|_| refused());
        }
        let member = device.and_then(|device| {
            state
                .certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == device)
        });
        if let Some(certificate) = &member {
            if certificate
                .capabilities
                .agent
                .as_ref()
                .is_some_and(|scope| !scope.reaches(&address.workspace))
            {
                return Err(refused());
            }
            if address.app != PRESENCE {
                state
                    .check_document_read(
                        certificate.device_public_key,
                        &address.app,
                        &address.record,
                        now,
                    )
                    .map_err(|_| refused())?;
            }
        }
        self.actor_decides(state, caller, member.is_some(), address, kind, now)
    }

    /// Whether `caller` may take `kind` at `address` as the actor it is in the workspace the
    /// address names, by the workspace's policy: an app acting for a person, bounded by
    /// the kinds it declares, an agent for its person, each the smaller of its own grant and
    /// theirs. A caller inside this device that the workspace did not admit as a device reads as
    /// the apps here name it (the person signed in, or before anyone signs in, this device in the
    /// workspace it founded).
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    fn actor_decides(
        &self,
        state: &StatePeer,
        caller: &Caller,
        member: bool,
        address: &ResourceAddress,
        kind: ActionKind,
        now: web_time::SystemTime,
    ) -> Result<(), ActionCallError> {
        use tilo_action_policy::Answer;
        let workspace =
            tilo_server_protocol::WorkspaceId::parse(&address.workspace).ok_or_else(refused)?;
        let policy = super::describe(workspace, "", state).ok_or_else(refused)?;
        let broker = self.broker();
        let decided = match (&broker, member) {
            (Some(broker), false) => broker.reading_caller(caller, &address.workspace)?,
            _ => match caller.workspace() {
                Some(admitted) if admitted != address.workspace => {
                    caller.clone().in_workspace(&address.workspace)
                }
                _ => caller.clone(),
            },
        };
        // An app acts as the one actor the dispatch decides its calls as, bounded by what it
        // declares; a reader with no apps to say that reads nothing for an app.
        let actor = match &broker {
            Some(broker) => broker.caller_actor(&decided),
            None if decided.acting().app().is_some() => return Err(refused()),
            None => tilo_broker_core::actor::BrokerActor::for_caller(&decided),
        };
        let seconds = now
            .duration_since(web_time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        // Who reads a record takes part in the workspace there: an agent granted only its app too.
        let node = collab_types::document_node(&address.workspace, &address.app, &address.record);
        let presence = (address.app == PRESENCE).then(|| policy.policy.node());
        let at = presence.unwrap_or_else(|| node.clone());
        if !may_act_at(&decided, &policy, &at, seconds) {
            return Err(refused());
        }
        if address.app == PRESENCE {
            return Ok(());
        }
        match actor.answer_in(&policy, &node, kind, seconds) {
            Answer::Allow => Ok(()),
            // The person is asked once, where the action is asked for; it is never let by.
            Answer::Ask => Err(ActionCallError::confirmation_required()),
            Answer::Deny => Err(refused()),
        }
    }

    /// Without the apps' policy a device acts by its membership alone, and nothing else does.
    #[cfg(not(all(feature = "apps", not(target_arch = "wasm32"))))]
    fn actor_decides(
        &self,
        _state: &StatePeer,
        _caller: &Caller,
        member: bool,
        _address: &ResourceAddress,
        _kind: ActionKind,
        _now: web_time::SystemTime,
    ) -> Result<(), ActionCallError> {
        if member { Ok(()) } else { Err(refused()) }
    }

    /// Whether the key `device` reads as, as `state`'s workspace records its grant, still reaches
    /// `workspace`: live, unexpired and scoped to it. A device the workspace records no key of is
    /// decided by its membership alone.
    fn key_reaches(
        state: &StatePeer,
        device: &[u8; 32],
        workspace: &str,
        now: web_time::SystemTime,
    ) -> Result<(), ActionCallError> {
        let id = WorkspaceGrant::id_of(device);
        let now_ms = now
            .duration_since(web_time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        match state
            .governance()
            .workspace_grants()
            .into_iter()
            .find(|grant| grant.id == id)
        {
            Some(grant)
                if grant.revoked_ms.is_some()
                    || grant.expires_ms.is_some_and(|until| until <= now_ms)
                    || grant
                        .scope
                        .as_deref()
                        .is_some_and(|scope| scope != workspace) =>
            {
                Err(refused())
            }
            _ => Ok(()),
        }
    }

    /// Whether `caller` reads the key `grant` of `state`'s workspace now: a key its own grant and
    /// the grants it minted, a device of a person every grant of theirs, and whoever the
    /// workspace lets revoke its grants every one.
    fn reads_key(
        state: &StatePeer,
        caller: &Caller,
        grant: &str,
        now: web_time::SystemTime,
    ) -> Result<(), String> {
        let (Some(device), Some(person)) = (caller.device_public_key(), caller.user_public_key())
        else {
            return Err("only a device reads a workspace's keys".to_owned());
        };
        let own = WorkspaceGrant::id_of(device);
        let persons_device = state
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == *device)
            .is_some_and(|certificate| certificate.capabilities.agent.is_none());
        let person = hex::encode(person);
        let theirs = state
            .governance()
            .workspace_grants()
            .into_iter()
            .find(|held| held.id == grant)
            .is_some_and(|held| {
                held.id == own
                    || held.parent.as_deref() == Some(own.as_str())
                    || (persons_device && held.person == person)
            });
        if theirs
            || state
                .reads_every_key(*device, now)
                .map_err(|error| error.to_string())?
        {
            Ok(())
        } else {
            Err(format!("the key {grant} is not this device's to read"))
        }
    }

    /// `bytes`, the copy of `address` read for `caller`, handed out only once the workspace
    /// decided again that the caller may read it: a grant revoked while it was read cuts it.
    pub async fn hand_out(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        address: &ResourceAddress,
        bytes: Option<Vec<u8>>,
    ) -> Result<ResourceCopy, ActionCallError> {
        let copy = bytes.map_or(ResourceCopy::Absent, ResourceCopy::Document);
        self.handed(workspaces, caller, address, copy).await
    }

    /// `copy`, what `address` was read as for `caller`, handed out as [`Self::hand_out`] hands
    /// one out.
    async fn handed(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        address: &ResourceAddress,
        copy: ResourceCopy,
    ) -> Result<ResourceCopy, ActionCallError> {
        let state = workspaces
            .state(&address.workspace)
            .await
            .ok_or_else(refused)?;
        self.decides(&state, caller, address, ActionKind::Read)?;
        Ok(copy)
    }

    /// `read`, asked as `correlation`, answered for `caller` where it goes: a read the app action
    /// server answers goes to it whole, waits included, and every other is read here.
    async fn routed_read(
        self: Arc<Self>,
        caller: Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::RemoteActionMessage {
        let address = ResourceAddress {
            workspace: read.workspace.clone(),
            app: read.app.clone(),
            record: read.record.clone(),
        };
        // An engine job is answered whole where it runs, its wait with it.
        if let Some(route) = self.jobs_route(&address) {
            return match route {
                Ok(JobsRoute::Own(jobs)) => jobs.answer_read(&caller, correlation, read).await,
                Ok(JobsRoute::Other(reads)) => reads.answer_read(&caller, correlation, read).await,
                Err(error) => RemoteActionMessage::ResourceReading {
                    correlation,
                    result: Err(error),
                },
            };
        }
        // A workspace's invocations are the calls this dispatch here kept, not a document of the
        // workspace it may hold: read here, without asking that workspace's state where it goes.
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if address.app == tilo_action_services::workspace_invocations::PROVIDER
            && self.kept_invocations(&address.workspace).await.is_some()
        {
            return tilo_action_calls::answer_read(self, caller, correlation, read).await;
        }
        // A device placed for records of one workspace keeps no copy of it: its host reads them,
        // deciding the read as this device.
        if let Some(reads) = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .and_then(|workspaces| {
                workspaces
                    .providers()
                    .host_answering_every(&address.workspace)
            })
            .and_then(|host| host.resource_reads())
        {
            return reads.answer_read(&caller, correlation, read).await;
        }
        // What the person's desktop session publishes itself (Settings' sections) is read where it
        // is published: the seat answers its reading over its link.
        if let Some((seat, record)) = self.seat_publishing(&caller, &address).await {
            let read = ResourceReadRequest { record, ..read };
            return seat.answer_read(&caller, correlation, read).await;
        }
        if self.own(&caller, &address).await.is_some() {
            return tilo_action_calls::answer_read(self, caller, correlation, read).await;
        }
        if let Ok(Route::Served(served)) = self.route(&caller, &address).await {
            return served.answer_read(&caller, correlation, read).await;
        }
        tilo_action_calls::answer_read(self, caller, correlation, read).await
    }

    /// The seat's reader, and the resource it reads, when `address` names a resource a provider
    /// on this device lists under its app and `caller` is the person at this device: what their
    /// desktop session publishes itself. A resource a view names by the device's key
    /// (`devices/{device}/settings/power`) is read at this device's.
    async fn seat_publishing(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Option<(Arc<dyn ResourceReads>, String)> {
        let workspaces = self.workspaces.as_ref()?.upgrade()?;
        let providers = workspaces.providers();
        let reads = providers.seat()?.resource_reads()?;
        // At this device: its own key, or a program a seat here handed over, which only this
        // device's daemon marks so (the seat refuses another seat's).
        let here = workspaces.signing_device().device();
        if !caller.acting().is_person()
            || (caller.device_public_key() != Some(&here) && caller.handing_seat().is_none())
        {
            return None;
        }
        let record = address.record.replace(
            tilo_actions::remote::settings_resources::DEVICE_KEY,
            &hex::encode(here),
        );
        let query = tilo_actions::remote::CatalogueQuery {
            resource_name: Some(record.clone()),
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        for registered in providers.all().into_iter().filter(|registered| {
            *registered.app == *address.app
                && registered.reach == super::providers::Reach::ThisDevice
        }) {
            let listed = registered.answers.query(caller, query.clone()).await;
            if listed.is_some_and(|listed| {
                listed.is_ok_and(|page| page.rows.iter().any(|row| row.resource.id == record))
            }) {
                return Some((reads, record));
            }
        }
        None
    }

    /// The workspace `address` names as this device holds it, when it does and this reader still
    /// reaches the device's workspaces.
    async fn held_state(&self, address: &ResourceAddress) -> Option<StatePeer> {
        let workspaces = self.workspaces.as_ref()?.upgrade()?;
        workspaces.state(&address.workspace).await
    }

    /// Who reads `address` for `caller`: this device, for a workspace it holds, once that
    /// workspace's governance let the caller read it; the app action server, for any other.
    async fn route(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<Route, ActionCallError> {
        if let Some(workspaces) = self.workspaces.as_ref().and_then(WeakWorkspaces::upgrade)
            && let Some(state) = workspaces.state(&address.workspace).await
        {
            self.decides(&state, caller, address, ActionKind::Read)?;
            return Ok(Route::Held(workspaces));
        }
        self.served.clone().map(Route::Served).ok_or_else(refused)
    }

    /// Who is in `address`'s workspace now, as `route` hears it for `caller`: `{"here": [Here]}`.
    async fn presence(
        route: Route,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let here = match route {
            Route::Held(workspaces) => {
                let mut here = Vec::new();
                super::PresentDevices::attach(&workspaces, &address.workspace, &mut here);
                Some(here)
            }
            Route::Served(served) => return served.read(caller, address).await,
        };
        let here = here.ok_or_else(refused)?;
        Ok(ResourceCopy::reading(
            serde_json::json!({ "here": here }),
            None,
        ))
    }

    /// The key `address` names, as `route` reads it for `caller`: its grant as the workspace's
    /// governance keeps it ([`WorkspaceGrant`]), absent where the workspace admitted no such key.
    async fn key(
        route: Route,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.read(caller, address).await,
        };
        let grant = workspaces
            .state(&address.workspace)
            .await
            .and_then(|state| {
                state
                    .governance()
                    .workspace_grants()
                    .into_iter()
                    .find(|grant| grant.id == address.record)
            });
        Ok(grant.map_or(ResourceCopy::Absent, |grant| {
            ResourceCopy::reading(serde_json::json!(grant), None)
        }))
    }

    /// The membership `address` names, as `route` reads it for `caller`: the role the workspace's
    /// member list gives its person and when the workspace was made, both as its governance keeps
    /// them; absent where the list holds no such person.
    async fn membership(
        route: Route,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.read(caller, address).await,
        };
        let Some(person) = tilo_server_protocol::workspaces::member_of_record(&address.record)
        else {
            return Ok(ResourceCopy::Absent);
        };
        let membership = workspaces
            .state(&address.workspace)
            .await
            .and_then(|state| {
                let governance = state.governance();
                let role = governance.member_policy().ok()?.active_role(person)?;
                Some(serde_json::json!({
                    "role": role.key(),
                    "made_at_ms": governance.made_at_ms(),
                }))
            });
        Ok(membership.map_or(ResourceCopy::Absent, |membership| {
            ResourceCopy::reading(membership, None)
        }))
    }
}

impl RecordReads {
    /// [`ResourceReads::may_read`], decided as the workspace the address names is held here.
    async fn read_decision(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> tilo_actions::remote::Decided {
        if let Some(own) = self.own(caller, address).await {
            return own?.may_read(caller, address).await;
        }
        // An app keeps open only the records its manifest lets it reach.
        Self::declared(caller, address)?;
        match self.held_state(address).await {
            Some(state) => self.decides(&state, caller, address, ActionKind::Read),
            None => Err(refused()),
        }
    }

    /// [`ResourceReads::may_write`], decided as the workspace the address names is held here.
    async fn write_decision(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> tilo_actions::remote::Decided {
        // An app changes only the records its manifest lets it change: another app's it reads
        // at most, and changes through that app's actions.
        match Self::declared(caller, address) {
            Ok(AppReach::Changes) => {}
            Ok(AppReach::Reads) | Err(_) => return Err(write_refused()),
        }
        match self.held_state(address).await {
            Some(state) => self
                .decides(&state, caller, address, ActionKind::Undoable)
                .map_err(|decision| match decision {
                    ActionCallError::Refused { .. } => write_refused(),
                    other => other,
                }),
            None => Err(write_refused()),
        }
    }

    /// [`ResourceReads::read`]: the copy of `address`, wherever it is read for `caller`.
    async fn reading(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if let Some(own) = self.own(caller, address).await {
            return own?.read(caller, address).await;
        }
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if address.app == tilo_action_services::workspace_invocations::PROVIDER {
            return self.invocations(caller, address).await;
        }
        let route = self.route(caller, address).await?;
        if address.app == PRESENCE {
            return Self::presence(route, caller, address).await;
        }
        if address.app == KEY_GRANTS {
            return Self::key(route, caller, address).await;
        }
        if address.app == MEMBERSHIP_APP {
            return Self::membership(route, caller, address).await;
        }
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.read(caller, address).await,
        };
        if let Some(kind) = built_kind(address) {
            let (kind, id) = (kind.to_owned(), address.record.clone());
            let (reader, content) = (caller.user_public_key(), Command::Content { kind, id });
            let copy = match workspaces
                .resource_read(
                    &address.workspace,
                    &reader.map(hex::encode).unwrap_or_default(),
                    &content,
                )
                .await
                .map_err(|reason| ActionCallError::Unavailable { reason })?
            {
                Reply::Content(Some(value)) => ResourceCopy::reading(value, None),
                _ => ResourceCopy::Absent,
            };
            return self.handed(&workspaces, caller, address, copy).await;
        }
        let unavailable = |error: super::CollaborationError| ActionCallError::Unavailable {
            reason: error.to_string(),
        };
        let bytes = workspaces
            .resource_document(&address.workspace, &address.app, &address.record)
            .await
            .map_err(unavailable)?;
        // A record the store holds with nothing written to its content yet holds its fields: it
        // reads as them, never as a record nobody wrote.
        if bytes.is_none()
            && let Some(fields) = workspaces
                .resource_fields(&address.workspace, &address.app, &address.record)
                .await
                .map_err(unavailable)?
        {
            let fields = ResourceCopy::Read(serde_json::Value::Object(fields));
            return self.handed(&workspaces, caller, address, fields).await;
        }
        self.hand_out(&workspaces, caller, address, bytes).await
    }

    /// [`ResourceReads::list`]: what `caller` may read of `workspace`'s resources here.
    async fn listing(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<tilo_actions::remote::ListedResource>, ActionCallError> {
        let workspaces = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .ok_or_else(refused)?;
        // A resource this device publishes as its own lists through the reader that holds it,
        // which decides each record as its read is: a device's settings among them.
        if let Some((_, reads)) = workspaces.own_reading(app)
            && workspace
                == tilo_server_protocol::servers::person_workspace_id(
                    &workspaces.device_cert().user_public_key,
                )
        {
            return reads.list(caller, workspace, app).await;
        }
        let state = workspaces.state(workspace).await.ok_or_else(refused)?;
        let listed = workspaces
            .own_listing(workspace)
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        Ok(listed
            .into_iter()
            .filter(|listed| app.is_empty() || listed.address.app == app)
            .filter(|listed| {
                self.decides(&state, caller, &listed.address, ActionKind::Read)
                    .is_ok()
            })
            .collect())
    }

    /// [`ResourceReads::store_read`], asked of whoever reads `address` for `caller`.
    async fn store_reading(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        let route = self.route(caller, address).await?;
        match route {
            Route::Held(_) => {
                // Decided already: the apps' store answers the read.
                #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
                if let Some(broker) = self.broker() {
                    return broker.store_read_record(caller, address, read).await;
                }
                Err(ActionCallError::Unavailable {
                    reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
                })
            }
            Route::Served(served) => served.store_read(caller, address, read).await,
        }
    }

    /// [`ResourceReads::watch`], a held record's wait for its next write run on `runtime`.
    async fn watching(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
        runtime: Option<tokio::runtime::Handle>,
    ) -> Result<(), ActionCallError> {
        if let Some(own) = self.own(caller, address).await {
            return own?.watch(caller, address, wake).await;
        }
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if address.app == tilo_action_services::workspace_invocations::PROVIDER {
            self.may_read_invocations(caller, &address.workspace)?;
            if let Some(broker) = self.kept_invocations(&address.workspace).await {
                return broker.watch_invocations(caller, &address.workspace, wake);
            }
            // A reader that runs no apps keeps no invocation of its own: what it lists is the
            // calls that wait here for the caller's own person, from their asked records of this
            // app. It is woken by the next of those written, never by a read the workspace's
            // policy decides of a record this device keeps none of.
            let Some(workspaces) = self.workspaces.as_ref().and_then(WeakWorkspaces::upgrade)
            else {
                return Ok(());
            };
            let Some(runtime) = runtime.clone().or_else(|| self.runtime()) else {
                return Err(ActionCallError::Unavailable {
                    reason: "this device is not running".into(),
                });
            };
            let workspace = address.workspace.clone();
            wake_at(
                &runtime,
                workspaces.document_writes(),
                move |write| {
                    write.workspace == workspace
                        && write.app == tilo_action_services::workspace_invocations::PROVIDER
                },
                wake,
            );
            return Ok(());
        }
        let route = self.route(caller, address).await?;
        if address.app == PRESENCE && !matches!(route, Route::Served(_)) {
            // Nothing is written when someone comes or goes: presence says when it moves, and the
            // roster when its membership does.
            super::personal::on_presence_change(wake);
            return Ok(());
        }
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.watch(caller, address, wake).await,
        };
        let runtime = runtime.ok_or_else(|| ActionCallError::Unavailable {
            reason: "this device is not running".into(),
        })?;
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        // A grant, a membership or the members move with the workspace's members and roles.
        if app == KEY_GRANTS || app == MEMBERSHIP_APP || built_kind(address).is_some() {
            let changes = workspaces.governance_changes();
            wake_at(
                &runtime,
                changes,
                move |changed| *changed == workspace,
                wake,
            );
            return Ok(());
        }
        let writes = workspaces.document_writes();
        wake_at(
            &runtime,
            writes,
            // A record the store keeps is written as its content document.
            move |write| {
                write.workspace == workspace
                    && write.app == app
                    && (write.record == record
                        || tilo_resource_store::content_resource(&write.record)
                            .is_some_and(|(_, id)| id == record))
            },
            wake,
        );
        Ok(())
    }

    /// The runtime whoever asks drives, or else the one this reader was made on.
    fn runtime(&self) -> Option<tokio::runtime::Handle> {
        tokio::runtime::Handle::try_current()
            .ok()
            .or_else(|| self.runtime.clone())
    }
}

/// Wake `wake` at the first of `heard` that `matches`, or once some went by unheard: one of them
/// may have matched.
fn wake_at<T: Clone + Send + 'static>(
    runtime: &tokio::runtime::Handle,
    mut heard: tokio::sync::broadcast::Receiver<T>,
    matches: impl Fn(&T) -> bool + Send + 'static,
    wake: &ResourceWake,
) {
    let wake = wake.clone();
    runtime.spawn(async move {
        loop {
            match heard.recv().await {
                Ok(one) if matches(&one) => {
                    wake.wake();
                    return;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    wake.wake();
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

impl Workspaces {
    /// This device's reader of the resources of the workspaces it keeps, for a caller this device
    /// holds a certificate of there: what the person's shell reads through.
    #[must_use]
    pub fn resource_reads(&self) -> Arc<RecordReads> {
        Arc::new(RecordReads::of_device(
            Some(WeakWorkspaces(Arc::downgrade(&self.inner))),
            None,
        ))
    }
}

impl ResourceReads for RecordReads {
    // Every answer below reads the workspace as this device holds it, which waits for that
    // workspace's lock: each is awaited where it is asked, so a task holding the lock across an
    // await (a write of the workspace's records) is never waited on by the thread it needs.

    /// A record of a workspace this device holds, decided as its read is; any other is not read.
    fn may_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(async move {
            if let Some(route) = self.jobs_route(address) {
                return match route? {
                    JobsRoute::Own(jobs) => jobs.may_read(caller, address),
                    // The device that runs the job decides: the answer comes over its link, and a
                    // read that goes unanswered refuses rather than holding the asker forever.
                    JobsRoute::Other(reads) => tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        reads.may_read(caller, address),
                    )
                    .await
                    .unwrap_or_else(|_| Err(refused())),
                };
            }
            self.read_decision(caller, address).await
        })
    }

    /// A record of a workspace this device holds, changed as its write is decided there: an app
    /// changes its own records, and another app's only where its grants allow a change, so a
    /// grant that reads alone answers a refusal. A record of a workspace this device does not
    /// hold is not changed here.
    fn may_write<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(self.write_decision(caller, address))
    }

    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        Box::pin(async move {
            if let Some(route) = self.jobs_route(address) {
                return match route? {
                    JobsRoute::Own(jobs) => jobs.read(caller, address),
                    JobsRoute::Other(reads) => reads.read(caller, address).await,
                };
            }
            self.reading(caller, address).await
        })
    }

    /// The resources of `workspace` the device `caller` holds a certificate for there may read:
    /// `app`'s, or every app's where `app` is empty, each decided as its read is.
    fn list<'a>(
        &'a self,
        caller: &'a Caller,
        workspace: &'a str,
        app: &'a str,
    ) -> ReadsAnswer<'a, Vec<tilo_actions::remote::ListedResource>> {
        Box::pin(async move {
            if app == DEVICE_JOBS
                && let Some(route) = self.jobs_route(&ResourceAddress {
                    workspace: workspace.to_owned(),
                    app: DEVICE_JOBS.to_owned(),
                    record: String::new(),
                })
            {
                return match route? {
                    JobsRoute::Own(jobs) => jobs.list(caller, workspace, app),
                    JobsRoute::Other(reads) => reads.list(caller, workspace, app).await,
                };
            }
            self.listing(caller, workspace, app).await
        })
    }

    /// Where the read goes is decided by the workspace as this device holds it: a read the app
    /// action server answers goes to it whole, waits included, and every other is read here.
    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> ReadingAnswer {
        Box::pin(self.routed_read(caller.clone(), correlation, read))
    }

    fn store_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        read: serde_json::Value,
    ) -> ReadsAnswer<'a, serde_json::Value> {
        Box::pin(self.store_reading(caller, address, read))
    }

    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(async move {
            if let Some(route) = self.jobs_route(address) {
                return match route? {
                    JobsRoute::Own(jobs) => jobs.watch(caller, address, wake),
                    JobsRoute::Other(reads) => reads.watch(caller, address, wake).await,
                };
            }
            self.watching(caller, address, wake, self.runtime()).await
        })
    }
}

#[cfg(test)]
#[path = "jobs_reads_tests.rs"]
mod jobs_reads_tests;

#[cfg(test)]
#[path = "app_reach_tests.rs"]
mod app_reach_tests;
