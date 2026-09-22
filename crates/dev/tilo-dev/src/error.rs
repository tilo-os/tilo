//! The one error type. Every variant says what went wrong, and `hint` is
//! always the exact next command; `see` names the guide topic to read.

use serde_json::json;
use tilo_dev_protocol::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("no VM selected")]
    NoVm,
    #[error("no VM named '{name}' in {registry}")]
    UnknownVm {
        name: String,
        registry: String,
        known: Vec<String>,
    },
    #[error("{path}: {detail}")]
    Config { path: String, detail: String },
    #[error("not inside a git worktree, so there is no default slot")]
    NotInWorktree,
    #[error("branch '{branch}' does not make a usable slot name")]
    SlotName { branch: String },
    #[error("cannot reach VM '{vm}' over SSH: {detail}")]
    Unreachable {
        vm: String,
        detail: String,
        debug: Vec<String>,
    },
    #[error("VM '{vm}' has no tilo-devd; it is not running a Tilo dev image")]
    DaemonMissing { vm: String },
    #[error("tilo-dev speaks protocol {client} but the VM's tilo-devd speaks {daemon}")]
    ProtocolMismatch { client: u32, daemon: u32 },
    #[error("{}", .0.message)]
    Remote(RpcError),
    #[error("{tool} is not installed (needed to {purpose})")]
    ToolMissing { tool: String, purpose: String },
    #[error("`{command}` failed ({status}){}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") })]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("{what} is not supported")]
    Unsupported { what: String, alternative: String },
    #[error("no component manifest at {path}")]
    NoManifest { path: String },
    #[error("no component '{name}' in the manifest")]
    UnknownComponent { name: String, known: Vec<String> },
    #[error("'{name}' is a boot component; swapping cannot replace what the VM booted from")]
    BootComponent { name: String, driver: String },
    #[error("'{component}' has {scope} scope and cannot go into slot '{slot}'")]
    ScopeMismatch {
        component: String,
        slot: String,
        scope: String,
    },
    #[error("this host is {host} but the VM would be {requested}")]
    ArchMismatch { host: String, requested: String },
    #[error("{place} has {free} free, below the {floor} floor")]
    LowDisk {
        place: String,
        free: String,
        floor: String,
    },
    #[error("{place} is not logged in to {registry}")]
    RegistryLogin { place: String, registry: String },
    #[error("refusing to deploy over raw SSH: `{command}`")]
    RawDeploy { command: String },
    #[error("unknown source '{0}'")]
    InvalidSource(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl CliError {
    pub fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    pub fn hint(&self) -> String {
        match self {
            Self::NoVm => {
                "tilo-dev vm list   then   tilo-dev vm use <name>   (or set TILO_DEV_VM)".into()
            }
            Self::UnknownVm { known, .. } if known.is_empty() => {
                "register one: tilo-dev vm create <name> --hypervisor <libvirt|qemu|utm|hyperv> --image <disk>".into()
            }
            Self::UnknownVm { known, .. } => format!(
                "tilo-dev vm use {}   (known: {})",
                known.first().map_or("", String::as_str),
                known.join(", ")
            ),
            Self::Config { path, .. } => format!("fix or remove {path}; tilo-dev doctor re-checks it"),
            Self::NotInWorktree => "name the slot explicitly, e.g. tilo-dev swap my-slot shell".into(),
            Self::SlotName { .. } => {
                "name the slot explicitly (1-28 lowercase letters, digits, dashes), e.g. tilo-dev slot create my-slot"
                    .into()
            }
            Self::Unreachable { debug, .. } => {
                let mut hint = String::from("tilo-dev vm start   (is it running?)   then   tilo-dev doctor");
                for command in debug {
                    hint.push_str("   |   ");
                    hint.push_str(command);
                }
                hint
            }
            Self::DaemonMissing { .. } => {
                "move the VM to a dev image: tilo-dev vm upgrade --image code.sandbox.example/qos/tilo-dev:<tag>".into()
            }
            Self::ProtocolMismatch { client, daemon } if client > daemon => {
                "the VM is older than this tilo-dev: tilo-dev vm upgrade".into()
            }
            Self::ProtocolMismatch { .. } => {
                "this tilo-dev is older than the VM: cargo install --locked --path crates/dev/tilo-dev".into()
            }
            Self::Remote(error) => error.hint.clone(),
            Self::ToolMissing { tool, .. } => format!("install {tool} and make sure it is on PATH; tilo-dev doctor re-checks"),
            Self::CommandFailed { .. } => {
                "the command's own output above names the cause; rerun it by hand to iterate".into()
            }
            Self::Unsupported { alternative, .. } => alternative.clone(),
            Self::NoManifest { .. } => {
                "run from a Tilo worktree that has os/components.toml, or swap a prebuilt layer: --from layer:<hash>".into()
            }
            Self::UnknownComponent { known, .. } => format!("pick one of: {}", known.join(", ")),
            Self::BootComponent { driver, .. } => format!("use the boot scenario driver instead: {driver}"),
            Self::ScopeMismatch { component, scope, .. } if scope == "vm" => {
                format!("VM-wide components go into the vm slot: tilo-dev swap vm {component}")
            }
            Self::ScopeMismatch { component, .. } => {
                format!("session components go into your own slot: tilo-dev swap {component}")
            }
            Self::ArchMismatch { .. } => {
                "use a VM of the host's architecture, or opt into slow emulation with --emulated".into()
            }
            Self::LowDisk { .. } => "tilo-dev gc --dry-run   then   tilo-dev gc".into(),
            Self::RegistryLogin { place, registry } if place == "this host" => {
                format!("podman login {registry}   (with a forge token that has package read access)")
            }
            Self::RegistryLogin { registry, .. } => format!(
                "tilo-dev vm ssh --raw -- podman login --authfile /etc/ostree/auth.json {registry}   (forge token with package read access)"
            ),
            Self::RawDeploy { .. } => {
                "deploy with tilo-dev swap <component> (it records a generation you can roll back); pass --raw to run it anyway"
                    .into()
            }
            Self::InvalidSource(_) => "use --from worktree or --from layer:<hash>".into(),
            Self::Io { .. } => "check the path and permissions named above; tilo-dev doctor checks the setup".into(),
        }
    }

    pub fn see(&self) -> Option<String> {
        let topic = match self {
            Self::NoVm
            | Self::UnknownVm { .. }
            | Self::Config { .. }
            | Self::ArchMismatch { .. } => "platforms",
            Self::NotInWorktree | Self::SlotName { .. } => "concepts",
            Self::Remote(error) => return error.see.clone(),
            Self::NoManifest { .. }
            | Self::UnknownComponent { .. }
            | Self::BootComponent { .. }
            | Self::ScopeMismatch { .. }
            | Self::RawDeploy { .. }
            | Self::InvalidSource(_) => "swapping",
            Self::LowDisk { .. } => "storage",
            Self::Unreachable { .. }
            | Self::DaemonMissing { .. }
            | Self::ProtocolMismatch { .. }
            | Self::ToolMissing { .. }
            | Self::RegistryLogin { .. }
            | Self::CommandFailed { .. }
            | Self::Unsupported { .. }
            | Self::Io { .. } => "troubleshooting",
        };
        Some(format!("tilo-dev guide {topic}"))
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({ "error": self.to_string(), "hint": self.hint(), "see": self.see() })
    }

    /// The human form: what, then how to fix it, then where to read more.
    pub fn render(&self) -> String {
        let mut text = format!("error: {self}\nhint: {}", self.hint());
        if let Some(see) = self.see() {
            text.push_str("\nsee: ");
            text.push_str(&see);
        }
        text
    }
}

impl From<tilo_dev_protocol::ClientError> for CliError {
    fn from(error: tilo_dev_protocol::ClientError) -> Self {
        use tilo_dev_protocol::ClientError;
        match error {
            ClientError::Rpc(error) => Self::Remote(error),
            ClientError::VersionMismatch { client, daemon } => {
                Self::ProtocolMismatch { client, daemon }
            }
            ClientError::Io(source) => Self::Io {
                context: "talking to tilo-devd".into(),
                source,
            },
            ClientError::Closed | ClientError::Protocol(_) => Self::CommandFailed {
                command: "tilo-devd rpc".into(),
                status: "connection closed".into(),
                stderr: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sample per variant; the match has no wildcard, so a new variant
    /// does not compile until it is added here and given a hint.
    #[expect(clippy::too_many_lines, reason = "one sample per variant")]
    fn samples() -> Vec<CliError> {
        let all = vec![
            CliError::NoVm,
            CliError::UnknownVm {
                name: "x".into(),
                registry: "r".into(),
                known: vec![],
            },
            CliError::UnknownVm {
                name: "x".into(),
                registry: "r".into(),
                known: vec!["dev".into()],
            },
            CliError::Config {
                path: "p".into(),
                detail: "d".into(),
            },
            CliError::NotInWorktree,
            CliError::SlotName { branch: "b".into() },
            CliError::Unreachable {
                vm: "dev".into(),
                detail: "d".into(),
                debug: vec!["tilo-dev vm serial".into()],
            },
            CliError::DaemonMissing { vm: "dev".into() },
            CliError::ProtocolMismatch {
                client: 2,
                daemon: 1,
            },
            CliError::ProtocolMismatch {
                client: 1,
                daemon: 2,
            },
            CliError::Remote(RpcError {
                code: 1,
                message: "m".into(),
                hint: "h".into(),
                see: None,
            }),
            CliError::ToolMissing {
                tool: "ssh".into(),
                purpose: "p".into(),
            },
            CliError::CommandFailed {
                command: "c".into(),
                status: "1".into(),
                stderr: String::new(),
            },
            CliError::Unsupported {
                what: "w".into(),
                alternative: "a".into(),
            },
            CliError::NoManifest { path: "p".into() },
            CliError::UnknownComponent {
                name: "x".into(),
                known: vec!["shell".into()],
            },
            CliError::BootComponent {
                name: "kernel".into(),
                driver: "os/dev/vm.sh boot".into(),
            },
            CliError::ScopeMismatch {
                component: "fs".into(),
                slot: "a".into(),
                scope: "vm".into(),
            },
            CliError::ScopeMismatch {
                component: "shell".into(),
                slot: "vm".into(),
                scope: "session".into(),
            },
            CliError::ArchMismatch {
                host: "x86_64".into(),
                requested: "aarch64".into(),
            },
            CliError::LowDisk {
                place: "VM".into(),
                free: "1 GB".into(),
                floor: "5 GB".into(),
            },
            CliError::RegistryLogin {
                place: "this host".into(),
                registry: "code.sandbox.example".into(),
            },
            CliError::RegistryLogin {
                place: "VM dev".into(),
                registry: "code.sandbox.example".into(),
            },
            CliError::RawDeploy {
                command: "scp".into(),
            },
            CliError::InvalidSource("x".into()),
            CliError::Io {
                context: "c".into(),
                source: std::io::Error::other("e"),
            },
        ];
        for error in &all {
            match error {
                CliError::NoVm
                | CliError::UnknownVm { .. }
                | CliError::Config { .. }
                | CliError::NotInWorktree
                | CliError::SlotName { .. }
                | CliError::Unreachable { .. }
                | CliError::DaemonMissing { .. }
                | CliError::ProtocolMismatch { .. }
                | CliError::Remote(_)
                | CliError::ToolMissing { .. }
                | CliError::CommandFailed { .. }
                | CliError::Unsupported { .. }
                | CliError::NoManifest { .. }
                | CliError::UnknownComponent { .. }
                | CliError::BootComponent { .. }
                | CliError::ScopeMismatch { .. }
                | CliError::ArchMismatch { .. }
                | CliError::LowDisk { .. }
                | CliError::RegistryLogin { .. }
                | CliError::RawDeploy { .. }
                | CliError::InvalidSource(_)
                | CliError::Io { .. } => {}
            }
        }
        all
    }

    #[test]
    fn every_error_variant_has_a_hint() {
        for error in samples() {
            assert!(!error.hint().trim().is_empty(), "{error:?} has no hint");
            let json = error.to_json();
            assert!(json["error"].as_str().is_some_and(|e| !e.is_empty()));
            assert!(json["hint"].as_str().is_some_and(|h| !h.is_empty()));
            assert!(error.render().contains("\nhint: "));
        }
    }

    #[test]
    fn remote_errors_keep_the_daemon_hint() {
        let error = CliError::Remote(RpcError {
            code: 5,
            message: "scope".into(),
            hint: "tilo-dev swap vm fs".into(),
            see: Some("tilo-dev guide swapping".into()),
        });
        assert_eq!(error.hint(), "tilo-dev swap vm fs");
        assert_eq!(error.see().as_deref(), Some("tilo-dev guide swapping"));
    }
}
