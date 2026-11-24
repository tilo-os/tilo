//! The device's one record reader: every read of a record, by whoever asks, decided by who
//! asks.
//!
//! A host is a device that stores more, and it answers every workspace it holds alike: one it
//! founded, the person's own, one it hosts. Another device reads for the certificate this device
//! holds for it in the workspace the record is in, decided by that workspace's governance, record
//! by record, as every member's read is: a device of the person's reads what the person may, an
//! agent's grant what its grants reach, in a workspace its scope reaches. A caller inside this
//! device (an app, the person signed in here, an agent acting for them) reads by the grants the
//! apps this device serves decide for it. Any other caller's read goes whole to the app action
//! server this host was given, when it was given one.
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
use tilo_actions::remote::{
    ActionCallError, Caller, KEY_GRANTS, PRESENCE, RESOURCE_READ_REFUSED, ResourceAddress,
    ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake,
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
}

/// Whether a caller reads the invocations of a workspace through a dispatch: the dispatch's own
/// workspace, as the run it admitted, still valid now.
#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub type Admitted = Arc<dyn Fn(&Caller, &str) -> Result<(), ActionCallError> + Send + Sync>;

/// Who reads a record for its caller.
enum Route {
    /// Another device, for the certificate this device holds for it there.
    Certified(Workspaces),
    /// A caller inside this device, by its grants.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    Inside(Arc<Broker>),
    /// The app action server this host was given.
    Served(Arc<dyn ResourceReads>),
}

fn refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: RESOURCE_READ_REFUSED.into(),
    }
}

