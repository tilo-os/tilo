//! The record API of a device's action service: another device reads a record of a workspace
//! this device holds, as the certificate this device holds for it in that workspace may.
//!
//! A host is a device that stores more, and it answers every workspace it holds alike: one it
//! founded, the person's own, one it hosts. The read is decided there for the asking device's own
//! certificate, by that workspace's governance, record by record, as every member's read is: a
//! device of the person's reads what the person may, an agent's grant what its grants reach, in a
//! workspace its scope reaches. A caller this device holds no certificate for in the workspace
//! reads through the apps this device serves, when it serves any.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, RESOURCE_READ_REFUSED, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads, ResourceWake,
};

use super::{WeakWorkspaces, Workspaces};

/// The record API of a device's action service.
pub(crate) struct DeviceReads {
    pub(crate) workspaces: WeakWorkspaces,
    /// The apps this device serves, which read for callers it holds no certificate for.
    pub(crate) apps: Option<Arc<dyn ResourceReads>>,
}

fn refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: RESOURCE_READ_REFUSED.into(),
    }
}

impl DeviceReads {
    fn workspaces(&self) -> Result<Workspaces, ActionCallError> {
        self.workspaces
            .upgrade()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this device stopped".into(),
            })
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

    /// Who reads `address` for `caller`: the device this device holds a certificate for, or
    /// the apps, or nobody.
    fn decided(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<Option<Workspaces>, ActionCallError> {
        let workspaces = self.workspaces()?;
        match Self::certificate(&workspaces, caller, &address.workspace) {
            Some(certificate) => {
                Self::may_read(&workspaces, &certificate, address)?;
                Ok(Some(workspaces))
            }
            None if self.apps.is_some() => Ok(None),
            None => Err(refused()),
        }
    }
}

impl ResourceReads for DeviceReads {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let Some(workspaces) = self.decided(caller, address)? else {
            return self
                .apps
                .as_ref()
                .ok_or_else(refused)?
                .read(caller, address);
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
        // A caller the apps read for is answered by them whole, waits included.
        let address = ResourceAddress {
            workspace: read.workspace.clone(),
            app: read.app.clone(),
            record: read.record.clone(),
        };
        if let Ok(None) = self.decided(caller, &address)
            && let Some(apps) = self.apps.clone()
        {
            return apps.answer_read(caller, correlation, read);
        }
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        let Some(workspaces) = self.decided(caller, address)? else {
            return self
                .apps
                .as_ref()
                .ok_or_else(refused)?
                .watch(caller, address, wake);
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
