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

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, RESOURCE_READ_REFUSED, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads, ResourceWake,
};

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
}

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
        }
    }

    /// The reader of the apps this device serves, for the callers inside it.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub(crate) fn of_apps(broker: std::sync::Weak<Broker>) -> Self {
        Self {
            workspaces: None,
            broker: Some(broker),
            served: None,
        }
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

    /// Whether the device `certificate` names may read `address` now: an agent's in a workspace
    /// its scope reaches, and every device's as the workspace's governance grants it the record.
    fn may_read(
        workspaces: &Workspaces,
        certificate: &tilo_user_identity::DeviceCert,
        address: &ResourceAddress,
    ) -> Result<(), ActionCallError> {
        if certificate
            .capabilities
            .agent
            .as_ref()
            .is_some_and(|scope| !scope.reaches(&address.workspace))
        {
            return Err(refused());
        }
        let workspaces = workspaces.clone();
        let device = certificate.device_public_key;
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        crate::device_documents::on_its_own(async move {
            workspaces
                .state(&workspace)
                .await
                .ok_or("this device does not hold the workspace".to_owned())?
                .check_document_read(device, &app, &record, tilo_clock::wall())
        })
        .map_err(|_| refused())
    }

    /// Who reads `address` for `caller`, decided already for another device.
    fn route(&self, caller: &Caller, address: &ResourceAddress) -> Result<Route, ActionCallError> {
        if let Some(workspaces) = self.workspaces.as_ref().and_then(WeakWorkspaces::upgrade)
            && let Some(certificate) = Self::certificate(&workspaces, caller, &address.workspace)
        {
            Self::may_read(&workspaces, &certificate, address)?;
            return Ok(Route::Certified(workspaces));
        }
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        if let Some(broker) = self.broker.as_ref().and_then(std::sync::Weak::upgrade) {
            return Ok(Route::Inside(broker));
        }
        self.served.clone().map(Route::Served).ok_or_else(refused)
    }
}

impl ResourceReads for RecordReads {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let workspaces = match self.route(caller, address)? {
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
        let workspaces = match self.route(caller, address)? {
            Route::Certified(workspaces) => workspaces,
            #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
            Route::Inside(broker) => return broker.watch_record(caller, address, wake),
            Route::Served(served) => return served.watch(caller, address, wake),
        };
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ActionCallError::Unavailable {
                reason: "this device is not running".into(),
            })?;
        let mut writes = workspaces.document_writes();
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        let wake = wake.clone();
        runtime.spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write)
                        if write.workspace == workspace
                            && write.app == app
                            && write.record == record =>
                    {
                        wake.wake();
                        return;
                    }
                    Ok(_) => {}
                    // Writes went by unheard: one of them may be this record's.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        wake.wake();
                        return;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(())
    }
}
