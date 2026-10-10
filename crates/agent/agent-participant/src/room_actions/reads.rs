//! Registered reads over the host's verified retained source, never its peer cache.
//!
//! The policy checks run here, in the host; the component whose reads they
//! are runs in a confined app worker ([`crate::worker::ConfinedRead`]), which
//! is handed the retained source and the caller's current policy and nothing
//! else.
use super::{AppCatalogue, Caller, DocumentRoute, RoomRecordsLink, ViewReadPolicy};
use crate::worker::ConfinedRead;
use tilo_actions::{
    ActionDescriptor, ActionOutput,
    door::{ActionAddress, DoorError},
};
use ui::record::RecordId;

pub(super) fn record_id(wire: &str) -> Result<RecordId, String> {
    if wire.is_empty() {
        Ok(RecordId::package_primary())
    } else {
        RecordId::parse(wire).ok_or_else(|| "invalid record route".into())
    }
}

pub(super) struct CanonicalRead {
    read: ConfinedRead,
    admitted: ViewReadPolicy,
    revision: String,
}

impl CanonicalRead {
    pub(super) fn prepare(
        catalogue: &AppCatalogue,
        link: &dyn RoomRecordsLink,
        workspace: u64,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Result<Self, String> {
        let admitted = link
            .view_read_policy(workspace, caller, &route.app_id, &route.record)
            .filter(|policy| policy.room == route.cluster_id)
            .ok_or("canonical read not permitted")?;
        // This source is replayed from retained, signature-verified host storage
        // under current certified Read and existing keys. The worker's
        // disposable cache carries that source; it never proves an
        // uncommitted peer write.
        let source = link.view_document(workspace, caller, route)?;
        let definition = catalogue.get(&route.app_id).ok_or("unknown source app")?;
        let record = record_id(&route.record)?;
        if definition.keeps_room_record() != record.is_package_primary() {
            return Err("source record scope mismatch".into());
        }
        let read = ConfinedRead::prepare(
            definition.app_id,
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

    pub(super) fn revision(&self) -> &str {
        &self.revision
    }

    pub(super) fn operations(&mut self) -> Vec<ActionDescriptor> {
        self.read.operations().unwrap_or_default()
    }

    pub(super) fn recheck(
        &mut self,
        link: &dyn RoomRecordsLink,
        workspace: u64,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Result<(), String> {
        let fresh = link
            .view_read_policy(workspace, caller, &route.app_id, &route.record)
            .ok_or("canonical read permission retired")?;
        if fresh.room != self.admitted.room
            || fresh.person != self.admitted.person
            || fresh.role != self.admitted.role
            || fresh.caps != self.admitted.caps
        {
            return Err("canonical read policy changed".into());
        }
        self.read.set_policy(&fresh)
    }

    pub(super) fn invoke(
        &mut self,
        link: &dyn RoomRecordsLink,
        workspace: u64,
        caller: &Caller,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, DoorError> {
        let refused = || DoorError::Unknown(address.clone());
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
