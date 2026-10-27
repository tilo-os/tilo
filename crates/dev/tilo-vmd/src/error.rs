//! Every daemon failure carries the next command to run.

use tilo_vm_protocol::{RpcError, code};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("tilo-vm speaks protocol {client} but tilo-vmd speaks {daemon}")]
    VersionMismatch { client: u32, daemon: u32 },
    #[error("the connection has not completed the hello handshake")]
    HelloRequired,
    #[error("invalid request: {0}")]
    InvalidParams(String),
    #[error("no slot named '{0}'")]
    SlotNotFound(String),
    #[error("slot '{name}' already exists as {kind}")]
    SlotKindConflict { name: String, kind: String },
    #[error("slot '{name}' already exists with {outputs}")]
    SlotOutputsConflict { name: String, outputs: String },
    #[error("slot '{0}' is not headful; only headful slots have outputs")]
    OutputsNeedHeadful(String),
    #[error("--outputs: {0}")]
    BadOutputs(String),
    #[error("slot '{slot}' outputs could not be set: {detail}")]
    DisplayLayout { slot: String, detail: String },
    #[error("slot '{slot}' could not get a throwaway test identity: {detail}")]
    TestIdentity { slot: String, detail: String },
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
    #[error("{0} is busy with another tilo-vm command")]
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
    #[error("this machine runs no session of its own, so it has no device slot")]
    NoDeviceSession,
    #[error("the {0} slot is the owner's login, and nobody is logged in")]
    LoggedOut(String),
    #[error("the device slot is the machine's own session; it cannot be {0}")]
    DeviceSlot(String),
    #[error("slot '{0}' has no running session to run the command in")]
    SessionStopped(String),
    #[error("{step} failed: {detail}")]
    Apply { step: String, detail: String },
    #[error("slot '{slot}' session differs from a greetd session: {detail}")]
    SessionParity { slot: String, detail: String },
    #[error("slot '{slot}' session did not start (unit is {state}); its journal ends with:\n{log}")]
    SessionFailed {
        slot: String,
        state: String,
        log: String,
    },
    #[error(
        "the desktop of the {slot} slot's login did not come back ({detail}); its journal ends with:\n{log}"
    )]
    DesktopFailed {
        slot: String,
        detail: String,
        log: String,
    },
    #[error("no component '{name}' in the synced os/components.toml")]
    UnknownComponent { name: String, known: Vec<String> },
    #[error("no app 'apps/{dir}' (with a tilo-app.toml) in the synced sources")]
    UnknownApp { dir: String, known: Vec<String> },
    #[error("no synced sources for worktree '{0}' in the VM")]
    SourceMissing(String),
    #[error("{step} failed: {detail}")]
    Job { step: String, detail: String },
    #[error("the VM's image has no {tool} (needed to {purpose})")]
    ToolMissing { tool: String, purpose: String },
    #[error("the registry refused to serve {image}")]
    RegistryAuth { image: String },
    #[error("{image} does not exist in the registry")]
    ImageNotFound { image: String },
    #[error("{image} has no {path}")]
    MissingInImage { image: String, path: String },
    #[error("slot '{0}' is headless: it has no seat, so no input devices")]
    HeadlessInput(String),
    #[error("not a kernel layer: {0}")]
    KernelLayer(String),
    #[error("cannot set the VM's boot entry: {detail}")]
    BootLoader { detail: String },
    #[error("dracut could not build the initramfs for {kver}: {detail}")]
    Initramfs { kver: String, detail: String },
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
            | Self::ControlMissing(_)
            | Self::UnknownComponent { .. }
            | Self::UnknownApp { .. }
            | Self::SourceMissing(_)
            | Self::ImageNotFound { .. }
            | Self::MissingInImage { .. }
            | Self::NoDeviceSession => code::NOT_FOUND,
            Self::SlotKindConflict { .. }
            | Self::SlotOutputsConflict { .. }
            | Self::SeatBusy { .. }
            | Self::Busy(_) => code::CONFLICT,
            Self::AbiMismatch { .. } => code::ABI,
            Self::ReservedName(_)
            | Self::InvalidName(_)
            | Self::InvalidLayer(_)
            | Self::NoEarlierGeneration(_)
            | Self::NoSession(_)
            | Self::DeviceSlot(_)
            | Self::SessionStopped(_)
            | Self::LoggedOut(_)
            | Self::HeadlessInput(_)
            | Self::OutputsNeedHeadful(_)
            | Self::BadOutputs(_)
            | Self::KernelLayer(_) => code::INVALID,
            Self::ScopeMismatch { .. } => code::SCOPE,
            Self::Apply { .. }
            | Self::DisplayLayout { .. }
            | Self::TestIdentity { .. }
            | Self::SessionParity { .. }
            | Self::SessionFailed { .. }
            | Self::DesktopFailed { .. }
            | Self::BootLoader { .. }
            | Self::Initramfs { .. } => code::APPLY,
            Self::Io { .. } => code::IO,
            Self::Job { .. } => code::JOB,
            Self::ToolMissing { .. } => code::TOOL,
            Self::RegistryAuth { .. } => code::AUTH,
        }
    }

    #[expect(clippy::too_many_lines, reason = "one arm per error variant")]
    pub fn hint(&self) -> String {
        match self {
            Self::VersionMismatch { client, daemon } if client > daemon => {
                "the VM's image is older than this tilo-vm: tilo-vm vm upgrade".into()
            }
            Self::VersionMismatch { .. } => {
                "this tilo-vm is older than the VM's image: rebuild it with cargo build -p tilo-vm   (any cargo drop-in works)"
                    .into()
            }
            Self::HelloRequired => "connect through tilo-vm rather than speaking to tilo-vmd by hand".into(),
            Self::InvalidParams(_) => {
                "tilo-vm and tilo-vmd disagree on the request shape: tilo-vm doctor".into()
            }
            Self::SlotNotFound(name) => {
                format!("tilo-vm slot create {name} --headless   (list existing: tilo-vm slot list)")
            }
            Self::SlotKindConflict { name, .. } => {
                format!("remove it first (tilo-vm slot rm {name}) or create it with its existing kind")
            }
            Self::SlotOutputsConflict { name, .. } => format!(
                "outputs are fixed when a slot is created: tilo-vm slot rm {name}, then tilo-vm slot create {name} --headful --outputs <list>   (or create another slot)"
            ),
            Self::OutputsNeedHeadful(name) => {
                format!("tilo-vm slot create {name} --headful --outputs 2560x1600@2,1920x1080")
            }
            Self::BadOutputs(_) => format!(
                "give 1 to {} outputs as WIDTHxHEIGHT[@SCALE], comma-separated: --outputs 2560x1600@2,1920x1080",
                tilo_vm_protocol::display::MAX_OUTPUTS
            ),
            Self::DisplayLayout { slot, .. } => format!(
                "rerun tilo-vm slot create {slot} --headful --outputs <same list> to apply it again; tilo-vm logs {slot} shows what the compositor did"
            ),
            Self::TestIdentity { slot, .. } => format!(
                "the slot's shell must be a dev build (a dev image, or a dev-profile swap) and its user must not hold a real identity; tilo-vm logs {slot} shows what the compositor did"
            ),
            Self::ReservedName(_) => {
                "pick another slot name; VM-wide components go in with: tilo-vm swap --slot vm <component>"
                    .into()
            }
            Self::InvalidName(_) => {
                "use 1-28 lowercase letters, digits or dashes, starting with a letter".into()
            }
            Self::LayerNotFound(_) => "tilo-vm swap <component> --from worktree   (builds and uploads a layer)".into(),
            Self::UploadNotFound(_) => "retry the swap; uploads do not survive a daemon restart".into(),
            Self::InvalidLayer(_) => {
                "a layer holds only usr/ (and opt/) with regular files, directories and symlinks".into()
            }
            Self::GenerationNotFound { slot, .. } => {
                format!("tilo-vm slot list   (shows the generations of '{slot}')")
            }
            Self::NoEarlierGeneration(slot) => {
                format!("nothing to roll back; tilo-vm swap --slot {slot} <component> installs a build")
            }
            Self::ScopeMismatch { component, scope, .. } if scope == "vm" => {
                format!("VM-wide components go into the vm pseudo-slot: tilo-vm swap --slot vm {component}")
            }
            Self::ScopeMismatch { component, .. } => {
                format!("session components go into your own slot: tilo-vm swap {component}")
            }
            Self::SeatBusy { slot, .. } => format!(
                "ask its owner to run: tilo-vm seat release {slot}   (or take it anyway: tilo-vm seat claim --steal)"
            ),
            Self::Busy(_) => "wait for the other command to finish, then retry; tilo-vm status shows what is running".into(),
            Self::AbiMismatch { field, .. } if field == "target" => {
                "build for the VM's target: tilo-vm swap <component> --builder vm   (the VM's own toolchain), or on a host of that architecture and triple"
                    .into()
            }
            Self::AbiMismatch { field, .. } if field == "Cargo.lock" => {
                "the layer was built from another Cargo.lock than the VM's image, so shared crates would differ: move the VM to an image of your commit (tilo-vm vm upgrade), or rebase your worktree onto the VM image's commit; never regenerate Cargo.lock to match"
                    .into()
            }
            Self::AbiMismatch { .. } => {
                "rebuild against this VM's base (tilo-vm vm upgrade first if the VM is older than your worktree)".into()
            }
            Self::NoDeviceSession => "a board dev image's own session, or on a laptop its owner's Tilo login through greetd on the built-in display, is the device slot; with neither, use your own slot: tilo-vm swap <component>   (a laptop: the owner logs in at the machine first)".into(),
            Self::LoggedOut(slot) => format!(
                "the owner logs in at the machine's login screen first; swaps and rollbacks (tilo-vm rollback --slot {slot}) work meanwhile and reach the next login"
            ),
            Self::DeviceSlot(_) => "swap into it (tilo-vm swap --slot device <component>) and undo every swap with tilo-vm rollback --slot device --to 0; its session comes and goes with the machine".into(),
            Self::NoSession(_) => "use a headful or headless slot: tilo-vm slot list   (the vm slot's layers are the VM's own /usr: tilo-vm vm ssh -- <command>)".into(),
            Self::SessionStopped(slot) => format!(
                "start it again: tilo-vm slot restart --slot {slot}   (tilo-vm logs --slot {slot} says why it stopped)"
            ),
            Self::ControlMissing(_) => {
                "run a dev image, which adds tilo-control: tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:<tag>"
                    .into()
            }
            Self::Apply { .. } => "tilo-vm logs   (the unit journal usually names the cause)".into(),
            Self::SessionParity { .. } => {
                "slot session capabilities or SELinux domain differ from a greetd session; see tilo-vm guide troubleshooting"
                    .into()
            }
            Self::SessionFailed { slot, .. } => format!(
                "the journal above names the cause; after fixing it: tilo-vm slot restart {slot}   (full log: tilo-vm logs {slot})"
            ),
            Self::DesktopFailed { slot, .. } => format!(
                "bring the previous build back: tilo-vm rollback --slot {slot}   (it works from the login screen too; full log: tilo-vm logs {slot})"
            ),
            Self::UnknownComponent { known, .. } => format!("pick one of: {}", known.join(", ")),
            Self::UnknownApp { known, .. } if known.is_empty() => {
                "apps live in apps/<dir>/tilo-app.toml; check the name, then rerun the swap".into()
            }
            Self::UnknownApp { known, .. } => {
                format!("tilo-vm swap app:<dir> with one of: {}", known.join(", "))
            }
            Self::SourceMissing(_) => {
                "rerun the swap: tilo-vm swap <component> --builder vm syncs the worktree first".into()
            }
            Self::Job { .. } => {
                "the build output above names the cause; fix it and rerun the swap".into()
            }
            Self::ToolMissing { tool, .. } if tool == "podman" => {
                "extract on the host instead (needs podman there), or move the VM to a newer dev image: tilo-vm vm upgrade".into()
            }
            Self::ToolMissing { .. } => {
                "build on a Linux host: tilo-vm swap <component> --builder host, or move the VM to a dev image that has it: tilo-vm vm upgrade".into()
            }
            Self::RegistryAuth { .. } => {
                "Tilo images on registry.tilo.so are public and need no login, so the registry refusing this pull means the reference is not a published Tilo image: check its name (registry.tilo.so/tilo/<image>:<tag>)".into()
            }
            Self::ImageNotFound { .. } => {
                "check the tag and that it built this architecture: CI publishes its dev image registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci for main commits (skopeo inspect --raw docker://registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci lists the platforms)".into()
            }
            Self::MissingInImage { .. } => {
                "that image does not ship this component (an optional one, or an older image); pick another tag or build it: --from worktree".into()
            }
            Self::HeadlessInput(slot) => format!(
                "drive headless slots through the compositor: tilo-vm control --slot {slot} key|type|click ...   (or create a headful slot: tilo-vm slot create <name> --headful)"
            ),
            Self::KernelLayer(_) => {
                "build and swap it with tilo-vm swap kernel: os/kernel/dev-build.sh stages usr/lib/modules/<release>/ with vmlinuz and the signed modules".into()
            }
            Self::BootLoader { .. } => {
                "kernel swaps need systemd-boot (the Tilo dev image) or GRUB with BLS entries in /boot/loader/entries; check with tilo-vm vm ssh -- bootctl status, then rerun the swap".into()
            }
            Self::Initramfs { kver, .. } => format!(
                "reproduce it in the VM: tilo-vm vm ssh -- dracut --force --kver {kver} /var/tmp/initramfs.img   (the vm slot already carries the modules; tilo-vm rollback --slot vm drops them)"
            ),
            Self::Io { .. } => "tilo-vm doctor   (checks the VM's disk and daemon state)".into(),
        }
    }

    pub fn see(&self) -> Option<&'static str> {
        match self {
            Self::SlotNotFound(_)
            | Self::SlotKindConflict { .. }
            | Self::SlotOutputsConflict { .. }
            | Self::OutputsNeedHeadful(_)
            | Self::BadOutputs(_)
            | Self::ReservedName(_)
            | Self::InvalidName(_)
            | Self::NoSession(_)
            | Self::SessionStopped(_) => Some("tilo-vm guide concepts"),
            Self::LayerNotFound(_)
            | Self::UploadNotFound(_)
            | Self::InvalidLayer(_)
            | Self::GenerationNotFound { .. }
            | Self::NoEarlierGeneration(_)
            | Self::ScopeMismatch { .. }
            | Self::AbiMismatch { .. }
            | Self::UnknownComponent { .. }
            | Self::UnknownApp { .. }
            | Self::SourceMissing(_)
            | Self::Job { .. }
            | Self::ToolMissing { .. }
            | Self::ImageNotFound { .. }
            | Self::MissingInImage { .. } => Some("tilo-vm guide swapping"),
            Self::SeatBusy { .. }
            | Self::ControlMissing(_)
            | Self::HeadlessInput(_)
            | Self::DisplayLayout { .. }
            | Self::TestIdentity { .. } => Some("tilo-vm guide driving"),
            Self::KernelLayer(_) | Self::BootLoader { .. } | Self::Initramfs { .. } => {
                Some("tilo-vm guide kernel")
            }
            Self::NoDeviceSession | Self::DeviceSlot(_) | Self::LoggedOut(_) => {
                Some("tilo-vm guide devices")
            }
            Self::VersionMismatch { .. }
            | Self::HelloRequired
            | Self::InvalidParams(_)
            | Self::Busy(_)
            | Self::Apply { .. }
            | Self::SessionParity { .. }
            | Self::SessionFailed { .. }
            | Self::DesktopFailed { .. }
            | Self::RegistryAuth { .. }
            | Self::Io { .. } => Some("tilo-vm guide troubleshooting"),
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
    #[expect(clippy::too_many_lines, reason = "one sample per variant")]
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
            DaemonError::SlotOutputsConflict {
                name: "a".into(),
                outputs: "one default output".into(),
            },
            DaemonError::OutputsNeedHeadful("a".into()),
            DaemonError::BadOutputs("9 outputs asked for".into()),
            DaemonError::DisplayLayout {
                slot: "a".into(),
                detail: "Virtual-1 offers no 1366x768 mode".into(),
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
                field: "Cargo.lock".into(),
                layer: "a".into(),
                vm: "b".into(),
            },
            DaemonError::AbiMismatch {
                field: "builder image".into(),
                layer: "a".into(),
                vm: "b".into(),
            },
            DaemonError::NoSession("vm".into()),
            DaemonError::NoDeviceSession,
            DaemonError::LoggedOut("device".into()),
            DaemonError::DeviceSlot("removed".into()),
            DaemonError::SessionStopped("a".into()),
            DaemonError::ControlMissing("/usr/libexec/tilo/tilo-control".into()),
            DaemonError::Apply {
                step: "s".into(),
                detail: "d".into(),
            },
            DaemonError::SessionParity {
                slot: "a".into(),
                detail: "CapAmb 800000000".into(),
            },
            DaemonError::TestIdentity {
                slot: "a".into(),
                detail: "error [identity_exists]: already has an identity".into(),
            },
            DaemonError::SessionFailed {
                slot: "a".into(),
                state: "activating".into(),
                log: "PAM failed: Authentication failure".into(),
            },
            DaemonError::DesktopFailed {
                slot: "device".into(),
                detail: "logged out".into(),
                log: "shell: panicked".into(),
            },
            DaemonError::UnknownComponent {
                name: "x".into(),
                known: vec!["shell".into()],
            },
            DaemonError::UnknownApp {
                dir: "x".into(),
                known: vec![],
            },
            DaemonError::UnknownApp {
                dir: "x".into(),
                known: vec!["notes".into()],
            },
            DaemonError::SourceMissing("wt".into()),
            DaemonError::Job {
                step: "cargo build".into(),
                detail: "exit status: 101".into(),
            },
            DaemonError::ToolMissing {
                tool: "cargo".into(),
                purpose: "build".into(),
            },
            DaemonError::ToolMissing {
                tool: "podman".into(),
                purpose: "pull".into(),
            },
            DaemonError::RegistryAuth {
                image: "registry.tilo.so/tilo/tilo:main-abc1234".into(),
            },
            DaemonError::ImageNotFound {
                image: "registry.tilo.so/tilo/tilo:nope".into(),
            },
            DaemonError::MissingInImage {
                image: "i".into(),
                path: "/usr/bin/browser".into(),
            },
            DaemonError::HeadlessInput("a".into()),
            DaemonError::KernelLayer("no vmlinuz".into()),
            DaemonError::BootLoader {
                detail: "no grub2-editenv".into(),
            },
            DaemonError::Initramfs {
                kver: "7.2.5+dev".into(),
                detail: "exit status: 1".into(),
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
                | DaemonError::SlotOutputsConflict { .. }
                | DaemonError::OutputsNeedHeadful(_)
                | DaemonError::BadOutputs(_)
                | DaemonError::DisplayLayout { .. }
                | DaemonError::TestIdentity { .. }
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
                | DaemonError::NoDeviceSession
                | DaemonError::LoggedOut(_)
                | DaemonError::DeviceSlot(_)
                | DaemonError::SessionStopped(_)
                | DaemonError::ControlMissing(_)
                | DaemonError::Apply { .. }
                | DaemonError::SessionParity { .. }
                | DaemonError::SessionFailed { .. }
                | DaemonError::DesktopFailed { .. }
                | DaemonError::UnknownComponent { .. }
                | DaemonError::UnknownApp { .. }
                | DaemonError::SourceMissing(_)
                | DaemonError::Job { .. }
                | DaemonError::ToolMissing { .. }
                | DaemonError::RegistryAuth { .. }
                | DaemonError::ImageNotFound { .. }
                | DaemonError::MissingInImage { .. }
                | DaemonError::HeadlessInput(_)
                | DaemonError::KernelLayer(_)
                | DaemonError::BootLoader { .. }
                | DaemonError::Initramfs { .. }
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
