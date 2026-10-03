//! What the app publishes to search, the palette and agents: the engine's
//! containers, images, volumes and distroboxes as resources with their
//! operations, and the handler that answers those operations.

use automerge::{AutoCommit, ObjId};
use ui::tilo_actions::{ActionSpec, OperationKind, PermissionRequirement, Resource, RetryPolicy};

use crate::app::{ContainersApp, ContinuationRuntimeIdentity};
use crate::doc::{DetailTab, Section};
use crate::podman::{self, Action};

pub(crate) const RESOURCE_SNAPSHOT_LIMIT: usize = 200;

pub(crate) fn container_resource(
    container: &crate::model::Container,
    engine_live: bool,
    continuation: Option<&ContinuationRuntimeIdentity>,
) -> Resource {
    let mut resource = Resource::new(
        format!("container:{}", container.id),
        "containers.container",
        container.name.clone(),
    );
    let selection = vec!["select".to_string(), container.id.clone()];
    if let Ok(spec) = ActionSpec::operation(
        "select",
        format!("Open {}", container.name),
        OperationKind::View,
        "containers.container",
        &selection,
    ) {
        resource.actions.push(spec);
    }
    let verbs: &[(&str, &str)] = match container.status {
        crate::model::ContainerStatus::Running | crate::model::ContainerStatus::Restarting => &[
            ("stop", "Stop"),
            ("restart", "Restart"),
            ("pause", "Pause"),
            ("shell", "Open shell"),
        ],
        crate::model::ContainerStatus::Paused => &[
            ("unpause", "Resume"),
            ("stop", "Stop"),
            ("remove", "Remove"),
            ("shell", "Open shell"),
        ],
        _ => &[("start", "Start"), ("remove", "Remove")],
    };
    for (verb, label) in verbs {
        let payload = vec![(*verb).to_string(), container.id.clone()];
        if let Ok(spec) = ActionSpec::operation(
            *verb,
            *label,
            OperationKind::Effect,
            "containers.container",
            &payload,
        ) {
            let mut spec = spec.requiring(PermissionRequirement::Admin).privileged();
            if *verb == "restart"
                && let Some(identity) = continuation
                && identity.target_id == container.id
            {
                spec = with_restart_continuation(spec, identity.clone());
            }
            if *verb == "remove" {
                spec = spec.destructive();
            } else if *verb != "restart" && *verb != "shell" {
                spec = spec.retry_policy(RetryPolicy::Idempotent);
            }
            if !engine_live {
                spec = spec.unavailable(
                    "engine_offline",
                    "Container engine actions are unavailable in demo mode",
                );
            }
            resource.actions.push(spec);
        }
    }
    let mut logs = Resource::new(
        format!("container:{}:logs", container.id),
        "containers.logs",
        format!("{} logs", container.name),
    );
    if let Ok(spec) = ActionSpec::operation(
        "open",
        "Open logs",
        OperationKind::View,
        "containers.logs",
        &["logs", container.id.as_str()],
    ) {
        logs.actions.push(spec);
    }
    resource.children.push(logs);
    resource
}

/// Declare the controlled-runtime continuation identity of a `restart` spec.
fn with_restart_continuation(
    spec: ActionSpec,
    identity: ContinuationRuntimeIdentity,
) -> ActionSpec {
    spec.with_continuation_identity(move |query| {
        let target = ui::tilo_actions::ContinuationKey::new(
            "containers.controlled.target",
            [
                identity.runtime_epoch.to_be_bytes().to_vec(),
                identity.target_epoch.to_be_bytes().to_vec(),
                identity.target_id.as_bytes().to_vec(),
            ],
        );
        let handler = ui::tilo_actions::ContinuationKey::new(
            "containers.controlled.handler",
            [b"restart-v1".to_vec()],
        );
        Some(match query {
            ui::tilo_actions::ContinuationQuery::Exact(_) => {
                ui::tilo_actions::ContinuationEvidence::Exact { target, handler }
            }
            ui::tilo_actions::ContinuationQuery::Draft => {
                ui::tilo_actions::ContinuationEvidence::Draft {
                    resolution_domain: target,
                    handler,
                }
            }
        })
    })
}

