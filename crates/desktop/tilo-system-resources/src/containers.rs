//! The `containers` resource: the rootless podman containers, images and volumes this computer
//! runs, the distroboxes it keeps, and the actions that change them.
//!
//! The reading is what the Containers window shows, and the actions are what a person, an
//! agent or a script calls to start, stop or remove a container, pull an image, or make a
//! volume. What talks to podman is the session's system service.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const CONTAINERS: &str = "containers";

/// How a container stands, normalised across what podman reports.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerState {
    Running,
    Exited,
    Created,
    Paused,
    Restarting,
    /// Needs attention: podman reported something this reading does not know.
    #[default]
    Dead,
}

/// A published port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

/// A mount in a container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Mount {
    pub source: String,
    pub destination: String,
    pub read_only: bool,
}

/// A named value: an environment entry, a label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Named {
    pub name: String,
    pub value: String,
}

/// The latest resource sample; a stopped container reports none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct ContainerStats {
    /// True only when podman answered a sample; missing telemetry is not a measured zero.
    #[serde(default)]
    pub available: bool,
    /// CPU use as a fraction of one core.
    pub cpu_percent: f64,
    pub mem_bytes: u64,
    /// Memory limit in bytes; 0 is unlimited.
    pub mem_limit_bytes: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub block_read_bytes: u64,
    pub block_write_bytes: u64,
    pub pids: u32,
    /// When the sample was read, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub fetched_unix_ms: Option<u64>,
}

/// A container, as podman lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: ContainerState,
    pub status_detail: String,
    pub command: String,
    pub created: String,
    pub ports: Vec<PortMapping>,
    pub mounts: Vec<Mount>,
    pub env: Vec<Named>,
    pub labels: Vec<Named>,
    pub stats: ContainerStats,
    /// Recent log lines, newest last.
    pub logs: Vec<String>,
    /// A failed log read is separate from an empty log.
    #[serde(default)]
    pub logs_error: Option<String>,
    /// When the log lines were read, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub logs_fetched_unix_ms: Option<u64>,
}

impl Container {
    /// The first twelve characters of the id, cut at a character boundary.
    #[must_use]
    pub fn short_id(&self) -> &str {
        let end = self
            .id
            .char_indices()
            .nth(12)
            .map_or(self.id.len(), |(index, _)| index);
        self.id.get(..end).unwrap_or_default()
    }

    /// Whether the container runs or restarts.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        matches!(
            self.state,
            ContainerState::Running | ContainerState::Restarting
        )
    }

    /// Memory used as a 0..=1 fraction of the limit; 0 when the limit is unknown.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a share of memory read to size a bar, where an exact byte count is not needed"
    )]
    pub fn mem_fraction(&self) -> f64 {
        if self.stats.mem_limit_bytes == 0 {
            0.0
        } else {
            (self.stats.mem_bytes as f64 / self.stats.mem_limit_bytes as f64).clamp(0.0, 1.0)
        }
    }
}

/// An image in the engine's store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Image {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size_bytes: u64,
    pub created: String,
    pub in_use_by: u32,
}

impl Image {
    /// The image as one written reference, `repository:tag`.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("{}:{}", self.repository, self.tag)
    }
}

/// A volume the engine holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    /// Where it is on this computer.
    pub mountpoint: String,
    pub created: String,
    pub in_use_by: u32,
}

/// A tightly integrated container, listed apart because its verbs differ.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct DistroBox {
    pub name: String,
    pub image: String,
    pub state: ContainerState,
    pub status_detail: String,
}

/// What the computer's containers read as now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Containers {
    /// Whether podman answered here at all.
    #[serde(default)]
    pub serving: bool,
    /// Which podman answered, or why none did.
    pub connection: String,
    /// Whether the reading came from a live engine.
    #[serde(default)]
    pub live: bool,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub distroboxes: Vec<DistroBox>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

impl Containers {
    /// The bytes the images hold.
    #[must_use]
    pub fn image_disk_bytes(&self) -> u64 {
        self.images.iter().map(|image| image.size_bytes).sum()
    }

    /// The container `id` names.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|container| container.id == id)
    }
}

/// The `containers` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.containers")]
pub enum ContainerAction {
    /// Read the engine again.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh containers, container neu laden"
    )]
    Refresh,
    /// Start a stopped container.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "play",
        keywords = "start container, container starten"
    )]
    Start {
        #[param("The container's id")]
        container: String,
    },
    /// Stop a running container.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "square",
        keywords = "stop container, container stoppen"
    )]
    Stop {
        #[param("The container's id")]
        container: String,
    },
    /// Restart a container.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "restart container, container neu starten"
    )]
    Restart {
        #[param("The container's id")]
        container: String,
    },
    /// Hold a container's processes.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "pause",
        keywords = "pause container, container pausieren"
    )]
    Pause {
        #[param("The container's id")]
        container: String,
    },
    /// Let a paused container run again.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "play",
        keywords = "resume container, container fortsetzen"
    )]
    Unpause {
        #[param("The container's id")]
        container: String,
    },
    /// Remove a container.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove container, container entfernen"
    )]
    Remove {
        #[param("The container's id")]
        container: String,
    },
    /// Fetch an image.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "download",
        keywords = "pull image, image herunterladen"
    )]
    Pull {
        #[param("The image reference")]
        image: String,
    },
    /// Give an image another reference.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "copy", keywords = "tag image, image taggen")]
    Tag {
        #[param("The image reference")]
        image: String,
        #[param("The new tag")]
        tag: String,
    },
    /// Remove an image.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove image, image entfernen"
    )]
    RemoveImage {
        #[param("The image reference")]
        image: String,
    },
    /// Make a volume.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "plus", keywords = "new volume, volume anlegen")]
    CreateVolume {
        #[param("The volume's name")]
        name: String,
    },
    /// Remove a volume.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove volume, volume entfernen"
    )]
    RemoveVolume {
        #[param("The volume's name")]
        volume: String,
    },
    /// Run an image as a new container.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "plus",
        keywords = "new container, container anlegen"
    )]
    Create {
        #[param("The container's name, or empty for one podman picks")]
        name: String,
        #[param("The image to run")]
        image: String,
        #[param("Host and container ports, comma separated")]
        ports: String,
        #[param("KEY=value pairs, comma separated")]
        env: String,
        #[param("Volume and path, comma separated")]
        volumes: String,
        #[param("The command, or empty for the image's own")]
        command: String,
    },
}
