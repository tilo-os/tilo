//! The installer window: the wizard's steps, what each one validates, and the
//! install they start.
//!
//! Every taffy node needs a widget in `children()` that mirrors it, or focus order
//! breaks. Nothing of the install enters a document: its secrets must never reach
//! history.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

use taffy::{AlignItems, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use tilo_ui_base::a11y::Politeness;
use tilo_ui_base::i18n::ArgValue;

use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::Rhythm;
use tilo_ui_base::tokens::typography::TextRole;
use tilo_ui_base::{widget::DrawChild as _, widget::Spacer, widget::Widget};
use ui::{
    widget::Dropdown, widget::ListRow, widget::Property, widget::PropertyList, widget::Row,
    widget::Spinner, widget::Stepper, widget::Text, widget::TextInput,
};

use install_profile_model::{
    FILESYSTEMS, valid_full_name, valid_hostname, valid_locale, valid_timezone_name, valid_username,
};

use crate::resource::CATALOG;
use crate::setup::destination::action_button::ActionButton;
use crate::setup::destination::control::{Bound, Control, Press, Presses};
use crate::setup::destination::disk_row::{capacity, disk_row};
use crate::setup::destination::effects::DestinationEffect;
use crate::setup::destination::keyboard_view::KeyboardView;
use crate::setup::destination::seg_choice::seg_choice;
use crate::setup::destination::status_log::status_log;
use imager_backend::destination::backend::{InstallBackend, SystemBackend};
use imager_backend::destination::disks::{Disk, GuidedAlongsidePlan};
use imager_backend::destination::install::{InstallOptions, PowerAction, SERVER_PURPOSE};
use imager_backend::destination::network_step::{Connectivity, NetworkPhase, NetworkStep, Wake};
use imager_backend::destination::power_readiness::current_insufficient_battery_percent;
use imager_backend::destination::probes;

pub(crate) mod console;
#[cfg(test)]
mod language_tests;
mod surface;
#[cfg(test)]
mod surface_tests;

pub use console::{CONSOLE_WAKE, Console, redact_recovery_material};

const FIDO2_KEYS: [&str; 2] = [
    "installer-security-key-off",
    "installer-security-key-enroll",
];

const DEFAULT_HOSTNAME: &str = "tilo";
const DEFAULT_TIMEZONE: &str = "UTC";
const DEFAULT_KEYMAP_NAME: &str = tilo_keyboard_map::DEFAULT_DISPLAY_NAME;

const RECOMMENDED_FS: usize = 0;

const INSTALL_METHOD_KEYS: [&str; 2] = ["installer-method-erase", "installer-method-alongside"];
const NEXT_KEY: &str = "imager-installer-next";
const WINDOWS_ALONGSIDE_PREPARATION: &str = "installer-storage-windows-preparation";
const LINUX_ALONGSIDE_READY: &str = "installer-storage-linux-ready";
const LINUX_ALONGSIDE_PREPARATION: &str = "installer-storage-linux-preparation";

/// The text of `key`, which may name the least room Tilo takes on a disk as `$minimum`.
fn storage_text(key: &str) -> String {
    crate::setup::strings::t_args(
        key,
        &[(
            "minimum",
            tilo_ui_base::i18n::ArgValue::Text(tilo_ui_base::format::memory(
                imager_backend::MIN_TARGET_BYTES,
            )),
        )],
    )
}
const ALONGSIDE_BOOT_GUIDANCE: &str = "installer-storage-boot-guidance";
const ALONGSIDE_FILES_GUIDANCE: &str = "installer-storage-files-guidance";

fn connect_power_message(percent: u8) -> String {
    CATALOG.t_args(
        "installer-battery-power",
        &[("percent", ArgValue::Number(f64::from(percent)))],
    )
}

fn alongside_install_impact(guided: bool, dev: &str) -> String {
    let change = crate::setup::strings::t_args(
        if guided {
            "installer-alongside-change-guided"
        } else {
            "installer-alongside-change-prepared"
        },
        &[("device", ArgValue::Text(dev.to_owned()))],
    );
    crate::setup::strings::t_args(
        "installer-alongside-change-note",
        &[("change", ArgValue::Text(change))],
    )
}

fn alongside_confirmation_operation(guided: bool) -> String {
    CATALOG.t(if guided {
        "installer-operation-guided-plan"
    } else {
        "installer-operation-tilo-format"
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMethod {
    Erase,
    Alongside,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DiskMethodIdentity {
    major_minor: String,
    wwn: String,
    serial: String,
    size_bytes: u64,
    partition_table_uuid: String,
}

impl DiskMethodIdentity {
    fn of(disk: &Disk) -> Option<Self> {
        disk.identity_ready().then(|| Self {
            major_minor: disk.major_minor.clone(),
            wwn: disk.wwn.clone(),
            serial: disk.serial.clone(),
            size_bytes: disk.size_bytes,
            partition_table_uuid: disk.partition_table_uuid.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FieldCompletion {
    #[default]
    NeedsRepair,
    Complete,
}

impl FieldCompletion {
    const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

impl From<bool> for FieldCompletion {
    fn from(complete: bool) -> Self {
        if complete {
            Self::Complete
        } else {
            Self::NeedsRepair
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProfileCompletion {
    hostname: FieldCompletion,
    username: FieldCompletion,
    full_name: FieldCompletion,
    locale: FieldCompletion,
    keyboard: FieldCompletion,
    timezone: FieldCompletion,
    filesystem: FieldCompletion,
}

impl ProfileCompletion {
    fn identity_complete(self) -> bool {
        self.username.is_complete() && self.full_name.is_complete()
    }

    fn options_complete(self) -> bool {
        self.hostname.is_complete()
            && self.locale.is_complete()
            && self.timezone.is_complete()
            && self.filesystem.is_complete()
    }
}

fn fs_description(i: usize) -> &'static str {
    match i {
        0 => "installer-fs-btrfs",
        1 => "installer-fs-ext4",
        2 => "installer-fs-xfs",
        _ => "",
    }
}

#[cfg(test)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent sampled probe result"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Protection {
    pub tpm2: bool,
    pub secure_boot: bool,
    pub kernel_lockdown: bool,
    pub target_verified_boot: bool,
}

#[derive(Clone, Copy)]
enum VerifiedBootState {
    NoTpm,
    SecureBootInactive,
    KernelLockdownInactive,
    TargetUnverified,
    TargetVerified,
}

fn encrypt_description(state: VerifiedBootState) -> String {
    let message = match state {
        VerifiedBootState::TargetVerified => "installer-encrypt-verified",
        VerifiedBootState::TargetUnverified => "installer-encrypt-target-unverified",
        VerifiedBootState::KernelLockdownInactive => "installer-encrypt-lockdown-off",
        VerifiedBootState::SecureBootInactive => "installer-encrypt-secure-boot-off",
        VerifiedBootState::NoTpm => "installer-encrypt-no-tpm",
    };
    CATALOG.t(message)
}

const CTRL_DONE: &str = "\u{1}done";
const CTRL_FAIL: &str = "\u{1}fail";
const PREFLIGHT_OK_PREFIX: &str = "\u{1}storage-preflight-ok:";
const PREFLIGHT_FAIL_PREFIX: &str = "\u{1}storage-preflight-fail:";
const RECOVERY_KEY_PREFIX: &str = "::tilo-recovery-key::";

const INSTALLED_REMOVE_MEDIUM: &str = "Installed. Remove the installer medium, then reboot.";
const INSTALLED_RESTART: &str = "Installed. Restart to start Tilo.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Keyboard,
    Network,
    Disk,
    Account,
    Options,
    Media,
    Review,
    Install,
}

pub const STEP_LABELS: [&str; 8] = [
    "Keyboard", "Network", "Disk", "Account", "Options", "Media", "Review", "Install",
];

const STEP_KEYS: [&str; 8] = [
    "installer-step-keyboard",
    "installer-step-network",
    "installer-step-disk",
    "installer-step-account",
    "installer-step-options",
    "installer-step-media",
    "installer-step-review",
    "installer-step-install",
];

fn step_captions() -> Vec<String> {
    captions(&STEP_KEYS)
}

fn captions(keys: &[&str]) -> Vec<String> {
    keys.iter()
        .map(|key| crate::setup::strings::t(key))
        .collect()
}

fn network_summary(network: &NetworkStep) -> String {
    if network.connectivity == Connectivity::Full {
        if let Some(row) = network.networks.iter().find(|row| row.connected) {
            return CATALOG.t_args(
                "installer-network-via",
                &[("ssid", ArgValue::Text(row.name.clone()))],
            );
        }
        return CATALOG.t("installer-network-available");
    }
    CATALOG.t_args(
        "installer-network-offline-available",
        &[(
            "connectivity",
            ArgValue::Text(CATALOG.t(connectivity_message(network.connectivity))),
        )],
    )
}

fn connectivity_message(state: Connectivity) -> &'static str {
    match state {
        Connectivity::Full => "installer-connectivity-full",
        Connectivity::Portal => "installer-connectivity-portal",
        Connectivity::Limited => "installer-connectivity-limited",
        Connectivity::Offline => "installer-connectivity-offline",
        Connectivity::Unknown => "installer-connectivity-unknown",
    }
}

pub fn step_index(step: Step) -> usize {
    match step {
        Step::Keyboard => 0,
        Step::Network => 1,
        Step::Disk => 2,
        Step::Account => 3,
        Step::Options => 4,
        Step::Media => 5,
        Step::Review => 6,
        Step::Install => 7,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    Idle,
    Running,
    Done,
    Failed,
}

pub fn next_step(step: Step) -> Option<Step> {
    match step {
        Step::Keyboard => Some(Step::Network),
        Step::Network => Some(Step::Disk),
        Step::Disk => Some(Step::Account),
        Step::Account => Some(Step::Options),
        Step::Options => Some(Step::Media),
        Step::Media => Some(Step::Review),
        Step::Review => Some(Step::Install),
        Step::Install => None,
    }
}

pub fn prev_step(step: Step) -> Option<Step> {
    match step {
        Step::Network => Some(Step::Keyboard),
        Step::Disk => Some(Step::Network),
        Step::Account => Some(Step::Disk),
        Step::Options => Some(Step::Account),
        Step::Media => Some(Step::Options),
        Step::Review => Some(Step::Media),
        Step::Keyboard | Step::Install => None,
    }
}

pub fn validate_step(
    step: Step,
    selected_disk: Option<usize>,
    username: &str,
    hostname: &str,
    purpose: &str,
) -> Result<(), String> {
    match step {
        Step::Disk => {
            if selected_disk.is_some() {
                Ok(())
            } else {
                Err("Select a disk to continue".to_string())
            }
        }
        // A Tilo Server has no login account; the step only collects the disk
        // secret, which `validate_account_details` checks.
        Step::Account if purpose == SERVER_PURPOSE => Ok(()),
        Step::Account => {
            if username.is_empty() {
                Err("Enter a username".to_string())
            } else if valid_username(username) {
                Ok(())
            } else {
                Err(
                    "Use up to 32 lowercase letters, digits, _ or -; start with a letter or _"
                        .to_string(),
                )
            }
        }
        Step::Options => {
            if valid_hostname(hostname) {
                Ok(())
            } else {
                Err("Enter a valid hostname (letters, digits, hyphens)".to_string())
            }
        }
        Step::Network | Step::Keyboard | Step::Media | Step::Review | Step::Install => Ok(()),
    }
}

pub fn validate_passwords(password: &str, confirmation: &str) -> Result<(), String> {
    if password.trim().is_empty() {
        Err("Enter an account password".to_string())
    } else if password != confirmation {
        Err("The passwords do not match".to_string())
    } else {
        Ok(())
    }
}

pub(crate) fn validate_startup_secret_for(secret: &str, tpm_pin: bool) -> Result<(), &'static str> {
    imager_backend::startup_password::validate_on_destination(secret, tpm_pin)
}

#[cfg(test)]
fn validate_startup_secret(secret: &str) -> Result<(), &'static str> {
    validate_startup_secret_for(secret, false)
}

const NETWORK_WAKE: &str = "\u{1}network";

#[derive(Clone)]
pub struct Mailbox {
    queue: Arc<Mutex<std::collections::VecDeque<String>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Mailbox {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            queue: Arc::default(),
            wake: Arc::new(wake),
        }
    }

    pub fn post(&self, payload: String) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(payload);
        }
        (self.wake)();
    }

    fn take(&self) -> Vec<String> {
        self.queue
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default()
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent wizard-state flag (encryption/FIDO2 opt-in, TPM presence, shot/dirty/focus one-shots), not a combinatorial state machine"
)]
pub struct InstallerApp {
    network: NetworkStep,
    disks: Vec<Disk>,
    selected_disk: Option<usize>,
    selected_guided_extent: Option<usize>,
    alongside_capacity: Option<(Disk, GuidedAlongsidePlan, u64)>,
    capacity_gib: tilo_ui_base::LocalState<f64>,
    capacity_seen: Cell<f64>,
    install_method: InstallMethod,
    disk_method_choices: HashMap<DiskMethodIdentity, InstallMethod>,
    guided_efi_partuuid: String,
    guided_boot_partuuid: String,
    guided_root_partuuid: String,
    selected_fs: usize,
    supported_filesystems: [bool; FILESYSTEMS.len()],
    filesystem_policy_error: Option<String>,
    fido2: bool,
    codecs: bool,
    recovery_ack: bool,
    password_is_hash: bool,
    force_startup_passphrase: bool,
    unattended_mode: bool,
    unattended_error: Option<String>,
    tpm2: bool,
    secure_boot: bool,
    kernel_lockdown: bool,
    hardware: crate::setup::destination::hardware_report::HardwareReport,
    target_verified_boot: bool,
    step: Step,
    install_state: InstallState,
    status_lines: Vec<String>,
    recovery_key: Option<String>,
    recovery_key_copied: bool,
    recovery_key_copy: tilo_ui_base::CopyWatch,
    recovery_key_saved: bool,
    username: tilo_ui_base::LocalState<String>,
    full_name: tilo_ui_base::LocalState<String>,
    /// Secrets never enter the document: deleting CRDT text leaves the old bytes behind.
    password: Arc<Mutex<String>>,
    password_confirmation: Arc<Mutex<String>>,
    disk_password: Arc<Mutex<String>>,
    wifi_password: Arc<Mutex<String>>,
    hostname: tilo_ui_base::LocalState<String>,
    purpose: String,
    /// The plan's unlock choice (`owner-device`, `tpm` or `passphrase`);
    /// empty when no plan decided it, which leaves the image's default.
    unlock: String,
    /// The plan's static addressing; `None` when it decided DHCP or no plan
    /// decided the network. It reaches the installer, which gives the machine
    /// its address before it can enroll.
    network_static: Option<(String, String, String)>,
    /// A complete plan stands in for the person's confirmation.
    install_plan: bool,
    device_label: String,
    locale: tilo_ui_base::LocalState<String>,
    timezone: tilo_ui_base::LocalState<String>,
    keymap: tilo_ui_base::LocalState<String>,
    erase_confirmation: tilo_ui_base::LocalState<String>,
    erase_confirmation_seen: String,
    timezones: Vec<String>,
    tz_open_for_shot: bool,
    status: String,
    presses: Presses,
    dirty_structure: bool,
    focus_reset: bool,
    rect: Rect,
    composed: ui::widget::Composed,
    install_launched: bool,
    storage_preflight_request: Option<String>,
    storage_preflight_passed: bool,
    profile_completion: Option<ProfileCompletion>,
    internal_handoff: Option<imager_backend::destination::internal_handoff::Handoff>,
    mailbox: Option<Mailbox>,
    power_probe: fn() -> Option<u8>,
    viewport_offset: Rc<Cell<(f32, f32)>>,
    backend: Arc<dyn InstallBackend>,
    console: Option<imager_backend::destination::console::ConsoleServer>,

    dirty: bool,
}

impl Default for InstallerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl InstallerApp {
    pub fn new() -> Self {
        let mut s = Self::with_disks(SystemBackend.read_disks());
        s.power_probe = current_insufficient_battery_percent;
        s.hardware = crate::setup::destination::hardware_report::HardwareReport::probe();
        s.timezones = crate::timezones::offered().to_vec();
        match imager_backend::destination::media_profile::load() {
            Ok(Some(profile)) => s.apply_media_profile(profile),
            Ok(None) => {
                if let Some(message) = imager_backend::destination::media_profile::boot_error() {
                    s.status = message;
                }
            }
            Err(message) => s.status = message,
        }
        match imager_backend::destination::internal_handoff::load() {
            Ok(Some(handoff)) => s.apply_internal_handoff(handoff),
            Ok(None) => {
                if let Some(message) = imager_backend::destination::internal_handoff::boot_error() {
                    s.status = message;
                }
            }
            Err(message) => s.status = message,
        }
        s.set_filesystem_support(probes::filesystem_support());
        s.focus_reset = true;
        s
    }

    fn apply_internal_handoff(
        &mut self,
        handoff: imager_backend::destination::internal_handoff::Handoff,
    ) {
        let profile = imager_backend::destination::internal_handoff::media_profile(&handoff);
        self.internal_handoff = Some(handoff);
        if !self.bind_internal_handoff() {
            self.internal_handoff = None;
            self.status = "Tilo Imager prepared an installation, but this disk no longer matches it. Installing onto the disk that holds this installer is unavailable; restart into your existing system and run Tilo Imager again.".to_string();
            return;
        }
        match profile {
            Ok(profile) => self.apply_media_profile(profile),
            Err(message) => self.status = message,
        }
        if let Some(host) = self.internal_host() {
            let prefix = format!(
                "Tilo Imager prepared space for Tilo next to {}.",
                host.display_name()
            );
            self.status = if self.status.is_empty() {
                prefix
            } else {
                format!("{prefix} {}", self.status)
            };
        }
    }

    fn bind_internal_handoff(&mut self) -> bool {
        let Some(handoff) = self.internal_handoff.clone() else {
            return false;
        };
        let mut bound = Vec::new();
        for (index, disk) in self.disks.iter_mut().enumerate() {
            if disk.apply_internal_handoff(&handoff).is_ok() {
                bound.push(index);
            }
        }
        if let [index] = bound.as_slice() {
            self.selected_disk = Some(*index);
            self.install_method = InstallMethod::Alongside;
            true
        } else {
            for disk in &mut self.disks {
                disk.internal_plan = None;
            }
            false
        }
    }

    fn internal_host(&self) -> Option<imager_backend::destination::internal_handoff::HostOs> {
        self.internal_handoff
            .as_ref()
            .filter(|_| self.disks.iter().any(Disk::hosts_staged_installer))
            .map(|handoff| handoff.host_os)
    }

    fn selected_disk_hosts_installer(&self) -> bool {
        self.selected_disk_ref()
            .is_some_and(Disk::hosts_staged_installer)
    }

    pub fn with_disks(disks: Vec<Disk>) -> Self {
        Self {
            dirty: true,
            network: NetworkStep::default(),
            disks,
            selected_disk: None,
            selected_guided_extent: None,
            alongside_capacity: None,
            capacity_gib: tilo_ui_base::LocalState::new(0.0),
            capacity_seen: Cell::new(0.0),
            install_method: InstallMethod::Erase,
            disk_method_choices: HashMap::new(),
            guided_efi_partuuid: uuid::Uuid::new_v4().to_string(),
            guided_boot_partuuid: uuid::Uuid::new_v4().to_string(),
            guided_root_partuuid: uuid::Uuid::new_v4().to_string(),
            selected_fs: 0,
            supported_filesystems: [true; FILESYSTEMS.len()],
            filesystem_policy_error: None,
            fido2: false,
            codecs: false,
            recovery_ack: false,
            password_is_hash: false,
            force_startup_passphrase: false,
            unattended_mode: false,
            unattended_error: None,
            hardware: crate::setup::destination::hardware_report::HardwareReport::default(),
            tpm2: probes::has_tpm2(),
            secure_boot: probes::secure_boot_enabled(),
            kernel_lockdown: probes::kernel_lockdown_active(),
            target_verified_boot: probes::target_verified_boot_ready(),
            step: Step::Keyboard,
            install_state: InstallState::Idle,
            status_lines: Vec::new(),
            recovery_key: None,
            recovery_key_copied: false,
            recovery_key_copy: tilo_ui_base::CopyWatch::default(),
            recovery_key_saved: false,
            username: tilo_ui_base::LocalState::new(String::new()),
            full_name: tilo_ui_base::LocalState::new(String::new()),
            password: Arc::new(Mutex::new(String::new())),
            password_confirmation: Arc::new(Mutex::new(String::new())),
            disk_password: Arc::new(Mutex::new(String::new())),
            wifi_password: Arc::new(Mutex::new(String::new())),
            hostname: tilo_ui_base::LocalState::new(DEFAULT_HOSTNAME.to_string()),
            locale: tilo_ui_base::LocalState::new("en_US.UTF-8".to_string()),
            timezone: tilo_ui_base::LocalState::new(DEFAULT_TIMEZONE.to_string()),
            purpose: install_profile_model::default_purpose(),
            unlock: String::new(),
            network_static: None,
            install_plan: false,
            device_label: String::new(),
            keymap: tilo_ui_base::LocalState::new(DEFAULT_KEYMAP_NAME.to_string()),
            erase_confirmation: tilo_ui_base::LocalState::new(String::new()),
            erase_confirmation_seen: String::new(),
            timezones: crate::timezones::offered().to_vec(),
            tz_open_for_shot: false,
            status: String::new(),
            presses: Presses::default(),
            dirty_structure: false,
            focus_reset: false,
            rect: Rect::ZERO,
            composed: ui::widget::Composed::default(),
            install_launched: false,
            storage_preflight_request: None,
            storage_preflight_passed: false,
            profile_completion: None,
            internal_handoff: None,
            mailbox: None,
            power_probe: || None,
            viewport_offset: Rc::new(Cell::new((0.0, 0.0))),
            backend: Arc::new(SystemBackend),
            console: None,
        }
    }

    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn InstallBackend>) -> Self {
        self.disks = backend.read_disks();
        self.backend = backend;
        self
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// Make this staged installation the one `plan` decides: the server
    /// destination, the machine's name, how it unlocks, and no person needed
    /// to confirm what the plan already answered. The disk is the one the
    /// staged handoff bound; a plan that names another disk is refused.
    pub(crate) fn apply_install_plan(
        &mut self,
        plan: &install_profile_model::plan::Plan,
    ) -> Result<(), String> {
        if plan.destination != install_profile_model::plan::SERVER {
            return Err("Only a Tilo Server installs from a plan with nobody at its screen".into());
        }
        if !plan.complete() {
            return Err(format!(
                "The plan still leaves {} open for one of your devices to answer",
                plan.open_choices().join(", ")
            ));
        }
        if plan.unlock == "passphrase" {
            return Err(
                "A plan installs with nobody at the screen, so it cannot unlock by a \
                 passphrase only its owner knows; plan it with another of your devices, or \
                 with the machine's hardware protection"
                    .into(),
            );
        }
        let disk = self
            .selected_disk_ref()
            .filter(|disk| self.internal_handoff.is_some() && disk.internal_plan.is_some())
            .ok_or(
                "The installer is not running from a staged installation this machine still \
                 matches; restart into your existing system and stage it again",
            )?;
        // A serial or WWN is spelled by whatever read it: strip the prefixes
        // the kernel's identity links add before comparing.
        let same = |have: &str, want: &str| {
            let plain = |value: &str| {
                let value = value.trim().to_ascii_lowercase();
                let value = value.strip_prefix("0x").unwrap_or(&value);
                let value = value.strip_prefix("eui.").unwrap_or(value);
                value.strip_prefix("wwn-").unwrap_or(value).to_owned()
            };
            plain(have) == plain(want)
        };
        let named = match plan.disk.kind.as_str() {
            "serial" => same(&disk.serial, &plan.disk.identity),
            "wwid" => same(&disk.wwn, &plan.disk.identity),
            _ => false,
        };
        if !named {
            return Err(format!(
                "The plan names {} but the staged installation is on {}",
                plan.disk.encode(),
                disk.name
            ));
        }
        SERVER_PURPOSE.clone_into(&mut self.purpose);
        self.hostname = tilo_ui_base::LocalState::new(plan.name.clone());
        self.unlock.clone_from(&plan.unlock);
        self.network_static = (plan.network.addressing == "static").then(|| {
            (
                plan.network.addresses.clone(),
                plan.network.gateway.clone(),
                plan.network.dns.clone(),
            )
        });
        self.install_plan = true;
        Ok(())
    }

    pub(crate) fn for_unattended() -> Self {
        let mut app = Self::with_disks(SystemBackend.read_disks());
        app.power_probe = current_insufficient_battery_percent;
        app.hardware = crate::setup::destination::hardware_report::HardwareReport::probe();
        app.timezones = crate::timezones::offered().to_vec();
        app.set_filesystem_support(probes::filesystem_support());
        app
    }

    pub(crate) fn enable_unattended_mode(&mut self) {
        self.unattended_mode = true;
        self.unattended_error = None;
    }

    pub(crate) fn note_unattended_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.set_status(&message);
        self.unattended_error = Some(message);
    }

    pub(crate) fn probe_disks(&self) -> Vec<Disk> {
        self.backend.read_disks()
    }

    pub(crate) fn recovery_key(&self) -> Option<&str> {
        self.recovery_key.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn with_protection_for_test(mut self, protection: Protection) -> Self {
        self.tpm2 = protection.tpm2;
        self.secure_boot = protection.secure_boot;
        self.kernel_lockdown = protection.kernel_lockdown;
        self.target_verified_boot = protection.target_verified_boot;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_hardware_for_test(mut self, report: hardware_report::SupportReport) -> Self {
        self.hardware =
            crate::setup::destination::hardware_report::HardwareReport::from_report(report);
        self
    }

    pub fn install_state(&self) -> InstallState {
        self.install_state
    }

    fn apply_media_profile(
        &mut self,
        profile: imager_backend::destination::media_profile::MediaProfile,
    ) {
        self.purpose.clone_from(&profile.purpose);
        self.device_label.clone_from(&profile.device_label);
        let status = profile.repair_summary();
        let keyboard_display = profile.keyboard_display_name().map(str::to_string);
        let completion = ProfileCompletion {
            hostname: profile.hostname.is_some().into(),
            username: profile.username.is_some().into(),
            full_name: profile.full_name.is_some().into(),
            locale: profile.locale.is_some().into(),
            keyboard: profile.keyboard.is_some().into(),
            timezone: profile.timezone.is_some().into(),
            filesystem: profile.filesystem.is_some().into(),
        };
        if let Some(value) = profile.hostname {
            self.hostname.set(value);
        }
        if let Some(value) = profile.username {
            self.username.set(value);
        }
        if let Some(value) = profile.full_name {
            self.full_name.set(value);
        }
        if let Some(value) = profile.locale {
            self.locale.set(value);
        }
        if let Some(value) = keyboard_display {
            self.keymap.set(value);
        }
        if let Some(value) = profile.timezone {
            self.timezone.set(value);
        }
        if let Some(value) = profile.filesystem {
            self.selected_fs = FILESYSTEMS
                .iter()
                .position(|filesystem| *filesystem == value)
                .unwrap_or(0);
        }
        self.step = if completion.keyboard.is_complete() {
            Step::Disk
        } else {
            Step::Keyboard
        };
        self.profile_completion = Some(completion);
        self.status = status;
    }

    fn set_filesystem_support(&mut self, result: Result<[bool; FILESYSTEMS.len()], String>) {
        match result {
            Ok(supported) => {
                self.supported_filesystems = supported;
                self.filesystem_policy_error = None;
                if !supported.get(self.selected_fs).copied().unwrap_or(false) {
                    let rejected = self.filesystem().to_string();
                    if let Some(completion) = self.profile_completion.as_mut() {
                        completion.filesystem = FieldCompletion::NeedsRepair;
                    }
                    if let Some(index) = supported.iter().position(|available| *available) {
                        self.selected_fs = index;
                    }
                    self.status = CATALOG.t_args(
                        "installer-filesystem-cannot-boot",
                        &[("filesystem", ArgValue::Text(rejected))],
                    );
                }
            }
            Err(message) => {
                self.supported_filesystems = [false; FILESYSTEMS.len()];
                self.filesystem_policy_error = Some(message.clone());
                if let Some(completion) = self.profile_completion.as_mut() {
                    completion.filesystem = FieldCompletion::NeedsRepair;
                }
                self.status = CATALOG.t_args(
                    "installer-install-unavailable",
                    &[("message", ArgValue::Text(message))],
                );
            }
        }
    }

    fn filesystem_policy_ready(&self) -> bool {
        self.filesystem_policy_error.is_none()
            && self.supported_filesystems.get(self.selected_fs) == Some(&true)
    }

    fn available_filesystem_indices(&self) -> Vec<usize> {
        (0..FILESYSTEMS.len())
            .filter(|index| self.supported_filesystems.get(*index) == Some(&true))
            .collect()
    }

    fn filesystem_policy_message(&self) -> String {
        if let Some(message) = &self.filesystem_policy_error {
            return CATALOG.t_args(
                "installer-install-unavailable",
                &[("message", ArgValue::Text(message.clone()))],
            );
        }
        CATALOG.t_args(
            "installer-filesystem-cannot-boot",
            &[("filesystem", ArgValue::Text(self.filesystem().to_owned()))],
        )
    }

    pub fn for_scenario(name: &str) -> Option<Self> {
        let staged = |step: Step, stage: fn(&mut Self)| {
            let mut app = Self::for_shot(step);
            stage(&mut app);
            app
        };
        Some(match name {
            "disk-alongside" => staged(Step::Disk, Self::shot_alongside),
            "review-alongside" => staged(Step::Review, Self::shot_alongside_review),
            "options-open" => staged(Step::Options, Self::shot_open_timezone),
            "profile-locale-repair" => staged(Step::Options, Self::shot_locale_repair),
            "keyboard" => staged(Step::Keyboard, |app| app.shot_keymap("German")),
            "media-chosen" => staged(Step::Media, Self::shot_codecs_chosen),
            "finish" => staged(Step::Install, Self::shot_finished),
            "finish-recovery" => staged(Step::Install, Self::shot_finished_with_recovery),
            "finish-recovery-saved" => {
                staged(Step::Install, Self::shot_finished_with_saved_recovery)
            }
            "failed" => staged(Step::Install, Self::shot_failed),
            "failed-recovery" => staged(Step::Install, Self::shot_failed_with_recovery),
            "network" => Self::for_shot(Step::Network),
            "disk" => Self::for_shot(Step::Disk),
            "account" => Self::for_shot(Step::Account),
            "keyboard-default" => Self::for_shot(Step::Keyboard),
            "options" => Self::for_shot(Step::Options),
            "media" => Self::for_shot(Step::Media),
            "review" => Self::for_shot(Step::Review),
            "install" => Self::for_shot(Step::Install),
            _ => return None,
        })
    }

    pub fn for_shot(step: Step) -> Self {
        let mut app = Self::with_disks(Disk::samples());
        app.step = step;
        app.username.set("ada".to_string());
        app.full_name.set("Ada Example".to_string());
        app.password = Arc::new(Mutex::new("correct horse battery staple".to_string()));
        app.password_confirmation =
            Arc::new(Mutex::new("correct horse battery staple".to_string()));
        app.hostname.set(DEFAULT_HOSTNAME.to_string());
        app.timezone.set(DEFAULT_TIMEZONE.to_string());
        app.keymap.set(DEFAULT_KEYMAP_NAME.to_string());
        if !matches!(step, Step::Network | Step::Keyboard | Step::Disk) {
            app.selected_disk = Some(0);
        }
        app.selected_fs = 0;
        if matches!(step, Step::Install) {
            app.install_state = InstallState::Running;
            app.status_lines = sample_install_log();
        }
        app
    }

    pub fn shot_open_timezone(&mut self) {
        self.step = Step::Options;
        self.tz_open_for_shot = true;
    }

    pub fn shot_locale_repair(&mut self) {
        self.apply_media_profile(imager_backend::destination::media_profile::MediaProfile {
            purpose: install_profile_model::default_purpose(),
            device_label: String::new(),
            hostname: Some("studio".into()),
            username: Some("ada".into()),
            full_name: Some("Ada Example".into()),
            locale: None,
            keyboard: Some("de".into()),
            timezone: Some("Europe/Berlin".into()),
            filesystem: Some("ext4".into()),
            repairs: vec![imager_backend::destination::media_profile::MediaProfileField::Locale],
        });
        self.selected_disk = Some(0);
        self.step = Step::Options;
    }

    pub fn shot_codecs_chosen(&mut self) {
        self.step = Step::Media;
        self.codecs = true;
    }

    pub fn shot_alongside(&mut self) {
        self.step = Step::Disk;
        self.install_method = InstallMethod::Alongside;
        self.selected_disk = Some(0);
    }

    pub fn shot_alongside_review(&mut self) {
        self.install_method = InstallMethod::Alongside;
        self.selected_disk = Some(0);
        self.step = Step::Review;
    }

    pub fn shot_keymap(&mut self, display_name: &str) {
        self.step = Step::Keyboard;
        self.keymap.set(display_name.to_string());
    }

    pub fn shot_finished(&mut self) {
        self.step = Step::Install;
        self.install_state = InstallState::Done;
        self.status_lines = sample_install_log();
        self.status_lines.push(INSTALLED_REMOVE_MEDIUM.to_string());
    }

    pub fn shot_finished_with_recovery(&mut self) {
        self.shot_finished();
        self.recovery_key = Some(
            "aaaa1111-bbbb2222-cccc3333-dddd4444-eeee5555-ffff6666-gggg7777-hhhh8888".to_string(),
        );
    }

    pub fn shot_finished_with_saved_recovery(&mut self) {
        self.shot_finished_with_recovery();
        self.recovery_key_copied = true;
        self.recovery_key_saved = true;
    }

    pub fn shot_failed(&mut self) {
        self.step = Step::Install;
        self.install_state = InstallState::Failed;
        self.status_lines = sample_install_log();
        self.status_lines
            .push("error: bootc install to-disk exited non-zero".to_string());
    }

    pub fn shot_failed_with_recovery(&mut self) {
        self.shot_failed();
        self.recovery_key = Some(
            "aaaa1111-bbbb2222-cccc3333-dddd4444-eeee5555-ffff6666-gggg7777-hhhh8888".to_string(),
        );
    }

    fn reset_disk_method(&mut self) {
        self.install_method = if self.selected_disk_hosts_installer() {
            InstallMethod::Alongside
        } else if let Some(disk) = self.selected_disk_ref() {
            DiskMethodIdentity::of(disk)
                .and_then(|identity| self.disk_method_choices.get(&identity).copied())
                .unwrap_or(if disk.partitions.is_empty() {
                    InstallMethod::Erase
                } else {
                    InstallMethod::Alongside
                })
        } else {
            InstallMethod::Alongside
        };
    }

    fn selected_disk_ref(&self) -> Option<&Disk> {
        self.selected_disk.and_then(|i| self.disks.get(i))
    }

    fn selected_dev(&self) -> Option<String> {
        self.selected_disk_ref().map(|d| d.name.clone())
    }

    fn guided_alongside_plan(&self) -> Option<GuidedAlongsidePlan> {
        let full = self.selected_full_guided_plan()?;
        let Some((disk, extent, bytes)) = &self.alongside_capacity else {
            return Some(full);
        };
        if self.selected_disk_ref()? != disk || extent != &full {
            return None;
        }
        full.with_capacity_bytes(*bytes)
    }

    fn selected_full_guided_plan(&self) -> Option<GuidedAlongsidePlan> {
        if self.install_method != InstallMethod::Alongside {
            return None;
        }
        let disk = self.selected_disk_ref()?;
        if disk.can_install_alongside() {
            return None;
        }
        let plans = disk.guided_alongside_plans();
        match plans.as_slice() {
            [only] => Some(only.clone()),
            _ => self
                .selected_guided_extent
                .and_then(|index| plans.get(index).cloned()),
        }
    }

    fn guided_alongside_plans(&self) -> Vec<GuidedAlongsidePlan> {
        if self.install_method != InstallMethod::Alongside {
            return Vec::new();
        }
        self.selected_disk_ref()
            .filter(|disk| !disk.can_install_alongside())
            .map_or_else(Vec::new, Disk::guided_alongside_plans)
    }

    fn uses_guided_alongside(&self) -> bool {
        self.install_method == InstallMethod::Alongside
            && self
                .selected_disk_ref()
                .is_some_and(|disk| !disk.can_install_alongside())
            && self.guided_alongside_plan().is_some()
    }

    fn selected_disk_can_install_alongside(&self) -> bool {
        self.selected_disk_ref().is_some_and(|disk| {
            disk.can_install_alongside() || self.guided_alongside_plan().is_some()
        })
    }

    fn filesystem(&self) -> &str {
        FILESYSTEMS
            .get(self.selected_fs)
            .copied()
            .unwrap_or("btrfs")
    }

    fn layout(&self) -> tilo_keyboard_map::Layout {
        tilo_keyboard_map::by_display_name(self.keymap().trim())
            .unwrap_or_else(tilo_keyboard_map::default_layout)
    }

    fn keymap(&self) -> String {
        local_text(&self.keymap)
    }

    fn locale(&self) -> String {
        local_text(&self.locale)
    }

    fn timezone(&self) -> String {
        local_text(&self.timezone)
    }

    fn verified_boot_state(&self) -> VerifiedBootState {
        if !self.tpm2 {
            VerifiedBootState::NoTpm
        } else if !self.secure_boot {
            VerifiedBootState::SecureBootInactive
        } else if !self.kernel_lockdown {
            VerifiedBootState::KernelLockdownInactive
        } else if !self.target_verified_boot {
            VerifiedBootState::TargetUnverified
        } else {
            VerifiedBootState::TargetVerified
        }
    }

    fn startup_secret_is_pin(&self) -> bool {
        !self.force_startup_passphrase && self.tpm_pin_policy()
    }

    fn startup_secret(&self) -> Zeroizing<String> {
        let separate = secret_copy(&self.disk_password);
        if separate.is_empty() {
            secret_copy(&self.password)
        } else {
            separate
        }
    }

    fn encryption_ready(&self) -> bool {
        self.recovery_ack
            && validate_startup_secret_for(&self.startup_secret(), self.startup_secret_is_pin())
                .is_ok()
    }

    fn destructive_confirmation_target(&self) -> Option<String> {
        match self.install_method {
            InstallMethod::Erase => self.selected_dev(),
            InstallMethod::Alongside => self.selected_disk_ref().and_then(|disk| {
                disk.alongside_root()
                    .map(|partition| partition.name.clone())
                    .or_else(|| self.guided_alongside_plan().map(|_| disk.name.clone()))
            }),
        }
    }

    fn destructive_confirmation_ready(&self) -> bool {
        self.destructive_confirmation_target()
            .is_some_and(|device| self.erase_confirmation.with(|typed| typed.trim() == device))
    }

    fn encryption_factors(&self) -> Vec<String> {
        let mut factors = vec!["recovery key".to_string()];
        if !self.startup_secret().trim().is_empty() {
            factors.push(
                if self.startup_secret_is_pin() {
                    "TPM-protected startup PIN"
                } else {
                    "boot passphrase"
                }
                .to_string(),
            );
        }
        if self.fido2 {
            factors.push("FIDO2".to_string());
        }
        factors
    }

    fn install_options(&self) -> InstallOptions {
        let layout = self.layout();
        let disk = self.selected_disk_ref();
        let (root, boot, efi) = if self.install_method == InstallMethod::Alongside {
            disk.map_or((None, None, None), |disk| {
                (
                    disk.alongside_root(),
                    disk.alongside_boot(),
                    disk.efi_partition(),
                )
            })
        } else {
            (None, None, None)
        };
        let guided = if root.is_none() && boot.is_none() && efi.is_none() {
            self.guided_alongside_plan()
        } else {
            None
        };
        InstallOptions {
            disk_size_bytes: disk.map_or(0, |disk| disk.size_bytes),
            disk_major_minor: disk.map_or_else(String::new, |disk| disk.major_minor.clone()),
            disk_wwn: disk.map_or_else(String::new, |disk| disk.wwn.clone()),
            disk_serial: disk.map_or_else(String::new, |disk| disk.serial.clone()),
            unlock: self.unlock.clone(),
            install_plan: self.install_plan,
            network_static: self
                .network_static
                .as_ref()
                .map_or_else(String::new, |(addresses, _, _)| addresses.clone()),
            network_gateway: self
                .network_static
                .as_ref()
                .map_or_else(String::new, |(_, gateway, _)| gateway.clone()),
            network_dns: self
                .network_static
                .as_ref()
                .map_or_else(String::new, |(_, _, dns)| dns.clone()),
            alongside_root: root.map_or_else(String::new, |partition| partition.name.clone()),
            alongside_root_partuuid: root
                .map_or_else(String::new, |partition| partition.part_uuid.clone()),
            alongside_root_size_bytes: root.map_or(0, |partition| partition.size_bytes),
            alongside_boot: boot.map_or_else(String::new, |partition| partition.name.clone()),
            alongside_boot_partuuid: boot
                .map_or_else(String::new, |partition| partition.part_uuid.clone()),
            alongside_boot_size_bytes: boot.map_or(0, |partition| partition.size_bytes),
            alongside_efi: efi.map_or_else(String::new, |partition| partition.name.clone()),
            alongside_efi_partuuid: efi
                .map_or_else(String::new, |partition| partition.part_uuid.clone()),
            alongside_efi_size_bytes: efi.map_or(0, |partition| partition.size_bytes),
            guided_free_start_bytes: guided.as_ref().map_or(0, |plan| plan.free_start_bytes),
            guided_free_size_bytes: guided.as_ref().map_or(0, |plan| plan.free_size_bytes),
            guided_efi_start_bytes: guided.as_ref().map_or(0, |plan| plan.efi_start_bytes),
            guided_efi_size_bytes: guided.as_ref().map_or(0, |plan| plan.efi_size_bytes),
            guided_boot_start_bytes: guided.as_ref().map_or(0, |plan| plan.boot_start_bytes),
            guided_boot_size_bytes: guided.as_ref().map_or(0, |plan| plan.boot_size_bytes),
            guided_root_start_bytes: guided.as_ref().map_or(0, |plan| plan.root_start_bytes),
            guided_root_size_bytes: guided.as_ref().map_or(0, |plan| plan.root_size_bytes),
            disk_partition_table_uuid: disk
                .map_or_else(String::new, |disk| disk.partition_table_uuid.clone()),
            disk_logical_sector_size: disk.map_or(0, |disk| disk.logical_sector_size),
            guided_efi_partuuid: self.guided_efi_partuuid.clone(),
            guided_boot_partuuid: self.guided_boot_partuuid.clone(),
            guided_root_partuuid: self.guided_root_partuuid.clone(),
            expected_partitions: if guided.is_some() {
                disk.and_then(Disk::occupied_extents).unwrap_or_default()
            } else {
                Vec::new()
            },
            filesystem: self.filesystem().to_string(),
            hostname: local_text(&self.hostname).trim().to_string(),
            purpose: self.purpose.clone(),
            device_label: self.device_label.clone(),
            full_name: local_text(&self.full_name).trim().to_string(),
            locale: self.locale().trim().to_string(),
            timezone: self.timezone().trim().to_string(),
            keymap: layout.xkb_layout().to_string(),
            keyvariant: layout.xkb_variant().to_string(),
            pin: self.startup_secret(),
            password_is_hash: self.password_is_hash,
            force_startup_passphrase: self.force_startup_passphrase,
            fido2: self.fido2,
            internal_handoff: if guided.is_some() && disk.is_some_and(Disk::hosts_staged_installer)
            {
                imager_backend::destination::internal_handoff::HANDOFF_PATH.to_string()
            } else {
                String::new()
            },
            codecs: self.codecs,
        }
    }

    fn set_status(&mut self, msg: &str) {
        if self.status != msg {
            self.status = msg.to_string();
            self.dirty_structure = true;
        }
    }

    fn reset_viewport(&self) {
        self.viewport_offset.set((0.0, 0.0));
    }

    fn clear_erase_confirmation(&mut self) {
        self.set_erase_confirmation(String::new());
    }

    fn set_erase_confirmation(&mut self, value: String) {
        self.erase_confirmation.set(value.clone());
        self.erase_confirmation_seen = value;
    }

    /// Whether this medium installs the Tilo Server image, which has no login
    /// account: the account step then collects only the disk's unlock secret.
    fn server_destination(&self) -> bool {
        self.purpose == SERVER_PURPOSE
    }

    fn validate_account_details(&self) -> Result<(), String> {
        if self.server_destination() {
            if self.startup_secret().is_empty() {
                return Err("Enter the disk's unlock secret".into());
            }
            return validate_startup_secret_for(
                &self.startup_secret(),
                self.startup_secret_is_pin(),
            )
            .map_err(str::to_string);
        }
        if !valid_full_name(local_text(&self.full_name).trim()) {
            return Err(
                "Use at most 100 characters for the name, without colons or control characters"
                    .into(),
            );
        }
        validate_passwords(
            &secret_copy(&self.password),
            &secret_copy(&self.password_confirmation),
        )?;
        let separate = secret_copy(&self.disk_password);
        if !separate.is_empty() && separate.as_str() == secret_copy(&self.password).as_str() {
            return Err("Choose a disk secret different from the account password".into());
        }
        validate_startup_secret_for(&self.startup_secret(), self.startup_secret_is_pin())
            .map_err(str::to_string)
    }

    fn validate_current_step_details(&self) -> Result<(), String> {
        if self.step == Step::Account {
            self.validate_account_details()?;
        }
        if self.step == Step::Options {
            if !self.filesystem_policy_ready() {
                return Err(self.filesystem_policy_message());
            }
            if !valid_locale(self.locale().trim()) {
                return Err("Choose a supported language and locale from the list".into());
            }
            let timezone = self.timezone();
            if !valid_timezone_name(timezone.trim())
                || !self.timezones.iter().any(|zone| zone == timezone.trim())
            {
                return Err("Choose a valid time zone from the list".into());
            }
        }
        Ok(())
    }

    fn next_setup_step(&self) -> Option<Step> {
        // A server supplies no account, so a profile that names one does not
        // let the installer skip the step that collects its disk secret.
        let account_ready = !self.server_destination()
            && self.step == Step::Disk
            && self
                .profile_completion
                .is_some_and(ProfileCompletion::identity_complete)
            && valid_username(&local_text(&self.username))
            && self.validate_account_details().is_ok();
        if self.step == Step::Account || account_ready {
            if self
                .profile_completion
                .is_some_and(ProfileCompletion::options_complete)
                && self.filesystem_policy_ready()
            {
                Some(Step::Media)
            } else {
                Some(Step::Options)
            }
        } else {
            next_step(self.step)
        }
    }

    fn go_next(&mut self) {
        if self.step == Step::Account
            && self
                .profile_completion
                .is_some_and(ProfileCompletion::options_complete)
            && !self.filesystem_policy_ready()
        {
            self.set_status(&self.filesystem_policy_message());
            return;
        }
        if self.step == Step::Disk {
            if self.selected_disk_ref().is_some_and(Disk::blocks_install) {
                self.set_status("The running installer disk cannot be used as its own destination");
                return;
            }
            if self.selected_disk_ref().is_some_and(|disk| disk.read_only) {
                self.set_status("This disk is read-only and cannot be used for installation");
                return;
            }
            if self.selected_disk_ref().is_some_and(Disk::too_small) {
                self.set_status(&crate::setup::destination::disk_row::too_small());
                return;
            }
            if self
                .selected_disk_ref()
                .is_some_and(|disk| !disk.identity_ready())
            {
                self.set_status(
                    "Could not establish a stable identity for this disk; reconnect it and refresh",
                );
                return;
            }
        }
        if self.step == Step::Disk
            && self.install_method == InstallMethod::Alongside
            && !self.selected_disk_can_install_alongside()
        {
            self.set_status(&self.alongside_unavailable_message(&self.guided_alongside_plans()));
            return;
        }
        if self.step == Step::Disk
            && self.install_method == InstallMethod::Alongside
            && self
                .selected_disk_ref()
                .is_some_and(Disk::can_install_alongside)
            && !self.storage_preflight_passed
        {
            self.start_storage_preflight();
            return;
        }
        match validate_step(
            self.step,
            self.selected_disk,
            &local_text(&self.username),
            &local_text(&self.hostname),
            &self.purpose,
        ) {
            Ok(()) => {
                if let Err(message) = self.validate_current_step_details() {
                    self.set_status(&message);
                    return;
                }
                if let Some(completion) = self.profile_completion.as_mut() {
                    match self.step {
                        Step::Keyboard => completion.keyboard = FieldCompletion::Complete,
                        Step::Account => {
                            completion.username = FieldCompletion::Complete;
                            completion.full_name = FieldCompletion::Complete;
                        }
                        Step::Options => {
                            completion.hostname = FieldCompletion::Complete;
                            completion.locale = FieldCompletion::Complete;
                            completion.timezone = FieldCompletion::Complete;
                            completion.filesystem = FieldCompletion::Complete;
                        }
                        Step::Network | Step::Disk | Step::Media | Step::Review | Step::Install => {
                        }
                    }
                }
                if let Some(n) = self.next_setup_step() {
                    if n != Step::Install {
                        self.step = n;
                        self.reset_viewport();
                        self.status.clear();
                        self.dirty_structure = true;
                        self.focus_reset = true;
                    }
                }
            }
            Err(msg) => self.set_status(&msg),
        }
    }

    fn start_storage_preflight(&mut self) {
        if self.storage_preflight_request.is_some() {
            self.set_status("Checking the prepared EFI partition…");
            return;
        }
        let Some(mailbox) = self.mailbox.clone() else {
            self.set_status(
                "The prepared-storage check is unavailable. Refresh disks, then try again.",
            );
            return;
        };
        let Some(disk) = self.selected_dev() else {
            self.set_status("Select the prepared Tilo disk before checking it.");
            return;
        };
        let opts = self.install_options();
        let request = uuid::Uuid::new_v4().to_string();
        self.storage_preflight_request = Some(request.clone());
        self.set_status("Checking FAT health and free space on the prepared EFI partition…");
        let backend = Arc::clone(&self.backend);
        std::thread::spawn(move || {
            let mut detail = String::new();
            let result = backend.run_alongside_preflight(&disk, &opts, &mut |line| {
                if !line.trim().is_empty() {
                    detail = line;
                }
            });
            let (prefix, message) = match result {
                Ok(true) => (PREFLIGHT_OK_PREFIX, detail),
                Ok(false) => (PREFLIGHT_FAIL_PREFIX, detail),
                Err(error) => (
                    PREFLIGHT_FAIL_PREFIX,
                    CATALOG.t_args(
                        "imager-installer-preflight-failed",
                        &[(
                            "detail",
                            tilo_ui_base::i18n::ArgValue::Text(error.to_string()),
                        )],
                    ),
                ),
            };
            mailbox.post(format!("{prefix}{request}:{message}"));
        });
    }

    fn go_back(&mut self) {
        if self.step == Step::Install {
            if self.install_state == InstallState::Failed {
                self.step = Step::Disk;
                self.selected_disk = None;
                self.selected_guided_extent = None;
                self.alongside_capacity = None;
                self.clear_erase_confirmation();
                self.guided_efi_partuuid = uuid::Uuid::new_v4().to_string();
                self.guided_boot_partuuid = uuid::Uuid::new_v4().to_string();
                self.guided_root_partuuid = uuid::Uuid::new_v4().to_string();
                self.reset_viewport();
                self.install_state = InstallState::Idle;
                self.install_launched = false;
                self.status_lines.clear();
                self.set_status(
                    "Refresh storage before continuing. Tilo will not reinitialize a target from unauthenticated on-disk metadata; use Ctrl+Alt+F2 if you need recovery tools.",
                );
                self.dirty_structure = true;
                self.focus_reset = true;
            }
            return;
        }
        let destination = prev_step(self.step);
        if let Some(p) = destination {
            self.step = p;
            self.reset_viewport();
            self.status.clear();
            self.dirty_structure = true;
            self.focus_reset = true;
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "installer preflight and worker launch are one ordered operation"
    )]
    fn begin_install(&mut self) {
        if self.install_launched {
            return;
        }
        if !self.filesystem_policy_ready() {
            self.set_status(&self.filesystem_policy_message());
            return;
        }
        if self.hardware_blocks_install() {
            self.set_status(&crate::setup::strings::t(
                "installer-hardware-blocks-install",
            ));
            return;
        }
        let Some(disk) = self.selected_dev() else {
            self.set_status("Select a disk to continue");
            return;
        };
        if let Some(percent) = (self.power_probe)() {
            self.set_status(&connect_power_message(percent));
            return;
        }
        if !self.destructive_confirmation_ready() {
            let expected = self
                .destructive_confirmation_target()
                .unwrap_or_else(|| disk.clone());
            self.set_status(&format!("Type {expected} exactly to confirm formatting it"));
            return;
        }
        // Encryption requires the recovery-key acknowledgement (defense in
        // depth — the Begin button and Enter both check on this too).
        if !self.recovery_ack {
            self.set_status("Confirm you'll save your recovery key first");
            return;
        }
        if let Err(message) =
            validate_startup_secret_for(&self.startup_secret(), self.startup_secret_is_pin())
        {
            self.set_status(message);
            return;
        }
        // A Tilo Server has no account password: the one secret is the disk's,
        // and it travels as the account line so the backend writes no user.
        let server = self.server_destination();
        let user = if server {
            String::new()
        } else {
            local_text(&self.username)
        };
        let password = if server {
            self.startup_secret()
        } else {
            secret_copy(&self.password)
        };
        let confirmation = if server {
            password.clone()
        } else {
            secret_copy(&self.password_confirmation)
        };
        if let Err(message) = validate_passwords(&password, &confirmation) {
            self.set_status(&message);
            return;
        }
        if !server && secret_copy(&self.disk_password).as_str() == password.as_str() {
            self.set_status("Choose a disk secret different from the account password");
            return;
        }
        let opts = self.install_options();

        clear_secret_buffer(&self.password);
        clear_secret_buffer(&self.password_confirmation);
        clear_secret_buffer(&self.disk_password);

        self.step = Step::Install;
        self.reset_viewport();
        self.install_state = InstallState::Running;
        self.install_launched = true;
        self.status_lines = vec![if server {
            crate::setup::strings::t_args(
                "installer-status-install-start-server",
                &[
                    ("disk", ArgValue::Text(disk.clone())),
                    ("filesystem", ArgValue::Text(opts.filesystem.clone())),
                ],
            )
        } else {
            crate::setup::strings::t_args(
                "installer-status-install-start",
                &[
                    ("disk", ArgValue::Text(disk.clone())),
                    ("user", ArgValue::Text(user.clone())),
                    ("filesystem", ArgValue::Text(opts.filesystem.clone())),
                ],
            )
        }];
        self.recovery_key = None;
        self.recovery_key_copied = false;
        self.recovery_key_copy = tilo_ui_base::CopyWatch::default();
        self.recovery_key_saved = false;
        self.dirty_structure = true;
        self.focus_reset = true;

        let backend = Arc::clone(&self.backend);
        if let Some(mailbox) = self.mailbox.clone() {
            std::thread::spawn(move || {
                let result = backend.run_install(&disk, &user, &password, &opts, &mut |line| {
                    mailbox.post(line);
                });
                let terminal = match result {
                    Ok(true) => CTRL_DONE.to_string(),
                    Ok(false) => CTRL_FAIL.to_string(),
                    Err(e) => {
                        mailbox.post(format!("error: failed to launch installer: {e}"));
                        CTRL_FAIL.to_string()
                    }
                };
                mailbox.post(terminal);
            });
        } else {
            let unattended_progress = self.unattended_mode;
            let mut lines = Vec::new();
            let result = backend.run_install(&disk, &user, &password, &opts, &mut |line| {
                if unattended_progress && let Some(line) = redact_recovery_material(&line) {
                    eprintln!("{line}");
                }
                lines.push(line);
            });
            for line in lines {
                self.ingest_backend_line(&line);
            }
            match result {
                Ok(true) => self.mark_install_done(),
                Ok(false) => self.mark_install_failed(None),
                Err(e) => self
                    .mark_install_failed(Some(format!("error: failed to launch installer: {e}"))),
            }
        }
    }

    fn mark_install_done(&mut self) {
        self.install_state = InstallState::Done;
        self.reset_viewport();
        self.status_lines.push(
            if self.internal_host().is_some() {
                INSTALLED_RESTART
            } else {
                INSTALLED_REMOVE_MEDIUM
            }
            .to_string(),
        );
        self.focus_reset = true;
        self.dirty_structure = true;
    }

    fn mark_install_failed(&mut self, extra: Option<String>) {
        if let Some(line) = extra {
            self.status_lines.push(line);
        }
        self.install_state = InstallState::Failed;
        self.reset_viewport();
        self.focus_reset = true;
        self.dirty_structure = true;
    }

    fn run_power(&mut self, action: PowerAction) -> bool {
        // Do not deliberately leave a disk-unlock factor in the live
        // session's clipboard after handing off to the installed system. If
        // the person copied something else since, preserve their newer value.
        if let Some(key) = &self.recovery_key
            && tilo_ui_base::clipboard_paste().as_deref() == Some(key.as_str())
        {
            tilo_ui_base::clipboard_copy("");
        }
        if matches!(self.backend.run_power(action), Ok(true)) {
            true
        } else {
            self.set_status(match action {
                PowerAction::Reboot => {
                    "This computer did not restart. Remove the installer medium and restart it by hand."
                }
                PowerAction::PowerOff => {
                    "This computer did not power off. Remove the installer medium and switch it off by hand."
                }
            });
            false
        }
    }

    fn control(&self, effect: DestinationEffect) -> Control {
        self.presses.control(Press::new(effect))
    }

    fn next_control(&self) -> Control {
        self.control(DestinationEffect::Next)
    }

    fn back_control(&self) -> Control {
        self.control(DestinationEffect::Back)
    }

    fn nav_row(
        &self,
        step: Step,
        forward: Option<(Control, &str)>,
        forward_disabled: bool,
    ) -> Box<dyn Widget> {
        let mut row = Row::new().no_wrap();
        if prev_step(step).is_some() {
            row = row.push(Box::new(
                ActionButton::new("installer-preview-back", self.back_control())
                    .with_icon(tilo_ui_base::icons::Icon::ChevronLeft)
                    .with_test_id("installer.back")
                    .large(),
            ));
        }
        row = row.push(Box::new(Spacer::new()));
        if let Some((forward, forward_key)) = forward {
            row = row.push(Box::new(
                ActionButton::new(forward_key, forward)
                    .with_test_id("installer.next")
                    .primary()
                    .large()
                    .disabled(forward_disabled),
            ));
        }
        Box::new(row)
    }

    fn finish_row(&self, disabled: bool) -> Box<dyn Widget> {
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    ActionButton::new(
                        "installer-action-power-off",
                        self.control(DestinationEffect::PowerOff),
                    )
                    .large()
                    .disabled(disabled),
                ))
                .push(Box::new(
                    ActionButton::new(
                        "installer-action-reboot",
                        self.control(DestinationEffect::Reboot),
                    )
                    .primary()
                    .large()
                    .disabled(disabled),
                )),
        )
    }

    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut children: Vec<Box<dyn Widget>> = Vec::new();
        let mut actions: Vec<Box<dyn Widget>> = Vec::new();
        match self.step {
            Step::Network => self.compose_network_step(theme, &mut children, &mut actions),
            Step::Disk => self.compose_disk_step(theme, &mut children, &mut actions),
            Step::Account => self.compose_account_step(theme, &mut children, &mut actions),
            Step::Keyboard => self.compose_keyboard_step(theme, &mut children, &mut actions),
            Step::Options => self.compose_options_step(theme, &mut children, &mut actions),
            Step::Media => self.compose_media_step(theme, &mut children, &mut actions),
            Step::Review => self.compose_review_step(theme, &mut children, &mut actions),
            Step::Install => self.compose_install_step(theme, &mut children, &mut actions),
        }

        if let Some(host) = self.internal_host()
            && !matches!(
                self.install_state,
                InstallState::Running | InstallState::Done
            )
        {
            actions.push(Box::new(
                ActionButton::new(
                    "installer-action-return-to-host",
                    self.control(DestinationEffect::ReturnToHost),
                )
                .with_args(&[("host", ArgValue::Text(host.display_name().to_owned()))])
                .with_test_id("return_to_host"),
            ));
        }

        if !self.status.is_empty() {
            actions.push(Box::new(
                Text::new(&self.status)
                    .with_id("installer.validation")
                    .with_role(TextRole::Caption)
                    .with_color(theme.tokens.colors.semantic.danger)
                    .with_live(Politeness::Assertive),
            ));
        }

        let mut body = crate::page::stack()
            .with_id("installer.content")
            .with_rhythm(Rhythm::Related);
        for child in children {
            body = body.push(child);
        }
        let stepper = Stepper::new(step_captions())
            .with_id("installer.stepper")
            .current(Some(step_index(self.step)));
        let mut frame = crate::page::Frame::new(body)
            .stepper_widget(stepper)
            .offset(Rc::clone(&self.viewport_offset));
        if !actions.is_empty() {
            let mut footer = crate::page::stack()
                .with_id("installer.actions")
                .with_rhythm(Rhythm::Related);
            for action in actions {
                footer = footer.push(action);
            }
            frame = frame.actions(footer);
        }
        frame.build()
    }

    fn compose_network_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-network-connect"))
                .with_role(TextRole::Title),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-network-optional"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            Text::new(&self.network.notice)
                .with_role(TextRole::Body)
                .with_color(theme.text_primary),
        ));

        if self.network.is_busy() {
            let operation = match &self.network.phase {
                NetworkPhase::Connecting(ssid) => CATALOG.t_args(
                    "imager-installer-connecting",
                    &[("network", tilo_ui_base::i18n::ArgValue::Text(ssid.clone()))],
                ),
                _ => CATALOG.t("imager-installer-checking-networks"),
            };
            children.push(Box::new(
                Row::new().push(Box::new(Spinner::new())).push(Box::new(
                    Text::new(&operation)
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                )),
            ));
        }

        self.compose_network_choices(theme, children);

        if let Some(selected) = self.network.selected_network()
            && !selected.connected
        {
            if selected.secure {
                children.push(Box::new(
                    Text::new(&crate::setup::strings::t("installer-wifi-password"))
                        .with_role(TextRole::Body),
                ));
                children.push(Box::new(
                    TextInput::new(Vec::new())
                        .with_id("installer.wifi_password")
                        .with_external_buffer(Arc::clone(&self.wifi_password))
                        .with_secret(true),
                ));
            }
            children.push(Box::new(
                ActionButton::new(
                    "installer-accounts-connect",
                    self.control(DestinationEffect::ConnectNetwork { password: None }),
                )
                .primary()
                .disabled(self.network.is_busy()),
            ));
        }

        if let Some(uri) = self.network.portal_uri.as_deref() {
            children.push(Box::new(
                Text::new(&CATALOG.t_args(
                    "installer-network-signin-address",
                    &[("uri", ArgValue::Text(uri.to_owned()))],
                ))
                .with_role(TextRole::Caption)
                .with_color(theme.text_primary),
            ));
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-network-privileged"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        } else if let Some(error) = self.network.portal_error.as_deref() {
            children.push(Box::new(
                Text::new(&CATALOG.t_args(
                    "installer-network-signin-unavailable",
                    &[("error", ArgValue::Text(error.to_owned()))],
                ))
                .with_role(TextRole::Caption)
                .with_color(dim),
            ));
        }

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-wifi-saved"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            ActionButton::new(
                "installer-action-refresh-networks",
                self.control(DestinationEffect::RefreshNetworks),
            )
            .disabled(self.network.is_busy()),
        ));
        let continue_key = if self.network.connectivity == Connectivity::Full {
            "imager-installer-continue"
        } else {
            "imager-installer-continue-offline"
        };
        actions.push(self.nav_row(
            Step::Network,
            Some((self.next_control(), continue_key)),
            false,
        ));
    }

    fn compose_network_choices(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;

        if let Some(error) = self.network.wifi_error.as_deref() {
            children.push(Box::new(
                Text::new(&CATALOG.t_args(
                    "installer-wifi-unavailable",
                    &[("error", ArgValue::Text(error.to_owned()))],
                ))
                .with_role(TextRole::Caption)
                .with_color(dim),
            ));
        } else {
            for (i, network) in self.network.networks.iter().take(5).enumerate() {
                let state = CATALOG.t(if network.connected {
                    "imager-network-connected"
                } else if network.secure {
                    "imager-network-secured"
                } else {
                    "imager-network-open"
                });
                let selected = self.network.selected_ssid.as_deref() == Some(network.name.as_str());
                let control = self.control(DestinationEffect::SelectNetwork {
                    ssid: network.name.clone(),
                });
                let fire = control.clone();
                let id = format!("wifi_network_{i}");
                let row = ListRow::new(id.as_str(), network.name.as_str())
                    .icon(if network.secure {
                        tilo_ui_base::icons::Icon::Lock
                    } else {
                        tilo_ui_base::icons::Icon::Wifi
                    })
                    .secondary(state)
                    .signal(ui::widget::SignalLevel::from_bars(network.signal))
                    .selected(selected)
                    .checked(selected)
                    .with_enabled(!self.network.is_busy())
                    .on_activate(move || fire.fire());
                children.push(Box::new(
                    Bound::new(Box::new(row), [(id, &control)]).activated_by_keys(&control),
                ));
            }
        }
    }

    fn compose_disk_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let danger = theme.tokens.colors.semantic.danger;

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-install-tilo"))
                .with_role(TextRole::Title),
        ));
        self.compose_install_method(theme, children);
        children.push(Box::new(
            ActionButton::new(
                "installer-action-refresh-disks",
                self.control(DestinationEffect::RefreshDisks),
            )
            .with_test_id("disk_refresh"),
        ));
        if self.disks.is_empty() {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-storage-no-drives"))
                    .with_role(TextRole::Body)
                    .with_color(danger),
            ));
        } else {
            let rows = self
                .disks
                .iter()
                .enumerate()
                .map(|(i, disk)| {
                    Box::new(disk_row(
                        disk,
                        &self.control(DestinationEffect::SelectDisk {
                            device: disk.name.clone(),
                        }),
                        self.selected_disk == Some(i),
                    )) as Box<dyn Widget>
                })
                .collect();
            children.push(Box::new(ui::widget::Section::new(rows)));
        }
        children.extend(self.windows_alongside_preparation(theme));
        if self.install_method == InstallMethod::Alongside
            && self
                .selected_disk_ref()
                .is_some_and(Disk::has_linux_storage)
            && !self.guided_alongside_plans().is_empty()
        {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t(LINUX_ALONGSIDE_READY))
                    .with_role(TextRole::Caption)
                    .with_color(theme.text_dim),
            ));
        }
        let read_only = self.selected_disk_ref().is_some_and(|disk| disk.read_only);
        let guided_plans = self.guided_alongside_plans();
        if self.install_method == InstallMethod::Alongside && guided_plans.len() > 1 {
            self.compose_guided_extent_choices(theme, children, &guided_plans);
        }
        self.compose_alongside_capacity(children);
        let unavailable = read_only
            || (self.install_method == InstallMethod::Alongside
                && !self.selected_disk_can_install_alongside());
        if read_only {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-disk-protected"))
                    .with_role(TextRole::Caption)
                    .with_color(danger),
            ));
        }
        if unavailable
            && self.selected_disk.is_some()
            && !read_only
            && self.install_method == InstallMethod::Alongside
        {
            let message = self.alongside_unavailable_message(&guided_plans);
            children.push(Box::new(
                Text::new(&message)
                    .with_role(TextRole::Caption)
                    .with_color(danger),
            ));
        } else if self.install_method == InstallMethod::Alongside
            && self.uses_guided_alongside()
            && let Some(plan) = self.guided_alongside_plan()
        {
            children.push(Box::new(
                Text::new(&self.alongside_allocation_summary(&plan))
                    .with_role(TextRole::Caption)
                    .with_color(theme.status_success),
            ));
        }
        actions.push(self.nav_row(
            Step::Disk,
            Some((self.next_control(), NEXT_KEY)),
            self.selected_disk.is_none() || unavailable,
        ));
    }

    fn compose_install_method(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
        if let Some(host) = self
            .internal_host()
            .filter(|_| self.selected_disk_hosts_installer())
        {
            children.push(Box::new(
                Text::new(&CATALOG.t_args(
                    "installer-install-alongside",
                    &[("host", ArgValue::Text(host.display_name().to_owned()))],
                ))
                .with_role(TextRole::Caption)
                .with_color(dim),
            ));
        } else {
            children.push(Box::new(seg_choice(
                captions(&INSTALL_METHOD_KEYS),
                ["erase", "alongside"]
                    .iter()
                    .map(|method| {
                        self.control(DestinationEffect::SetMethod {
                            method: (*method).to_owned(),
                        })
                    })
                    .collect(),
                usize::from(self.install_method == InstallMethod::Alongside),
                "install_method",
            )));
        }
        let guidance = storage_text(if self.install_method == InstallMethod::Erase {
            "installer-storage-erase-warning"
        } else {
            "installer-storage-keep-guidance"
        });
        children.push(Box::new(
            Text::new(&guidance)
                .with_role(TextRole::Caption)
                .with_color(if self.install_method == InstallMethod::Erase {
                    danger
                } else {
                    dim
                }),
        ));
    }

    fn windows_alongside_preparation(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        (self.install_method == InstallMethod::Alongside
            && self
                .selected_disk_ref()
                .is_some_and(Disk::has_windows_storage))
        .then(|| {
            Box::new(
                Text::new(&crate::setup::strings::t(WINDOWS_ALONGSIDE_PREPARATION))
                    .with_role(TextRole::Caption)
                    .with_color(theme.tokens.colors.semantic.warning),
            ) as Box<dyn Widget>
        })
    }

    fn compose_alongside_capacity(&self, children: &mut Vec<Box<dyn Widget>>) {
        let Some(full) = self.selected_full_guided_plan() else {
            return;
        };
        if self
            .selected_disk_ref()
            .is_some_and(Disk::hosts_staged_installer)
        {
            return;
        }
        let Some(plan) = self.guided_alongside_plan() else {
            return;
        };
        if self.capacity_effect().is_none() {
            return;
        }
        let gib = tilo_ui_base::num::f64_of_u64(plan.free_size_bytes / (1 << 30));
        self.capacity_gib.set(gib);
        self.capacity_seen.set(gib);
        children.push(Box::new(Text::new(&crate::setup::strings::t(
            "installer-storage-use-capacity",
        ))));
        children.push(Box::new(
            ui::widget::NumberInput::from_state(&self.capacity_gib)
                .with_range(
                    tilo_ui_base::num::f64_of_u64(imager_backend::MIN_TARGET_BYTES >> 30),
                    tilo_ui_base::num::f64_of_u64(full.free_size_bytes / (1 << 30)),
                )
                .with_step(1.0)
                .with_suffix("GiB")
                .with_id("installer.alongside-capacity")
                .with_accessible_name(crate::setup::strings::t("installer-storage-use-capacity")),
        ));
    }

    fn alongside_allocation_summary(&self, plan: &GuidedAlongsidePlan) -> String {
        let remaining = self.selected_full_guided_plan().map_or(0, |full| {
            full.free_size_bytes.saturating_sub(plan.free_size_bytes)
        });
        crate::setup::strings::t_args(
            "installer-storage-allocation-summary",
            &[
                (
                    "capacity",
                    tilo_ui_base::format::memory(plan.free_size_bytes).into(),
                ),
                ("remaining", tilo_ui_base::format::memory(remaining).into()),
                ("details", Self::alongside_capacity_summary(plan).into()),
            ],
        )
    }

    fn alongside_capacity_summary(plan: &GuidedAlongsidePlan) -> String {
        crate::setup::strings::t_args(
            "installer-storage-alongside-capacity",
            &[
                (
                    "root",
                    tilo_ui_base::format::memory(plan.root_size_bytes).into(),
                ),
                (
                    "total",
                    tilo_ui_base::format::memory(plan.free_size_bytes).into(),
                ),
            ],
        )
    }

    fn alongside_storage_guidance(&self) -> String {
        let plans = self.guided_alongside_plans();
        let mut message = if self.selected_disk_ref().is_none() {
            CATALOG.t("imager-installer-select-disk")
        } else if !self.selected_disk_can_install_alongside() {
            self.alongside_unavailable_message(&plans)
        } else if self.uses_guided_alongside() {
            self.guided_alongside_plan()
                .as_ref()
                .map(|plan| self.alongside_allocation_summary(plan))
                .unwrap_or_default()
        } else {
            CATALOG.t("imager-installer-format-prepared")
        };
        if self
            .selected_disk_ref()
            .is_some_and(Disk::has_linux_storage)
            && !plans.is_empty()
        {
            message.push(' ');
            message.push_str(&crate::setup::strings::t(LINUX_ALONGSIDE_READY));
        }
        if self
            .selected_disk_ref()
            .is_some_and(Disk::has_windows_storage)
        {
            message.push(' ');
            message.push_str(&crate::setup::strings::t(WINDOWS_ALONGSIDE_PREPARATION));
        }
        message
    }

    fn alongside_unavailable_message(&self, guided_plans: &[GuidedAlongsidePlan]) -> String {
        if guided_plans.len() > 1 {
            return "Select one of the unallocated regions above to continue.".to_string();
        }
        if let Some(issue) = self
            .selected_disk_ref()
            .filter(|disk| disk.alongside_root().is_some())
            .and_then(Disk::prepared_efi_issue)
        {
            return format!(
                "{issue} The privileged install check will also verify FAT health and at least 1 GiB of free space before formatting anything."
            );
        }
        if self
            .selected_disk_ref()
            .is_some_and(Disk::has_linux_storage)
        {
            return crate::setup::strings::t_args(
                "installer-storage-linux-no-space",
                &[(
                    "preparation",
                    storage_text(LINUX_ALONGSIDE_PREPARATION).into(),
                )],
            );
        }
        format!(
            "No safe alongside space was found. Back up your files and use the existing OS's supported partitioning tools to leave at least {} contiguous unallocated, or prepare an empty Linux root and {} boot plus {} EFI partition, then refresh disks. Tilo will not resize existing partitions or switch to Erase automatically.",
            tilo_ui_base::format::memory(imager_backend::MIN_TARGET_BYTES),
            tilo_ui_base::format::memory(imager_backend::BOOT_BYTES),
            tilo_ui_base::format::memory(imager_backend::MIN_ALONGSIDE_ESP_BYTES),
        )
    }

    fn compose_guided_extent_choices(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        plans: &[GuidedAlongsidePlan],
    ) {
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-region-choose"))
                .with_role(TextRole::Body),
        ));
        for (index, plan) in plans.iter().enumerate() {
            let start = tilo_ui_base::format::memory(plan.free_start_bytes);
            let size = tilo_ui_base::format::memory(plan.free_size_bytes);
            let key = if self.selected_guided_extent == Some(index) {
                "installer-region-selected"
            } else {
                "installer-region-use"
            };
            children.push(Box::new(
                ActionButton::new(
                    key,
                    self.control(DestinationEffect::SelectRegion {
                        index: i64::try_from(index).unwrap_or(i64::MAX),
                    }),
                )
                .with_args(&[
                    ("size", ArgValue::Text(size.clone())),
                    ("start", ArgValue::Text(start.clone())),
                ])
                .with_test_id(&format!("guided_extent_{index}")),
            ));
        }
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-region-measured"))
                .with_role(TextRole::Caption)
                .with_color(theme.text_dim),
        ));
    }

    fn compose_account_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;

        let completion = self.profile_completion.unwrap_or_default();
        let identity_complete = self
            .profile_completion
            .is_some_and(ProfileCompletion::identity_complete);
        let server = self.server_destination();
        children.push(Box::new(
            Text::new(&if server {
                crate::setup::strings::t("installer-account-server-title")
            } else if identity_complete {
                crate::setup::strings::t("installer-account-secure")
            } else {
                crate::setup::strings::t("installer-account-create")
            })
            .with_role(TextRole::Title),
        ));
        if server {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-account-server-note"))
                    .with_role(TextRole::Body)
                    .with_color(dim),
            ));
        }
        if !server && (self.profile_completion.is_none() || !completion.full_name.is_complete()) {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-account-name"))
                    .with_role(TextRole::Body),
            ));
            children.push(Box::new(
                TextInput::from_state(&self.full_name)
                    .with_id("installer.full_name")
                    .with_accessible_name(crate::setup::strings::t("installer-account-name")),
            ));
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-account-name-note"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }
        if !server && (self.profile_completion.is_none() || !completion.username.is_complete()) {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-account-username"))
                    .with_role(TextRole::Body),
            ));
            children.push(Box::new(
                TextInput::from_state(&self.username)
                    .with_id("installer.username")
                    .with_accessible_name(crate::setup::strings::t("installer-account-username")),
            ));
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-account-username-note"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }
        let tpm_pin = self.startup_secret_is_pin();
        // A server's one secret unlocks its disk; it is not a login password,
        // so the labels say what it is for.
        let (secret, secret_note, secret_confirm) = if server {
            (
                "installer-account-server-secret",
                "installer-account-server-secret-note",
                "installer-account-server-secret-confirm",
            )
        } else if tpm_pin {
            (
                "installer-account-pin",
                "installer-account-pin-note",
                "installer-account-pin-confirm",
            )
        } else {
            (
                "installer-account-password",
                "installer-account-password-note",
                "installer-account-password-confirm",
            )
        };
        children.push(Box::new(
            Text::new(&crate::setup::strings::t(secret)).with_role(TextRole::Body),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password")
                .with_accessible_name(crate::setup::strings::t(secret))
                .with_external_buffer(Arc::clone(&self.password))
                .with_secret(true),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t(secret_note))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t(secret_confirm)).with_role(TextRole::Body),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password_confirm")
                .with_accessible_name(crate::setup::strings::t(secret_confirm))
                .with_external_buffer(Arc::clone(&self.password_confirmation))
                .with_secret(true),
        ));
        actions.push(self.nav_row(Step::Account, Some((self.next_control(), NEXT_KEY)), false));
    }

    fn compose_keyboard_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-welcome-title"))
                .with_role(TextRole::Title),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-welcome-note"))
                .with_role(TextRole::Body),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-keyboard-layout"))
                .with_role(TextRole::Body),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-keyboard-note"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            Dropdown::from_state(tilo_keyboard_map::display_names(), &self.keymap)
                .with_id("installer.keymap")
                .with_placeholder(&crate::setup::strings::t("installer-keyboard-layout")),
        ));
        children.push(Box::new(KeyboardView::new(&self.keymap)));
        actions.push(self.nav_row(Step::Keyboard, Some((self.next_control(), NEXT_KEY)), false));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-recovery-tools"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
    }

    fn compose_options_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;
        let completion = self.profile_completion.unwrap_or_default();

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-options-heading"))
                .with_role(TextRole::Title),
        ));

        if self.profile_completion.is_none() || !completion.filesystem.is_complete() {
            self.compose_filesystem_choice(theme, children);
        }

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-disk-encryption"))
                .with_role(TextRole::Body),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-encryption-always"))
                .with_role(TextRole::Body)
                .with_color(theme.status_success),
        ));
        children.push(Box::new(
            Text::new(&encrypt_description(self.verified_boot_state()))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-security-key"))
                .with_role(TextRole::Body),
        ));
        children.push(Box::new(seg_choice(
            captions(&FIDO2_KEYS),
            (0..FIDO2_KEYS.len())
                .map(|i| self.control(DestinationEffect::SetSecurityKey { enabled: i == 1 }))
                .collect(),
            usize::from(self.fido2),
            "fido2_choice",
        )));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-security-key-option"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));

        if self.profile_completion.is_none() || !completion.hostname.is_complete() {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-hostname"))
                    .with_role(TextRole::Body),
            ));
            children.push(Box::new(
                TextInput::from_state(&self.hostname).with_id("installer.hostname"),
            ));
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-hostname-note"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }

        if self.profile_completion.is_none() || !completion.locale.is_complete() {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-language-locale"))
                    .with_role(TextRole::Body),
            ));
            children.push(Box::new(
                Dropdown::from_state(
                    install_profile_model::locales()
                        .map(str::to_owned)
                        .collect(),
                    &self.locale,
                )
                .with_id("installer.locale")
                .with_accessible_name(crate::setup::strings::t("installer-a11y-language-locale"))
                .with_placeholder(&crate::setup::strings::t("installer-language-locale")),
            ));
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-language-note"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }

        if self.profile_completion.is_none() || !completion.timezone.is_complete() {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-time-zone"))
                    .with_role(TextRole::Body),
            ));
            children.push(Box::new(
                Dropdown::from_state(self.timezones.clone(), &self.timezone)
                    .with_id("installer.timezone")
                    .with_placeholder(&crate::setup::strings::t("installer-time-zone"))
                    .open_if(self.tz_open_for_shot),
            ));
            children.push(Box::new(
                Text::new(&crate::setup::strings::t("installer-time-zone-pick"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }

        actions.push(self.nav_row(
            Step::Options,
            Some((self.next_control(), NEXT_KEY)),
            !self.filesystem_policy_ready(),
        ));
    }

    fn compose_media_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;
        children.push(Box::new(Spacer::fixed(theme.tokens.spacing.s6)));
        children.push(centered_media_control(Box::new(
            ui::widget::IconWidget::new(tilo_ui_base::icons::Icon::Film)
                .sized(ui::widget::IconSize::Large)
                .accent(),
        )));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-media-title"))
                .with_role(TextRole::LargeTitle)
                .with_align(tilo_ui_base::ParagraphAlign::Center),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-media-note"))
                .with_role(TextRole::Caption)
                .with_color(dim)
                .with_align(tilo_ui_base::ParagraphAlign::Center),
        ));
        let children = actions;
        if self.codecs {
            children.push(centered_media_control(Box::new(
                Row::new()
                    .with_padding(0.0, 0.0)
                    .no_wrap()
                    .with_gap(theme.tokens.spacing.s2)
                    .push(Box::new(
                        ui::widget::IconWidget::new(tilo_ui_base::icons::Icon::Check)
                            .with_color(theme.status_success),
                    ))
                    .push(Box::new(
                        Text::new(&crate::setup::strings::t("installer-media-chosen"))
                            .with_role(TextRole::Body)
                            .with_color(theme.status_success)
                            .with_id("codecs_chosen"),
                    )),
            )));
            children.push(centered_media_control(Box::new(
                ActionButton::new(
                    "installer-media-undo",
                    self.control(DestinationEffect::SetMediaCodecs { enabled: false }),
                )
                .quiet()
                .with_test_id("codecs_undo"),
            )));
            children.push(Box::new(Spacer::fixed(theme.tokens.spacing.s6)));
            children.push(self.nav_row(Step::Media, Some((self.next_control(), NEXT_KEY)), false));
        } else {
            children.push(centered_media_control(Box::new(
                ActionButton::new(
                    "installer-media-install",
                    self.control(DestinationEffect::SetMediaCodecs { enabled: true }),
                )
                .primary()
                .large()
                .with_test_id("codecs_install"),
            )));
            children.push(centered_media_control(Box::new(
                ActionButton::new("installer-media-skip", self.next_control())
                    .quiet()
                    .with_test_id("codecs_skip"),
            )));
            children.push(Box::new(Spacer::fixed(theme.tokens.spacing.s6)));
            children.push(self.nav_row(Step::Media, None, false));
        }
    }

    fn compose_filesystem_choice(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-filesystem")).with_role(TextRole::Body),
        ));
        let choices = self.available_filesystem_indices();
        if choices.is_empty() {
            children.push(Box::new(
                Text::new(&self.filesystem_policy_message())
                    .with_role(TextRole::Caption)
                    .with_color(theme.tokens.colors.semantic.danger),
            ));
            return;
        }
        let selected = choices
            .iter()
            .position(|index| *index == self.selected_fs)
            .unwrap_or(0);
        let labels: Vec<&str> = choices
            .iter()
            .filter_map(|index| FILESYSTEMS.get(*index).copied())
            .collect();
        children.push(Box::new(seg_choice(
            labels.iter().map(|name| (*name).to_owned()).collect(),
            labels
                .iter()
                .map(|filesystem| {
                    self.control(DestinationEffect::SetFilesystem {
                        filesystem: (*filesystem).to_owned(),
                    })
                })
                .collect(),
            selected,
            "filesystem_choice",
        )));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t(fs_description(self.selected_fs)))
                .with_role(TextRole::Caption)
                .with_color(theme.text_dim),
        ));
        if self.selected_fs == RECOMMENDED_FS {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t(
                    "installer-filesystem-recommended",
                ))
                .with_role(TextRole::Caption),
            ));
        }
    }

    fn hardware_blocks_install(&self) -> bool {
        self.hardware
            .report()
            .is_some_and(hardware_report::SupportReport::blocks_install)
    }

    fn compose_hardware_report(&self, children: &mut Vec<Box<dyn Widget>>) {
        children.push(crate::setup::destination::hardware_report::summary_row(
            &self.hardware,
        ));
        if let Some(report) = self.hardware.report() {
            children.extend(crate::setup::destination::hardware_report::component_rows(
                report,
            ));
        }
    }

    fn compose_destructive_confirmation(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        disk: &str,
    ) {
        let (expected, operation) = match self.install_method {
            InstallMethod::Erase => (disk, CATALOG.t("installer-operation-erase")),
            InstallMethod::Alongside => (
                if self.uses_guided_alongside() {
                    disk
                } else {
                    self.selected_disk_ref()
                        .and_then(|selected| selected.alongside_root())
                        .map_or("<missing root>", |partition| partition.name.as_str())
                },
                alongside_confirmation_operation(self.uses_guided_alongside()),
            ),
        };
        children.push(Box::new(
            Text::new(&CATALOG.t_args(
                "installer-type-to-confirm",
                &[
                    ("expected", ArgValue::Text(expected.to_owned())),
                    ("operation", ArgValue::Text(operation)),
                ],
            ))
            .with_role(TextRole::Caption)
            .with_color(theme.text_dim),
        ));
        children.push(Box::new(
            TextInput::from_state(&self.erase_confirmation)
                .with_id("installer.erase_confirmation")
                .with_placeholder(expected),
        ));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the review is one linear visual composition and splitting it would obscure its exact destructive-plan ordering"
    )]
    fn compose_review_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-review")).with_role(TextRole::Title),
        ));

        let dev = self
            .selected_dev()
            .unwrap_or_else(|| "<no disk>".to_string());
        let disk_value = match self.selected_disk_ref() {
            Some(d) if d.model.is_empty() => format!("{} ({})", d.name, capacity(d)),
            Some(d) => format!("{} ({}, {})", d.name, capacity(d), d.model),
            None => CATALOG.t("imager-installer-no-disk"),
        };
        let mut summary = Vec::new();
        summary.push(Property::new(
            CATALOG.t("installer-property-disk"),
            &disk_value,
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-method"),
            if self.install_method == InstallMethod::Alongside {
                "Alongside"
            } else {
                "Erase the entire disk"
            },
        ));
        if self.install_method == InstallMethod::Alongside
            && let Some(disk) = self.selected_disk_ref()
        {
            if let Some(plan) = self.guided_alongside_plan() {
                summary.push(Property::new(
                    CATALOG.t("installer-property-storage"),
                    self.alongside_allocation_summary(&plan),
                ));
                summary.push(Property::new(
                    CATALOG.t("installer-property-existing-partitions"),
                    "Preserved byte-for-byte; none resized, moved, or deleted",
                ));
                if let Some(host) = self
                    .internal_host()
                    .filter(|_| self.selected_disk_hosts_installer())
                {
                    let host = host.display_name();
                    summary.push(Property::new(
                        CATALOG.t("installer-property-this-disk"),
                        CATALOG.t_args(
                            "installer-disk-holds-installer",
                            &[("host", ArgValue::Text(host.to_owned()))],
                        ),
                    ));
                    summary.push(Property::new(
                        CATALOG.t("installer-property-installer-space"),
                        "Returned to Tilo automatically after you first sign in",
                    ));
                    summary.push(Property::new(
                        CATALOG.t("installer-property-choose-os"),
                        CATALOG.t_args(
                            "installer-choose-os-restart",
                            &[("host", ArgValue::Text(host.to_owned()))],
                        ),
                    ));
                } else {
                    summary.push(Property::new(CATALOG.t("installer-property-choose-os"),
                        "Use the firmware boot picker; its current default and existing entries are preserved",
                    ));
                }
            } else {
                summary.push(Property::new(
                    CATALOG.t("installer-property-tilo-root"),
                    disk.alongside_root()
                        .map_or("<missing>", |p| p.name.as_str()),
                ));
                summary.push(Property::new(
                    CATALOG.t("installer-property-tilo-efi"),
                    disk.efi_partition()
                        .map_or("<missing>", |p| p.name.as_str()),
                ));
                summary.push(Property::new(
                    CATALOG.t("installer-property-existing-partitions"),
                    "Preserved byte-for-byte; none resized, moved, or deleted",
                ));
                summary.push(Property::new(CATALOG.t("installer-property-choose-os"),
                    "Tilo adds safe menu shortcuts for unambiguous Windows/Linux loaders on this shared EFI partition",
                ));
            }
        }
        if self.install_method == InstallMethod::Alongside {
            summary.push(Property::new(
                CATALOG.t("installer-property-boot-choice"),
                crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE),
            ));
            summary.push(Property::new(
                CATALOG.t("installer-property-existing-files"),
                crate::setup::strings::t(ALONGSIDE_FILES_GUIDANCE),
            ));
        }
        summary.push(Property::new(
            CATALOG.t("installer-property-filesystem"),
            self.filesystem(),
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-encryption"),
            "LUKS",
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-unlocks-with"),
            self.encryption_factors().join(", "),
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-hostname"),
            local_text(&self.hostname).trim(),
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-timezone"),
            self.timezone().trim(),
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-keyboard"),
            self.keymap().trim(),
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-formats"),
            if self.codecs {
                "Install once the new system is online"
            } else {
                "Not now (available later in Settings, Updates)"
            },
        ));
        summary.push(Property::new(
            CATALOG.t("installer-property-username"),
            local_text(&self.username).trim(),
        ));
        let full_name = local_text(&self.full_name);
        if !full_name.trim().is_empty() {
            summary.push(Property::new(
                CATALOG.t("installer-property-name"),
                full_name.trim(),
            ));
        }
        summary.push(Property::new(
            CATALOG.t("installer-property-network"),
            network_summary(&self.network),
        ));
        children.push(Box::new(
            PropertyList::new(summary).with_id("installer.review.summary"),
        ));
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-image-note"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));

        if let Some(percent) = (self.power_probe)() {
            children.push(Box::new(
                Text::new(&connect_power_message(percent))
                    .with_role(TextRole::Body)
                    .with_color(theme.tokens.colors.semantic.warning),
            ));
        }

        self.compose_hardware_report(children);

        if self
            .hardware
            .report()
            .is_some_and(hardware_report::SupportReport::blocks_install)
        {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t(
                    "installer-hardware-blocks-install",
                ))
                .with_id("installer.hardware.blocked")
                .with_role(TextRole::Caption)
                .with_color(danger),
            ));
        }

        let impact = if self.install_method == InstallMethod::Alongside {
            alongside_install_impact(self.uses_guided_alongside(), &dev)
        } else {
            CATALOG.t_args(
                "imager-installer-erase",
                &[("device", tilo_ui_base::i18n::ArgValue::Text(dev.clone()))],
            )
        };
        children.push(Box::new(
            Text::new(&impact)
                .with_role(TextRole::Body)
                .with_color(danger),
        ));
        self.compose_destructive_confirmation(theme, children, &dev);

        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-recovery-key-note"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            ActionButton::new(
                "installer-action-recovery-promise",
                self.control(DestinationEffect::SetRecoveryPromise {
                    acknowledged: !self.recovery_ack,
                }),
            )
            .with_test_id("recovery_ack")
            .checkbox(self.recovery_ack),
        ));

        let begin_disabled = !self.encryption_ready()
            || !self.destructive_confirmation_ready()
            || !self.filesystem_policy_ready()
            || self.hardware_blocks_install();
        actions.push(self.nav_row(
            Step::Review,
            Some((self.control(self.begin_effect()), "installer-action-begin")),
            begin_disabled,
        ));
    }

    fn compose_install_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
        let friendly_status = friendly_install_status(&self.status_lines);

        match self.install_state {
            InstallState::Done => {
                children.push(Box::new(
                    Text::new(&crate::setup::strings::t("installer-installed"))
                        .with_role(TextRole::Title)
                        .with_color(theme.status_success)
                        .with_role(TextRole::Title)
                        .with_live(Politeness::Polite),
                ));
                let completion = self.internal_host().map_or_else(
                    || CATALOG.t("imager-installer-complete"),
                    |host| {
                        CATALOG.t_args(
                            "imager-installer-complete-host",
                            &[(
                                "host",
                                tilo_ui_base::i18n::ArgValue::Text(host.display_name().to_owned()),
                            )],
                        )
                    },
                );
                children.push(Box::new(
                    Text::new(&completion)
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                ));
                if self.install_method == InstallMethod::Alongside {
                    children.push(Box::new(
                        Text::new(&if self.uses_guided_alongside() {
                            crate::setup::strings::t("installer-boot-order")
                        } else {
                            crate::setup::strings::t("installer-boot-shortcuts")
                        })
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                    ));
                }
                self.compose_recovery_key(theme, children);
                children.push(status_log(&friendly_status, theme));
                if !self.status.is_empty() {
                    children.push(Box::new(
                        Text::new(&self.status)
                            .with_role(TextRole::Caption)
                            .with_color(theme.tokens.colors.semantic.warning)
                            .with_live(Politeness::Assertive),
                    ));
                }
                actions.push(self.finish_row(!self.recovery_key_saved));
            }
            InstallState::Failed => {
                children.push(Box::new(
                    Text::new(&crate::setup::strings::t("installer-failed"))
                        .with_role(TextRole::Title)
                        .with_color(danger)
                        .with_live(Politeness::Assertive),
                ));
                children.push(Box::new(
                    Text::new(&crate::setup::strings::t("installer-failed-note"))
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                ));
                children.push(Box::new(
                    Text::new(&crate::setup::strings::t("installer-details"))
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                ));
                self.compose_recovery_key(theme, children);
                children.push(status_log(&self.status_lines, theme));
                actions.push(Box::new(
                    ActionButton::new("installer-action-review-storage", self.back_control())
                        .with_test_id("installer.review_storage"),
                ));
            }
            _ => {
                children.push(Box::new(
                    Text::new(&crate::setup::strings::t("installer-installing"))
                        .with_role(TextRole::Title)
                        .with_role(TextRole::Title),
                ));
                children.push(Box::new(
                    Row::new().push(Box::new(Spinner::new())).push(Box::new(
                        Text::new(&crate::setup::strings::t("installer-installing-note"))
                            .with_role(TextRole::Caption)
                            .with_color(dim),
                    )),
                ));
                self.compose_recovery_key(theme, children);
                children.push(status_log(&friendly_status, theme));
            }
        }
    }

    fn compose_recovery_key(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let Some(key) = &self.recovery_key else {
            return;
        };
        children.push(Box::new(
            Text::new(&crate::setup::strings::t("installer-recovery-key-heading"))
                .with_role(TextRole::Caption)
                .with_color(theme.status_success),
        ));
        // Read off the screen when nothing can be copied: monospace at the body size, which
        // follows the person's text size, and wrapped inside its groups rather than cut off.
        children.push(Box::new(
            Text::new(key)
                .with_role(TextRole::Code)
                .with_wrap_mode(tilo_ui_base::TextWrapMode::WordOrGlyph)
                .with_id("recovery_key"),
        ));
        if tilo_ui_base::clipboard_available() {
            children.push(Box::new(
                ActionButton::new(
                    if self.recovery_key_copied {
                        "installer-action-copy-recovery-key-again"
                    } else {
                        "installer-action-copy-recovery-key"
                    },
                    self.control(DestinationEffect::CopyRecoveryKey),
                )
                .with_test_id("recovery_key_copy"),
            ));
            children.push(Box::new(
                Text::new(&if self.recovery_key_copied {
                    crate::setup::strings::t("installer-recovery-key-copied")
                } else {
                    crate::setup::strings::t("installer-recovery-key-copy-note")
                })
                .with_role(TextRole::Caption)
                .with_color(if self.recovery_key_copied {
                    theme.status_success
                } else {
                    theme.text_dim
                })
                .with_live(Politeness::Polite),
            ));
        } else {
            children.push(Box::new(
                Text::new(&crate::setup::strings::t(
                    "installer-recovery-key-write-note",
                ))
                .with_role(TextRole::Caption)
                .with_color(theme.text_dim),
            ));
        }
        children.push(Box::new(
            ActionButton::new(
                "installer-action-recovery-saved",
                self.control(DestinationEffect::ConfirmRecoverySaved {
                    saved: !self.recovery_key_saved,
                }),
            )
            .with_test_id("recovery_key_saved")
            .checkbox(self.recovery_key_saved),
        ));
    }
}