fn image_resource(image: &crate::model::Image, engine_live: bool) -> Resource {
    let reference = image.reference();
    let mut resource = Resource::new(
        format!("image:{reference}"),
        "containers.image",
        reference.clone(),
    );
    if let Ok(spec) = ActionSpec::operation(
        "open",
        "Show image",
        OperationKind::View,
        "containers.image",
        &["open", reference.as_str()],
    ) {
        resource.actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::operation(
        "remove",
        "Remove image",
        OperationKind::Effect,
        "containers.image",
        &["remove", reference.as_str()],
    ) {
        let mut spec = spec
            .requiring(PermissionRequirement::Admin)
            .privileged()
            .destructive();
        if !engine_live {
            spec = spec.unavailable(
                "engine_offline",
                "Container engine actions are unavailable in demo mode",
            );
        }
        resource.actions.push(spec);
    }
    resource
}

fn volume_resource(volume: &crate::model::Volume, engine_live: bool) -> Resource {
    let mut resource = Resource::new(
        format!("volume:{}", volume.name),
        "containers.volume",
        volume.name.clone(),
    );
    if let Ok(spec) = ActionSpec::operation(
        "open",
        "Show volume",
        OperationKind::View,
        "containers.volume",
        &["open", volume.name.as_str()],
    ) {
        resource.actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::operation(
        "remove",
        "Remove volume",
        OperationKind::Effect,
        "containers.volume",
        &["remove", volume.name.as_str()],
    ) {
        let mut spec = spec
            .requiring(PermissionRequirement::Admin)
            .privileged()
            .destructive();
        if !engine_live {
            spec = spec.unavailable(
                "engine_offline",
                "Container engine actions are unavailable in demo mode",
            );
        }
        resource.actions.push(spec);
    }
    resource
}

fn distrobox_resource(distrobox: &crate::model::DistroBox, engine_live: bool) -> Resource {
    let mut resource = Resource::new(
        format!("distrobox:{}", distrobox.name),
        "containers.distrobox",
        distrobox.name.clone(),
    );
    if let Ok(spec) = ActionSpec::operation(
        "open",
        "Show distrobox",
        OperationKind::View,
        "containers.distrobox",
        &["open", distrobox.name.as_str()],
    ) {
        resource.actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::operation(
        "enter",
        "Enter distrobox",
        OperationKind::Effect,
        "containers.distrobox",
        &["enter", distrobox.name.as_str()],
    ) {
        let mut spec = spec.requiring(PermissionRequirement::Admin).privileged();
        if !engine_live {
            spec = spec.unavailable(
                "engine_offline",
                "Distrobox actions are unavailable in demo mode",
            );
        }
        resource.actions.push(spec);
    }
    resource
}

impl ContainersApp {
    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`]
    /// with live engine children overlaid.
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Containers::read(doc).unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Containers>(
            "containers",
            "Containers",
            &state,
        );
        if let Some(create) = root.actions.iter_mut().find(|action| action.id == "create")
            && !self.engine.live
        {
            *create = create.clone().unavailable(
                "engine_offline",
                "Creating containers requires a connected Podman engine",
            );
        }
        root.children = self.engine_child_resources();
        vec![root]
    }

    /// Live engine rows (and an in-flight pull) without a `containers.engine` root.
    pub(crate) fn live_engine_extras(&self) -> Vec<Resource> {
        let mut extras = self.engine_child_resources();
        if let Some(pull) = &self.pull {
            extras.push(Resource::new(
                "containers.pull-job",
                "containers.job",
                format!("Pulling {}", pull.reference),
            ));
        }
        extras
    }

    fn engine_child_resources(&self) -> Vec<Resource> {
        let mut children = Vec::new();
        for container in &self.engine.containers {
            if children.len() >= RESOURCE_SNAPSHOT_LIMIT {
                break;
            }
            children.push(container_resource(
                container,
                self.engine.live,
                self.continuation_runtime_identity.as_ref(),
            ));
        }
        for image in &self.engine.images {
            if children.len() >= RESOURCE_SNAPSHOT_LIMIT {
                break;
            }
            children.push(image_resource(image, self.engine.live));
        }
        for volume in &self.engine.volumes {
            if children.len() >= RESOURCE_SNAPSHOT_LIMIT {
                break;
            }
            children.push(volume_resource(volume, self.engine.live));
        }
        for distrobox in &self.engine.distroboxes {
            if children.len() >= RESOURCE_SNAPSHOT_LIMIT {
                break;
            }
            children.push(distrobox_resource(distrobox, self.engine.live));
        }
        children
    }
}

impl ContainersApp {
    pub(crate) fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let Some(verb) = args.first().map(String::as_str) else {
            return Err("missing operation".into());
        };
        let target = args.get(1).map(String::as_str).unwrap_or_default();
        if kind == OperationKind::View && type_id == "containers.engine" && verb == "create" {
            self.set_create_open(true);
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "containers.container" && verb == "select" {
            self.pending_resource_selection = Some(target.to_string());
            return Ok(true);
        }
        if kind == OperationKind::View && type_id == "containers.logs" && verb == "logs" {
            self.pending_resource_selection = Some(target.to_string());
            self.pending_resource_section = Some(Section::Containers);
            self.pending_resource_tab = Some(DetailTab::Logs);
            return Ok(true);
        }
        if kind == OperationKind::View && verb == "open" {
            self.pending_resource_section = Some(match type_id {
                "containers.image" => Section::Images,
                "containers.volume" => Section::Volumes,
                "containers.distrobox" => Section::Distrobox,
                _ => return Ok(false),
            });
            return Ok(true);
        }
        if kind != OperationKind::Effect {
            return Ok(false);
        }
        if !self.engine.live {
            return Err("container engine actions are unavailable while Podman is offline".into());
        }
        #[cfg(test)]
        if let Some(result) = self.controlled_immediate(
            format!("{type_id}.{verb}"),
            target.to_string(),
            args.clone(),
        ) {
            result?;
            self.spawn_refresh();
            self.show_toast(self.done_message(type_id, verb, target), false);
            return Ok(true);
        }
        match type_id {
            "containers.container" => match verb {
                "start" => {
                    podman::run_action(Action::Start, target)?;
                }
                "stop" => {
                    podman::run_action(Action::Stop, target)?;
                }
                "restart" => {
                    podman::run_action(Action::Restart, target)?;
                }
                "pause" => {
                    podman::run_action(Action::Pause, target)?;
                }
                "unpause" => {
                    podman::run_action(Action::Unpause, target)?;
                }
                "remove" => {
                    podman::run_action(Action::Remove, target)?;
                }
                "shell" => {
                    crate::terminal::spawn_exec_attach(target, "/bin/sh")
                        .map_err(|e| e.to_string())?;
                }
                _ => return Ok(false),
            },
            "containers.image" if verb == "remove" => {
                podman::remove_image(target, true)?;
            }
            "containers.volume" if verb == "remove" => {
                podman::remove_volume(target, false)?;
            }
            "containers.distrobox" if verb == "enter" => {
                crate::terminal::spawn_distrobox_enter(target).map_err(|e| e.to_string())?;
            }
            _ => return Ok(false),
        }
        self.spawn_refresh();
        self.show_toast(self.done_message(type_id, verb, target), false);
        Ok(true)
    }

    /// How a finished engine operation reads in a toast.
    fn done_message(&self, type_id: &str, verb: &str, target: &str) -> String {
        match (type_id, verb) {
            ("containers.container", "shell") => {
                format!("Opened a shell in {}", self.container_name_of(target))
            }
            ("containers.container", verb) => {
                let action = match verb {
                    "start" => Action::Start,
                    "stop" => Action::Stop,
                    "restart" => Action::Restart,
                    "pause" => Action::Pause,
                    "unpause" => Action::Unpause,
                    _ => Action::Remove,
                };
                format!(
                    "{} {}",
                    crate::app::past_tense(action),
                    self.container_name_of(target)
                )
            }
            ("containers.distrobox", _) => format!("Entered {target}"),
            (_, _) => format!("Removed {target}"),
        }
    }

    fn container_name_of(&self, id: &str) -> String {
        self.engine
            .find(id)
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    }
}

impl ui::resources::PublishesResources for ContainersApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the engine door — keep only live engine rows and pull jobs.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        if self.is_hosted() && id == "containers" {
            return None;
        }
        if let Some(found) = find(&self.publish_resources(doc), id) {
            return Some(found);
        }
        if let Some(found) = find(&self.resources(doc), id) {
            return Some(found);
        }
        if let Some(container_id) = id
            .strip_prefix("container:")
            .and_then(|rest| rest.strip_suffix(":logs"))
        {
            let container = self
                .engine
                .containers
                .iter()
                .find(|item| item.id == container_id)?;
            return container_resource(
                container,
                self.engine.live,
                self.continuation_runtime_identity.as_ref(),
            )
            .children
            .into_iter()
            .find(|child| child.id.0 == id);
        }
        if let Some(container_id) = id.strip_prefix("container:") {
            return self
                .engine
                .containers
                .iter()
                .find(|item| item.id == container_id)
                .map(|item| {
                    container_resource(
                        item,
                        self.engine.live,
                        self.continuation_runtime_identity.as_ref(),
                    )
                });
        }
        if let Some(reference) = id.strip_prefix("image:") {
            return self
                .engine
                .images
                .iter()
                .find(|item| item.reference() == reference)
                .map(|item| image_resource(item, self.engine.live));
        }
        if let Some(name) = id.strip_prefix("volume:") {
            return self
                .engine
                .volumes
                .iter()
                .find(|item| item.name == name)
                .map(|item| volume_resource(item, self.engine.live));
        }
        if let Some(name) = id.strip_prefix("distrobox:") {
            return self
                .engine
                .distroboxes
                .iter()
                .find(|item| item.name == name)
                .map(|item| distrobox_resource(item, self.engine.live));
        }
        None
    }
}
