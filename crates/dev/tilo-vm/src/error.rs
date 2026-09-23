//! The one error type. Every variant says what went wrong, and `hint` is
//! always the exact next command; `see` names the guide topic to read.

use serde_json::json;
use tilo_vm_protocol::RpcError;

use crate::host::Missing;

/// The script that builds a dev disk locally, relative to the worktree.
pub const LOCAL_BUILD_SCRIPT: &str = "os/dev/build-dev-image.sh";

// Two separate credentials, never interchangeable, although both take a
// forge token with package read access as the secret.

/// What the forge token (`FORGEJO_TOKEN`, or an fgj or tea login) is for.
pub const FORGE_TOKEN_USE: &str =
    "read only for dev disk downloads (vm create without --image, doctor's published-disk check)";
/// What a podman registry login for code.podesta.ai is for.
pub const REGISTRY_LOGIN_USE: &str = "used only for image pulls: swap --from ci:/sha: layers (pulled in the VM when it has podman, else on this host) and vm upgrade (in the VM, with the VM's own login)";

/// What to do when no dev disk is published: `local_build` is the path of
/// [`LOCAL_BUILD_SCRIPT`] when this checkout has it.
pub fn no_disk_help(local_build: Option<&str>) -> String {
    let local = match local_build {
        Some(script) => format!(
            "or build a disk locally: {script}, then tilo-vm vm create <name> --image <disk.qcow2>"
        ),
        None => format!(
            "no local dev-image build is available in this checkout ({LOCAL_BUILD_SCRIPT} is missing)"
        ),
    };
    format!(
        "ask a human to dispatch the \"Release / Dev VM disk\" workflow (.forgejo/workflows/dev-disk.yml)   |   or, if you may dispatch CI yourself: fgj workflow run dev-disk.yml -R qos/qos, then fgj run watch   |   or ask a human for a shared VM and use it: tilo-vm vm use <name>   |   {local}"
    )
}

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
    #[error("VM '{vm}' has no tilo-vmd; it is not running a Tilo dev image")]
    DaemonMissing { vm: String },
    #[error("VM '{vm}' refuses its root SSH key {key}")]
    RootKeyRefused {
        vm: String,
        key: String,
        /// The one-time command that installs the key.
        install: String,
        /// Whether the adapter passed the key as a credential at creation.
        credentials: bool,
    },
    #[error("tilo-vm speaks protocol {client} but the VM's tilo-vmd speaks {daemon}")]
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
    NoDiskPublished {
        tag: String,
        url: String,
        /// The local build script, when this checkout has one.
        local_build: Option<String>,
    },
    #[error("tag '{tag}' has no {format} disk for {arch} (published: {})", if available.is_empty() { "none".to_owned() } else { available.join(", ") })]
    NoDiskForHost {
        tag: String,
        arch: String,
        format: String,
        available: Vec<String>,
        local_build: Option<String>,
    },
    #[error("the {} adapter cannot run here: {}", .0.adapter, .0.detail)]
    NoHypervisor(Missing),
    #[error("--image {path} is not a file")]
    ImageNotFound { path: String },
    #[error("the VM disk {path} already exists")]
    DiskExists { path: String },
    #[error("{place} has {free} free, but {what} needs {needed}")]
    NoSpace {
        place: String,
        free: String,
        what: String,
        needed: String,
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
    #[error("{what} is busy with another tilo-vm command")]
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
                "create one: tilo-vm vm create <name>   |   or ask a human which shared VM to use; they set it with tilo-vm vm use <name> --global, or in {registry}"
            ),
            Self::UnknownVm { known, .. } if known.is_empty() => {
                "create one: tilo-vm vm create <name>   (downloads the published dev disk)   |   or ask a human which shared VM to use".into()
            }
            Self::UnknownVm { known, .. } => format!(
                "tilo-vm vm use {}   (known: {})",
                known.first().map_or("", String::as_str),
                known.join(", ")
            ),
            Self::Config { path, .. } => format!("fix or remove {path}; tilo-vm doctor re-checks it"),
            Self::NotInWorktree => "name the slot explicitly, e.g. tilo-vm swap --slot my-slot shell".into(),
            Self::SlotName { .. } => {
                "name the slot explicitly (1-28 lowercase letters, digits, dashes), e.g. tilo-vm slot create my-slot"
                    .into()
            }
            Self::Unreachable { debug, .. } => {
                let mut hint = String::from("tilo-vm vm start   (is it running?)   then   tilo-vm doctor");
                for command in debug {
                    hint.push_str("   |   ");
                    hint.push_str(command);
                }
                hint
            }
            Self::RootKeyRefused { install, credentials: true, .. } => format!(
                "the key goes in at creation as the systemd credential ssh.authorized_keys.root, and this VM did not take it (created before that, or from another disk). Install it once, with the dev account's password from os/account.env (quickuser, tilo): {install}"
            ),
            Self::RootKeyRefused { install, .. } => format!(
                "this hypervisor has no credential channel, so root's key is installed once after the first boot, with the dev account's password from os/account.env (quickuser, tilo): {install}"
            ),
            Self::DaemonMissing { .. } => {
                "move the VM to a dev image: tilo-vm vm upgrade --image code.podesta.ai/qos/tilo-vm:<tag>".into()
            }
            Self::ProtocolMismatch { client, daemon } if client > daemon => {
                "the VM is older than this tilo-vm: tilo-vm vm upgrade".into()
            }
            Self::ProtocolMismatch { .. } => {
                "this tilo-vm is older than the VM: cargo install --locked --path crates/dev/tilo-vm".into()
            }
            Self::Remote(error) => error.hint.clone(),
            Self::ToolMissing { tool, .. } => format!("install {tool} and make sure it is on PATH; tilo-vm doctor re-checks"),
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
                format!("VM-wide components go into the vm slot: tilo-vm swap --slot vm {component}")
            }
            Self::ScopeMismatch { component, .. } => {
                format!("session components go into your own slot: tilo-vm swap {component}")
            }
            Self::ArchMismatch { .. } => {
                "use a VM of the host's architecture, or opt into slow emulation with --emulated".into()
            }
            Self::LowDisk { .. } => "tilo-vm gc --dry-run   then   tilo-vm gc".into(),
            Self::RegistryLogin { place, registry } if place == "this host" => format!(
                "podman login {registry}   (user: your forge login, password: a forge token with package read access). podman keeps its own login: FORGEJO_TOKEN and fgj/tea logins do not count for image pulls"
            ),
            Self::RegistryLogin { registry, .. } => format!(
                "tilo-vm vm ssh --raw -- podman login --authfile /etc/ostree/auth.json {registry}   (password: a forge token with package read access). The VM keeps its own login for image pulls; this host's logins do not reach it"
            ),
            Self::RawDeploy { .. } => {
                "deploy with tilo-vm swap <component> (it records a generation you can roll back); pass --raw to run it anyway"
                    .into()
            }
            Self::InvalidSource(_) => {
                "use --from worktree, layer:<hash>, sha:<commit> or ci:<tag>, e.g. tilo-vm swap shell --from ci:main-1a2b3c4".into()
            }
            Self::HostBuildUnsupported { .. } => {
                "build in the VM instead: tilo-vm swap <component> --builder vm   (the default)".into()
            }
            Self::VmCannotBuild { .. } if cfg!(target_os = "linux") => {
                "build on this host: tilo-vm swap <component> --builder host   |   or move the VM to a dev image with a toolchain: tilo-vm vm upgrade".into()
            }
            Self::VmCannotBuild { .. } => {
                "move the VM to a dev image with a toolchain: tilo-vm vm upgrade   |   or swap a CI build: --from ci:<tag>".into()
            }
            Self::NoDiskPublished { tag, local_build, .. } if tag != "(any)" => format!(
                "pick a published tag (tilo-vm vm create <name> without --tag takes the newest)   |   {}",
                no_disk_help(local_build.as_deref())
            ),
            Self::NoDiskPublished { local_build, .. } => no_disk_help(local_build.as_deref()),
            Self::NoDiskForHost { local_build, .. } => format!(
                "pick a tag that has one: tilo-vm vm create <name> --tag <tag>   |   {}",
                no_disk_help(local_build.as_deref())
            ),
            Self::NoHypervisor(missing) => missing.fix.clone(),
            Self::ImageNotFound { .. } => {
                "check the path, or leave out --image to download the published dev disk (tilo-vm doctor says whether one is published)".into()
            }
            Self::DiskExists { .. } => {
                "pass another path with --disk, or delete that file if no registered VM uses it (tilo-vm vm list)".into()
            }
            Self::NoSpace { .. } => {
                "free space on that filesystem and rerun; an interrupted download resumes".into()
            }
            Self::ForgeAuth { token: None, .. } => {
                "export FORGEJO_TOKEN=<forge token with package read access>, or log in with fgj or tea, then rerun. Disk downloads read only these, never podman's registry login".into()
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
                "tilo-vm swap app:<dir> with one of: {}",
                known.join(", ")
            ),
            Self::MissingInImage { .. } => {
                "that image does not ship this component (optional, or an older image); pick another tag or build it: --from worktree".into()
            }
            Self::Busy { .. } => {
                "wait for the other command to finish and retry, or pass --force to go ahead anyway".into()
            }
            Self::InvalidInput(_) => {
                "e.g. tilo-vm input key ctrl+alt+t   |   input type 'hello'   |   input move 400 300   |   input click left --at 400 300   (text beyond a US layout: tilo-vm control type '<text>')".into()
            }
            Self::Io { .. } => "check the path and permissions named above; tilo-vm doctor checks the setup".into(),
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
            | Self::NoHypervisor(_)
            | Self::ImageNotFound { .. }
            | Self::DiskExists { .. }
            | Self::ChecksumMismatch { .. }
            | Self::DownloadFailed { .. }
            | Self::RootKeyRefused { .. } => "platforms",
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
            Self::LowDisk { .. } | Self::NoSpace { .. } => "storage",
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
        Some(format!("tilo-vm guide {topic}"))
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