impl RecordReads {
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
        }
    }

    /// The reader of the apps this device serves, for the callers inside it.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub(crate) fn of_apps(broker: std::sync::Weak<Broker>) -> Self {
        Self {
            workspaces: None,
            broker: Some(broker),
            served: None,
            admitted: None,
        }
    }

    /// The reader of a device's dispatch: the workspaces it holds, for a caller whose
    /// certificate a workspace keeps, and the apps `broker` serves, for every caller inside it;
    /// its invocations for the callers `admitted` admits.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub fn with_apps(
        workspaces: &Workspaces,
        broker: &Arc<Broker>,
        admitted: Admitted,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Some(WeakWorkspaces(Arc::downgrade(&workspaces.inner))),
            broker: Some(Arc::downgrade(broker)),
            served: None,
            admitted: Some(admitted),
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
        let broker = self.broker().ok_or_else(refused)?;
        let asked = self
            .workspaces
            .as_ref()
            .and_then(WeakWorkspaces::upgrade)
            .and_then(|workspaces| workspaces.asked_store())
            .map(|store| store.waiting_in(&address.workspace))
            .unwrap_or_default();
        broker.read_invocations(caller, &address.workspace, &address.record, asked)
    }

    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    fn broker(&self) -> Option<Arc<Broker>> {
        self.broker.as_ref().and_then(std::sync::Weak::upgrade)
    }

    /// The certificate this device holds for `caller`'s device in `workspace`, when it holds
    /// one that is the caller's own: signed by its person, and no stand-in.
    fn certificate(
        workspaces: &Workspaces,
        caller: &Caller,
        workspace: &str,
    ) -> Option<tilo_user_identity::DeviceCert> {
        let (person, device) = (*caller.user_public_key()?, *caller.device_public_key()?);
        let workspaces = workspaces.clone();
        let workspace = workspace.to_owned();
        let certificate = crate::device_documents::on_its_own(async move {
            workspaces
                .state(&workspace)
                .await?
                .certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == device)
        })?;
        (certificate.user_public_key == person
            && tilo_user_identity::verify_device_cert(&certificate).is_ok()
            && !certificate.capabilities.stand_in)
            .then_some(certificate)
    }

    /// Whether `caller`, for the device `certificate` names, may read `address` now: an agent's
    /// in a workspace its scope reaches, and every device's as the workspace's governance grants
    /// it the record. Every member's device sees who else is there, and a key is read as
    /// [`Self::reads_key`] decides. A call an app makes is decided by the grants the apps here
    /// give that app besides.
    fn may_read(
        &self,
        workspaces: &Workspaces,
        certificate: &tilo_user_identity::DeviceCert,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<(), ActionCallError> {
        #[cfg(not(all(feature = "apps", not(target_arch = "wasm32"))))]
        let _ = caller;
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if caller.acting().app().is_some()
            && let Some(broker) = self.broker()
        {
            broker.may_read_record(caller, address)?;
        }
        if certificate
            .capabilities
            .agent
            .as_ref()
            .is_some_and(|scope| !scope.reaches(&address.workspace))
        {
            return Err(refused());
        }
        if address.app == PRESENCE {
            return Ok(());
        }
        let workspaces = workspaces.clone();
        let certificate = certificate.clone();
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        crate::device_documents::on_its_own(async move {
            let state = workspaces
                .state(&workspace)
                .await
                .ok_or("this device does not hold the workspace".to_owned())?;
            let now = tilo_clock::wall();
            if app == KEY_GRANTS {
                return Self::reads_key(&state, &certificate, &record, now);
            }
            state.check_document_read(certificate.device_public_key, &app, &record, now)
        })
        .map_err(|_| refused())
    }

    /// Whether the device `certificate` names reads the key `grant` of `state`'s workspace now: a
    /// key its own grant and the grants it minted, a device of a person every grant of theirs,
    /// and whoever the workspace lets revoke its grants every one.
    fn reads_key(
        state: &StatePeer,
        certificate: &tilo_user_identity::DeviceCert,
        grant: &str,
        now: web_time::SystemTime,
    ) -> Result<(), String> {
        let own = WorkspaceGrant::id_of(&certificate.device_public_key);
        let person = hex::encode(certificate.user_public_key);
        let theirs = state
            .governance()
            .workspace_grants()
            .into_iter()
            .find(|held| held.id == grant)
            .is_some_and(|held| {
                held.id == own
                    || held.parent.as_deref() == Some(own.as_str())
                    || (certificate.capabilities.agent.is_none() && held.person == person)
            });
        if theirs
            || state
                .reads_every_key(certificate.device_public_key, now)
                .map_err(|error| error.to_string())?
        {
            Ok(())
        } else {
            Err(format!("the key {grant} is not this device's to read"))
        }
    }

    /// Who reads `address` for `caller`, decided already for another device.
    fn route(&self, caller: &Caller, address: &ResourceAddress) -> Result<Route, ActionCallError> {
        if let Some(workspaces) = self.workspaces.as_ref().and_then(WeakWorkspaces::upgrade)
            && let Some(certificate) = Self::certificate(&workspaces, caller, &address.workspace)
        {
            self.may_read(&workspaces, &certificate, caller, address)?;
            return Ok(Route::Certified(workspaces));
        }
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if let Some(broker) = self.broker() {
            return Ok(Route::Inside(broker));
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
            Route::Certified(workspaces) => {
                let mut here = Vec::new();
                super::PresentDevices::attach(&workspaces, &address.workspace, &mut here);
                Some(here)
            }
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            Route::Inside(broker) => broker.here(&caller.clone().in_workspace(&address.workspace)),
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
            Route::Certified(workspaces) => workspaces,
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            Route::Inside(broker) => return broker.read_record(caller, address),
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
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if address.app == tilo_actions::remote::workspace_invocations::PROVIDER
            && self.broker.is_some()
        {
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
            Route::Certified(workspaces) => workspaces,
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            Route::Inside(broker) => return broker.read_record(caller, address),
            Route::Served(served) => return served.read(caller, address),
        };
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        let bytes = crate::device_documents::on_its_own(async move {
            workspaces
                .resource_document(&workspace, &app, &record)
                .await
        })
        .map_err(|error| ActionCallError::Unavailable {
            reason: error.to_string(),
        })?;
        Ok(bytes.map_or(ResourceCopy::Absent, ResourceCopy::Document))
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
        let certificate = Self::certificate(&workspaces, caller, workspace).ok_or_else(refused)?;
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
                self.may_read(&workspaces, &certificate, caller, address)
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
            Route::Certified(_) => Err(ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            }),
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            Route::Inside(broker) => broker.store_read_record(caller, address, read),
            Route::Served(served) => served.store_read(caller, address, read),
        }
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
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
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            if let Route::Inside(broker) = route
                && let Some(changed) = broker.changed(
                    &caller.clone().in_workspace(&address.workspace),
                    tilo_actions::remote::WatchScope::Here,
                )
            {
                let wake = wake.clone();
                changed.then(move |_| wake.wake());
            }
            return Ok(());
        }
        let workspaces = match route {
            Route::Certified(workspaces) => workspaces,
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            Route::Inside(broker) => return broker.watch_record(caller, address, wake),
            Route::Served(served) => return served.watch(caller, address, wake),
        };
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ActionCallError::Unavailable {
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
