//! What the app publishes to search, the palette and agents: the engine's
//! containers, images, volumes and distroboxes as live rows with their view
//! and effect operations, and the handler that answers those operations.

use ui::framework::LiveRow;
use ui::tilo_actions::{ActionSpec, OperationKind, PermissionRequirement, RetryPolicy};

use crate::app::ContainersApp;
use crate::model::{DetailTab, Section};
use crate::podman::{self, Action};

/// How many rows the published tree carries; the rest resolve by id.
pub(crate) const RESOURCE_SNAPSHOT_LIMIT: usize = 200;

/// A view or effect operation on a row, its payload the verb and target.
fn row_operation(
    id: &str,
    label: impl Into<String>,
    kind: OperationKind,
    type_id: &str,
    payload: &[&str],
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, kind, type_id, &payload).ok()
}

/// An engine effect: needs an administrator, and the engine.
fn engine_effect(spec: ActionSpec, engine_live: bool, offline: &str) -> ActionSpec {
    let spec = spec.requiring(PermissionRequirement::Admin).privileged();
    if engine_live {
        spec
    } else {
        spec.unavailable("engine_offline", offline)
    }
}

const ENGINE_OFFLINE: &str = "Podman isn't available. Start its user service to manage containers.";

fn container_row(container: &crate::model::Container, engine_live: bool) -> LiveRow {
    let mut row = LiveRow::new(
        format!("container:{}", container.id),
        "containers.container",
        container.name.clone(),
    );
    if let Some(spec) = row_operation(
        "select",
        format!("Open {}", container.name),
        OperationKind::View,
        "containers.container",
        &["select", &container.id],
    ) {
        row = row.operation(spec);
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
        if let Some(spec) = row_operation(
            verb,
            *label,
            OperationKind::Effect,
            "containers.container",
            &[verb, &container.id],
        ) {
            let mut spec = engine_effect(spec, engine_live, ENGINE_OFFLINE);
            if *verb == "remove" {
                spec = spec.destructive();
            } else if *verb != "restart" && *verb != "shell" {
                spec = spec.retry_policy(RetryPolicy::Idempotent);
            }
            row = row.operation(spec);
        }
    }
    row.row(logs_row(container))
}

fn logs_row(container: &crate::model::Container) -> LiveRow {
    let mut logs = LiveRow::new(
        format!("container:{}:logs", container.id),
        "containers.logs",
        format!("{} logs", container.name),
    );
    if let Some(spec) = row_operation(
        "open",
        "Open logs",
        OperationKind::View,
        "containers.logs",
        &["logs", &container.id],
    ) {
        logs = logs.operation(spec);
    }
    logs
}

fn image_row(image: &crate::model::Image, engine_live: bool) -> LiveRow {
    let reference = image.reference();
    let mut row = LiveRow::new(
        format!("image:{reference}"),
        "containers.image",
        reference.clone(),
    );
    if let Some(spec) = row_operation(
        "open",
        "Show image",
        OperationKind::View,
        "containers.image",
        &["open", &reference],
    ) {
        row = row.operation(spec);
    }
    if let Some(spec) = row_operation(
        "remove",
        "Remove image",
        OperationKind::Effect,
        "containers.image",
        &["remove", &reference],
    ) {
        row = row.operation(engine_effect(spec, engine_live, ENGINE_OFFLINE).destructive());
    }
    row
}

fn volume_row(volume: &crate::model::Volume, engine_live: bool) -> LiveRow {
    let mut row = LiveRow::new(
        format!("volume:{}", volume.name),
        "containers.volume",
        volume.name.clone(),
    );
    if let Some(spec) = row_operation(
        "open",
        "Show volume",
        OperationKind::View,
        "containers.volume",
        &["open", &volume.name],
    ) {
        row = row.operation(spec);
    }
    if let Some(spec) = row_operation(
        "remove",
        "Remove volume",
        OperationKind::Effect,
        "containers.volume",
        &["remove", &volume.name],
    ) {
        row = row.operation(engine_effect(spec, engine_live, ENGINE_OFFLINE).destructive());
    }
    row
}

fn distrobox_row(distrobox: &crate::model::DistroBox, engine_live: bool) -> LiveRow {
    let mut row = LiveRow::new(
        format!("distrobox:{}", distrobox.name),
        "containers.distrobox",
        distrobox.name.clone(),
    );
    if let Some(spec) = row_operation(
        "open",
        "Show distrobox",
        OperationKind::View,
        "containers.distrobox",
        &["open", &distrobox.name],
    ) {
        row = row.operation(spec);
    }
    if let Some(spec) = row_operation(
        "enter",
        "Enter distrobox",
        OperationKind::Effect,
        "containers.distrobox",
        &["enter", &distrobox.name],
    ) {
        row = row.operation(engine_effect(
            spec,
            engine_live,
            "Podman isn't available. Start its user service to enter boxes.",
        ));
    }
    row
}

impl ContainersApp {
    /// The live rows of the engine, a bounded page of them, and an
    /// in-flight pull.
    pub(crate) fn engine_rows(&self) -> Vec<LiveRow> {
        let live = self.engine.live;
        let mut rows: Vec<LiveRow> = self
            .engine
            .containers
            .iter()
            .map(|container| container_row(container, live))
            .chain(
                self.engine
                    .images
                    .iter()
                    .map(|image| image_row(image, live)),
            )
            .chain(
                self.engine
                    .volumes
                    .iter()
                    .map(|volume| volume_row(volume, live)),
            )
            .chain(
                self.engine
                    .distroboxes
                    .iter()
                    .map(|distrobox| distrobox_row(distrobox, live)),
            )
            .take(RESOURCE_SNAPSHOT_LIMIT)
            .collect();
        if let Some(pull) = &self.pull {
            rows.push(LiveRow::new(
                "containers.pull-job",
                "containers.job",
                format!("Pulling {}", pull.reference),
            ));
        }
        rows
    }

    /// The live row `id`, wherever it is in the engine.
    pub(crate) fn engine_row(&self, id: &str) -> Option<LiveRow> {
        let live = self.engine.live;
        if let Some(container_id) = id
            .strip_prefix("container:")
            .and_then(|rest| rest.strip_suffix(":logs"))
        {
            return self.engine.find(container_id).map(logs_row);
        }
        if let Some(container_id) = id.strip_prefix("container:") {
            return self
                .engine
                .find(container_id)
                .map(|container| container_row(container, live));
        }
        if let Some(reference) = id.strip_prefix("image:") {
            return self
                .engine
                .images
                .iter()
                .find(|item| item.reference() == reference)
                .map(|item| image_row(item, live));
        }
        if let Some(name) = id.strip_prefix("volume:") {
            return self
                .engine
                .volumes
                .iter()
                .find(|item| item.name == name)
                .map(|item| volume_row(item, live));
        }
        if let Some(name) = id.strip_prefix("distrobox:") {
            return self
                .engine
                .distroboxes
                .iter()
                .find(|item| item.name == name)
                .map(|item| distrobox_row(item, live));
        }
        None
    }
}

impl ContainersApp {
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
