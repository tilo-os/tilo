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

use std::sync::Arc;

use collab_types::workspace_grant::WorkspaceGrant;
use tilo_action_policy::ActionKind;
use tilo_actions::remote::{
    ActionCallError, Caller, KEY_GRANTS, PRESENCE, RESOURCE_READ_REFUSED, RESOURCE_WRITE_REFUSED,
    ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake,
};
use tilo_peer_state::StatePeer;

use super::{WeakWorkspaces, Workspaces};

#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
type Broker = crate::apps::collab_app_broker::AppBrokerState;

/// The device's record reader.
pub struct RecordReads {
    /// The workspaces this device holds, read for another device's certificate there.
    workspaces: Option<WeakWorkspaces>,
    /// The apps this device serves itself, which decide a caller inside it by its grants.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    broker: Option<std::sync::Weak<Broker>>,
    /// The app action server this host was given, which reads for every other caller.
    served: Option<Arc<dyn ResourceReads>>,
    /// What admits a caller to the invocations of a dispatch's workspace: its admitted run.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    admitted: Option<Admitted>,
    /// The runtime the reader was made on, which a held read's wait for the next write runs on:
    /// a read is answered on a thread of its own, outside any runtime.
    runtime: Option<tokio::runtime::Handle>,
}

/// Whether a caller reads the invocations of a workspace through a dispatch: the dispatch's own
/// workspace, as the run it admitted, still valid now.
#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub type Admitted = Arc<dyn Fn(&Caller, &str) -> Result<(), ActionCallError> + Send + Sync>;

