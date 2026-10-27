//! The one error type. Every variant says what went wrong, and `hint` is
//! always the exact next command; `see` names the guide topic to read.

use serde_json::json;
use tilo_vm_protocol::RpcError;

use crate::host::Missing;

/// The script that builds a dev disk locally, relative to the worktree.
pub const LOCAL_BUILD_SCRIPT: &str = "os/dev/build-dev-image.sh";

// Images are public on registry.tilo.so: pulls need no login. Only dev disk
// downloads, from the forge's package registry, need a credential.

/// What the forge token (`FORGEJO_TOKEN`, or an fgj or tea login) is for.
pub const FORGE_TOKEN_USE: &str =
    "read only for dev disk downloads (vm create without --image, doctor's published-disk check)";

/// How the docs build tilo-vm; what to run when it is older than the VM.
pub const REBUILD: &str =
    "cargo build -p tilo-vm, then run target/debug/tilo-vm   (any cargo drop-in works)";

/// What to do with a VM that has no bootc, so `vm upgrade` cannot move it.
pub const NOT_BOOTC_FIX: &str = "create a VM from a dev image instead: tilo-vm vm create <name>   (the published dev disk; or --image <dev disk.qcow2>)   |   or ask whoever set this VM up to move it onto a dev image";

/// Whether a VM has bootc could not be checked.
const UNCHECKED_BOOTC: &str = "whether it runs a bootc dev image could not be checked";

/// What to do when no dev disk is published: `local_build` is the path of
/// [`LOCAL_BUILD_SCRIPT`] when this checkout has it.
pub fn no_disk_help(local_build: Option<&str>) -> String {
    let local = match local_build {
        Some(script) => format!(
            "or build a disk locally: {script}, then tilo-vm vm create <name> --image <disk.qcow2>   |   or run tilo-vm image build on a build machine, bring back target/tilo-vm/image, write its disk here: tilo-vm image disk, then tilo-vm vm create <name> --image target/tilo-vm/disk.qcow2 (tilo-vm guide images)"
        ),
        None => format!(
            "no local dev-image build is available in this checkout ({LOCAL_BUILD_SCRIPT} is missing)"
        ),
    };
    format!(
        "t publishes main's dev disks, each architecture on its own: t dispatch publish-os-dev-disk-x86_64 --ref main (publish-os-dev-disk-aarch64 for ARM) if you may dispatch t, or ask a human who may   |   or ask a human for a shared VM and use it: tilo-vm vm use <name>   |   {local}"
    )
}

/// How a dial over the peer network failed, from `tilo-device-admin dial`'s
/// exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerFailure {
    /// No answer: offline, asleep, or no relay in common (2).
    Unreachable,
    /// The device refused this machine (3).
    Refused,
    /// Another device answered at the recorded endpoint (4).
    OtherDevice,
    /// This machine is not enrolled (5).
    NotEnrolled,
    /// Anything else.
    Failed,
}

impl PeerFailure {
    pub fn from_exit(code: Option<i32>) -> Self {
        match code {
            Some(2) => Self::Unreachable,
            Some(3) => Self::Refused,
            Some(4) => Self::OtherDevice,
            Some(5) => Self::NotEnrolled,
            _ => Self::Failed,
        }
    }
}

