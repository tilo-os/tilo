//! Registered reads over the host's verified retained source, never its peer cache.
//!
//! The policy checks run here, in the host; the component whose reads they
//! are runs in a confined app worker ([`crate::worker::ConfinedRead`]), which
//! is handed the retained source and the caller's current policy and nothing
//! else.
use super::{AppCatalogue, Caller, DocumentRoute, ViewReadPolicy, WorkspaceDocumentsLink};
use crate::worker::ConfinedRead;
use tilo_actions::{
    ActionDescriptor, ActionOutput,
    api::{ActionAddress, ApiError},
};
use tilo_server_protocol::WorkspaceId;
use ui::resource::DocumentId;

pub(super) fn document_id(wire: &str) -> Result<DocumentId, String> {
    DocumentId::from_route(wire).ok_or_else(|| "invalid record route".into())
}

pub(super) struct CanonicalRead {
    read: ConfinedRead,
    admitted: ViewReadPolicy,
    revision: String,
}

impl CanonicalRead {
    pub(super) fn prepare(
        catalogue: &AppCatalogue,
        link: &dyn WorkspaceDocumentsLink,
        workspace: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Result<Self, String> {
        let admitted = link
            .view_read_policy(workspace, caller, &route.app_id, &route.document)
            .filter(|policy| policy.workspace == route.cluster_id)
            .ok_or("canonical read not permitted")?;
        // This source is replayed from retained, signature-verified host storage
        // under current certified Read and existing keys. The worker's
        // disposable cache carries that source; it never proves an
        // uncommitted peer write.
        let source = link.view_document(workspace, caller, route)?;
        let definition = catalogue.get(&route.app_id).ok_or("unknown source app")?;
        let record = document_id(&route.document)?;
        if definition.keeps_workspace_resource() != record.is_package_primary() {
            return Err("source record scope mismatch".into());
        }
        let read = ConfinedRead::prepare(
            definition.app_id(),
            &route.cluster_id,
            &record,
            source.bytes,
            &admitted,
        )?;
        let mut read = Self {
            read,
            admitted,
            revision: source.revision,
        };
        read.recheck(link, workspace, caller, route)?;
        Ok(read)
    }

    /// Whether the record's component registers any reader.
    pub(super) fn registers(&mut self) -> Result<bool, String> {
        self.read.registers()
    }

    pub(super) fn revision(&self) -> &str {
        &self.revision
    }

    pub(super) fn operations(&mut self) -> Vec<ActionDescriptor> {
        self.read.operations().unwrap_or_default()
    }

    pub(super) fn recheck(
        &mut self,
        link: &dyn WorkspaceDocumentsLink,
        workspace: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Result<(), String> {
        let fresh = link
            .view_read_policy(workspace, caller, &route.app_id, &route.document)
            .ok_or("canonical read permission retired")?;
        if fresh.workspace != self.admitted.workspace
            || fresh.access != self.admitted.access
            || fresh.sandbox != self.admitted.sandbox
        {
            return Err("canonical read policy changed".into());
        }
        self.read.set_policy(&fresh)
    }

    pub(super) fn invoke(
        &mut self,
        link: &dyn WorkspaceDocumentsLink,
        workspace: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, ApiError> {
        let refused = || ApiError::Unknown(address.clone());
        self.recheck(link, workspace, caller, route)
            .map_err(|_| refused())?;
        if !self
            .operations()
            .iter()
            .any(|entry| &entry.address == address)
        {
            return Err(refused());
        }
        let output = self.read.invoke(address, arguments)?;
        self.recheck(link, workspace, caller, route)
            .map_err(|_| refused())?;
        if !self
            .operations()
            .iter()
            .any(|entry| &entry.address == address)
        {
            return Err(refused());
        }
        Ok(output)
    }
}
