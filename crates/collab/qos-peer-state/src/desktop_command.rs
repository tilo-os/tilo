//! Typed semantic requests. Neither this payload nor private transport is a mutation receipt.
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "dev.qos.desktop";
pub const RESOURCE: &str = "desktop:session";
pub const ACTION: &str = "command";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopCommandError {
    /// Rejected before any canonical state or durable identity changed.
    Rejected(String),
    /// The durable barrier failed or its outcome cannot be established.
    Uncertain(String),
}
impl std::fmt::Display for DesktopCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Uncertain(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for DesktopCommandError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSource {
    pub room: String,
    /// Exact admitted Automerge heads observed by the initiating desktop.
    pub heads: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundCommand {
    pub source: CommandSource,
    pub command: DesktopCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabLocation {
    pub strip: u64,
    pub group: u64,
    pub tab: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabPlacement {
    /// Native default placement when an admitted workspace has no strips yet.
    EmptyWorkspace,
    InGroup {
        strip: u64,
        group: u64,
        after: Option<u64>,
    },
    NewGroup {
        strip: u64,
        after: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesktopCommand {
    OpenTab {
        app: String,
        placement: TabPlacement,
    },
    CloseTab {
        location: TabLocation,
    },
    DuplicateTab {
        location: TabLocation,
    },
    ReorderTab {
        location: TabLocation,
        before: Option<u64>,
    },
    ReorderGroup {
        strip: u64,
        group: u64,
        before: Option<u64>,
    },
    MoveTab {
        location: TabLocation,
        placement: TabPlacement,
    },
    SplitTab {
        location: TabLocation,
        before: Option<u64>,
    },
    CreateStrip {
        name: String,
    },
    RenameStrip {
        strip: u64,
        name: String,
    },
    RemoveStrip {
        strip: u64,
    },
    RenameWorkspace {
        name: String,
    },
}