fn secret_copy(buffer: &Arc<Mutex<String>>) -> Zeroizing<String> {
    Zeroizing::new(
        buffer
            .lock()
            .map_or_else(|_| String::new(), |value| value.clone()),
    )
}

fn clear_secret_buffer(buffer: &Arc<Mutex<String>>) {
    if let Ok(mut value) = buffer.lock() {
        value.zeroize();
        value.clear();
    }
}

fn take_secret(buffer: &Arc<Mutex<String>>) -> Zeroizing<String> {
    let value = buffer
        .lock()
        .map_or_else(|_| String::new(), |mut value| std::mem::take(&mut *value));
    Zeroizing::new(value)
}

fn local_text(state: &tilo_ui_base::LocalState<String>) -> String {
    state.with(Clone::clone)
}

fn centered_media_control(widget: Box<dyn Widget>) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .with_padding(0.0, 0.0)
            .no_wrap()
            .push(Box::new(Spacer::new()))
            .push(widget)
            .push(Box::new(Spacer::new())),
    )
}

fn sample_install_log() -> Vec<String> {
    vec![
        "Starting install to /dev/sda for user ada (btrfs).".to_string(),
        "==> Copying the running Tilo image into local container storage".to_string(),
        "Copying local image docker://localhost/tilo-installer:latest to containers-storage:localhost/bootc".to_string(),
        "==> Installing Tilo onto /dev/sda (this ERASES the disk)".to_string(),
        "  base system installed.".to_string(),
        "==> Preparing the 'ada' account on the installed system".to_string(),
        "==> Setting hostname 'tilo' and time zone 'UTC'".to_string(),
    ]
}