/// Who reads a record for its caller.
enum Route {
    /// A workspace this device holds, the read decided by its governance already.
    Held(Workspaces),
    /// The app action server this host was given.
    Served(Arc<dyn ResourceReads>),
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
pub(crate) enum AppReach {
    /// Its own records, the workspace's (a record of no app) and a record every app of the
    /// workspace opens: it reads and changes them as its caller's grants decide.
    Changes,
    /// Another app's record of a kind it declares it reads (`reads` in its manifest), or a record
    /// of the person's its manifest declares: read, live, and never changed. It changes another
    /// app's resources only by invoking that app's actions.
    Reads,
}

/// What `app` reaches of `address` by what its manifest declares: its own records, a record of no
/// app and one every app of the workspace opens ([`collab_types::WORKSPACE_DOCUMENTS`]) to read and
/// change; another app's record, read-only, where it declares that it reads that app's kinds. In
/// the person's own workspace (`own_workspace`) only the records of theirs it declares
/// ([`persons_record`]). Anything else is refused, saying why.
pub(crate) fn app_reach(
    app: &str,
    address: &ResourceAddress,
    own_workspace: bool,
) -> Result<AppReach, String> {
    if own_workspace {
        persons_record(app, address)?;
    }
    if address.app.is_empty() || address.app == app {
        return Ok(AppReach::Changes);
    }
    // A workspace record is changed from any app's window of its workspace; the workspace's
    // grants on its owner decide each change.
    if collab_types::shared_workspace_document(&address.app, &address.record).is_some() {
        return Ok(AppReach::Changes);
    }
    if !own_workspace && !collab_types::app_surface::reads_app(app, &address.app) {
        return Err(format!(
            "{app} declares no read of {}'s resources; it changes another app's resources only \
             by invoking its actions",
            address.app
        ));
    }
    Ok(AppReach::Reads)
}

/// Whether `app` reaches `address` in the person's own workspace: its own record of the
/// workspace naming no record, which holds none; one of the workspace's records, when it is the
/// record's app or declares that it reads that app's kinds; or a record of the person's whose
/// kind its manifest declares (its own, or another app's it reads). The person's workspace holds
/// nothing else for an app.
pub(crate) fn persons_record(app: &str, address: &ResourceAddress) -> Result<(), String> {
    let own = address.app.is_empty() || address.app == app;
    if own && address.record.is_empty() {
        return Ok(());
    }
    if collab_types::shared_workspace_document(&address.app, &address.record).is_some() {
        if own || collab_types::app_surface::reads_app(app, &address.app) {
            return Ok(());
        }
        return Err(format!(
            "{app} declares no read of {}'s resources",
            address.app
        ));
    }
    let kind = collab_types::person_resource_type(&address.record)
        .ok_or("an app reaches the person's workspace only for one of their records")?;
    if !collab_types::app_surface::keeps_person_resource(app, kind) {
        return Err(format!(
            "{app} declares no record of the person's of kind {kind}"
        ));
    }
    Ok(())
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
            tilo_actions::remote::workspace_invocations::PROVIDER,
        ]
        .contains(&address.app.as_str())
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
    pub(crate) fn of_device(
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
        }
    }

    /// The same reader, answering the workspace's invocations from `apps`' record: the
    /// invocations this device's app calls were asked for, read over a device link.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    #[must_use]
    pub(crate) fn with_invocations(mut self, apps: Option<std::sync::Weak<Broker>>) -> Self {
        self.broker = apps;
        self
    }

    /// The reader of a device's dispatch: the workspaces it holds, for a caller whose
    /// certificate a workspace keeps, and the apps `broker` serves, for every caller inside it;
    /// its invocations for the callers `admitted` admits, every caller's where it admits none.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub fn with_apps(
        workspaces: &Workspaces,
        broker: &Arc<Broker>,
        admitted: Option<Admitted>,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Some(WeakWorkspaces(Arc::downgrade(&workspaces.inner))),
            broker: Some(Arc::downgrade(broker)),
            served: None,
            admitted,
            runtime: tokio::runtime::Handle::try_current().ok(),
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

    /// The workspace's invocations ([`tilo_actions::remote::workspace_invocations::PROVIDER`]),
    /// whole or one by its id: the calls the dispatch here keeps a record of, each as the
    /// caller's grants let it read the call, with the calls that wait for a person's answer.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    fn invocations(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        self.may_read_invocations(caller, &address.workspace)?;
        let asked = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .and_then(|workspaces| workspaces.asked_store())
            .map(|store| store.waiting_in(&address.workspace))
            .unwrap_or_default();
        match self.broker() {
            Some(broker) => {
                broker.read_invocations(caller, &address.workspace, &address.record, asked)
            }
            // A host that runs no apps keeps no invocations of its own: what it reads is the
            // calls that wait here for the caller's own person, and nobody else's.
            None => {
                let person = caller.user_public_key().map(hex::encode);
                let asked = asked
                    .into_iter()
                    .filter(|waiting| Some(&waiting.person) == person.as_ref())
                    .collect();
                crate::apps::collab_app_broker::questions_reading(asked, &address.record)
            }
        }
    }

    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    fn broker(&self) -> Option<Arc<Broker>> {
        self.broker.as_ref().and_then(std::sync::Weak::upgrade)
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
    fn own(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Option<Result<Arc<dyn ResourceReads>, ActionCallError>> {
        let workspaces = self.workspaces.as_ref()?.upgrade()?;
        let (readers, reads) = workspaces.own_reading(&address.app)?;
        let device = workspaces.device_cert();
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
                == tilo_action_policy::Acting::person(hex::encode(device.user_public_key));
        if mine {
            return Some(Ok(reads));
        }
        match readers {
            super::OwnReaders::ThisDevice => Some(Err(refused())),
            super::OwnReaders::Workspace => {
                let admitted = Self::kept_here(&workspaces, &address.workspace)
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
    /// address names, by that workspace's governed policy: an app acting for a person, bounded by
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
            Some(broker) => broker.caller_actor(&decided, &address.workspace),
            None if decided.acting().app().is_some() => return Err(refused()),
            None => tilo_broker_core::actor::BrokerActor::for_caller(&decided),
        };
        let seconds = now
            .duration_since(web_time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if !Broker::may_act_in(&decided, &policy, seconds) {
            return Err(refused());
        }
        if address.app == PRESENCE {
            return Ok(());
        }
        let node = collab_types::document_node(&address.workspace, &address.app, &address.record);
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
    fn hand_out(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        address: &ResourceAddress,
        bytes: Option<Vec<u8>>,
    ) -> Result<ResourceCopy, ActionCallError> {
        let state = Self::kept_here(workspaces, &address.workspace).ok_or_else(refused)?;
        self.decides(&state, caller, address, ActionKind::Read)?;
        Ok(bytes.map_or(ResourceCopy::Absent, ResourceCopy::Document))
    }

    /// `workspace` as this device holds it, when it does.
    fn kept_here(workspaces: &Workspaces, workspace: &str) -> Option<StatePeer> {
        let (workspaces, workspace) = (workspaces.clone(), workspace.to_owned());
        crate::device_documents::on_its_own(async move { workspaces.state(&workspace).await })
    }

    /// Who reads `address` for `caller`: this device, for a workspace it holds, once that
    /// workspace's governance let the caller read it; the app action server, for any other.
    fn route(&self, caller: &Caller, address: &ResourceAddress) -> Result<Route, ActionCallError> {
        if let Some(workspaces) = self.workspaces.as_ref().and_then(WeakWorkspaces::upgrade)
            && let Some(state) = Self::kept_here(&workspaces, &address.workspace)
        {
            self.decides(&state, caller, address, ActionKind::Read)?;
            return Ok(Route::Held(workspaces));
        }
        self.served.clone().map(Route::Served).ok_or_else(refused)
    }

    /// Who is in `address`'s workspace now, as `route` hears it for `caller`: `{"here": [Here]}`.
    fn presence(
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
            Route::Served(served) => return served.read(caller, address),
        };
        let here = here.ok_or_else(refused)?;
        Ok(ResourceCopy::reading(
            serde_json::json!({ "here": here }),
            None,
        ))
    }

    /// The key `address` names, as `route` reads it for `caller`: its grant as the workspace's
    /// governance keeps it ([`WorkspaceGrant`]), absent where the workspace admitted no such key.
    fn key(
        route: Route,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.read(caller, address),
        };
        let (workspace, record) = (address.workspace.clone(), address.record.clone());
        let grant = crate::device_documents::on_its_own(async move {
            let state = workspaces.state(&workspace).await?;
            state
                .governance()
                .workspace_grants()
                .into_iter()
                .find(|grant| grant.id == record)
        });
        Ok(grant.map_or(ResourceCopy::Absent, |grant| {
            ResourceCopy::reading(serde_json::json!(grant), None)
        }))
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
    /// A record of a workspace this device holds, decided as its read is; any other is not read.
    fn may_read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::Decided> {
        if let Some(own) = self.own(caller, address) {
            return match own {
                Ok(reads) => reads.may_read(caller, address),
                Err(refusal) => tilo_actions::remote::Pending::ready(Err(refusal)),
            };
        }
        // An app keeps open only the records its manifest lets it reach.
        if let Err(refusal) = Self::declared(caller, address) {
            return tilo_actions::remote::Pending::ready(Err(refusal));
        }
        let state = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .and_then(|workspaces| Self::kept_here(&workspaces, &address.workspace));
        tilo_actions::remote::Pending::ready(match state {
            Some(state) => self.decides(&state, caller, address, ActionKind::Read),
            None => Err(refused()),
        })
    }

    /// A record of a workspace this device holds, changed as its write is decided there: an app
    /// changes its own records, and another app's only where its grants allow a change, so a
    /// grant that reads alone answers a refusal. A record of a workspace this device does not
    /// hold is not changed here.
    fn may_write(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::Decided> {
        // An app changes only the records its manifest lets it change: another app's it reads
        // at most, and changes through that app's actions.
        match Self::declared(caller, address) {
            Ok(AppReach::Changes) => {}
            Ok(AppReach::Reads) | Err(_) => {
                return tilo_actions::remote::Pending::ready(Err(write_refused()));
            }
        }
        let state = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .and_then(|workspaces| Self::kept_here(&workspaces, &address.workspace));
        tilo_actions::remote::Pending::ready(match state {
            Some(state) => self
                .decides(&state, caller, address, ActionKind::Undoable)
                .map_err(|decision| match decision {
                    ActionCallError::Refused { .. } => write_refused(),
                    other => other,
                }),
            None => Err(write_refused()),
        })
    }

    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if let Some(own) = self.own(caller, address) {
            return own?.read(caller, address);
        }
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if address.app == tilo_actions::remote::workspace_invocations::PROVIDER {
            return self.invocations(caller, address);
        }
        let route = self.route(caller, address)?;
        if address.app == PRESENCE {
            return Self::presence(route, caller, address);
        }
        if address.app == KEY_GRANTS {
            return Self::key(route, caller, address);
        }
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.read(caller, address),
        };
        let (held, workspace, app, record) = (
            workspaces.clone(),
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        let bytes = crate::device_documents::on_its_own(async move {
            held.resource_document(&workspace, &app, &record).await
        })
        .map_err(|error| ActionCallError::Unavailable {
            reason: error.to_string(),
        })?;
        self.hand_out(&workspaces, caller, address, bytes)
    }

    /// The resources of `workspace` the device `caller` holds a certificate for there may read:
    /// `app`'s, or every app's where `app` is empty, each decided as its read is.
    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
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
            return reads.list(caller, workspace, app);
        }
        let state = Self::kept_here(&workspaces, workspace).ok_or_else(refused)?;
        let listed = workspaces
            .own_listing(workspace)
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        Ok(listed
            .into_iter()
            .filter(|(of, _)| app.is_empty() || of == app)
            .map(|(app, record)| ResourceAddress {
                workspace: workspace.to_owned(),
                app,
                record,
            })
            .filter(|address| {
                self.decides(&state, caller, address, ActionKind::Read)
                    .is_ok()
            })
            .collect())
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        // A read the app action server answers goes to it whole, waits included.
        let address = ResourceAddress {
            workspace: read.workspace.clone(),
            app: read.app.clone(),
            record: read.record.clone(),
        };
        if self.own(caller, &address).is_some() {
            return tilo_actions::remote::answer_read(self, caller, correlation, read);
        }
        if let Ok(Route::Served(served)) = self.route(caller, &address) {
            return served.answer_read(caller, correlation, read);
        }
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn store_read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        match self.route(caller, address)? {
            Route::Held(_) => {
                // Decided already: the apps' store answers the read.
                #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
                if let Some(broker) = self.broker() {
                    return broker.store_read_record(caller, address, read);
                }
                Err(ActionCallError::Unavailable {
                    reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
                })
            }
            Route::Served(served) => served.store_read(caller, address, read),
        }
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        if let Some(own) = self.own(caller, address) {
            return own?.watch(caller, address, wake);
        }
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if address.app == tilo_actions::remote::workspace_invocations::PROVIDER
            && let Some(broker) = self.broker()
        {
            self.may_read_invocations(caller, &address.workspace)?;
            return broker.watch_invocations(caller, &address.workspace, wake);
        }
        let route = self.route(caller, address)?;
        if address.app == PRESENCE && !matches!(route, Route::Served(_)) {
            // Nothing is written when someone comes or goes: presence says when it moves, and the
            // roster when its membership does.
            super::personal::on_presence_change(wake);
            return Ok(());
        }
        let workspaces = match route {
            Route::Held(workspaces) => workspaces,
            Route::Served(served) => return served.watch(caller, address, wake),
        };
        let runtime = tokio::runtime::Handle::try_current()
            .ok()
            .or_else(|| self.runtime.clone())
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this device is not running".into(),
            })?;
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        // A grant is no document: it moves with the workspace's governance.
        if app == KEY_GRANTS {
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
            move |write| write.workspace == workspace && write.app == app && write.record == record,
            wake,
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "record_reads_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_reach_tests.rs"]
mod app_reach_tests;
