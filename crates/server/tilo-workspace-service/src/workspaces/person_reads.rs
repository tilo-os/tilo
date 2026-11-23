//! Reads of the person's own workspace's records through the record API, answered by a host
//! that keeps that workspace: what a device of theirs reads from the moment it entered with its
//! ticket, while its own copy still fills in behind, and what an agent's grant of theirs reads of
//! it.
//!
//! The read is decided for the certificate the host holds for the asking device. A device of
//! the person's reads every record of their workspace. An agent's grant reads only the person's
//! list of workspaces ([`IDENTITY_LIST`]), and of it only the workspaces its scope reaches, as a
//! grant filters what an agent sees everywhere. Every other record, of every other workspace, is
//! the apps' this host serves, read as they read it.
//!
//! The list answers as a reading ([`PersonWorkspaces`]): each workspace by its id and the name the
//! person last saw it by, in their order, so every reader reads one shape whatever keeps it.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads,
    ResourceWake,
};
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{IDENTITY_LIST, ListedWorkspace, PersonWorkspaces};
use tilo_user_identity::DeviceCert;

use super::person_workspace;
use super::{WeakWorkspaces, Workspaces};

/// The record API of a host's device action service: the person's own workspace answered here,
/// every other record by the apps it serves.
pub(crate) struct PersonReads {
    pub(crate) workspaces: WeakWorkspaces,
    pub(crate) apps: Option<Arc<dyn ResourceReads>>,
}

/// Who reads the person's workspace.
enum Reading {
    /// A device of the person's: every record.
    Device,
    /// An agent's grant of theirs, whose scope is this certificate's.
    Agent(DeviceCert),
}

impl PersonReads {
    fn workspaces(&self) -> Result<Workspaces, ActionCallError> {
        self.workspaces
            .upgrade()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this host stopped".into(),
            })
    }

    /// Whether `address` is a record of `caller`'s person's own workspace: the reads answered
    /// here.
    fn own(caller: &Caller, address: &ResourceAddress) -> bool {
        caller
            .user_public_key()
            .is_some_and(|person| address.workspace == person_workspace_id(person))
    }

    /// Who `caller` reads `address` as, decided for the certificate this host holds for its
    /// device: in the person's workspace, which a device of theirs entered with its ticket, or
    /// in the workspace the caller was admitted to.
    fn reading(
        workspaces: &Workspaces,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<Reading, ActionCallError> {
        let refused = || ActionCallError::Refused {
            reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
        };
        let (Some(person), Some(device)) = (
            caller.user_public_key().copied(),
            caller.device_public_key().copied(),
        ) else {
            return Err(refused());
        };
        let admitted = caller.workspace().map(str::to_owned);
        let workspaces = workspaces.clone();
        let own = address.workspace.clone();
        let certificate = crate::device_documents::on_its_own(async move {
            for workspace in std::iter::once(own).chain(admitted) {
                if let Some(state) = workspaces.state(&workspace).await
                    && let Some(certificate) = state
                        .certificates()
                        .into_iter()
                        .find(|certificate| certificate.device_public_key == device)
                {
                    return Some(certificate);
                }
            }
            None
        })
        .ok_or_else(refused)?;
        if certificate.user_public_key != person
            || tilo_user_identity::verify_device_cert(&certificate).is_err()
            || certificate.capabilities.stand_in
        {
            return Err(refused());
        }
        if certificate.capabilities.agent.is_some() {
            if (address.app.as_str(), address.record.as_str()) != IDENTITY_LIST {
                return Err(refused());
            }
            return Ok(Reading::Agent(certificate));
        }
        Ok(Reading::Device)
    }

    /// `address`, a record of `person`'s own workspace, as `reading` reads it.
    fn copy(
        workspaces: &Workspaces,
        person: [u8; 32],
        reading: &Reading,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let workspaces = workspaces.clone();
        let (workspace, app, record) = (
            address.workspace.clone(),
            address.app.clone(),
            address.record.clone(),
        );
        if (app.as_str(), record.as_str()) == IDENTITY_LIST {
            let directory =
                crate::device_documents::on_its_own(
                    async move { workspaces.list_of(&person).await },
                );
            let reaches = |id: &str| match reading {
                Reading::Device => true,
                Reading::Agent(certificate) => certificate
                    .capabilities
                    .agent
                    .is_none_or(|scope| scope.reaches(id)),
            };
            let listed = PersonWorkspaces {
                workspaces: super::personal::ordered(&directory)
                    .into_iter()
                    .map(|kept| ListedWorkspace {
                        workspace: kept.id.to_string(),
                        name: kept.name,
                    })
                    .filter(|kept| reaches(&kept.workspace))
                    .collect(),
            };
            let value =
                serde_json::to_value(listed).map_err(|error| unavailable(error.to_string()))?;
            return Ok(ResourceCopy::reading(value, None));
        }
        let bytes = crate::device_documents::on_its_own(async move {
            person_workspace::document_on(&workspaces, &workspace, (&app, &record)).await
        })
        .map_err(unavailable)?;
        Ok(bytes.map_or(ResourceCopy::Absent, ResourceCopy::Document))
    }
}

impl ResourceReads for PersonReads {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if !Self::own(caller, address) {
            return match &self.apps {
                Some(apps) => apps.read(caller, address),
                None => Err(ActionCallError::Refused {
                    reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
                }),
            };
        }
        let workspaces = self.workspaces()?;
        let reading = Self::reading(&workspaces, caller, address)?;
        let person = *caller
            .user_public_key()
            .expect("a caller reading its own workspace names its person");
        Self::copy(&workspaces, person, &reading, address)
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        if !Self::own(caller, address) {
            return match &self.apps {
                Some(apps) => apps.watch(caller, address, wake),
                None => Err(ActionCallError::Refused {
                    reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
                }),
            };
        }
        let workspaces = self.workspaces()?;
        Self::reading(&workspaces, caller, address)?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ActionCallError::Unavailable {
                reason: "this host is not running".into(),
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