fn friendly_install_status(lines: &[String]) -> Vec<String> {
    let mut stages: Vec<String> = Vec::new();
    let mut add = |stage: &str| {
        if stages.last().is_none_or(|last| last != stage) {
            stages.push(stage.to_string());
        }
    };

    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.contains("starting install") {
            add("Installation choices verified");
        } else if lower.contains("copying the running tilo image")
            || lower.contains("copying blob")
            || lower.contains("copying config")
        {
            add("Copying verified Tilo system files…");
        } else if lower.contains("preparing an encrypted root") || lower.contains("formatting luks")
        {
            add("Preparing encrypted storage…");
        } else if lower.contains("installing tilo onto") || lower.contains("base system installed")
        {
            add("Installing the signed Tilo system…");
        } else if lower.contains("fido2") && (lower.contains("touch") || lower.contains("enroll")) {
            add("Touch your security key to continue…");
        } else if lower.contains("enrolling disk-encryption")
            || lower.contains("recovery key verified")
            || lower.contains("tpm2+pin enrolled")
            || lower.contains("passphrase enrolled")
        {
            add("Securing disk unlock and recovery…");
        } else if lower.contains("preparing the '") && lower.contains("account") {
            add("Creating your account…");
        } else if lower.contains("setting hostname")
            || lower.contains("time zone")
            || lower.contains("keyboard layout")
        {
            add("Applying your preferences…");
        } else if lower.contains("boot loader")
            || lower.contains("bootloader")
            || lower.contains("initramfs")
            || lower.contains("finalizing")
        {
            add("Finalizing the secure boot setup…");
        } else if line == INSTALLED_REMOVE_MEDIUM || line == INSTALLED_RESTART {
            add("Tilo is ready");
        }
    }

    if stages.is_empty() {
        stages.push("Preparing installation…".to_string());
    }
    let keep_from = stages.len().saturating_sub(5);
    stages.split_off(keep_from)
}

