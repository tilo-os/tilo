//! The one error type. Every variant says what went wrong, and `hint` is
//! always the exact next command; `see` names the guide topic to read.

use serde_json::json;
use tilo_dev_protocol::RpcError;

/// How to get a dev disk without the published one.
pub const LOCAL_DISK: &str = "or build a disk locally: os/dev/build-dev-image.sh, then tilo-dev vm create <name> --image <disk.qcow2>";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("no VM selected")]
    NoVm { registry: String },
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
    #[error("building on the host needs Linux; this host runs {os}")]
    HostBuildUnsupported { os: String },
    #[error("the VM's image cannot build: it has no {}", missing.join(", "))]
    VmCannotBuild { missing: Vec<String> },
    #[error("no dev disk is published for tag '{tag}' ({url})")]
    NoDiskPublished { tag: String, url: String },
    #[error("tag '{tag}' has no {format} disk for {arch} (published: {})", if available.is_empty() { "none".to_owned() } else { available.join(", ") })]
    NoDiskForHost {
        tag: String,
        arch: String,
        format: String,
        available: Vec<String>,
    },
    #[error("{url} refused the request (HTTP {status})")]
    ForgeAuth {
        url: String,
        status: u16,
        /// Where the token came from, when one was found.
        token: Option<String>,
    },
    #[error("{file} does not match its published checksum (expected {expected}, got {actual})")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("downloading {url} failed")]
    DownloadFailed { url: String },
    #[error("no app 'apps/{name}' with a tilo-app.toml in this worktree")]
    UnknownApp { name: String, known: Vec<String> },
    #[error("{image} has no {path}")]
    MissingInImage { image: String, path: String },
    #[error("{what} is busy with another tilo-dev command")]
    Busy { what: String },
    #[error("{0}")]
    InvalidInput(String),
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

    #[expect(clippy::too_many_lines, reason = "one hint per variant, side by side")]
    pub fn hint(&self) -> String {
        match self {
            Self::NoVm { registry } => format!(
                "create one: tilo-dev vm create <name>   |   or ask a human which shared VM to use; they set it with tilo-dev vm use <name> --global, or in {registry}"
            ),
            Self::UnknownVm { known, .. } if known.is_empty() => {
                "create one: tilo-dev vm create <name>   (downloads the published dev disk)   |   or ask a human which shared VM to use".into()
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
                "move the VM to a dev image: tilo-dev vm upgrade --image code.podesta.ai/qos/tilo-dev:<tag>".into()
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
            Self::InvalidSource(_) => {
                "use --from worktree, layer:<hash>, sha:<commit> or ci:<tag>, e.g. tilo-dev swap shell --from ci:main-1a2b3c4".into()
            }
            Self::HostBuildUnsupported { .. } => {
                "build in the VM instead: tilo-dev swap <component> --builder vm   (the default)".into()
            }
            Self::VmCannotBuild { .. } if cfg!(target_os = "linux") => {
                "build on this host: tilo-dev swap <component> --builder host   |   or move the VM to a dev image with a toolchain: tilo-dev vm upgrade".into()
            }
            Self::VmCannotBuild { .. } => {
                "move the VM to a dev image with a toolchain: tilo-dev vm upgrade   |   or swap a CI build: --from ci:<tag>".into()
            }
            Self::NoDiskPublished { .. } | Self::NoDiskForHost { .. } => format!(
                "pick a published tag: tilo-dev vm create <name> --tag <tag>   |   {LOCAL_DISK}"
            ),
            Self::ForgeAuth { token: None, .. } => {
                "export FORGEJO_TOKEN=<forge token with package read access>, or log in with fgj or tea, then rerun".into()
            }
            Self::ForgeAuth { token: Some(source), .. } => format!(
                "the token from {source} lacks package read access or has expired; export FORGEJO_TOKEN=<token with package read access> and rerun"
            ),
            Self::ChecksumMismatch { .. } => {
                "rerun the command; the bad download was deleted. If it repeats, the published disk is broken: tell a human".into()
            }
            Self::DownloadFailed { .. } => {
                "rerun the same command: the download resumes where it stopped".into()
            }
            Self::UnknownApp { known, .. } if known.is_empty() => {
                "apps live in apps/<dir>/tilo-app.toml; run from a Tilo worktree".into()
            }
            Self::UnknownApp { known, .. } => format!(
                "tilo-dev swap app:<dir> with one of: {}",
                known.join(", ")
            ),
            Self::MissingInImage { .. } => {
                "that image does not ship this component (optional, or an older image); pick another tag or build it: --from worktree".into()
            }
            Self::Busy { .. } => {
                "wait for the other command to finish and retry, or pass --force to go ahead anyway".into()
            }
            Self::InvalidInput(_) => {
                "e.g. tilo-dev input key ctrl+alt+t   |   input type 'hello'   |   input move 400 300   |   input click left --at 400 300".into()
            }
            Self::Io { .. } => "check the path and permissions named above; tilo-dev doctor checks the setup".into(),
        }
    }

    pub fn see(&self) -> Option<String> {
        let topic = match self {
            Self::NoVm { .. } => "quickstart",
            Self::UnknownVm { .. }
            | Self::Config { .. }
            | Self::ArchMismatch { .. }
            | Self::NoDiskPublished { .. }
            | Self::NoDiskForHost { .. }
            | Self::ChecksumMismatch { .. }
            | Self::DownloadFailed { .. } => "platforms",
            Self::NotInWorktree | Self::SlotName { .. } => "concepts",
            Self::Remote(error) => return error.see.clone(),
            Self::NoManifest { .. }
            | Self::UnknownComponent { .. }
            | Self::BootComponent { .. }
            | Self::ScopeMismatch { .. }
            | Self::RawDeploy { .. }
            | Self::InvalidSource(_)
            | Self::HostBuildUnsupported { .. }
            | Self::VmCannotBuild { .. }
            | Self::UnknownApp { .. }
            | Self::MissingInImage { .. } => "swapping",
            Self::LowDisk { .. } => "storage",
            Self::InvalidInput(_) => "driving",
            Self::Unreachable { .. }
            | Self::DaemonMissing { .. }
            | Self::ProtocolMismatch { .. }
            | Self::ToolMissing { .. }
            | Self::RegistryLogin { .. }
            | Self::ForgeAuth { .. }
            | Self::CommandFailed { .. }
            | Self::Unsupported { .. }
            | Self::Busy { .. }
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
            CliError::NoVm {
                registry: "/home/u/.config/tilo-dev/vms.toml".into(),
            },
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
                registry: "code.podesta.ai".into(),
            },
            CliError::RegistryLogin {
                place: "VM dev".into(),
                registry: "code.podesta.ai".into(),
            },
            CliError::RawDeploy {
                command: "scp".into(),
            },
            CliError::InvalidSource("x".into()),
            CliError::HostBuildUnsupported { os: "macos".into() },
            CliError::VmCannotBuild {
                missing: vec!["cargo".into()],
            },
            CliError::NoDiskPublished {
                tag: "main-abc1234".into(),
                url: "https://x/tilo-dev-disk.json".into(),
            },
            CliError::NoDiskForHost {
                tag: "t".into(),
                arch: "aarch64".into(),
                format: "qcow2".into(),
                available: vec!["x86_64 qcow2".into()],
            },
            CliError::ForgeAuth {
                url: "u".into(),
                status: 401,
                token: None,
            },
            CliError::ForgeAuth {
                url: "u".into(),
                status: 403,
                token: Some("FORGEJO_TOKEN".into()),
            },
            CliError::ChecksumMismatch {
                file: "f".into(),
                expected: "a".into(),
                actual: "b".into(),
            },
            CliError::DownloadFailed { url: "u".into() },
            CliError::UnknownApp {
                name: "x".into(),
                known: vec![],
            },
            CliError::UnknownApp {
                name: "x".into(),
                known: vec!["notes".into()],
            },
            CliError::MissingInImage {
                image: "i".into(),
                path: "/usr/bin/browser".into(),
            },
            CliError::Busy {
                what: "the VM".into(),
            },
            CliError::InvalidInput("bad".into()),
            CliError::Io {
                context: "c".into(),
                source: std::io::Error::other("e"),
            },
        ];
        for error in &all {
            match error {
                CliError::NoVm { .. }
                | CliError::HostBuildUnsupported { .. }
                | CliError::VmCannotBuild { .. }
                | CliError::NoDiskPublished { .. }
                | CliError::NoDiskForHost { .. }
                | CliError::ForgeAuth { .. }
                | CliError::ChecksumMismatch { .. }
                | CliError::DownloadFailed { .. }
                | CliError::UnknownApp { .. }
                | CliError::MissingInImage { .. }
                | CliError::Busy { .. }
                | CliError::InvalidInput(_)
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
            if !matches!(error, CliError::Remote(_)) {
                assert!(
                    error
                        .see()
                        .is_some_and(|s| s.starts_with("tilo-dev guide ")),
                    "{error:?} has no see: pointer"
                );
            }
        }
    }

    #[test]
    fn first_contact_hints_cover_both_ways_to_a_vm() {
        let hint = CliError::NoVm {
            registry: "/cfg/vms.toml".into(),
        }
        .hint();
        assert!(hint.contains("tilo-dev vm create <name>"), "{hint}");
        assert!(hint.contains("ask a human"), "{hint}");
        assert!(hint.contains("tilo-dev vm use"), "{hint}");
        assert!(hint.contains("/cfg/vms.toml"), "{hint}");
        let host = CliError::HostBuildUnsupported { os: "macos".into() }.hint();
        assert!(host.contains("--builder vm"), "{host}");
        let disk = CliError::NoDiskPublished {
            tag: "t".into(),
            url: "u".into(),
        }
        .hint();
        assert!(disk.contains("os/dev/build-dev-image.sh"), "{disk}");
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
