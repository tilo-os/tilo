//! Every daemon failure carries the next command to run.

use tilo_dev_protocol::{RpcError, code};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("tilo-dev speaks protocol {client} but tilo-devd speaks {daemon}")]
    VersionMismatch { client: u32, daemon: u32 },
    #[error("the connection has not completed the hello handshake")]
    HelloRequired,
    #[error("invalid request: {0}")]
    InvalidParams(String),
    #[error("no slot named '{0}'")]
    SlotNotFound(String),
    #[error("slot '{name}' already exists as {kind}")]
    SlotKindConflict { name: String, kind: String },
    #[error("'{0}' is reserved for VM-wide components")]
    ReservedName(String),
    #[error("'{0}' is not a valid slot name")]
    InvalidName(String),
    #[error("no layer {0}")]
    LayerNotFound(String),
    #[error("no upload {0}")]
    UploadNotFound(String),
    #[error("layer rejected: {0}")]
    InvalidLayer(String),
    #[error("slot '{slot}' has no generation {generation}")]
    GenerationNotFound { slot: String, generation: u64 },
    #[error("slot '{0}' is already at its base generation")]
    NoEarlierGeneration(String),
    #[error("component '{component}' has {scope} scope and cannot go into slot '{slot}'")]
    ScopeMismatch {
        component: String,
        slot: String,
        scope: String,
    },
    #[error("seat0 is held by slot '{slot}' ({owner}) since {since}")]
    SeatBusy {
        slot: String,
        owner: String,
        since: String,
    },
    #[error("{0} is busy with another tilo-dev command")]
    Busy(String),
    #[error("layer {field} is {layer} but the VM expects {vm}")]
    AbiMismatch {
        field: String,
        layer: String,
        vm: String,
    },
    #[error("the VM has no {0}; production images do not ship it")]
    ControlMissing(String),
    #[error("'{0}' has no session")]
    NoSession(String),
    #[error("{step} failed: {detail}")]
    Apply { step: String, detail: String },
    #[error("slot '{slot}' session differs from a greetd session: {detail}")]
    SessionParity { slot: String, detail: String },
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl DaemonError {
    pub fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    pub fn code(&self) -> i64 {
        match self {
            Self::VersionMismatch { .. } | Self::HelloRequired => code::VERSION_MISMATCH,
            Self::InvalidParams(_) => code::INVALID_PARAMS,
            Self::SlotNotFound(_)
            | Self::LayerNotFound(_)
            | Self::UploadNotFound(_)
            | Self::GenerationNotFound { .. }
            | Self::ControlMissing(_) => code::NOT_FOUND,
            Self::SlotKindConflict { .. } | Self::SeatBusy { .. } | Self::Busy(_) => code::CONFLICT,
            Self::AbiMismatch { .. } => code::ABI,
            Self::ReservedName(_)
            | Self::InvalidName(_)
            | Self::InvalidLayer(_)
            | Self::NoEarlierGeneration(_)
            | Self::NoSession(_) => code::INVALID,
            Self::ScopeMismatch { .. } => code::SCOPE,
            Self::Apply { .. } | Self::SessionParity { .. } => code::APPLY,
            Self::Io { .. } => code::IO,
        }
    }

    pub fn hint(&self) -> String {
        match self {
            Self::VersionMismatch { client, daemon } if client > daemon => {
                "the VM's image is older than this tilo-dev: tilo-dev vm upgrade".into()
            }
            Self::VersionMismatch { .. } => {
                "this tilo-dev is older than the VM's image: rebuild it (cargo install --path crates/dev/tilo-dev)"
                    .into()
            }
            Self::HelloRequired => "connect through tilo-dev rather than speaking to tilo-devd by hand".into(),
            Self::InvalidParams(_) => {
                "tilo-dev and tilo-devd disagree on the request shape: tilo-dev doctor".into()
            }
            Self::SlotNotFound(name) => {
                format!("tilo-dev slot create {name} --headless   (list existing: tilo-dev slot list)")
            }
            Self::SlotKindConflict { name, .. } => {
                format!("remove it first (tilo-dev slot rm {name}) or create it with its existing kind")
            }
            Self::ReservedName(_) => {
                "pick another slot name; VM-wide components go in with: tilo-dev swap vm <component>"
                    .into()
            }
            Self::InvalidName(_) => {
                "use 1-28 lowercase letters, digits or dashes, starting with a letter".into()
            }
            Self::LayerNotFound(_) => "tilo-dev swap <component> --from worktree   (builds and uploads a layer)".into(),
            Self::UploadNotFound(_) => "retry the swap; uploads do not survive a daemon restart".into(),
            Self::InvalidLayer(_) => {
                "a layer holds only usr/ (and opt/) with regular files, directories and symlinks".into()
            }
            Self::GenerationNotFound { slot, .. } => {
                format!("tilo-dev slot list   (shows the generations of '{slot}')")
            }
            Self::NoEarlierGeneration(slot) => {
                format!("nothing to roll back; tilo-dev swap {slot} <component> installs a build")
            }
            Self::ScopeMismatch { component, scope, .. } if scope == "vm" => {
                format!("VM-wide components go into the vm pseudo-slot: tilo-dev swap vm {component}")
            }
            Self::ScopeMismatch { component, .. } => {
                format!("session components go into your own slot: tilo-dev swap {component}")
            }
            Self::SeatBusy { slot, .. } => format!(
                "ask its owner to run: tilo-dev seat release {slot}   (or take it anyway: tilo-dev seat claim --steal)"
            ),
            Self::Busy(_) => "wait for the other command to finish, then retry; tilo-dev status shows what is running".into(),
            Self::AbiMismatch { field, .. } if field == "target" => {
                "build for the VM's architecture: tilo-dev swap <component> --from worktree on a host of that architecture"
                    .into()
            }
            Self::AbiMismatch { .. } => {
                "rebuild against this VM's base (tilo-dev vm upgrade first if the VM is older than your worktree)".into()
            }
            Self::NoSession(_) => "use a headful or headless slot: tilo-dev slot list".into(),
            Self::ControlMissing(_) => {
                "run a dev image, which adds tilo-control: tilo-dev vm upgrade --image code.podesta.ai/qos/tilo-dev:<tag>"
                    .into()
            }
            Self::Apply { .. } => "tilo-dev logs   (the unit journal usually names the cause)".into(),
            Self::SessionParity { .. } => {
                "slot session capabilities or SELinux domain differ from a greetd session; see tilo-dev guide troubleshooting"
                    .into()
            }
            Self::Io { .. } => "tilo-dev doctor   (checks the VM's disk and daemon state)".into(),
        }
    }

    pub fn see(&self) -> Option<&'static str> {
        match self {
            Self::SlotNotFound(_)
            | Self::SlotKindConflict { .. }
            | Self::ReservedName(_)
            | Self::InvalidName(_)
            | Self::NoSession(_) => Some("tilo-dev guide concepts"),
            Self::LayerNotFound(_)
            | Self::UploadNotFound(_)
            | Self::InvalidLayer(_)
            | Self::GenerationNotFound { .. }
            | Self::NoEarlierGeneration(_)
            | Self::ScopeMismatch { .. }
            | Self::AbiMismatch { .. } => Some("tilo-dev guide swapping"),
            Self::SeatBusy { .. } | Self::ControlMissing(_) => Some("tilo-dev guide driving"),
            Self::VersionMismatch { .. }
            | Self::HelloRequired
            | Self::InvalidParams(_)
            | Self::Busy(_)
            | Self::Apply { .. }
            | Self::SessionParity { .. }
            | Self::Io { .. } => Some("tilo-dev guide troubleshooting"),
        }
    }

    pub fn to_rpc(&self) -> RpcError {
        RpcError {
            code: self.code(),
            message: self.to_string(),
            hint: self.hint(),
            see: self.see().map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sample per variant. The match below has no wildcard, so adding a
    /// variant fails to compile until it gets a sample here.
    fn samples() -> Vec<DaemonError> {
        let all = vec![
            DaemonError::VersionMismatch {
                client: 2,
                daemon: 1,
            },
            DaemonError::VersionMismatch {
                client: 1,
                daemon: 2,
            },
            DaemonError::HelloRequired,
            DaemonError::InvalidParams("x".into()),
            DaemonError::SlotNotFound("a".into()),
            DaemonError::SlotKindConflict {
                name: "a".into(),
                kind: "headless".into(),
            },
            DaemonError::ReservedName("vm".into()),
            DaemonError::InvalidName("A".into()),
            DaemonError::LayerNotFound("h".into()),
            DaemonError::UploadNotFound("u".into()),
            DaemonError::InvalidLayer("x".into()),
            DaemonError::GenerationNotFound {
                slot: "a".into(),
                generation: 9,
            },
            DaemonError::NoEarlierGeneration("a".into()),
            DaemonError::ScopeMismatch {
                component: "fs".into(),
                slot: "a".into(),
                scope: "vm".into(),
            },
            DaemonError::ScopeMismatch {
                component: "shell".into(),
                slot: "vm".into(),
                scope: "session".into(),
            },
            DaemonError::SeatBusy {
                slot: "b".into(),
                owner: "host/wt".into(),
                since: "now".into(),
            },
            DaemonError::Busy("the VM".into()),
            DaemonError::AbiMismatch {
                field: "target".into(),
                layer: "a".into(),
                vm: "b".into(),
            },
            DaemonError::AbiMismatch {
                field: "builder".into(),
                layer: "a".into(),
                vm: "b".into(),
            },
            DaemonError::NoSession("vm".into()),
            DaemonError::ControlMissing("/usr/bin/tilo-control".into()),
            DaemonError::Apply {
                step: "s".into(),
                detail: "d".into(),
            },
            DaemonError::SessionParity {
                slot: "a".into(),
                detail: "CapAmb 800000000".into(),
            },
            DaemonError::Io {
                context: "c".into(),
                source: std::io::Error::other("e"),
            },
        ];
        for error in &all {
            match error {
                DaemonError::VersionMismatch { .. }
                | DaemonError::HelloRequired
                | DaemonError::InvalidParams(_)
                | DaemonError::SlotNotFound(_)
                | DaemonError::SlotKindConflict { .. }
                | DaemonError::ReservedName(_)
                | DaemonError::InvalidName(_)
                | DaemonError::LayerNotFound(_)
                | DaemonError::UploadNotFound(_)
                | DaemonError::InvalidLayer(_)
                | DaemonError::GenerationNotFound { .. }
                | DaemonError::NoEarlierGeneration(_)
                | DaemonError::ScopeMismatch { .. }
                | DaemonError::SeatBusy { .. }
                | DaemonError::Busy(_)
                | DaemonError::AbiMismatch { .. }
                | DaemonError::NoSession(_)
                | DaemonError::ControlMissing(_)
                | DaemonError::Apply { .. }
                | DaemonError::SessionParity { .. }
                | DaemonError::Io { .. } => {}
            }
        }
        all
    }

    #[test]
    fn every_error_has_a_hint_and_a_guide_topic() {
        for error in samples() {
            let rpc = error.to_rpc();
            assert!(!rpc.hint.trim().is_empty(), "{error:?} has no hint");
            assert!(rpc.see.is_some(), "{error:?} has no guide topic");
            assert!(!rpc.message.is_empty());
        }
    }
}