impl InstallerApp {
    fn notice_capacity_edit(&mut self) {
        let gib = self.capacity_gib.with(|gib| *gib);
        if (gib - self.capacity_seen.get()).abs() < f64::EPSILON {
            return;
        }
        self.capacity_seen.set(gib);
        let Some(DestinationEffect::SetSize { extent, .. }) = self.capacity_effect() else {
            return;
        };
        let Ok(gib) = gib.to_string().parse::<i64>() else {
            self.set_status("Choose a whole number of GiB");
            return;
        };
        self.presses
            .control(Press::new(DestinationEffect::SetSize { gib, extent }))
            .fire();
    }

    fn notice_confirmation_edit(&mut self) {
        let confirmation = local_text(&self.erase_confirmation);
        if self.erase_confirmation_seen != confirmation {
            let was_ready = self
                .destructive_confirmation_target()
                .is_some_and(|device| self.erase_confirmation_seen.trim() == device);
            let had_status = !self.status.is_empty();
            self.erase_confirmation_seen = confirmation;
            self.status.clear();
            self.dirty_structure |=
                had_status || was_ready != self.destructive_confirmation_ready();
        }
    }

    fn network_wake(&self) -> Option<Wake> {
        let mailbox = self.mailbox.clone()?;
        Some(Arc::new(move || mailbox.post(NETWORK_WAKE.to_owned())))
    }