impl From<tilo_vm_protocol::ClientError> for CliError {
    fn from(error: tilo_vm_protocol::ClientError) -> Self {
        use tilo_vm_protocol::ClientError;
        match error {
            ClientError::Rpc(error) => Self::Remote(error),
            ClientError::VersionMismatch { client, daemon } => {
                Self::ProtocolMismatch { client, daemon }
            }
            ClientError::Io(source) => Self::Io {
                context: "talking to tilo-vmd".into(),
                source,
            },
            ClientError::Closed | ClientError::Protocol(_) => Self::CommandFailed {
                command: "tilo-vmd rpc".into(),
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
                registry: "/home/u/.config/tilo-vm/vms.toml".into(),
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
                debug: vec!["tilo-vm vm serial".into()],
            },
            CliError::DaemonMissing { vm: "dev".into() },
            CliError::RootKeyRefused {
                vm: "dev".into(),
                key: "/cfg/keys/dev_ed25519".into(),
                install: "ssh -t quickuser@vm '...'".into(),
                credentials: true,
            },
            CliError::RootKeyRefused {
                vm: "dev".into(),
                key: "/cfg/keys/dev_ed25519".into(),
                install: "ssh -t quickuser@vm '...'".into(),
                credentials: false,
            },
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
                url: "https://x/tilo-vm-disk.json".into(),
                local_build: None,
            },
            CliError::NoDiskPublished {
                tag: "(any)".into(),
                url: "u".into(),
                local_build: Some("/w/os/dev/build-dev-image.sh".into()),
            },
            CliError::NoDiskForHost {
                tag: "t".into(),
                arch: "aarch64".into(),
                format: "qcow2".into(),
                available: vec!["x86_64 qcow2".into()],
                local_build: None,
            },
            CliError::NoHypervisor(Missing {
                adapter: "libvirt".into(),
                detail: "virsh not on PATH".into(),
                fix: "sudo dnf install libvirt-daemon-kvm virt-install virt-manager".into(),
            }),
            CliError::ImageNotFound { path: "x".into() },
            CliError::DiskExists { path: "x".into() },
            CliError::NoSpace {
                place: "p".into(),
                free: "1 GB".into(),
                what: "the download".into(),
                needed: "4 GB".into(),
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
                | CliError::NoHypervisor(_)
                | CliError::ImageNotFound { .. }
                | CliError::DiskExists { .. }
                | CliError::NoSpace { .. }
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
                | CliError::RootKeyRefused { .. }
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
                    error.see().is_some_and(|s| s.starts_with("tilo-vm guide ")),
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
        assert!(hint.contains("tilo-vm vm create <name>"), "{hint}");
        assert!(hint.contains("ask a human"), "{hint}");
        assert!(hint.contains("tilo-vm vm use"), "{hint}");
        assert!(hint.contains("/cfg/vms.toml"), "{hint}");
        let host = CliError::HostBuildUnsupported { os: "macos".into() }.hint();
        assert!(host.contains("--builder vm"), "{host}");
    }

    #[test]
    fn no_published_disk_says_plainly_what_to_do() {
        let without = CliError::NoDiskPublished {
            tag: "(any)".into(),
            url: "u".into(),
            local_build: None,
        }
        .hint();
        assert!(without.contains("Release / Dev VM disk"), "{without}");
        assert!(
            without.contains(".forgejo/workflows/dev-disk.yml"),
            "{without}"
        );
        assert!(
            without.contains("fgj workflow run dev-disk.yml"),
            "{without}"
        );
        assert!(without.contains("tilo-vm vm use <name>"), "{without}");
        assert!(
            without.contains("no local dev-image build is available in this checkout"),
            "{without}"
        );
        let with = CliError::NoDiskPublished {
            tag: "(any)".into(),
            url: "u".into(),
            local_build: Some("/w/os/dev/build-dev-image.sh".into()),
        }
        .hint();
        assert!(
            with.contains("build a disk locally: /w/os/dev/build-dev-image.sh"),
            "{with}"
        );
    }

    #[test]
    fn credential_hints_name_the_one_login_each_operation_reads() {
        let host = CliError::RegistryLogin {
            place: "this host".into(),
            registry: "code.podesta.ai".into(),
        }
        .hint();
        assert!(host.contains("podman login code.podesta.ai"), "{host}");
        assert!(
            host.contains("FORGEJO_TOKEN and fgj/tea logins do not count"),
            "{host}"
        );
        let forge = CliError::ForgeAuth {
            url: "u".into(),
            status: 401,
            token: None,
        }
        .hint();
        assert!(forge.contains("FORGEJO_TOKEN"), "{forge}");
        assert!(forge.contains("never podman's registry login"), "{forge}");
        assert!(FORGE_TOKEN_USE.contains("dev disk downloads"));
        assert!(REGISTRY_LOGIN_USE.contains("swap --from ci:/sha:"));
        assert!(REGISTRY_LOGIN_USE.contains("vm upgrade"));
    }

    #[test]
    fn a_missing_hypervisor_names_the_adapter_and_the_fix() {
        let error = CliError::NoHypervisor(Missing {
            adapter: "libvirt".into(),
            detail: "virsh not on PATH".into(),
            fix: "sudo dnf install libvirt".into(),
        });
        assert_eq!(
            error.to_string(),
            "the libvirt adapter cannot run here: virsh not on PATH"
        );
        assert_eq!(error.hint(), "sudo dnf install libvirt");
    }

    #[test]
    fn remote_errors_keep_the_daemon_hint() {
        let error = CliError::Remote(RpcError {
            code: 5,
            message: "scope".into(),
            hint: "tilo-vm swap --slot vm fs".into(),
            see: Some("tilo-vm guide swapping".into()),
        });
        assert_eq!(error.hint(), "tilo-vm swap --slot vm fs");
        assert_eq!(error.see().as_deref(), Some("tilo-vm guide swapping"));
    }
}
