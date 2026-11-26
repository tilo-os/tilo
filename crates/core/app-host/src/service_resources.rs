//! The device's one store, as apps write and read their resources in it.
//!
//! An app holds no store of its own: the broker certifies the app and workspace its route names,
//! and this answers the command from the service that holds the workspace. A write goes through
//! the workspace's store and is committed as this device's signed write of every document it
//! changed; a read asks the store, which answers only what the device's person may read.

use collab_types::store::wire::Command;

use super::services::{ResourceAnswer, ResourcePages, ResourceRequestContext, ResourceServices};
use tilo_workspace_service::workspaces::Workspaces;

/// The store of the workspaces this device's service holds, as apps write and read in it.
#[derive(Clone)]
pub struct ServiceResources {
    workspaces: Workspaces,
    /// The runtime the service makes its writes on.
    runtime: Option<tokio::runtime::Handle>,
}

impl ServiceResources {
    /// The store of the workspaces `service` keeps.
    #[must_use]
    pub fn of(service: &tilo_workspace_service::service::WorkspaceService) -> Self {
        Self {
            workspaces: service.workspaces().clone(),
            runtime: service.writes_runtime(),
        }
    }
}

/// `work` done on the service's writes thread, when it has one: a write holds the workspace's lock
/// while it waits, and the broker's thread that asked may block on that lock to answer a read, so
/// the write must not need that thread, or any other a read can block, to finish.
pub(super) async fn on_the_service<T: Send + 'static>(
    runtime: Option<tokio::runtime::Handle>,
    work: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    match runtime {
        Some(runtime) => runtime
            .spawn(work)
            .await
            .map_err(|_| "the workspace service stopped before it answered".to_owned())?,
        None => work.await,
    }
}

impl ResourceServices for ServiceResources {
    fn command(&self, context: &ResourceRequestContext, command: Command) -> ResourceAnswer {
        let context = context.clone();
        let workspaces = self.workspaces.clone();
        // A read answers where it is asked: it takes no lock a write holds, so the one thread
        // that makes the service's writes must not be the queue every read of every app waits
        // in.
        if !command.writes() {
            return Box::pin(async move {
                Box::pin(workspaces.resource_command(&context, command)).await
            });
        }
        Box::pin(on_the_service(self.runtime.clone(), async move {
            Box::pin(workspaces.resource_command(&context, command)).await
        }))
    }

    fn open(
        &self,
        context: &ResourceRequestContext,
        query: collab_types::store::Query,
    ) -> ResourcePages {
        let context = context.clone();
        let workspaces = self.workspaces.clone();
        Box::pin(async move { Box::pin(workspaces.open_resources(&context, query)).await })
    }
}