    fn refresh_disks_with(&mut self, disks: Vec<Disk>) {
        let previous = self.selected_disk_ref().and_then(DiskMethodIdentity::of);
        self.disks = disks;
        self.selected_disk = previous.and_then(|identity| {
            self.disks.iter().position(|disk| {
                DiskMethodIdentity::of(disk).as_ref() == Some(&identity)
                    && !disk.blocks_install()
                    && !disk.read_only
                    && !disk.too_small()
            })
        });
        self.reset_disk_method();
        self.selected_guided_extent = None;
        self.alongside_capacity = None;
        self.guided_efi_partuuid = uuid::Uuid::new_v4().to_string();
        self.guided_boot_partuuid = uuid::Uuid::new_v4().to_string();
        self.guided_root_partuuid = uuid::Uuid::new_v4().to_string();
        self.clear_erase_confirmation();
        self.storage_preflight_request = None;
        self.storage_preflight_passed = false;
        if self.internal_handoff.is_some() && !self.bind_internal_handoff() {
            self.internal_handoff = None;
        }
        self.status = CATALOG.t(if self.disks.is_empty() {
            "imager-installer-refresh-none"
        } else if self.selected_disk.is_some() {
            "imager-installer-refresh-review"
        } else {
            "imager-installer-refresh-select"
        });
        self.dirty_structure = true;
        self.focus_reset = true;
    }

