//! This device's server settings, as the daemon publishes them: the work it
//! takes from the person's other devices, and the services it offers them.
//!
//! The resource is the daemon's (`device:server`), because the configuration
//! it changes is the daemon's to keep: a machine takes work through the server
//! configuration at [`SERVER`], and Settings shows and changes it through the
//! resource's reading and actions rather than through a file of its own.

use serde::{Deserialize, Serialize};
use tilo_resource::Effect;

/// The resource this device's server settings are published under.
pub const SERVER: &str = "device:server";

/// The kinds of work a device takes, by whose it is: the chooser's values, and
/// the values the setting writes.
pub const WORK: [&str; 3] = ["nobody", "own-devices", "anyone"];

/// The shares of the processors or memory the setting writes.
pub const ROOM: [u8; 3] = [0, 20, 50];

/// The parts of the machine a share can be kept free of work on.
pub const ROOM_PARTS: [&str; 2] = ["processors", "memory"];

/// The services of this device a person offers or withdraws.
pub const SERVICES: [&str; 5] = [
    "workspace_replication",
    "inbound_server",
    "remote_control_server",
    "app_serving",
    "compute",
];

/// Whose work a device takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkFrom {
    /// Nobody else's.
    #[default]
    Nobody,
    /// Its own person's devices.
    OwnDevices,
    /// Anyone whose workspace it holds.
    Anyone,
}

impl WorkFrom {
    /// The work named `name` ([`WORK`]).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "nobody" => Some(Self::Nobody),
            "own-devices" => Some(Self::OwnDevices),
            "anyone" => Some(Self::Anyone),
            _ => None,
        }
    }

    /// The name its action takes ([`WORK`]).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Nobody => "nobody",
            Self::OwnDevices => "own-devices",
            Self::Anyone => "anyone",
        }
    }

    /// Whether the device takes work of anyone but its own person.
    #[must_use]
    pub const fn takes_others(self) -> bool {
        matches!(self, Self::Anyone)
    }
}

/// The states a device takes work in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct WorkWhen {
    /// Take work only while the machine is charging.
    pub charging_only: bool,
    /// Keep at least this share of the processors free.
    pub free_cpu_percent: u8,
    /// Keep at least this share of memory free.
    pub free_memory_percent: u8,
}

/// The work this device takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Work {
    /// Whose work it takes.
    pub from: WorkFrom,
    /// The states it takes it in.
    pub when: WorkWhen,
}

/// The services this device offers the person's other devices.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each service is offered or withdrawn on its own"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Services {
    pub workspace_replication: bool,
    pub inbound_server: bool,
    pub remote_control_server: bool,
    pub app_serving: bool,
    pub compute: bool,
}

impl Services {
    /// The service named `name` ([`SERVICES`]) set to `on`.
    ///
    /// # Errors
    /// `name` names no service.
    pub fn set(&mut self, name: &str, on: bool) -> Result<(), String> {
        let field = match name {
            "workspace_replication" => &mut self.workspace_replication,
            "inbound_server" => &mut self.inbound_server,
            "remote_control_server" => &mut self.remote_control_server,
            "app_serving" => &mut self.app_serving,
            "compute" => &mut self.compute,
            _ => return Err(format!("{name} is no service of this device")),
        };
        *field = on;
        Ok(())
    }
}

/// The `device:server` resource: this device's server settings as the daemon
/// reads and changes them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ServerSettings {
    /// Whether this device has a server configuration to change. A device
    /// without one takes no work, and its settings are written when its person
    /// changes them.
    pub configured: bool,
    /// Why the configuration could not be read or saved, when it could not.
    pub problem: Option<String>,
    /// Where this device's server keeps its state: the heartbeat Settings
    /// reads to say whether the server is running.
    pub state_root: String,
    pub work: Work,
    pub services: Services,
}

impl ServerSettings {
    /// Take `from`'s work, keeping the states it is taken in.
    pub fn set_work(&mut self, from: WorkFrom) {
        self.work.from = from;
    }

    /// Take work only while charging, or whatever the machine is doing.
    pub fn set_work_while_charging(&mut self, on: bool) {
        self.work.when.charging_only = on;
    }

    /// Keep `percent` of the processors or memory free for the people at the
    /// machine.
    ///
    /// # Errors
    /// `part` is neither of the machine's parts, or `percent` is no share.
    pub fn set_work_room(&mut self, part: &str, percent: u8) -> Result<(), String> {
        if percent > 100 {
            return Err(format!("{percent} is no share of the machine"));
        }
        match part {
            "processors" => self.work.when.free_cpu_percent = percent,
            "memory" => self.work.when.free_memory_percent = percent,
            _ => return Err(format!("{part} is no part of the machine")),
        }
        Ok(())
    }
}

/// The `device:server` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.device.server", label = "Server settings")]
pub struct ServerSettingsReading;

/// The `device:server` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.device.server")]
pub enum ServerSettingsAction {
    /// Take the work of this device's own devices, of anyone's in the
    /// workspaces it holds, or none.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "server",
        keywords = "take work, server, work from, arbeit annehmen"
    )]
    SetWork {
        #[param("Whose work this device takes", choices = WORK)]
        work: String,
    },
    /// Take work only while the machine is charging.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "battery-charging",
        keywords = "charging, battery"
    )]
    SetWorkWhileCharging {
        #[param("Whether work is taken only while charging")]
        on: bool,
    },
    /// Keep a share of the processors or memory free for the people at the
    /// machine.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "cpu",
        keywords = "keep free, cpu, memory"
    )]
    SetWorkRoom {
        #[param("The part of the machine", choices = ROOM_PARTS)]
        part: String,
        #[param("The share kept free, as a percentage")]
        percent: u8,
    },
    /// Offer one service of this device to the person's other devices, or stop.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "server", keywords = "services, serve")]
    SetService {
        #[param("The service", choices = SERVICES)]
        service: String,
        #[param("Whether the device offers it")]
        on: bool,
    },
}