impl std::fmt::Display for PeerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unreachable => "it did not answer",
            Self::Refused => "it refused this machine",
            Self::OtherDevice => "another device answered for it",
            Self::NotEnrolled => "this machine cannot dial it",
            Self::Failed => "the dial failed",
        })
    }
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
    #[error("not inside a git worktree, so there is no default slot")]
    NotInWorktree,
    #[error("branch '{branch}' does not make a usable slot name")]
    SlotName { branch: String },
    #[error("device '{vm}' does not answer at {address} ({detail}){lookup}: it is asleep or gone")]
    DeviceAsleep {
        vm: String,
        address: String,
        detail: String,
        /// What the lookup by name found, e.g. `, and yoga.local does not resolve`.
        lookup: String,
        /// Whether a MAC is recorded, so `vm wake` can send its magic packet.
        wakeable: bool,
    },
    #[error("device '{vm}' is not on this network, and {failure} over the peer network: {detail}")]
    PeerRoute {
        vm: String,
        failure: PeerFailure,
        /// What `tilo-device-admin dial` said.
        detail: String,
    },
    #[error("this machine is not enrolled to administer devices: {detail}")]
    NotEnrolledAdmin { detail: String },
    #[error("sent {vm}'s magic packet ({sent}), but it did not answer within {seconds}s")]
    WakeTimeout {
        vm: String,
        sent: String,
        seconds: u64,
        /// What the device reported about its wake setting when last reached.
        wake: Option<String>,
    },
    #[error("cannot reach VM '{vm}' over SSH: {detail}")]
    Unreachable {
        vm: String,
        detail: String,
        debug: Vec<String>,
    },
    #[error("VM '{vm}' has no tilo-vmd; it is not running a Tilo dev image")]
    DaemonMissing {
        vm: String,
        /// Whether the VM has bootc, when it was checked.
        bootc: Option<bool>,
    },
    #[error("VM '{vm}' is not running a bootc dev image; {command} needs one")]
    NotBootc { vm: String, command: String },
    #[error("VM '{vm}' refuses its root SSH key {key}")]
    RootKeyRefused {
        vm: String,
        key: String,
        /// The one-time command that installs the key.
        install: String,
        /// Whether the adapter passed the key as a credential at creation.
        credentials: bool,
    },
    #[error("{user} on '{vm}' cannot run commands as root without a password (sudo -n: {detail})")]
    SudoRefused {
        vm: String,
        /// `<user>@<host>[:<port>]`, as `vm add-device --ssh` takes it.
        ssh: String,
        user: String,
        detail: String,
    },
    #[error("tilo-vm speaks protocol {client} but the VM's tilo-vmd speaks {daemon}")]
    ProtocolMismatch {
        /// The VM's name; empty when not known where the mismatch surfaced.
        vm: String,
        client: u32,
        daemon: u32,
        /// Whether the VM has bootc, when it was checked.
        bootc: Option<bool>,
    },
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
        /// Whether the VM has bootc (so vm upgrade can bring a toolchain).
        bootc: Option<bool>,
    },
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
    #[error("the install scenario failed in {phase}: {detail}")]
    Scenario {
        phase: String,
        detail: String,
        /// The run's artifact directory.
        artifacts: String,
        /// The VM's console socket (gone once the VM stopped).
        console: String,
    },
    #[error("{what} runs from a Tilo worktree (it drives os/dev/vm.sh)")]
    NeedsWorktree { what: String },
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
            Self::NotInWorktree => "name the slot explicitly, e.g. tilo-vm swap --slot my-slot shell".into(),
            Self::SlotName { .. } => {
                "name the slot explicitly (1-28 lowercase letters, digits, dashes), e.g. tilo-vm slot create my-slot"
                    .into()
            }
            Self::DeviceAsleep { vm, wakeable: true, .. } => format!(
                "wake it: tilo-vm vm wake {vm}   (its magic packet, sent from this machine on its LAN; vm status says whether that can wake it)   |   or at its power button, then tilo-vm vm status {vm}. Next time keep it from sleeping: tilo-vm vm awake {vm} --for 2h"
            ),
            Self::DeviceAsleep { vm, wakeable: false, .. } => format!(
                "wake it at its power button or keyboard, then tilo-vm vm status {vm}: that records its MAC and hostname, so a new address is found by name. Next time keep it from sleeping: tilo-vm vm awake {vm} --for 2h"
            ),
            Self::PeerRoute { vm, failure: PeerFailure::Unreachable, .. } => format!(
                "it is asleep, offline, or cannot reach a relay: wake it (at the machine, or tilo-vm vm wake {vm} from its LAN), check its network, then rerun. Next time keep it from sleeping: tilo-vm vm awake {vm} --for 2h"
            ),
            Self::PeerRoute { failure: PeerFailure::Refused, .. } => {
                "the device said why above; fix that, then rerun (enrolling this machine is tilo-vm vm enrol-admin)".into()
            }
            Self::PeerRoute { vm, failure: PeerFailure::OtherDevice, .. } => format!(
                "the recorded route is stale: on its LAN, tilo-vm vm status {vm} records it again"
            ),
            Self::PeerRoute { failure: PeerFailure::NotEnrolled, .. } | Self::NotEnrolledAdmin { .. } => {
                "enrol this machine as one of the owner's devices: tilo-vm vm enrol-admin --via <the device that holds the identity>   (tilo-vm guide devices, \"Beyond the LAN\")".into()
            }
            Self::PeerRoute { vm, failure: PeerFailure::Failed, .. } => format!(
                "rerun with the device on its LAN (tilo-vm vm status {vm}); tilo-device-admin dial --help shows the route by hand"
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
            Self::RootKeyRefused { install, credentials: true, .. } => format!(
                "the key goes in at creation as the systemd credential ssh.authorized_keys.root, and this VM did not take it (created before that, or from another disk). Install it once, with the dev account's password from os/account.env (quickuser, tilo): {install}"
            ),
            Self::RootKeyRefused { install, .. } => format!(
                "this hypervisor has no credential channel, so root's key is installed once after the first boot, with the dev account's password from os/account.env (quickuser, tilo): {install}"
            ),
            Self::SudoRefused { vm, ssh, user, .. } => format!(
                "tilo-vm runs tilo-vmd and its own commands as root, through sudo when it logs in as {user}; allow that once, typing {user}'s password on this terminal: tilo-vm vm add {vm} --ssh {ssh} --grant-sudo   (or log in as root: --ssh root@<host>)"
            ),
            Self::DaemonMissing { vm, bootc: Some(true) } => format!(
                "move the VM onto a dev image: tilo-vm vm upgrade {vm} --image registry.tilo.so/tilo/tilo-vm:<tag>   |   or one built here: tilo-vm vm upgrade {vm} --from local"
            ),
            Self::DaemonMissing { bootc: Some(false), .. } | Self::NotBootc { .. } => {
                NOT_BOOTC_FIX.into()
            }
            Self::DaemonMissing { vm, bootc: None } => format!(
                "{UNCHECKED_BOOTC}: tilo-vm vm status {vm}   (it says whether vm upgrade can move this VM onto a dev image)"
            ),
            Self::ProtocolMismatch { vm, client, daemon, bootc } if client > daemon => {
                let name = if vm.is_empty() { String::new() } else { format!(" {vm}") };
                match bootc {
                    Some(true) => format!(
                        "the VM is older than this tilo-vm: move it onto a current dev image with tilo-vm vm upgrade{name}"
                    ),
                    Some(false) => format!(
                        "the VM is older than this tilo-vm, and it is not running a bootc dev image, so vm upgrade cannot move it: {NOT_BOOTC_FIX}"
                    ),
                    None => format!(
                        "the VM is older than this tilo-vm; {UNCHECKED_BOOTC}: tilo-vm vm status{name}   (it says whether vm upgrade can move this VM)"
                    ),
                }
            }
            Self::ProtocolMismatch { .. } => format!(
                "this tilo-vm is older than the VM's tilo-vmd: update this checkout and rebuild it: {REBUILD}"
            ),
            Self::Remote(error) => error.hint.clone(),
            Self::ToolMissing { tool, .. } => format!("install {tool} and make sure it is on PATH; tilo-vm doctor re-checks"),
            // A command that explained itself needs no advice beyond reading it.
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
                format!("VM-wide components go into the vm slot: tilo-vm swap --slot vm {component}")
            }
            Self::ScopeMismatch { component, .. } => {
                format!("session components go into your own slot: tilo-vm swap {component}")
            }
            Self::ArchMismatch { .. } => {
                "use a VM of the host's architecture, or opt into slow emulation with --emulated".into()
            }
            Self::LowDisk { .. } => "tilo-vm gc --dry-run   then   tilo-vm gc".into(),
            Self::RawWrite { .. } => {
                "change the VM through tilo-vm instead, so it can be rolled back and other agents see it: a component: tilo-vm swap <component>   |   the image (/etc, units, packages): tilo-vm vm upgrade --from local   |   to run it by hand anyway, --raw is the escape hatch: tilo-vm vm ssh --raw -- <command>"
                    .into()
            }
            Self::InvalidSource(_) => {
                "use --from worktree, layer:<hash>, sha:<commit> or ci:<tag>, e.g. tilo-vm swap shell --from ci:main-1a2b3c4".into()
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
            Self::NoDiskPublished { tag, local_build, .. } if tag != "(any)" => format!(
                "pick a published tag (tilo-vm vm create <name> without --tag takes the newest)   |   {}",
                no_disk_help(local_build.as_deref())
            ),
            Self::NoDiskPublished { local_build, .. } => no_disk_help(local_build.as_deref()),
            Self::NoDiskForHost {
                arch, local_build, ..
            } => format!(
                "each architecture publishes on its own, so {arch}'s lane may still be running or may have failed: fgj run list -R tilo/tilo   |   pick a tag that has one: tilo-vm vm create <name> --tag <tag>   |   {}",
                no_disk_help(local_build.as_deref())
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
                "free space there and rerun: what was already downloaded is reused, so a retry fetches only the rest (in a VM, tilo-vm gc --dry-run shows what it can delete)".into()
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
                "pick one of: {}   (tilo-vm slot apps list shows what each installs)",
                known.join(", ")
            ),
            Self::PackagesFailed { .. } => {
                "the package manager's output above names the cause; the VM needs network access to the distribution's mirrors (tilo-vm vm ssh -- dnf repolist checks it), then rerun the install".into()
            }
            Self::InvalidInput(_) => {
                "e.g. tilo-vm input key ctrl+alt+t   |   input type 'hello'   |   input click left --at 400 300   |   input touch tap 400 300   |   input gamepad press a   (each device: tilo-vm input <key|touch|pen|gamepad> --help; text beyond a US layout: tilo-vm control type '<text>')".into()
            }
            Self::KernelBuild { log } => format!(
                "the build output above names the cause (full log: {log}); fix os/kernel/config-fragments/ or os/kernel/patches/ and rerun tilo-vm swap kernel   (os/kernel/dev-build.sh runs the same build by hand)"
            ),
            Self::KernelNotRunning { vm, .. } => format!(
                "the VM fell back to its default boot entry. Watch the new kernel boot on the console: tilo-vm vm serial {vm}   (tilo-vm vm shot {vm} for the screen); fix it and swap again, or go back: tilo-vm rollback --slot vm"
            ),
            Self::ImageRefused { fix, .. } => fix.clone(),
            Self::BaseOlder { .. } => {
                "rebuild the base from this worktree: tilo-vm vm upgrade --from local --rebuild-base   |   or rebase onto a newer commit   |   or, to go back on purpose: add --allow-older-base".into()
            }
            Self::Io { .. } => "check the path and permissions named above; tilo-vm doctor checks the setup".into(),
            Self::ScenarioMedium { .. } => {
                "build one: ./os/build.sh --local, then ./os/dev/build-custom-installer.sh (writes os/output/installer/qcow2/disk.qcow2)   |   or pass one: tilo-vm scenario install --medium <installer.qcow2>".into()
            }
            Self::Scenario { phase, artifacts, console, .. } => {
                let inspect = if phase == "install" || phase == "boot-installer" {
                    format!("rerun with --keep and read the installer yourself while it runs: tilo-control --socket {console} state-dump --pretty   (actions list shows every step and why it is blocked)")
                } else {
                    "rerun with --keep to keep the target disk for inspection".to_owned()
                };
                format!("read the evidence in {artifacts} (installer and greeter state, target serial log)   |   {inspect}")
            }
            Self::NeedsWorktree { what } => format!(
                "run {what} from a checkout of the Tilo repository, where os/dev/vm.sh lives"
            ),
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
            | Self::RootKeyRefused { .. } => "platforms",
            Self::SudoRefused { .. }
            | Self::DeviceAsleep { .. }
            | Self::WakeTimeout { .. }
            | Self::PeerRoute { .. }
            | Self::NotEnrolledAdmin { .. } => "devices",
            Self::NotInWorktree | Self::SlotName { .. } => "concepts",
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
            Self::InvalidInput(_) | Self::UnknownAppSet { .. } | Self::PackagesFailed { .. } => {
                "driving"
            }
            Self::KernelBuild { .. } | Self::KernelNotRunning { .. } => "kernel",
            Self::ScenarioMedium { .. } | Self::Scenario { .. } | Self::NeedsWorktree { .. } => {
                "scenarios"
            }
            Self::Unreachable { .. }
            | Self::DaemonMissing { .. }
            | Self::ProtocolMismatch { .. }
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
            ClientError::VersionMismatch { client, daemon } => Self::ProtocolMismatch {
                vm: String::new(),
                client,
                daemon,
                bootc: None,
            },
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
        let all =
            vec![
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
            CliError::TargetConflict { target: "review".into(), name: "other".into() },
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
            CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: Some(true),
            },
            CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: Some(false),
            },
            CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: None,
            },
            CliError::NotBootc {
                vm: "dev".into(),
                command: "vm upgrade".into(),
            },
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
            CliError::SudoRefused {
                vm: "yoga".into(),
                ssh: "me@10.0.0.2".into(),
                user: "me".into(),
                detail: "a password is required".into(),
            },
            CliError::ProtocolMismatch {
                vm: "dev".into(),
                client: 2,
                daemon: 1,
                bootc: Some(true),
            },
            CliError::ProtocolMismatch {
                vm: "dev".into(),
                client: 2,
                daemon: 1,
                bootc: Some(false),
            },
            CliError::ProtocolMismatch {
                vm: String::new(),
                client: 2,
                daemon: 1,
                bootc: None,
            },
            CliError::ProtocolMismatch {
                vm: "dev".into(),
                client: 1,
                daemon: 2,
                bootc: Some(true),
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
            CliError::KernelBuild {
                log: "/home/u/.cache/tilo-kernel/linux-7.2.5/build.log".into(),
            },
            CliError::KernelNotRunning {
                vm: "dev".into(),
                expected: "7.2.5+dev".into(),
                running: "7.2.5".into(),
                detail: "no SSH answer within 240s; the VM was reset".into(),
            },
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
            CliError::RawWrite {
                command: "cp shell /usr/bin/shell".into(),
                change: "writes /usr/bin/shell".into(),
            },
            CliError::InvalidSource("x".into()),
            CliError::HostBuildUnsupported { os: "macos".into() },
            CliError::VmCannotBuild {
                missing: vec!["cargo".into()],
                bootc: Some(true),
            },
            CliError::VmCannotBuild {
                missing: vec!["cargo".into()],
                bootc: Some(false),
            },
            CliError::CommandFailed {
                command: "c".into(),
                status: "1".into(),
                stderr: "ls: cannot access '/x': No such file or directory".into(),
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
                fix:
                    "sudo dnf install libvirt-daemon-kvm libvirt-client virt-manager qemu-img passt"
                        .into(),
            }),
            CliError::ImageNotFound { path: "x".into() },
            CliError::ImageFormat {
                flag: "--image".into(),
                path: "x".into(),
                detail: "is 0 bytes".into(),
            },
            CliError::ImageFormat {
                flag: "--boot-esp".into(),
                path: "x".into(),
                detail: "is 0 bytes".into(),
            },
            CliError::VmRunning { vm: "try1".into() },
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
            CliError::DeviceAsleep {
                vm: "yoga".into(),
                address: "10.0.0.223".into(),
                detail: "No route to host".into(),
                lookup: ", and yoga.local does not find it elsewhere".into(),
                wakeable: true,
            },
            CliError::DeviceAsleep {
                vm: "opi".into(),
                address: "10.0.0.108".into(),
                detail: "timed out".into(),
                lookup: ", and no hostname is recorded to look it up by".into(),
                wakeable: false,
            },
            CliError::NotEnrolledAdmin {
                detail: "no device key".into(),
            },
            CliError::PeerRoute {
                vm: "yoga".into(),
                failure: PeerFailure::Unreachable,
                detail: "no answer within 15s".into(),
            },
            CliError::PeerRoute {
                vm: "yoga".into(),
                failure: PeerFailure::Refused,
                detail: "NotAdministrator: ...".into(),
            },
            CliError::PeerRoute {
                vm: "yoga".into(),
                failure: PeerFailure::OtherDevice,
                detail: "device ab answered instead".into(),
            },
            CliError::PeerRoute {
                vm: "yoga".into(),
                failure: PeerFailure::NotEnrolled,
                detail: "no certificate".into(),
            },
            CliError::PeerRoute {
                vm: "yoga".into(),
                failure: PeerFailure::Failed,
                detail: "?".into(),
            },
            CliError::WakeTimeout {
                vm: "yoga".into(),
                sent: "for 02:11:22:33:44:55 to 255.255.255.255:9".into(),
                seconds: 150,
                wake: Some("Wi-Fi WoWLAN on wlan0 is supported but disabled".into()),
            },
            CliError::WakeTimeout {
                vm: "opi".into(),
                sent: "for aa:bb:cc:00:11:22 to 255.255.255.255:9".into(),
                seconds: 150,
                wake: None,
            },
            CliError::InvalidInput("bad".into()),
            CliError::UnknownAppSet {
                set: "x".into(),
                known: vec!["verification".into()],
            },
            CliError::PackagesFailed {
                set: "verification".into(),
                vm: "dev".into(),
            },
            CliError::ImageRefused {
                image: "localhost/tilo:latest".into(),
                problem: "is not a dev image".into(),
                fix: "tilo-vm vm upgrade --from local".into(),
            },
            CliError::BaseOlder {
                image: "localhost/tilo-vm:latest".into(),
                base: "localhost/tilo:latest".into(),
                new: "2026-09-01T00:00:00+00:00".into(),
                running: "2026-09-20T00:00:00+00:00".into(),
            },
            CliError::Io {
                context: "c".into(),
                source: std::io::Error::other("e"),
            },
            CliError::ScenarioMedium {
                path: "/w/os/output/installer/qcow2/disk.qcow2".into(),
            },
            CliError::Scenario {
                phase: "install".into(),
                detail: "begin@installer.review was refused".into(),
                artifacts: "/w/target/tilo-vm-scenarios/x".into(),
                console: "/run/user/1000/tilo-vm-x/console.sock".into(),
            },
            CliError::Scenario {
                phase: "first-boot".into(),
                detail: "no greeter answered".into(),
                artifacts: "/w/target/tilo-vm-scenarios/x".into(),
                console: "/run/user/1000/tilo-vm-x/console.sock".into(),
            },
            CliError::NeedsWorktree {
                what: "tilo-vm scenario install".into(),
            },
        ];
        for error in &all {
            match error {
                CliError::ScenarioMedium { .. }
                | CliError::Scenario { .. }
                | CliError::NeedsWorktree { .. }
                | CliError::NoVm { .. }
                | CliError::KernelBuild { .. }
                | CliError::KernelNotRunning { .. }
                | CliError::HostBuildUnsupported { .. }
                | CliError::VmCannotBuild { .. }
                | CliError::NoDiskPublished { .. }
                | CliError::NoDiskForHost { .. }
                | CliError::NoHypervisor(_)
                | CliError::ImageNotFound { .. }
                | CliError::ImageFormat { .. }
                | CliError::VmRunning { .. }
                | CliError::DiskExists { .. }
                | CliError::NoSpace { .. }
                | CliError::ForgeAuth { .. }
                | CliError::ChecksumMismatch { .. }
                | CliError::DownloadFailed { .. }
                | CliError::UnknownApp { .. }
                | CliError::MissingInImage { .. }
                | CliError::Busy { .. }
                | CliError::InvalidInput(_)
                | CliError::UnknownAppSet { .. }
                | CliError::PackagesFailed { .. }
                | CliError::ImageRefused { .. }
                | CliError::BaseOlder { .. }
                | CliError::UnknownVm { .. }
                | CliError::TargetConflict { .. }
                | CliError::Config { .. }
                | CliError::NotInWorktree
                | CliError::SlotName { .. }
                | CliError::Unreachable { .. }
                | CliError::DeviceAsleep { .. }
                | CliError::PeerRoute { .. }
                | CliError::NotEnrolledAdmin { .. }
                | CliError::WakeTimeout { .. }
                | CliError::DaemonMissing { .. }
                | CliError::NotBootc { .. }
                | CliError::RootKeyRefused { .. }
                | CliError::SudoRefused { .. }
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
                | CliError::RawWrite { .. }
                | CliError::InvalidSource(_)
                | CliError::Io { .. } => {}
            }
        }
        all
    }

    /// Every error variant has a hint; first-contact hints cover both ways to a VM; no published disk
    /// says plainly what to do; credential hints name the one login each operation reads; a missing
    /// hypervisor names the adapter and fix; protocol mismatch hints pick the older side; a VM without bootc
    /// is named plainly with the fix; a VM that cannot build gets vm upgrade only with bootc; remote errors
    /// keep the daemon hint.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn every_error_carries_a_hint_that_names_the_fix() {
        // Phase: every_error_variant_has_a_hint.
        {
            for error in samples() {
                assert!(!error.hint().trim().is_empty(), "{error:?} has no hint");
                let json = error.to_json();
                assert!(json["error"].as_str().is_some_and(|e| !e.is_empty()));
                assert!(json["hint"].as_str().is_some_and(|h| !h.is_empty()));
                assert!(error.render().contains("\nhint: "));
                if !matches!(error, CliError::Remote(_)) {
                    let see = error.see().unwrap_or_default();
                    let topic = see.strip_prefix("tilo-vm guide ");
                    assert!(
                        topic.is_some_and(|t| crate::guide::topic(t).is_some()),
                        "{error:?} has no see: pointer to an existing guide topic ({see})"
                    );
                }
            }
        }

        // Phase: first_contact_hints_cover_both_ways_to_a_vm.
        {
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

        // Phase: no_published_disk_says_plainly_what_to_do.
        {
            let without = CliError::NoDiskPublished {
                tag: "(any)".into(),
                url: "u".into(),
                local_build: None,
            }
            .hint();
            assert!(
                without.contains("t dispatch publish-os-dev-disk-x86_64 --ref main"),
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

        // Phase: disk_downloads_name_the_forge_token.
        {
            let forge = CliError::ForgeAuth {
                url: "u".into(),
                status: 401,
                token: None,
            }
            .hint();
            assert!(forge.contains("FORGEJO_TOKEN"), "{forge}");
            assert!(FORGE_TOKEN_USE.contains("dev disk downloads"));
        }

        // Phase: a_missing_hypervisor_names_the_adapter_and_the_fix.
        {
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

        // Phase: protocol_mismatch_hints_pick_the_side_that_is_older.
        {
            let mismatch = |client, daemon, bootc| CliError::ProtocolMismatch {
                vm: "dev".into(),
                client,
                daemon,
                bootc,
            };
            let upgrade = mismatch(2, 1, Some(true)).hint();
            assert!(upgrade.contains("tilo-vm vm upgrade dev"), "{upgrade}");
            let no_bootc = mismatch(2, 1, Some(false)).hint();
            assert!(!no_bootc.contains("vm upgrade dev"), "{no_bootc}");
            assert!(
                no_bootc.contains("not running a bootc dev image"),
                "{no_bootc}"
            );
            assert!(no_bootc.contains("tilo-vm vm create <name>"), "{no_bootc}");
            assert!(
                no_bootc.contains("ask whoever set this VM up"),
                "{no_bootc}"
            );
            let unknown = mismatch(2, 1, None).hint();
            assert!(!unknown.contains("tilo-vm vm upgrade"), "{unknown}");
            assert!(unknown.contains("tilo-vm vm status dev"), "{unknown}");
            for bootc in [Some(true), Some(false), None] {
                let rebuild = mismatch(1, 2, bootc).hint();
                assert!(rebuild.contains("cargo build -p tilo-vm"), "{rebuild}");
                assert!(!rebuild.contains("vm upgrade"), "{rebuild}");
            }
        }

        // Phase: a_vm_without_bootc_is_named_plainly_with_the_fix.
        {
            let error = CliError::NotBootc {
                vm: "dev".into(),
                command: "vm upgrade".into(),
            };
            assert_eq!(
                error.to_string(),
                "VM 'dev' is not running a bootc dev image; vm upgrade needs one"
            );
            assert!(error.hint().contains("tilo-vm vm create <name>"));
            assert!(error.hint().contains("ask whoever set this VM up"));
            assert!(!error.hint().contains("rerun it by hand"));
            let missing = CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: Some(false),
            }
            .hint();
            assert!(!missing.contains("vm upgrade"), "{missing}");
            let upgradable = CliError::DaemonMissing {
                vm: "dev".into(),
                bootc: Some(true),
            }
            .hint();
            assert!(
                upgradable.contains("tilo-vm vm upgrade dev"),
                "{upgradable}"
            );
        }

        // Phase: a_vm_that_cannot_build_gets_vm_upgrade_only_with_bootc.
        {
            let hint = |bootc| {
                CliError::VmCannotBuild {
                    missing: vec!["cargo".into()],
                    bootc,
                }
                .hint()
            };
            assert!(hint(Some(true)).contains("tilo-vm vm upgrade"));
            for bootc in [Some(false), None] {
                assert!(!hint(bootc).contains("vm upgrade"), "{}", hint(bootc));
                assert!(hint(bootc).contains("tilo-vm vm create <name>"));
            }
        }

        // Phase: remote_errors_keep_the_daemon_hint.
        {
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

    #[test]
    fn only_a_silent_failure_is_worth_rerunning_by_hand() {
        let failed = |stderr: &str| CliError::CommandFailed {
            command: "rsync a b".into(),
            status: "exit 1".into(),
            stderr: stderr.into(),
        };
        let explained = failed("rsync: No such file or directory").hint();
        assert!(!explained.contains("rerun"), "{explained}");
        let silent = failed("").hint();
        assert!(silent.contains("rerun it by hand"), "{silent}");
        assert!(silent.contains("rsync a b"), "{silent}");
    }
}