    fn ingest_backend_line(&mut self, payload: &str) {
        if payload == CTRL_DONE {
            self.mark_install_done();
        } else if payload == CTRL_FAIL {
            self.mark_install_failed(None);
        } else if let Some(key) = payload.strip_prefix(RECOVERY_KEY_PREFIX) {
            if !key.trim().is_empty() {
                self.recovery_key = Some(key.trim().to_string());
                self.recovery_key_copied = false;
                self.recovery_key_copy = tilo_ui_base::CopyWatch::default();
                self.recovery_key_saved = false;
                self.dirty_structure = true;
            }
        } else {
            self.status_lines.push(payload.to_string());
            self.dirty_structure = true;
        }
    }

    fn run_presses(&mut self) {
        while let Some(press) = self.presses.take() {
            self.run_press(press);
        }
    }

    fn run_press(&mut self, press: Press) {
        let mut effect = press.effect;
        if let DestinationEffect::ConnectNetwork { password } = &mut effect {
            let typed = take_secret(&self.wifi_password);
            if !typed.is_empty() {
                *password = Some(typed.to_string());
            }
        }
        let phrase = local_text(&self.erase_confirmation);
        self.invoke_control(effect, Some(&phrase));
    }

    fn take_mail(&mut self) {
        let Some(mailbox) = self.mailbox.clone() else {
            return;
        };
        for payload in mailbox.take() {
            self.receive(&payload);
        }
    }

