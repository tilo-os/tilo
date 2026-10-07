//! Registered reads over the host's verified retained source, never its peer cache.
use super::{AppCatalogue, Caller, DocumentRoute, RoomRecordsLink, ViewReadPolicy};
use std::{cell::RefCell, rc::Rc};
use tilo_actions::{
    ActionDescriptor, ActionOutput,
    door::{ActionAddress, DoorError},
};
use ui::{
    application::{
        ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    },
    application_view::InstanceViewStore,
    headless::FrameHarness,
    permissions::{PermissionContext, UserId},
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners},
};

const INSTANCE: u64 = 1;

pub(super) fn record_id(wire: &str) -> Result<RecordId, String> {
    if wire.is_empty() {
        Ok(RecordId::package_primary())
    } else {
        RecordId::parse(wire).ok_or_else(|| "invalid record route".into())
    }
}

pub(super) struct CanonicalRead {
    frame: FrameHarness,
    permissions: Rc<RefCell<PermissionContext>>,
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
        // under current certified Read and existing keys. The disposable cache
        // below carries that source; it never proves an uncommitted peer write.
        let source = link.view_document(workspace, caller, route)?;
        let definition = catalogue.get(&route.app_id).ok_or("unknown source app")?;
        let checkout =
            RecordCheckout::new(record_id(&route.record)?, definition.record_kind.clone());
        if definition.keeps_room_record() != checkout.id.is_package_primary() {
            return Err("source record scope mismatch".into());
        }
        // Hydrate existing history; a new-record initializer must never replace
        // its deterministic genesis actor with an arriving snapshot.
        let cache = PortableStorage::new("canonical-reader", "canonical-reader")?;
        let key = if checkout.id.is_package_primary() {
            StorageKey::Primary {
                package: definition.app_id.into(),
                workspace: route.cluster_id.clone(),
            }
        } else {
            StorageKey::Record {
                package: definition.app_id.into(),
                workspace: route.cluster_id.clone(),
                record: checkout.id.as_str().into(),
            }
        };
        cache.hydrate(
            key,
            source.bytes,
            Some(automerge::ActorId::random().to_bytes().to_vec()),
        )?;
        let records = RecordOwners::portable(cache.clone())
            .open(definition.app_id, &route.cluster_id)
            .map_err(|error| error.to_string())?;
        let views = Rc::new(
            InstanceViewStore::portable(cache, &route.cluster_id)
                .map_err(|error| error.to_string())?,
        );
        let permissions = Rc::new(RefCell::new(admitted.permissions()));
        let target = LaunchTarget::Record {
            checkout,
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views,
                device_id: "canonical-reader".into(),
                instance_id: INSTANCE,
                permissions: permissions.clone(),
                author: UserId(admitted.person.clone()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )
        .map_err(|error| error.to_string())?;
        let component = definition
            .instantiate(context, target, RestoreInput::None)
            .map_err(|error| error.to_string())?;
        let frame = FrameHarness::from_component(
            component,
            (1280, 720),
            ComponentRunnerInputs {
                theme: ui::Theme::default(),
                text_renderer: ui::TextRenderer::new(),
            },
        )
        .map_err(|error| error.to_string())?;
        let read = Self {
            frame,
            permissions,
            admitted,
            revision: source.revision,
        };
        read.recheck(link, workspace, caller, route)?;
        Ok(read)
    }

    pub(super) fn revision(&self) -> &str {
        &self.revision
    }

    pub(super) fn operations(&self) -> Vec<ActionDescriptor> {
        ui::semantic_document_read::registered_operations(
            self.frame.root_widget(),
            &self.frame.resources(),
        )
    }

    pub(super) fn recheck(
        &self,
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
        *self.permissions.borrow_mut() = fresh.permissions();
        Ok(())
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
        let started = self.frame.invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: address.clone(),
            },
            arguments,
        )?;
        let output = started.completed().ok_or_else(refused)?;
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
