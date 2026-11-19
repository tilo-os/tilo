//! Apps' resources, written and read in this device's one store ([`Engine::put_resource`] and
//! the reads beside it). An app holds no store of its own: it asks by kind and id over its
//! connection, the broker names the app and workspace its route certified, and this answers a
//! write as this device's person and a read of only what they may read.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_resource_store::wire::{Command, Reply};
use tilo_workspace_service::apps::services::{
    ResourceAnswer, ResourceRequestContext, ResourceServices,
};

use crate::engine::Engine;

/// The device's store, as the broker hands apps' resource commands to it.
pub(crate) struct AppResources(pub(crate) Arc<Engine>);

impl ResourceServices for AppResources {
    fn command(&self, context: &ResourceRequestContext, command: Command) -> ResourceAnswer {
        let engine = Arc::clone(&self.0);
        let app = context.app_id().to_owned();
        let workspace = WorkspaceId::parse(context.workspace_id());
        Box::pin(async move {
            let workspace = workspace.ok_or("the route names no workspace")?;
            answer(&engine, workspace, &app, command).await
        })
    }
}

async fn answer(
    engine: &Engine,
    workspace: WorkspaceId,
    app: &str,
    command: Command,
) -> Result<Reply, String> {
    let written = match command {
        Command::Put {
            kind,
            id,
            resource,
            stamp,
        } => {
            engine
                .put_resource(workspace, app, (&kind, &id), resource, &stamp)
                .await
        }
        Command::Remove { kind, id, stamp } => {
            engine
                .remove_resource(workspace, app, (&kind, &id), &stamp)
                .await
        }
        Command::Undo { invocation, stamp } => {
            engine
                .undo_resources(workspace, app, &invocation, &stamp)
                .await
        }
        Command::Get { kind, id } => {
            return Ok(Reply::Resource(
                engine.read_resources(workspace, |store, may_read| {
                    store.get(&kind, &id, may_read).cloned()
                }),
            ));
        }
        Command::Query(query) => {
            return Ok(Reply::Page(
                engine.read_resources(workspace, |store, may_read| store.query(&query, may_read)),
            ));
        }
        Command::Count { kind, filters } => {
            return Ok(Reply::Count(
                engine.read_resources(workspace, |store, may_read| {
                    store.count(&kind, &filters, may_read)
                }),
            ));
        }
    };
    written.map(|_| Reply::Written).map_err(String::from)
}