    fn receive(&mut self, payload: &str) {
        if payload == CONSOLE_WAKE {
            self.serve_console();
        } else if payload == NETWORK_WAKE {
            if self.network.drain_results() {
                self.dirty_structure = true;
            }
        } else if let Some(result) = payload.strip_prefix(PREFLIGHT_OK_PREFIX) {
            if let Some((request, message)) = result.split_once(':')
                && self.storage_preflight_request.as_deref() == Some(request)
            {
                self.storage_preflight_request = None;
                self.storage_preflight_passed = true;
                self.set_status(if message.trim().is_empty() {
                    "Prepared EFI partition passed its read-only health and free-space check."
                } else {
                    message.trim()
                });
                self.go_next();
            }
        } else if let Some(result) = payload.strip_prefix(PREFLIGHT_FAIL_PREFIX) {
            if let Some((request, message)) = result.split_once(':')
                && self.storage_preflight_request.as_deref() == Some(request)
            {
                self.storage_preflight_request = None;
                self.storage_preflight_passed = false;
                self.set_status(if message.trim().is_empty() {
                    "The prepared EFI partition failed its read-only health or free-space check."
                } else {
                    message.trim()
                });
            }
        } else {
            self.ingest_backend_line(payload);
        }
    }
}

impl Widget for InstallerApp {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        self.dirty = true;
        if self.recovery_key_copy.failed() {
            self.recovery_key_copied = false;
            self.dirty_structure = true;
            self.set_status("Could not copy the recovery key to the clipboard");
        }
        self.take_mail();
        self.notice_capacity_edit();
        self.run_presses();
        self.notice_confirmation_edit();
        if self.network.ensure_initial_scan(self.network_wake()) {
            self.dirty_structure = true;
        }
        self.composed.sync(state, root);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = false;
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition(theme);
        let card_node = self
            .composed
            .build_layout(composition, taffy, theme, node_map);

        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            justify_content: Some(JustifyContent::Center),
            align_items: Some(AlignItems::Center),
            size: Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &[card_node], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = taffy.children(node)
            && let Some(&card_node) = children.first()
        {
            c.apply_layout(taffy, card_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if let Some(c) = self.composed.element_mut() {
            c.draw_child(scene, transform, text_renderer, theme, &env);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&card_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, card_node));
        }
        self.run_presses();

