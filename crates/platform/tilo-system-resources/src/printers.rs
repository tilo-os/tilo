//! The `printers` resource: the queues this computer prints to, the jobs they carry, the
//! printers it can find nearby and the scanners it can reach, with the actions that change
//! them.
//!
//! The reading is what Settings' Printers page and Print Centre both show; the actions are
//! what a person, an agent or a script calls to add, remove, pause or choose a printer and to
//! cancel or retry a job. What talks to CUPS and SANE is the session's system service.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const PRINTERS: &str = "printers";

/// How a queue stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueState {
    /// Ready for work.
    #[default]
    Ready,
    /// Printing now.
    Printing,
    /// Paused by a person.
    Paused,
    /// Its device cannot be reached.
    Offline,
}

/// What a queue's device says its consumables hold: a toner or ink cartridge, the paper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterSupply {
    /// What the supply is, as the device names it: "Black cartridge", "Paper".
    pub name: String,
    /// Its colour, when the device gives one: "black", "cyan".
    pub color: Option<String>,
    /// How much is left, 0 to 100.
    pub level: Option<u8>,
}

/// A queue: one printer this computer sends jobs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterQueue {
    pub name: String,
    pub state: QueueState,
    /// Whether it is the queue a new job goes to unless another is chosen.
    pub default: bool,
    /// Whether it takes jobs now.
    pub accepting: bool,
    /// Why it stands as it does, in CUPS's words.
    pub reason: Option<String>,
    /// What its device says about its supplies, empty when it answers none.
    pub supplies: Vec<PrinterSupply>,
    /// Whether the device answered at all.
    pub reachable: bool,
}

/// How a job stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    /// Waiting for its turn.
    #[default]
    Waiting,
    /// Held by a person.
    Held,
    /// Printing now.
    Printing,
    /// Stopped: it waits for someone to retry it.
    Stopped,
    Cancelled,
    /// It could not print.
    Failed,
    Printed,
}

/// A job: one document in a queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintJob {
    /// The job's number, as CUPS names it.
    pub id: String,
    /// The document's name, empty when the sender gave none.
    pub title: String,
    pub owner: String,
    pub size_bytes: u64,
    /// When it was submitted, as CUPS writes it.
    pub submitted: String,
    /// The queue it goes to.
    pub destination: String,
    pub state: JobState,
    /// How far it is, 0 to 100, where the device counts pages.
    pub progress: Option<u8>,
}

/// A printer found on the network that this computer does not print to yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredPrinter {
    /// Its address, which adding it takes.
    pub uri: String,
    pub name: String,
    /// Where it stands, when it says.
    pub location: Option<String>,
}

/// What the computer's printers read as now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Printers {
    /// Whether this computer can print at all: a printer service answers here.
    #[serde(default)]
    pub serving: bool,
    pub queues: Vec<PrinterQueue>,
    pub jobs: Vec<PrintJob>,
    /// The printers found nearby, kept while the person looks at them.
    pub nearby: Vec<DiscoveredPrinter>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `printers` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.printers")]
pub enum PrintAction {
    /// Look for printers on the network.
    #[effect(kind = "read")]
    #[effect(own_device, icon = "search", keywords = "find printer, nearby printer, drucker suchen")]
    Find,
    /// Print to a printer found on the network, under the name it is known by here.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "plus",
        keywords = "add printer, drucker hinzufügen"
    )]
    Add {
        #[param("The printer's address")] uri: String,
        #[param("The name this computer knows it by")] name: String,
    },
    /// Stop printing to a queue.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove printer, delete printer, drucker entfernen"
    )]
    Remove {
        #[param("The queue's name")] printer: String,
    },
    /// Send new jobs to this queue unless another is chosen.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "check", keywords = "default printer, standarddrucker")]
    SetDefault {
        #[param("The queue's name")] printer: String,
    },
    /// Hold a queue's work, or let it print again.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "pause", keywords = "pause printer, resume printer, drucker pausieren")]
    SetPaused {
        #[param("The queue's name")] printer: String,
        #[param("Whether it holds its work")] paused: bool,
    },
    /// Take a job out of its queue.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "x",
        keywords = "cancel print, cancel job, druck abbrechen"
    )]
    CancelJob {
        #[param("The job's number")] job: String,
    },
    /// Send a stopped job to its printer again.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "rotate-cw", keywords = "retry print, drucken erneut")]
    RetryJob {
        #[param("The job's number")] job: String,
    },
}
