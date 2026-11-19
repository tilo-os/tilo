//! The device's one store, as apps write and read their resources in it.
//!
//! An app holds no store of its own: the broker certifies the app and workspace its route names,
//! and this answers the command from the service that holds the workspace. A write goes through
//! the workspace's store and is committed as this device's signed write of every document it
//! changed; a read asks the store, which answers only what the device's person may read.

use tilo_resource_store::wire::{Command, Reply};

use super::services::{ResourceAnswer, ResourceRequestContext, ResourceServices};
use crate::service::ServiceSlot;
use crate::workspaces::Workspaces;

/// The store of the workspaces this device's service holds, as apps write and read in it.
#[derive(Clone)]
pub struct ServiceResources(ServiceSlot);

impl ServiceResources {
    /// The store of the workspaces held by the service whose embedder holds `service`, from the
    /// moment it is: a session's apps reach it without waiting for the service to open.
    #[must_use]
    pub const fn on(service: ServiceSlot) -> Self {
        Self(service)
    }

    /// The workspaces to answer from, once a service holds any.
    fn workspaces(&self) -> Option<Workspaces> {
        Some(self.0.service()?.workspaces().clone())
    }
}

impl ResourceServices for ServiceResources {
    fn command(&self, context: &ResourceRequestContext, command: Command) -> ResourceAnswer {
        let app = context.app_id().to_owned();
        let workspace = context.workspace_id().to_owned();
        let workspaces = self.workspaces();
        Box::pin(async move {
            let workspaces =
                workspaces.ok_or("this device's workspace service holds no workspaces")?;
            answer(&workspaces, &workspace, &app, command).await
        })
    }
}

async fn answer(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    command: Command,
) -> Result<Reply, String> {
    match command {
        Command::Put {
            kind,
            id,
            resource,
            stamp,
        } => {
            workspaces
                .write_resources(workspace, app, |store| {
                    store.put(&kind, &id, resource, &stamp)
                })
                .await?;
            Ok(Reply::Written)
        }
        Command::Remove { kind, id, stamp } => {
            workspaces
                .write_resources(workspace, app, |store| {
                    store
                        .remove(&kind, &id, &stamp)
                        .map(|written| vec![written])
                })
                .await?;
            Ok(Reply::Written)
        }
        Command::Undo { invocation, stamp } => {
            workspaces
                .write_resources(workspace, app, |store| store.undo(&invocation, &stamp))
                .await?;
            Ok(Reply::Written)
        }
        Command::Get { kind, id } => Ok(Reply::Resource(
            workspaces.resource_get(workspace, &kind, &id).await?,
        )),
        Command::Query(query) => Ok(Reply::Page(
            workspaces.resource_query(workspace, &query).await?,
        )),
        Command::Count { kind, filters } => Ok(Reply::Count(
            workspaces
                .resource_count(workspace, &kind, &filters)
                .await?,
        )),
    }
}