        if !*ctx.handled
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            match self.step {
                Step::Review => {
                    if self.encryption_ready()
                        && self.destructive_confirmation_ready()
                        && !self.hardware_blocks_install()
                    {
                        self.run_press(Press::new(self.begin_effect()));
                    }
                    *ctx.handled = true;
                }
                Step::Network => {
                    if self
                        .network
                        .selected_network()
                        .is_some_and(|network| !network.connected)
                    {
                        self.run_press(Press::new(DestinationEffect::ConnectNetwork {
                            password: None,
                        }));
                    } else {
                        self.run_press(Press::new(DestinationEffect::Next));
                    }
                    *ctx.handled = true;
                }
                Step::Disk | Step::Account | Step::Keyboard | Step::Options | Step::Media => {
                    self.run_press(Press::new(DestinationEffect::Next));
                    *ctx.handled = true;
                }
                Step::Install => {}
            }
        }
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mine = self.dirty_structure;
        self.dirty_structure = false;
        let child = self.composed.take_structure_dirty();
        mine || child
    }

    fn wants_pointer_capture(&self) -> bool {
        self.composed
            .element()
            .is_some_and(Widget::wants_pointer_capture)
    }

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "InstallerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl InstallerApp {
    pub(crate) fn attach_live(&mut self, waker: ui::WakeHandle) {
        let mailbox = Mailbox::new(move || waker.wake());
        let console = imager_backend::destination::console::ConsoleServer::new(
            crate::setup::destination::effects::PROVIDER,
        );
        let wake = mailbox.clone();
        console.serve_discovered(Arc::new(move || wake.post(CONSOLE_WAKE.to_owned())));
        self.mailbox = Some(mailbox);
        self.console = Some(console);
    }
}

#[cfg(test)]
pub(super) mod host_journeys_tests;
#[cfg(test)]
mod tests;
