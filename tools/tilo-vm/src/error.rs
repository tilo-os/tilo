//! The one error type. Every variant says what went wrong, and `hint` is
//! always the exact next command; `see` names the guide topic to read.

use serde_json::json;
use tilo_device_protocol::RpcError;

use crate::host::Missing;

// Images are public on registry.tilo.so: pulls need no login. Only dev disk
// downloads, from the forge's package registry, need a credential.

pub const FORGE_TOKEN_USE: &str =
    "read only for dev disk downloads (vm create without --image, doctor's published-disk check)";

pub const NOT_BOOTC_FIX: &str = "create a VM from a dev image instead: tilo-vm vm create <name>   (the published dev disk; or --image <dev disk.qcow2>)   |   or ask whoever set this VM up to move it onto a dev image";

pub fn no_disk_help() -> String {
    format!(
        "ask a human for a shared VM and use it: tilo-vm vm use <name>   |   or build a disk of this worktree on t: tilo-vm image build --disk, then tilo-vm vm create <name> --image target/tilo-vm/target/publish/os-dev-disk/{arch}/disk/disk.qcow2 (tilo-vm guide images)",
        arch = std::env::consts::ARCH
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
    #[error("--target {target} conflicts with target {name}; name one development target")]
    TargetConflict { target: String, name: String },
    #[error("{path}: {detail}")]
    Config { path: String, detail: String },
    #[error("not inside a git worktree, so there is no default account")]
    NotInWorktree,
    #[error("branch '{branch}' does not make a usable account name")]
    AccountName { branch: String },
    #[error("device '{vm}' does not answer at {address} ({detail}){lookup}: it is asleep or gone")]
    DeviceAsleep {
        vm: String,
        address: String,
        detail: String,
        lookup: String,
        wakeable: bool,
    },
    #[error("this machine has no Tilo login to reach devices as: {detail}")]
    NotLoggedIn { detail: String },
    #[error(
        "'{vm}' has not answered as a Tilo device yet, so tilo-vm does not know which device it is"
    )]
    NotJoined { vm: String },
    #[error("'{vm}' does not let this machine act on {resource}")]
    NotGranted { vm: String, resource: String },
    #[error("'{vm}' runs an image that predates the {action} step")]
    DeviceTooOld { vm: String, action: String },
    #[error("sent {vm}'s magic packet ({sent}), but it did not answer within {seconds}s")]
    WakeTimeout {
        vm: String,
        sent: String,
        seconds: u64,
        wake: Option<String>,
    },
    #[error("cannot reach '{vm}': {detail}")]
    Unreachable {
        vm: String,
        detail: String,
        debug: Vec<String>,
    },
    #[error("VM '{vm}' is not running a bootc dev image; {command} needs one")]
    NotBootc { vm: String, command: String },
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
    #[error("'{component}' has {scope} scope and cannot go into account '{account}'")]
    ScopeMismatch {
        component: String,
        account: String,
        scope: String,
    },
    #[error("this host is {host} but the VM would be {requested}")]
    ArchMismatch { host: String, requested: String },
    #[error("not enough space on {place}: {free} is free, and tilo-vm keeps at least {floor} free")]
    LowDisk {
        place: String,
        free: String,
        floor: String,
    },
    #[error("vm ssh refuses `{command}`: it {change}")]
    RawWrite { command: String, change: String },
    #[error("unknown source '{0}'")]
    InvalidSource(String),
    #[error("building on the host needs Linux; this host runs {os}")]
    HostBuildUnsupported { os: String },
    #[error("the VM's image cannot build: it has no {}", missing.join(", "))]
    VmCannotBuild {
        missing: Vec<String>,
        bootc: Option<bool>,
    },
    #[error("no dev disk is published for tag '{tag}' ({url})")]
    NoDiskPublished { tag: String, url: String },
    #[error("tag '{tag}' has no {format} disk for {arch} (published: {})", if available.is_empty() { "none".to_owned() } else { available.join(", ") })]
    NoDiskForHost {
        tag: String,
        arch: String,
        format: String,
        available: Vec<String>,
    },
    #[error("the {} adapter cannot run here: {}", .0.adapter, .0.detail)]
    NoHypervisor(Missing),
    #[error("--image {path} is not a file")]
    ImageNotFound { path: String },
    #[error("{flag} {path} {detail}")]
    ImageFormat {
        flag: String,
        path: String,
        detail: String,
    },
    #[error("VM {vm} is running")]
    VmRunning { vm: String },
    #[error("the VM disk {path} already exists")]
    DiskExists { path: String },
    #[error("cannot branch {what}")]
    Branch { what: String, fix: String },
    #[error("not enough space on {place}: {what} needs {needed}, {free} is free")]
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
    #[error("render node {path} does not exist")]
    RenderNodeMissing { path: String, nodes: Vec<String> },
    #[error("source file '{path}' is not what the swap snapshotted")]
    SourceChanged { path: String },
    #[error("no app set named '{set}'")]
    UnknownAppSet { set: String, known: Vec<String> },
    #[error("installing the {set} apps into VM '{vm}' failed")]
    PackagesFailed { set: String, vm: String },
    #[error("the kernel build failed")]
    KernelBuild { log: String },
    #[error("VM '{vm}' runs kernel {running}, not the {expected} just installed ({detail})")]
    KernelNotRunning {
        vm: String,
        expected: String,
        running: String,
        detail: String,
    },
    #[error("{image} {problem}")]
    ImageRefused {
        image: String,
        problem: String,
        fix: String,
    },
    #[error(
        "{image} is built FROM {base}, created {new}, which is older than the base of the image the VM runs (created {running})"
    )]
    BaseOlder {
        image: String,
        base: String,
        new: String,
        running: String,
    },
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
    #[error("no installer medium at {path}")]
    ScenarioMedium { path: String },
    #[error("no {what} at {path}")]
    ScenarioInput { what: String, path: String },
    #[error("the install scenario failed in {phase}: {detail}")]
    Scenario {
        phase: String,
        detail: String,
        artifacts: String,
        console: String,
    },
    #[error("{what} runs from a Tilo worktree (it drives os/dev/vm.sh)")]
    NeedsWorktree { what: String },
    #[error("{detail}")]
    Qemu { detail: String, remedy: String },
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
            Self::TargetConflict { target, name } => format!(
                "use --target {target} without a positional target, or omit --target and use {name}"
            ),
            Self::Config { path, .. } => format!("fix or remove {path}; tilo-vm doctor re-checks it"),
            Self::NotInWorktree => "name the account explicitly, e.g. tilo-vm swap --account my-account tilo-session".into(),
            Self::AccountName { .. } => {
                "name the account explicitly (1-28 lowercase letters, digits, dashes), e.g. tilo-vm account create my-account"
                    .into()
            }
            Self::DeviceAsleep { vm, wakeable: true, .. } => format!(
                "wake it: tilo-vm vm wake {vm}   (its magic packet, sent from this machine on its LAN; vm status says whether that can wake it)   |   or at its power button, then tilo-vm vm status {vm}. Next time keep it from sleeping: tilo-vm vm awake {vm} --for 2h"
            ),
            Self::DeviceAsleep { vm, wakeable: false, .. } => format!(
                "wake it at its power button or keyboard, then tilo-vm vm status {vm}: that records its MAC and hostname, so a new address is found by name. Next time keep it from sleeping: tilo-vm vm awake {vm} --for 2h"
            ),
            Self::NotLoggedIn { .. } => {
                "log this machine in as one of your devices: on a device of yours run tilo device add, then here tilo login <the ticket it prints>   |   a machine of its own: tilo login --new-identity   (tilo-vm guide devices)".into()
            }
            Self::NotJoined { vm } => format!(
                "start it and let it answer once: tilo-vm vm start {vm}   |   a machine you set up yourself: tilo-vm vm add {vm} --device <its device key>   (tilo-vm guide devices)"
            ),
            Self::NotGranted { resource, .. } => format!(
                "grant this machine {resource} in your workspace, from a device that may grant it: tilo grant {resource} <this machine's device key>   (tilo-vm doctor names the key; tilo-vm guide devices)"
            ),
            Self::DeviceTooOld { vm, .. } => format!(
                "move it onto a current dev image: tilo-vm vm upgrade {vm}   |   or create a new VM: tilo-vm vm create <name>"
            ),
            Self::WakeTimeout { vm, wake: Some(wake), .. } => format!(
                "it reported: {wake}. Someone has to wake it at the machine; tilo-vm vm status {vm} tells when it answers, and tilo-vm vm awake {vm} --for 2h keeps it from sleeping again"
            ),
            Self::WakeTimeout { vm, wake: None, .. } => format!(
                "a magic packet wakes a machine only when its network interface keeps listening in suspend (Ethernet Wake-on-LAN, enabled); someone has to wake it at the machine, then tilo-vm vm status {vm} records what it can do, and tilo-vm vm awake {vm} --for 2h keeps it from sleeping again"
            ),
            Self::Unreachable { vm, debug, .. } => {
                let mut hint =
                    format!("tilo-vm vm start {vm}   (is it running?)   then   tilo-vm doctor");
                for command in debug {
                    hint.push_str("   |   ");
                    hint.push_str(command);
                }
                hint
            }
            Self::NotBootc { .. } => NOT_BOOTC_FIX.into(),
            Self::Remote(error) => error.hint.clone(),
            Self::ToolMissing { tool, .. } => format!("install {tool} and make sure it is on PATH; tilo-vm doctor re-checks"),
            Self::CommandFailed { stderr, .. } if stderr.contains("no surface") => {
                "a --gl VM renders through a GL context outside the guest, and QEMU keeps no readable surface for that scanout: its real display cannot be captured from outside the guest. Use tilo-vm shot while the compositor is up, or a VM created without --gl when a frame from outside the guest is what a test needs (tilo-vm guide driving)".into()
            }
            Self::CommandFailed { stderr, .. } if !stderr.is_empty() => {
                "the command's own message above names the cause".into()
            }
            Self::CommandFailed { command, .. } => format!(
                "it printed no reason here; rerun it by hand to see its full output: {command}"
            ),
            Self::Unsupported { alternative, .. } => alternative.clone(),
            Self::NoManifest { .. } => {
                "run from a Tilo worktree that has os/components.toml, or swap a prebuilt layer: --from layer:<hash>".into()
            }
            Self::UnknownComponent { known, .. } => format!("pick one of: {}", known.join(", ")),
            Self::BootComponent { driver, .. } => format!(
                "use the boot scenario driver instead: {driver}   (of the boot components only the kernel swaps: tilo-vm swap kernel)"
            ),
            Self::ScopeMismatch { component, scope, .. } if scope == "vm" => {
                format!("VM-wide components go into the vm account: tilo-vm swap --account vm {component}")
            }
            Self::ScopeMismatch { component, .. } => {
                format!("session components go into your own account: tilo-vm swap {component}")
            }
            Self::ArchMismatch { .. } => {
                "use a VM of the host's architecture, or opt into slow emulation with --emulated".into()
            }
            Self::LowDisk { .. } => {
                "free space there (tilo-vm gc --dry-run, then tilo-vm gc)   |   or keep the VM's disk on a filesystem with room: tilo-vm vm create <name> --disk /path/on/another/filesystem.qcow2".into()
            }
            Self::RawWrite { .. } => {
                "change the VM through tilo-vm instead, so it can be rolled back and other agents see it: a component: tilo-vm swap <component>   |   the image (/etc, units, packages): tilo-vm vm upgrade --from local   |   to run it by hand anyway, --raw is the escape hatch: tilo-vm vm ssh --raw -- <command>"
                    .into()
            }
            Self::InvalidSource(_) => {
                "use --from worktree, layer:<hash>, sha:<commit> or ci:<tag>, e.g. tilo-vm swap tilo-session --from ci:main-1a2b3c4".into()
            }
            Self::HostBuildUnsupported { .. } => {
                "build in the VM instead: tilo-vm swap <component> --builder vm   (the default)".into()
            }
            Self::VmCannotBuild { bootc, .. } => {
                let image = if *bootc == Some(true) {
                    "move the VM to a dev image with a toolchain: tilo-vm vm upgrade"
                } else {
                    "use a VM created from a current dev image, which has the toolchain: tilo-vm vm create <name>"
                };
                if cfg!(target_os = "linux") {
                    format!("build on this host: tilo-vm swap <component> --builder host   |   or {image}")
                } else {
                    format!("{image}   |   or swap a CI build: --from ci:<tag>")
                }
            }
            Self::NoDiskPublished { tag, .. } if tag != "(any)" => format!(
                "pick a published tag (tilo-vm vm create <name> without --tag takes the newest)   |   {}",
                no_disk_help()
            ),
            Self::NoDiskPublished { .. } => no_disk_help(),
            Self::NoDiskForHost { arch, .. } => format!(
                "each architecture publishes on its own, so {arch}'s check may still be running or may have failed: t main   |   pick a tag that has one: tilo-vm vm create <name> --tag <tag>   |   {}",
                no_disk_help()
            ),
            Self::NoHypervisor(missing) => missing.fix.clone(),
            Self::ImageNotFound { .. } => {
                "check the path, or leave out --image to download the published dev disk (tilo-vm doctor says whether one is published)".into()
            }
            Self::ImageFormat { flag, .. } if flag == "--boot-esp" => {
                "pass the raw ESP disk os/dev/make-boot-esp.sh wrote (its --output), on a local drive".into()
            }
            Self::ImageFormat { .. } => {
                "pass a qcow2 dev disk, or a raw disk with a GPT (os/dev/disk-image.sh --raw writes one), or leave out --image to download the published dev disk".into()
            }
            Self::VmRunning { vm } => format!(
                "stop it first: tilo-vm vm stop {vm}   |   or power it off and delete it: tilo-vm vm rm {vm} --force"
            ),
            Self::DiskExists { .. } => {
                "pass another path with --disk, or delete that file if no registered VM uses it (tilo-vm vm list)".into()
            }
            Self::NoSpace { .. } => {
                "free space there and rerun: what was already downloaded is reused, so a retry fetches only the rest   |   or keep the downloads on a filesystem with room: set XDG_CACHE_HOME (the cache lives in $XDG_CACHE_HOME/tilo-vm/disks, by default ~/.cache)".into()
            }
            Self::ForgeAuth { token: None, .. } => {
                "export FORGEJO_TOKEN=<forge token with package read access>, or log in with fgj or tea, then rerun. Disk downloads read only these".into()
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
            Self::UnknownAppSet { known, .. } => format!(
                "pick one of: {}   (tilo-vm account apps list shows what each installs)",
                known.join(", ")
            ),
            Self::PackagesFailed { .. } => {
                "the package manager's output above names the cause; the VM needs network access to the distribution's mirrors (tilo-vm vm ssh -- dnf repolist checks it), then rerun the install".into()
            }
            Self::SourceChanged { .. } => {
                "the worktree changed while the build ran: commit or stash the edit and rerun (nothing was applied)".into()
            }
            Self::RenderNodeMissing { nodes, .. } => {
                let available = if nodes.is_empty() {
                    "this machine exposes none".to_owned()
                } else {
                    nodes.join(", ")
                };
                format!(
                    "this machine renders on {available}; pass one with --render-node, or omit it and tilo-vm picks the first node whose driver is not NVIDIA"
                )
            }
            Self::InvalidInput(_) => {
                "e.g. tilo-vm input key ctrl+alt+t   |   input type 'hello'   |   input click left --at 400 300   |   input touch tap 400 300   |   input gamepad press a   (each device: tilo-vm input <key|touch|pen|gamepad> --help; text beyond a US layout: tilo-vm control type '<text>')".into()
            }
            Self::KernelBuild { log } => format!(
                "the build output above names the cause (full log: {log}); fix os/kernel/config-fragments/ or os/kernel/patches/ and rerun tilo-vm swap kernel   (os/kernel/dev-build.sh runs the same build by hand)"
            ),
            Self::KernelNotRunning { vm, .. } => format!(
                "the VM fell back to its default boot entry. Watch the new kernel boot on the console: tilo-vm vm serial {vm}   (tilo-vm vm shot {vm} for the screen); fix it and swap again, or go back: tilo-vm rollback --account vm"
            ),
            Self::ImageRefused { fix, .. } | Self::Branch { fix, .. } => fix.clone(),
            Self::BaseOlder { .. } => {
                "rebase onto a newer commit and build again: tilo-vm vm upgrade --from local   |   or a published image: tilo-vm vm upgrade   |   or, to go back on purpose: add --allow-older-base".into()
            }
            Self::Io { .. } => "check the path and permissions named above; tilo-vm doctor checks the setup".into(),
            Self::ScenarioMedium { .. } => {
                "build one: ./os/build.sh --local, then ./os/dev/build-custom-installer.sh (writes os/output/installer/qcow2/disk.qcow2)   |   or pass one: tilo-vm scenario install --medium <installer.qcow2>".into()
            }
            Self::ScenarioInput { .. } => {
                "build the desktop and laptop disks with ./os/build.sh --local   |   install the server image onto a disk and pass it with --server-disk   |   run --dry-run to see every step and what it needs".into()
            }
            Self::Scenario { phase, artifacts, console, .. } => {
                let inspect = if phase == "install" || phase == "boot-installer" {
                    format!("rerun with --keep and read the installer yourself while it runs: tilo-control --socket {console} state-dump --pretty")
                } else {
                    "rerun with --keep to keep the target disk for inspection".to_owned()
                };
                format!("read the evidence in {artifacts} (installer and greeter state, target serial log)   |   {inspect}")
            }
            Self::NeedsWorktree { what } => format!(
                "run {what} from a checkout of the Tilo repository, where os/dev/vm.sh lives"
            ),
            Self::Qemu { remedy, .. } => remedy.clone(),
        }
    }

    pub fn see(&self) -> Option<String> {
        let topic = match self {
            Self::NoVm { .. } => "quickstart",
            Self::UnknownVm { .. }
            | Self::TargetConflict { .. }
            | Self::Config { .. }
            | Self::ArchMismatch { .. }
            | Self::NoDiskPublished { .. }
            | Self::NoDiskForHost { .. }
            | Self::NoHypervisor(_)
            | Self::ImageNotFound { .. }
            | Self::ImageFormat { .. }
            | Self::VmRunning { .. }
            | Self::DiskExists { .. }
            | Self::ChecksumMismatch { .. }
            | Self::DownloadFailed { .. }
            | Self::Qemu { .. } => "platforms",
            Self::RenderNodeMissing { .. }
            | Self::DeviceAsleep { .. }
            | Self::WakeTimeout { .. }
            | Self::NotLoggedIn { .. }
            | Self::NotJoined { .. }
            | Self::NotGranted { .. } => "devices",
            Self::NotInWorktree | Self::AccountName { .. } | Self::Branch { .. } => "concepts",
            Self::Remote(error) => return error.see.clone(),
            Self::NoManifest { .. }
            | Self::UnknownComponent { .. }
            | Self::BootComponent { .. }
            | Self::ScopeMismatch { .. }
            | Self::RawWrite { .. }
            | Self::InvalidSource(_)
            | Self::HostBuildUnsupported { .. }
            | Self::VmCannotBuild { .. }
            | Self::UnknownApp { .. }
            | Self::MissingInImage { .. } => "swapping",
            Self::LowDisk { .. } | Self::NoSpace { .. } => "storage",
            Self::ImageRefused { .. } | Self::BaseOlder { .. } | Self::NotBootc { .. } => "images",
            Self::InvalidInput(_)
            | Self::SourceChanged { .. }
            | Self::UnknownAppSet { .. }
            | Self::PackagesFailed { .. } => "driving",
            Self::KernelBuild { .. } | Self::KernelNotRunning { .. } => "kernel",
            Self::ScenarioMedium { .. }
            | Self::ScenarioInput { .. }
            | Self::Scenario { .. }
            | Self::NeedsWorktree { .. } => "scenarios",
            Self::Unreachable { .. }
            | Self::DeviceTooOld { .. }
            | Self::ToolMissing { .. }
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

    pub fn render(&self) -> String {
        let mut text = format!("error: {self}\nhint: {}", self.hint());
        if let Some(see) = self.see() {
            text.push_str("\nsee: ");
            text.push_str(&see);
        }
        text
    }
}
