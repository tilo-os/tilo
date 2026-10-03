//! [`InstallerApp`] — the root installer widget + wizard state machine.
//!
//! Structured like the greeter: a root widget owning a single composed child (a
//! `Card`) and delegating the five `Widget` passes into it.
//!
//! Focus handling: the App discovers focusable widgets by walking the
//! widget tree (`Widget::children`) in lock-step with the taffy tree.
//! Any intermediate taffy node MUST have a matching widget in
//! `children()`, or the recursion derails and inputs never enter
//! `focusable_order` — breaking Tab / arrow navigation. The card is a
//! `Card` widget and the two-button rows are `ui::Row`s, so the widget tree mirrors the
//! taffy tree exactly.
//!
//! Keyboard-focus continuity across the per-step rebuild is handled by
//! the App's opt-in `autofocus_on_structure_change`: on a step change the
//! installer raises [`Widget::take_focus_reset`] so the new step's first
//! field is focused; on an incidental rebuild (a status line appearing)
//! focus is preserved by ordinal so the caret never vanishes mid-edit.
//!
//! State lives in the CRDT (toolkit convention):
//!   * `installer.username` — `Text`
//!   * `installer.hostname` — `Text` (seeded `tilo`)
//!   * `installer.timezone` — `Text` (seeded `UTC`)
//!   * `installer.next` / `back` / `begin` — `bool` rising-edge flags
//!   * `installer.reboot` / `poweroff` — `bool` rising-edge flags (Finish)
//!   * `installer.disk.<i>` — `bool` disk-row selection rising edge
//!   * `installer.fs.<i>`   — `bool` filesystem-segment selection rising edge
//!
//! A drained rising edge does not act by itself: it invokes the action the
//! installer publishes for that control (see the `surface` module), through the same
//! door the console and the host use, so a control can never do something
//! its published action would refuse.
//!
//! The install runs on a **worker thread**: on the `begin` rising edge the
//! app spawns [`crate::install::run_install`] and streams each output line
//! back through the winit `EventLoopProxy` as a `ui::UserEvent::External`,
//! folded into the status log by [`InstallerApp::on_external_event`] — so
//! the UI stays live and the log animates. If no proxy is wired (tests /
//! no event loop) it falls back to a synchronous run.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::doc::{read_bool, read_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, transaction::Transactable};
use taffy::style_helpers::{length, percent};
use taffy::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, NodeId, Size, Style, TaffyTree,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use ui::a11y::Politeness;
use ui::app::UserEvent;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::typography::TextRole;
use ui::widget::{Dropdown, Row, Scrollable, Spacer, Spinner, Text, TextInput, Widget};

use crate::action_button::ActionButton;
use crate::backend::{InstallBackend, SystemBackend};
use crate::disk_row::DiskRow;
use crate::disks::{
    Disk, DiskPartition, EFI_SYSTEM_GUID, GuidedAlongsidePlan, LINUX_FILESYSTEM_GUID,
};
use crate::install::{InstallOptions, PowerAction};
use crate::installer_network::{InstallerNetwork, NETWORK_WAKE_EVENT, NetworkPhase};
use crate::keyboard_view::KeyboardView;
use crate::layouts;
use crate::operation as op;
use crate::power_readiness::current_insufficient_battery_percent;
use crate::seg_choice::SegChoice;
use crate::status_log::StatusLog;
use crate::step_indicator::StepIndicator;

mod console;
mod surface;
#[cfg(test)]
mod surface_tests;

pub use console::{CONSOLE_WAKE, redact_recovery_material};

const USERNAME_PATH: &str = "installer.username";
const FULL_NAME_PATH: &str = "installer.full_name";
const HOSTNAME_PATH: &str = "installer.hostname";
const LOCALE_PATH: &str = "installer.locale";
const TIMEZONE_PATH: &str = "installer.timezone";
/// CRDT path holding the selected keyboard layout's *display name*.
const KEYMAP_PATH: &str = "installer.keymap";
/// Peer-local typed confirmation for the exact object that will be formatted:
/// the whole disk for Erase, or the prepared root partition for Alongside.
const ERASE_CONFIRM_PATH: &str = "installer.erase_confirmation";
const NEXT_PATH: &str = "installer.next";
const BACK_PATH: &str = "installer.back";
const BEGIN_PATH: &str = "installer.begin";
const REBOOT_PATH: &str = "installer.reboot";
const POWEROFF_PATH: &str = "installer.poweroff";
/// Rising-edge action that copies the one-time recovery key. The key itself
/// never enters the CRDT; only this non-secret intent flag does.
const RECOVERY_COPY_PATH: &str = "installer.recovery_copy";
/// Post-generation acknowledgement. Unlike the Review-step promise, this can
/// be raised only after the actual key exists and gates terminal power actions.
const RECOVERY_SAVED_PATH: &str = "installer.recovery_saved";
const NETWORK_REFRESH_PATH: &str = "installer.network.refresh";
const NETWORK_CONNECT_PATH: &str = "installer.network.connect";
const DISK_REFRESH_PATH: &str = "installer.disk.refresh";
/// Staged (USB-free) installs only: leave the installer and restart into the
/// retained OS. Firmware already consumed the one-shot `BootNext`.
const RETURN_TO_HOST_PATH: &str = "installer.return_to_host";
fn network_path(i: usize) -> String {
    format!("installer.network.{i}")
}
/// Per-disk selection flag path: `installer.disk.<index>`.
fn disk_path(i: usize) -> String {
    format!("installer.disk.{i}")
}
fn guided_extent_path(i: usize) -> String {
    format!("installer.guided_extent.{i}")
}
fn install_method_path(i: usize) -> String {
    format!("installer.install_method.{i}")
}
/// Per-filesystem selection flag path: `installer.fs.<index>`.
fn fs_path(i: usize) -> String {
    format!("installer.fs.{i}")
}
/// Per-FIDO2-choice selection flag path: `installer.fido2.<index>`
/// (index 0 = off, 1 = on). Mirrors the encryption segmented selector.
fn fido2_path(i: usize) -> String {
    format!("installer.fido2.{i}")
}
/// Per-choice selection flag path for the media codecs opt-in:
/// `installer.codecs.<index>` (index 0 = not now, 1 = install).
fn codecs_path(i: usize) -> String {
    format!("installer.codecs.{i}")
}
/// Rising-edge flag toggling the "I've saved my recovery key" acknowledgement.
const RECOVERY_ACK_PATH: &str = "installer.recovery_ack";

/// Labels for the FIDO2 segmented selector, in display order. Index 0 (off)
/// is the default; index 1 enrolls a hardware security key as an extra
/// unlock factor. Only shown when encryption is on.
pub const FIDO2_LABELS: [&str; 2] = ["No security key", "Enroll security key"];

/// Labels for the media codecs opt-in, in display order. Index 0 (not now) is
/// the default: the owner must choose to download third-party codecs.
pub const CODECS_LABELS: [&str; 2] = ["Not now", "Install media codecs"];

/// What installing media codecs does and where they come from, shown under
/// the choice. The installer only records the choice; the installed system
/// downloads the packages directly from their publishers.
pub const CODECS_DESCRIPTION: &str = "Enables hardware video on AMD and Intel graphics and \
H.264/HEVC playback and encoding for videos, screen sharing and calls. After the first start, this \
computer downloads them directly from RPM Fusion and Cisco (OpenH264) under their own licences, as \
soon as it is online; Tilo does not host or distribute them. You can install or remove them later \
in Settings, Updates.";

/// Default hostname seeded into the CRDT (and the installed system).
const DEFAULT_HOSTNAME: &str = "tilo";
/// Default time zone seeded into the CRDT.
const DEFAULT_TIMEZONE: &str = "UTC";
/// Default keyboard-layout display name seeded into the CRDT.
const DEFAULT_KEYMAP_NAME: &str = layouts::DEFAULT_DISPLAY_NAME;

/// Filesystem choices, in display order. Index 0 is the default.
pub const FILESYSTEMS: [&str; 3] = ["btrfs", "ext4", "xfs"];
const OMITTED_INITRAMFS_DRIVERS: &str = "/usr/lib/tilo/initramfs-omitted-drivers.txt";

fn filesystem_support_from_manifest(manifest: &str) -> Result<[bool; FILESYSTEMS.len()], String> {
    let mut supported = [true; FILESYSTEMS.len()];
    let mut seen = HashSet::new();
    for line in manifest.lines() {
        if !seen.insert(line) {
            return Err("The image has a duplicate initramfs driver entry".into());
        }
        if let Some(index) = FILESYSTEMS
            .iter()
            .position(|filesystem| *filesystem == line)
        {
            let Some(flag) = supported.get_mut(index) else {
                return Err("The image has an invalid initramfs driver manifest".into());
            };
            *flag = false;
        } else {
            // Every image also omits the large-firmware GPU drivers to keep its
            // UKI small. Those entries do not change which filesystem the
            // installer can offer.
            if !matches!(line, "amdgpu" | "nouveau" | "xe") {
                return Err("The image has an invalid initramfs driver manifest".into());
            }
        }
    }
    Ok(supported)
}

fn load_filesystem_support() -> Result<[bool; FILESYSTEMS.len()], String> {
    use std::os::unix::fs::MetadataExt as _;
    let path = std::path::Path::new(OMITTED_INITRAMFS_DRIVERS);
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "The image's initramfs driver manifest is unavailable".to_string())?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("The image's initramfs driver manifest is not a trusted regular file".into());
    }
    let manifest = std::fs::read_to_string(path)
        .map_err(|_| "The image's initramfs driver manifest is unreadable".to_string())?;
    filesystem_support_from_manifest(&manifest)
}
pub const INSTALL_METHOD_LABELS: [&str; 2] = ["Erase disk", "Install alongside"];
const WINDOWS_ALONGSIDE_PREPARATION: &str = "Windows or NTFS storage was detected on this disk. Before continuing, boot Windows, save its BitLocker recovery key, and turn off Fast Startup. If you still need unallocated space, shrink Windows with Disk Management. Tilo cannot make these Windows changes and will not resize NTFS.";

fn connect_power_message(percent: u8) -> String {
    format!(
        "Connect this computer to power before installing — the battery is at {percent}%. Installation has not started."
    )
}

fn alongside_install_impact(guided: bool, dev: &str) -> String {
    let change = if guided {
        format!(
            "Three new Tilo partitions (EFI, boot, and encrypted root) will be created and formatted only in the reviewed unallocated space on {dev}. Existing OS partitions are preserved."
        )
    } else {
        format!(
            "Only the prepared Tilo boot and root partitions on {dev} will be formatted. The existing EFI partition and other OS partitions are preserved."
        )
    };
    format!(
        "{change} Firmware/Secure Boot changes can still trigger a Windows BitLocker recovery prompt; save that recovery key before continuing."
    )
}

fn alongside_confirmation_operation(guided: bool) -> &'static str {
    if guided {
        "guided free-space partition plan"
    } else {
        "Tilo boot and root format"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMethod {
    Erase,
    Alongside,
}

/// Which non-secret choices were independently validated from the medium.
/// This deliberately records fields rather than treating a profile as one
/// all-or-nothing shortcut: an invalid value can be repaired without asking
/// for the sound choices again.
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

/// One-line explanation for each filesystem, shown under the chooser.
fn fs_description(i: usize) -> &'static str {
    match i {
        0 => "Btrfs: snapshots, compression and rollback — recommended for Tilo.",
        1 => "Ext4: simple and battle-tested; no snapshot support.",
        2 => "XFS: high throughput for large files; no snapshot support.",
        _ => "",
    }
}

/// Does this machine have a TPM2 device? Encryption does not require one — the
/// volume is always built around a recovery key and the TPM is an extra key slot
/// — but it decides whether the disk can unlock itself, and therefore whether
/// the secret below is optional convenience or the only thing standing between
/// the user and typing a 48-character recovery key at every boot.
fn has_tpm2() -> bool {
    std::path::Path::new("/dev/tpmrm0").exists() || std::path::Path::new("/dev/tpm0").exists()
}

fn secure_boot_enabled() -> bool {
    std::fs::read_dir("/sys/firmware/efi/efivars")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("SecureBoot-")
        })
        .and_then(|entry| std::fs::read(entry.path()).ok())
        .and_then(|bytes| bytes.get(4).copied())
        == Some(1)
}

fn kernel_lockdown_active() -> bool {
    std::fs::read_to_string("/sys/kernel/security/lockdown")
        .is_ok_and(|state| state.contains("[integrity]") || state.contains("[confidentiality]"))
}

/// Does this installer image prove that the target it writes will retain the
/// verified-boot boundary after reboot? The current split install flow cannot
/// make that promise merely because the live medium itself booted securely.
/// A future UKI/signing integration owns this executable and must fail closed
/// unless the target UKI, initramfs, command line and firmware trust chain are
/// all covered.
fn target_verified_boot_ready() -> bool {
    bounded_command::run(
        "/usr/libexec/tilo-verified-boot-install-preflight",
        &[],
        None,
        64 * 1024,
        std::time::Duration::from_secs(30),
    )
    .is_ok_and(|output| output.status.success())
}

/// Explanation shown under the encryption chooser, reflecting the choice and
/// what this machine's hardware can actually do.
#[derive(Clone, Copy)]
enum VerifiedBootState {
    NoTpm,
    SecureBootInactive,
    KernelLockdownInactive,
    TargetUnverified,
    TargetVerified,
}

fn encrypt_description(state: VerifiedBootState) -> &'static str {
    match state {
        VerifiedBootState::TargetVerified => {
            "Tilo encrypts the disk with LUKS2 and binds unlock to this computer's Secure Boot state plus the release-authorized Tilo boot measurement. You will use a short startup PIN; a recovery key is shown once during install."
        }
        VerifiedBootState::TargetUnverified => {
            "Tilo always encrypts the disk with LUKS2. Verified boot is unavailable for this installation, so use one strong startup passphrase. A short PIN is only safe when the TPM can verify the complete boot chain."
        }
        VerifiedBootState::KernelLockdownInactive => {
            "Tilo always encrypts the disk with LUKS2. Secure Boot is active but kernel lockdown is not, so Tilo cannot treat this boot as verified and uses a strong disk passphrase instead."
        }
        VerifiedBootState::SecureBootInactive => {
            "Tilo always encrypts the disk with LUKS2. TPM2 short-PIN protection requires verified boot, but Secure Boot is not active, so this installation uses a strong disk passphrase instead."
        }
        VerifiedBootState::NoTpm => {
            "Tilo always encrypts the disk with LUKS2. This computer has no TPM2 chip, so you will enter a disk passphrase at startup. A recovery key is shown once during install as a backup."
        }
    }
}

/// Control sentinels sent over the `External` channel by the install
/// worker thread to mark terminal outcomes (anything else is a log line).
const CTRL_DONE: &str = "\u{1}done";
const CTRL_FAIL: &str = "\u{1}fail";
const PREFLIGHT_OK_PREFIX: &str = "\u{1}storage-preflight-ok:";
const PREFLIGHT_FAIL_PREFIX: &str = "\u{1}storage-preflight-fail:";
const RECOVERY_KEY_PREFIX: &str = "::tilo-recovery-key::";

/// Card geometry (logical px).
const CARD_WIDTH: f32 = 520.0;
const CARD_PADDING: f32 = 28.0;
const CARD_GAP: f32 = 12.0;
/// Breathing room around the card on compact displays. The card itself can
/// narrow to the remaining width rather than extending beyond the viewport.
const VIEWPORT_PADDING: f32 = 16.0;
/// Reserved lane for the always-visible compact-view scrollbar.
const SCROLLBAR_GUTTER: f32 = 14.0;
/// Height of the install status log box.
const STATUS_LOG_HEIGHT: f32 = 200.0;

/// The wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Keyboard,
    Network,
    Disk,
    Account,
    Options,
    Review,
    Install,
}

/// Captions for the step-progress breadcrumb, index-aligned with
/// [`step_index`].
pub const STEP_LABELS: [&str; 7] = [
    "Keyboard", "Network", "Disk", "Account", "Options", "Review", "Install",
];

/// Position of `step` in the breadcrumb (0-based). Keyboard is first so the
/// layout is chosen BEFORE the user types anything (username/password).
pub fn step_index(step: Step) -> usize {
    match step {
        Step::Keyboard => 0,
        Step::Network => 1,
        Step::Disk => 2,
        Step::Account => 3,
        Step::Options => 4,
        Step::Review => 5,
        Step::Install => 6,
    }
}

/// Lifecycle of the install backend call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    Idle,
    Running,
    Done,
    Failed,
}

/// The step reached by pressing Next from `step`, if any. `Review` →
/// `Install` is intentionally gated behind the explicit Begin button, so
/// callers must not auto-advance into `Install` via Next (see
/// `InstallerApp::go_next`).
pub fn next_step(step: Step) -> Option<Step> {
    match step {
        Step::Keyboard => Some(Step::Network),
        Step::Network => Some(Step::Disk),
        Step::Disk => Some(Step::Account),
        Step::Account => Some(Step::Options),
        Step::Options => Some(Step::Review),
        Step::Review => Some(Step::Install),
        Step::Install => None,
    }
}

/// The step reached by pressing Back from `step`, if any.
pub fn prev_step(step: Step) -> Option<Step> {
    match step {
        Step::Network => Some(Step::Keyboard),
        Step::Disk => Some(Step::Network),
        Step::Account => Some(Step::Disk),
        Step::Options => Some(Step::Account),
        Step::Review => Some(Step::Options),
        // Keyboard is the first step; Back out of Install is handled
        // specially (only on failure) by the app — see `go_back`.
        Step::Keyboard | Step::Install => None,
    }
}

/// Whether `h` is a usable hostname (letters/digits/hyphens, no leading
/// or trailing hyphen, ≤ 63 chars).
pub fn valid_hostname(h: &str) -> bool {
    let h = h.trim();
    !h.is_empty()
        && h.len() <= 63
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !h.starts_with('-')
        && !h.ends_with('-')
}

/// Whether `username` is portable across the Linux `useradd`
/// implementations supported by the Tilo installer.
pub fn valid_username(username: &str) -> bool {
    let bytes = username.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes
            .first()
            .is_some_and(|c| matches!(c, b'a'..=b'z' | b'_'))
        && bytes
            .iter()
            .skip(1)
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        && !matches!(
            username,
            "root"
                | "bin"
                | "daemon"
                | "adm"
                | "lp"
                | "sync"
                | "shutdown"
                | "halt"
                | "mail"
                | "operator"
                | "games"
                | "ftp"
                | "nobody"
                | "dbus"
                | "polkitd"
                | "sshd"
                | "chrony"
                | "tss"
                | "greetd"
                | "tilo"
        )
}

pub fn valid_full_name(full_name: &str) -> bool {
    full_name.chars().count() <= 100
        && !full_name
            .chars()
            .any(|c| c.is_control() || matches!(c, ':' | '\n' | '\r'))
}

pub fn valid_locale(locale: &str) -> bool {
    crate::locales::contains(locale)
}

pub fn valid_timezone_name(timezone: &str) -> bool {
    !timezone.is_empty()
        && timezone.len() <= 128
        && !timezone.starts_with('/')
        && !timezone.contains("..")
        && timezone
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-'))
}

/// Guard for advancing out of `step`. `Ok(())` means it's valid to move
/// forward; `Err(msg)` is a user-facing reason it isn't yet.
///
/// # Errors
///
/// Returns a user-facing message when `step` is `Disk` with no disk
/// selected, `Account` with a blank `username`, or `Options` with a
/// `hostname` that fails [`valid_hostname`].
pub fn validate_step(
    step: Step,
    selected_disk: Option<usize>,
    username: &str,
    hostname: &str,
) -> Result<(), String> {
    match step {
        Step::Disk => {
            if selected_disk.is_some() {
                Ok(())
            } else {
                Err("Select a disk to continue".to_string())
            }
        }
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
        // A layout is always selected (seeded to the default) and
        // Review/Install have nothing left to validate, so these steps
        // never block forward progress.
        Step::Network | Step::Keyboard | Step::Review | Step::Install => Ok(()),
    }
}

/// Validate the two account password fields. Keeping this separate from
/// [`validate_step`] avoids ever passing either secret through generic wizard
/// summaries or status formatting.
///
/// # Errors
///
/// Returns a user-facing message when the password is blank or the confirmation
/// does not match it.
pub fn validate_passwords(password: &str, confirmation: &str) -> Result<(), String> {
    if password.trim().is_empty() {
        Err("Enter an account password".to_string())
    } else if password != confirmation {
        Err("The passwords do not match".to_string())
    } else {
        Ok(())
    }
}

/// Validate either the measured-boot TPM PIN or the independent strong LUKS
/// passphrase selected when the target cannot prove that policy.
fn validate_startup_secret_for(secret: &str, tpm_pin: bool) -> Result<(), &'static str> {
    const COMMON_BASES: &[&str] = &[
        "password",
        "qwerty",
        "letmein",
        "welcome",
        "administrator",
        "admin",
        "tilo",
        "quickdesk",
        "tilo",
    ];

    if secret.trim() != secret {
        return Err("Do not begin or end the disk passphrase with spaces");
    }
    let chars: Vec<char> = secret.chars().collect();
    if tpm_pin {
        if chars.len() < 6 {
            return Err("Use at least 6 digits for the TPM-protected startup PIN");
        }
        if !chars.iter().all(char::is_ascii_digit) {
            return Err("The TPM-protected startup PIN must contain digits only");
        }
        if chars
            .first()
            .is_some_and(|first| chars.iter().all(|c| c == first))
            || ["012345", "123456", "234567", "345678", "456789", "987654"]
                .iter()
                .any(|bad| secret.contains(bad))
        {
            return Err("Choose a less predictable startup PIN");
        }
        return Ok(());
    }
    if chars.len() < 15 {
        return Err("Use at least 15 characters for the disk passphrase");
    }
    let lower = secret.to_lowercase();
    if COMMON_BASES.iter().any(|base| {
        lower
            .strip_prefix(base)
            .is_some_and(|tail| tail.chars().all(|c| !c.is_alphabetic()))
    }) {
        return Err("That passphrase is too easy to guess; use several unrelated words");
    }
    if chars.iter().all(char::is_ascii_digit) && chars.len() < 20 {
        return Err("Use at least 20 digits for an all-numeric disk passphrase");
    }
    let unique: std::collections::HashSet<char> = chars.iter().copied().collect();
    if unique.len() < 5 {
        return Err("That passphrase is too repetitive; use several unrelated words");
    }
    for period in 1..=4 {
        if chars.len().is_multiple_of(period)
            && chars
                .get(..period)
                .is_some_and(|prefix| chars.chunks(period).all(|chunk| chunk == prefix))
        {
            return Err("That passphrase is a repeated pattern; use several unrelated words");
        }
    }
    Ok(())
}

#[cfg(test)]
fn validate_startup_secret(secret: &str) -> Result<(), &'static str> {
    validate_startup_secret_for(secret, false)
}

// ===================================================================
// Card — a responsive, padded, vertically-stacked rounded container.
// Its own `Widget` (not a bare taffy node) so the widget tree mirrors
// the taffy tree for focus traversal. Mirrors greeter's `Card`.
// ===================================================================

struct Card {
    children: Vec<Box<dyn Widget>>,
    rect: Rect,
}

impl Card {
    fn new(children: Vec<Box<dyn Widget>>) -> Self {
        Self {
            children,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for Card {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_nodes: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        // Card is a vertical flex container. Let its height grow with its
        // controls instead of allowing Taffy to collapse form fields and the
        // recovery acknowledgement when Review is taller than the viewport.
        // ActionButton's zero flex basis and growth are for horizontal nav
        // rows; as direct children of this column they must use intrinsic
        // height. The surrounding Scrollable handles vertical overflow.
        for &child_node in &child_nodes {
            if let Ok(style) = taffy.style(child_node) {
                let mut style = style.clone();
                style.flex_basis = Dimension::auto();
                style.flex_grow = 0.0;
                style.flex_shrink = 0.0;
                let _ = taffy.set_style(child_node, style);
            }
        }
        // `new_with_children` only errors for a child `NodeId` from a
        // different tree, which never happens here since every id in
        // `child_nodes` was just built in this same tree by the calls above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just built in this same tree"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    align_items: Some(AlignItems::Stretch),
                    gap: Size {
                        width: length(0.0_f32),
                        height: length(CARD_GAP),
                    },
                    padding: taffy::Rect {
                        left: length(CARD_PADDING),
                        right: length(CARD_PADDING),
                        top: length(CARD_PADDING),
                        bottom: length(CARD_PADDING),
                    },
                    size: Size {
                        width: percent(1.0_f32),
                        height: Dimension::auto(),
                    },
                    max_size: Size {
                        width: length(CARD_WIDTH),
                        height: Dimension::auto(),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let card_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(child_nodes) = taffy.children(node) {
            for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
                child.apply_layout(taffy, child_node, card_origin);
            }
        }
        // Taffy can cap an auto-height flex card at its available viewport
        // height while still placing nonshrinking controls beyond that cap.
        // Paint and expose the full measured content, including bottom inset.
        if let Some(bottom) = self
            .children
            .iter()
            .map(|child| child.get_rect().y1)
            .reduce(f64::max)
        {
            self.rect.y1 = self.rect.y1.max(bottom + f64::from(CARD_PADDING));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &shape,
        );
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
            if let Some(&cnode) = child_nodes.get(idx)
                && let Some(child) = self.children.get_mut(idx)
            {
                child.handle_event(event, &mut ctx.reborrow(ctx.path, cnode));
            }
            return;
        }
        for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.children.iter().any(|c| c.wants_pointer_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Every child must be polled regardless of an earlier `true` — this
        // "takes" (clears) each child's own dirty flag, unlike a
        // short-circuiting `any`.
        let mut dirty = false;
        for child in &mut self.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn name(&self) -> &'static str {
        "InstallerCard"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for Card {
    fn needs_commit(&self) -> bool {
        self.children
            .iter()
            .any(|c| ui::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        for child in &mut self.children {
            ui::undo::on_commit(child.as_mut());
        }
    }
}

// ===================================================================
// ViewportStage — centres short cards, then lets the spacers collapse before
// a tall card overflows into the surrounding Scrollable. This keeps the
// spacious desktop presentation while making every control reachable on a
// small laptop panel, portrait screen, or large-text/high-DPI setup.
// ===================================================================

struct ViewportStage {
    children: Vec<Box<dyn Widget>>,
    rect: Rect,
}

impl ViewportStage {
    fn new(card: Box<dyn Widget>) -> Self {
        Self {
            children: vec![Box::new(Spacer::new()), card, Box::new(Spacer::new())],
            rect: Rect::ZERO,
        }
    }
}

impl Widget for ViewportStage {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_nodes: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|child| child.build_layout(taffy, theme, node_map))
            .collect();
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    align_items: Some(AlignItems::Center),
                    padding: taffy::Rect {
                        left: length(VIEWPORT_PADDING),
                        right: length(VIEWPORT_PADDING),
                        top: length(VIEWPORT_PADDING),
                        bottom: length(VIEWPORT_PADDING),
                    },
                    size: Size {
                        width: percent(1.0_f32),
                        // The scrollable measures this node for its scroll
                        // extent. A fixed viewport height hid the overflowing
                        // Review controls from the scrollbar.
                        height: Dimension::auto(),
                    },
                    min_size: Size {
                        width: Dimension::auto(),
                        // Short steps still fill the viewport so the two
                        // flexible spacers can centre the card.
                        height: percent(1.0_f32),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let own_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(child_nodes) = taffy.children(node) {
            for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
                child.apply_layout(taffy, child_node, own_origin);
            }
        }
        if let Some(card) = self.children.get(1) {
            // The trailing centring spacer may itself end at the viewport
            // edge. Only the card's overflow should enlarge scroll extent.
            self.rect.y1 = self
                .rect
                .y1
                .max(card.get_rect().y1 + f64::from(VIEWPORT_PADDING));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(index) = self
            .children
            .iter()
            .position(|child| child.wants_pointer_capture())
        {
            if let (Some(child), Some(&child_node)) =
                (self.children.get_mut(index), child_nodes.get(index))
            {
                child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            }
            return;
        }
        for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.children
            .iter()
            .any(|child| child.wants_pointer_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in &mut self.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn name(&self) -> &'static str {
        "InstallerViewportStage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for ViewportStage {
    fn needs_commit(&self) -> bool {
        self.children
            .iter()
            .any(|child| ui::undo::needs_commit(child.as_ref()))
    }
    fn on_commit(&mut self) {
        for child in &mut self.children {
            ui::undo::on_commit(child.as_mut());
        }
    }
}

// ===================================================================
// InstallerApp
// ===================================================================

/// Shared slot for the winit `EventLoopProxy`. `main` fills it once the
/// event loop exists (via `run_with_setup`); the install worker thread
/// reads it to pump progress lines back as `UserEvent::External`.
pub type ProxySlot = Arc<Mutex<Option<EventLoopProxy<UserEvent>>>>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent wizard-state flag (encryption/FIDO2 opt-in, TPM presence, shot/bootstrap/dirty/focus one-shots), not a combinatorial state machine"
)]
pub struct InstallerApp {
    network: InstallerNetwork,
    disks: Vec<Disk>,
    selected_disk: Option<usize>,
    /// Explicit choice when a disk has multiple safe unallocated extents.
    /// A sole candidate is selected implicitly; ambiguity never is.
    selected_guided_extent: Option<usize>,
    install_method: InstallMethod,
    /// UUIDs reviewed for the partitions the guided free-space path will add.
    /// Generated once per installer run; changing disks clears confirmation but
    /// retaining these stable IDs keeps Review and the backend handoff identical.
    guided_efi_partuuid: String,
    guided_boot_partuuid: String,
    guided_root_partuuid: String,
    /// Index into [`FILESYSTEMS`].
    selected_fs: usize,
    /// Image initramfs policy; false means its root driver is omitted.
    supported_filesystems: [bool; FILESYSTEMS.len()],
    filesystem_policy_error: Option<String>,
    /// Enroll a FIDO2 security key as an extra unlock factor (only with
    /// encryption). Default `false`.
    fido2: bool,
    /// Owner opted in to the third-party media codecs pack, installed by the
    /// new system once it is online. Default `false`.
    codecs: bool,
    /// User has acknowledged they will save the install-time recovery key.
    /// Required before Begin Install when encryption is on. Default `false`.
    recovery_ack: bool,
    /// Whether this machine has a TPM2 device, sampled once at construction.
    /// Together with the verified-target preflight, this selects either the
    /// measured-boot PIN policy or the strong passphrase policy.
    tpm2: bool,
    /// Diagnostic state used to explain the measured-boot boundary.
    secure_boot: bool,
    /// Secure Boot must have propagated into the kernel's runtime restrictions;
    /// the EFI variable alone is not sufficient evidence for TPM unlock.
    kernel_lockdown: bool,
    /// A dedicated preflight must also prove the installed target, not merely
    /// the running live medium, retains the complete verified-boot boundary.
    target_verified_boot: bool,
    step: Step,
    install_state: InstallState,
    /// Streamed install output (also the success/failure summary).
    status_lines: Vec<String>,
    /// One-time LUKS recovery key, kept visible until the machine restarts.
    recovery_key: Option<String>,
    /// Whether the visible key has been copied during this installer session.
    /// This is presentation state only; a newly emitted key resets it.
    recovery_key_copied: bool,
    /// Explicit confirmation made after the concrete recovery key was shown.
    /// Copying to the live clipboard alone is not evidence of an off-device copy.
    recovery_key_saved: bool,
    /// Cached account/system fields (mirror their CRDT Text objects) for
    /// the Review display + backend argv.
    username: String,
    full_name: String,
    // Secrets deliberately bypass Automerge: deleting CRDT text leaves the
    // old characters in document history. These buffers are local to the
    // privileged installer process and are zeroized when consumed.
    password: Arc<Mutex<String>>,
    password_confirmation: Arc<Mutex<String>>,
    /// Optional action-supplied disk secret. Empty means the account password
    /// is also the startup secret, preserving the graphical wizard behavior.
    disk_password: Arc<Mutex<String>>,
    wifi_password: Arc<Mutex<String>>,
    hostname: String,
    locale: String,
    timezone: String,
    /// Selected keyboard-layout *display name* (mirrors the
    /// `installer.keymap` CRDT Text). Resolved to xkb layout/variant via
    /// [`crate::layouts`] when building the install command.
    keymap: String,
    erase_confirmation: String,
    /// Time-zone choices shown by the Options-step [`Dropdown`], sourced
    /// from the system at runtime (see [`crate::timezones::list_timezones`]).
    timezones: Vec<String>,
    /// Headless-shot only: build the Options-step time-zone dropdown in
    /// its open state so `--shots --step options-open` captures the popup.
    tz_open_for_shot: bool,
    /// Transient validation / error line under the controls.
    status: String,
    bootstrap_done: bool,
    dirty_structure: bool,
    /// One-shot: raised on a step transition so the App re-focuses the new
    /// step's first field. Drained via [`Widget::take_focus_reset`].
    focus_reset: bool,
    rect: Rect,
    composed: Option<Box<dyn Widget>>,
    /// Set once the install worker thread has been spawned, so a stray
    /// extra `begin` can't launch a second install.
    install_launched: bool,
    /// Read-only prepared-partition probe launched before leaving Disk. The
    /// request UUID prevents a late result from a previously selected disk
    /// from affecting the current choice; the backend independently binds the
    /// probe to disk/root/ESP identities.
    storage_preflight_request: Option<String>,
    storage_preflight_passed: bool,
    /// Independently validated choices supplied by Tilo Imager. `None` is the
    /// ordinary manual flow; incomplete fields route to their repair screen.
    profile_completion: Option<ProfileCompletion>,
    /// A verified staged (USB-free) installation, when the installer booted
    /// from the staging ESP Tilo Imager prepared. It is a hint: the backend
    /// re-derives every fact before it writes anything.
    internal_handoff: Option<crate::internal_handoff::Handoff>,
    /// Wired by `main` so the worker thread can pump progress back.
    proxy_slot: Option<ProxySlot>,
    /// Live power probe. Tests and deterministic screenshots use a no-op;
    /// [`Self::new`] replaces it with the shared sysfs-backed Tilo reader.
    power_probe: fn() -> Option<u8>,
    /// Scroll position survives incidental composition rebuilds (network
    /// status, validation, progress). Deliberate step changes reset it.
    viewport_offset: Rc<Cell<(f32, f32)>>,
    /// Disk discovery, install, preflight and power. The live medium runs
    /// the real helpers; host tests substitute a recording fake.
    backend: Arc<dyn InstallBackend>,
    /// Queued `tilo-control` requests from the console endpoints, answered
    /// on the UI thread (see [`console`]).
    console: Option<console_control::ConsoleServer>,
}

impl Default for InstallerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl InstallerApp {
    /// Construct an installer reading the live disk list from `lsblk` and
    /// the live time-zone list from the system.
    pub fn new() -> Self {
        let mut s = Self::with_disks(SystemBackend.read_disks());
        s.power_probe = current_insufficient_battery_percent;
        s.timezones = crate::timezones::list_timezones();
        match crate::media_profile::load() {
            Ok(Some(profile)) => s.apply_media_profile(profile),
            Ok(None) => {
                if let Ok(message) =
                    std::fs::read_to_string(crate::media_profile::PROFILE_ERROR_PATH)
                {
                    s.status = message.trim().to_string();
                }
            }
            Err(message) => s.status = message,
        }
        match crate::internal_handoff::load() {
            Ok(Some(handoff)) => s.apply_internal_handoff(handoff),
            Ok(None) => {
                if let Ok(message) =
                    std::fs::read_to_string(crate::internal_handoff::HANDOFF_ERROR_PATH)
                {
                    s.status = message.trim().to_string();
                }
            }
            Err(message) => s.status = message,
        }
        s.set_filesystem_support(load_filesystem_support());
        s
    }

    /// Adopt a staged installation: bind it to the one disk it describes,
    /// preselect guided alongside in its prepared extent, and prefill the
    /// Imager's setup choices. Nothing is confirmed; Review still shows every
    /// choice and requires the typed confirmation.
    fn apply_internal_handoff(&mut self, handoff: crate::internal_handoff::Handoff) {
        let profile = handoff.media_profile();
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

    /// Re-bind the staged plan after (re)discovery. Returns whether exactly
    /// one disk accepted it; that disk is then selected for guided alongside.
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

    /// The retained OS on the staged disk, when this is a staged install.
    fn internal_host(&self) -> Option<crate::internal_handoff::HostOs> {
        self.internal_handoff
            .as_ref()
            .filter(|_| self.disks.iter().any(Disk::hosts_staged_installer))
            .map(crate::internal_handoff::Handoff::host)
    }

    fn selected_disk_hosts_installer(&self) -> bool {
        self.selected_disk_ref()
            .is_some_and(Disk::hosts_staged_installer)
    }

    /// Construct with an explicit disk list (tests / headless rendering).
    /// Uses a small deterministic time-zone list so tests don't shell out;
    /// [`Self::new`] swaps in the full system list.
    pub fn with_disks(disks: Vec<Disk>) -> Self {
        Self {
            network: InstallerNetwork::default(),
            disks,
            selected_disk: None,
            selected_guided_extent: None,
            install_method: InstallMethod::Erase,
            guided_efi_partuuid: uuid::Uuid::new_v4().to_string(),
            guided_boot_partuuid: uuid::Uuid::new_v4().to_string(),
            guided_root_partuuid: uuid::Uuid::new_v4().to_string(),
            selected_fs: 0,
            supported_filesystems: [true; FILESYSTEMS.len()],
            filesystem_policy_error: None,
            fido2: false,
            codecs: false,
            recovery_ack: false,
            tpm2: has_tpm2(),
            secure_boot: secure_boot_enabled(),
            kernel_lockdown: kernel_lockdown_active(),
            target_verified_boot: target_verified_boot_ready(),
            step: Step::Keyboard,
            install_state: InstallState::Idle,
            status_lines: Vec::new(),
            recovery_key: None,
            recovery_key_copied: false,
            recovery_key_saved: false,
            username: String::new(),
            full_name: String::new(),
            password: Arc::new(Mutex::new(String::new())),
            password_confirmation: Arc::new(Mutex::new(String::new())),
            disk_password: Arc::new(Mutex::new(String::new())),
            wifi_password: Arc::new(Mutex::new(String::new())),
            hostname: DEFAULT_HOSTNAME.to_string(),
            locale: "en_US.UTF-8".to_string(),
            timezone: DEFAULT_TIMEZONE.to_string(),
            keymap: DEFAULT_KEYMAP_NAME.to_string(),
            erase_confirmation: String::new(),
            timezones: default_timezones(),
            tz_open_for_shot: false,
            status: String::new(),
            bootstrap_done: false,
            dirty_structure: false,
            focus_reset: false,
            rect: Rect::ZERO,
            composed: None,
            install_launched: false,
            storage_preflight_request: None,
            storage_preflight_passed: false,
            profile_completion: None,
            internal_handoff: None,
            proxy_slot: None,
            power_probe: || None,
            viewport_offset: Rc::new(Cell::new((0.0, 0.0))),
            backend: Arc::new(SystemBackend),
            console: None,
        }
    }

    /// Replace the disk, install and power helpers. The disk list is read
    /// from the new backend immediately.
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn InstallBackend>) -> Self {
        self.disks = backend.read_disks();
        self.backend = backend;
        self
    }

    /// The current step.
    pub fn step(&self) -> Step {
        self.step
    }

    /// The install backend's lifecycle.
    pub fn install_state(&self) -> InstallState {
        self.install_state
    }

    fn apply_media_profile(&mut self, profile: crate::media_profile::MediaProfile) {
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
            self.hostname = value;
        }
        if let Some(value) = profile.username {
            self.username = value;
        }
        if let Some(value) = profile.full_name {
            self.full_name = value;
        }
        if let Some(value) = profile.locale {
            self.locale = value;
        }
        if let Some(value) = keyboard_display {
            self.keymap = value;
        }
        if let Some(value) = profile.timezone {
            self.timezone = value;
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
                    if let Some(completion) = self.profile_completion.as_mut() {
                        completion.filesystem = FieldCompletion::NeedsRepair;
                    }
                    if let Some(index) = supported.iter().position(|available| *available) {
                        self.selected_fs = index;
                    }
                    self.status = "The preselected filesystem cannot boot from this image; choose a supported filesystem.".into();
                }
            }
            Err(message) => {
                self.supported_filesystems = [false; FILESYSTEMS.len()];
                self.filesystem_policy_error = Some(message.clone());
                if let Some(completion) = self.profile_completion.as_mut() {
                    completion.filesystem = FieldCompletion::NeedsRepair;
                }
                self.status = format!("{message}. Installation is unavailable.");
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
            return format!("{message}. Installation is unavailable.");
        }
        format!(
            "{} cannot boot from this image's initramfs; choose a supported filesystem.",
            self.filesystem()
        )
    }

    /// Wire the shared event-loop-proxy slot used to stream install
    /// progress from the worker thread. `main` calls this before handing
    /// the widget to `ui::App`.
    #[must_use]
    pub fn with_proxy(mut self, slot: ProxySlot) -> Self {
        self.proxy_slot = Some(slot);
        self
    }

    /// Build a pre-seeded app for a headless `--shots` render of `step`.
    /// Live runs never use this; it just stages enough state that each
    /// screen renders something representative. The `Install` step renders
    /// the live "installing…" look; see [`Self::shot_finished`] /
    /// [`Self::shot_failed`] for the terminal variants.
    pub fn for_shot(step: Step) -> Self {
        let mut app = Self::with_disks(sample_disks());
        app.step = step;
        app.username = "quinten".to_string();
        app.full_name = "Quinten Example".to_string();
        app.password = Arc::new(Mutex::new("correct horse battery staple".to_string()));
        app.password_confirmation =
            Arc::new(Mutex::new("correct horse battery staple".to_string()));
        app.hostname = DEFAULT_HOSTNAME.to_string();
        app.timezone = DEFAULT_TIMEZONE.to_string();
        app.keymap = DEFAULT_KEYMAP_NAME.to_string();
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

    /// Mutate a `for_shot(Step::Options)` app so its time-zone dropdown
    /// renders open — used by `--shots --step options-open` to capture the
    /// overlay popup state.
    pub fn shot_open_timezone(&mut self) {
        self.step = Step::Options;
        self.tz_open_for_shot = true;
    }

    /// Pose the exact shortened Options screen shown when every transported
    /// choice except locale was valid. This keeps the repair UX available to
    /// visual regression checks instead of testing it only as widget state.
    pub fn shot_locale_repair(&mut self) {
        self.apply_media_profile(crate::media_profile::MediaProfile {
            hostname: Some("studio".into()),
            username: Some("quinten".into()),
            full_name: Some("Quinten Example".into()),
            locale: None,
            keyboard: Some("de".into()),
            timezone: Some("Europe/Berlin".into()),
            filesystem: Some("ext4".into()),
            repairs: vec![crate::media_profile::MediaProfileField::Locale],
        });
        self.selected_disk = Some(0);
        self.step = Step::Options;
    }

    /// Pose the Disk screen with the preserving dual-boot choice selected.
    pub fn shot_alongside(&mut self) {
        self.step = Step::Disk;
        self.install_method = InstallMethod::Alongside;
        self.selected_disk = Some(0);
    }

    /// Pose the final review for the preserving dual-boot path.
    pub fn shot_alongside_review(&mut self) {
        self.install_method = InstallMethod::Alongside;
        self.selected_disk = Some(0);
        self.step = Step::Review;
    }

    /// Seed a non-default keyboard layout (by display name) for a
    /// `--shots --step keyboard` render, so the visual preview clearly
    /// shows remapped keys. The value is seeded into the CRDT on the first
    /// `sync` via `Self::bootstrap_crdt`.
    pub fn shot_keymap(&mut self, display_name: &str) {
        self.step = Step::Keyboard;
        self.keymap = display_name.to_string();
    }

    /// Mutate a `for_shot(Install)` app into the success/Finish screen.
    pub fn shot_finished(&mut self) {
        self.step = Step::Install;
        self.install_state = InstallState::Done;
        self.status_lines = sample_install_log();
        self.status_lines
            .push("Installed — remove the medium and reboot.".to_string());
    }

    /// Pose the finish card after the real recovery-key marker has arrived.
    /// The representative value uses the real eight-group shape closely enough
    /// to exercise wrapping without ever being a usable secret.
    pub fn shot_finished_with_recovery(&mut self) {
        self.shot_finished();
        self.recovery_key = Some(
            "aaaa1111-bbbb2222-cccc3333-dddd4444-eeee5555-ffff6666-gggg7777-hhhh8888".to_string(),
        );
    }

    /// Pose the post-copy/post-save state, including the enabled power actions.
    pub fn shot_finished_with_saved_recovery(&mut self) {
        self.shot_finished_with_recovery();
        self.recovery_key_copied = true;
        self.recovery_key_saved = true;
    }

    /// Mutate a `for_shot(Install)` app into the failure screen.
    pub fn shot_failed(&mut self) {
        self.step = Step::Install;
        self.install_state = InstallState::Failed;
        self.status_lines = sample_install_log();
        self.status_lines
            .push("error: bootc install to-disk exited non-zero".to_string());
    }

    /// Pose the failure card after encryption generated a recovery key, proving
    /// the rescue action remains available even though installation stopped.
    pub fn shot_failed_with_recovery(&mut self) {
        self.shot_failed();
        self.recovery_key = Some(
            "aaaa1111-bbbb2222-cccc3333-dddd4444-eeee5555-ffff6666-gggg7777-hhhh8888".to_string(),
        );
    }

    /// The selected disk, if any.
    fn selected_disk_ref(&self) -> Option<&Disk> {
        self.selected_disk.and_then(|i| self.disks.get(i))
    }

    /// The selected disk's device path, if any.
    fn selected_dev(&self) -> Option<String> {
        self.selected_disk_ref().map(|d| d.name.clone())
    }

    fn guided_alongside_plan(&self) -> Option<GuidedAlongsidePlan> {
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

    /// The chosen filesystem name (e.g. `btrfs`).
    fn filesystem(&self) -> &str {
        FILESYSTEMS
            .get(self.selected_fs)
            .copied()
            .unwrap_or("btrfs")
    }

    /// Resolve the selected layout's display name to its dataset entry,
    /// falling back to the default (English / US) if it doesn't match.
    fn layout(&self) -> &'static layouts::Layout {
        layouts::by_display_name(self.keymap.trim()).unwrap_or_else(layouts::default_layout)
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

    fn startup_secret(&self) -> Zeroizing<String> {
        let separate = secret_copy(&self.disk_password);
        if separate.is_empty() {
            secret_copy(&self.password)
        } else {
            separate
        }
    }

    /// May the install start, as far as encryption is concerned? Native Tilo
    /// always requires both a reproducible recovery factor and a startup
    /// secret. A short PIN is accepted only for a signed, measured target.
    fn encryption_ready(&self) -> bool {
        self.recovery_ack
            && validate_startup_secret_for(
                &self.startup_secret(),
                matches!(
                    self.verified_boot_state(),
                    VerifiedBootState::TargetVerified
                ),
            )
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
            .is_some_and(|device| self.erase_confirmation.trim() == device)
    }

    /// Human-readable summary of the encryption factors for the Review step.
    fn encryption_summary(&self) -> String {
        // Recovery is always present; the primary startup factor reflects the
        // same verified-target decision used by validation and the backend.
        let mut factors = vec!["recovery key".to_string()];
        if !self.startup_secret().trim().is_empty() {
            factors.push(
                if matches!(
                    self.verified_boot_state(),
                    VerifiedBootState::TargetVerified
                ) {
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
        format!("On (LUKS — {})", factors.join(", "))
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
            hostname: self.hostname.trim().to_string(),
            full_name: self.full_name.trim().to_string(),
            locale: self.locale.trim().to_string(),
            timezone: self.timezone.trim().to_string(),
            keymap: layout.xkb_layout.to_string(),
            keyvariant: layout.variant_or_empty().to_string(),
            pin: self.startup_secret(),
            fido2: self.fido2,
            internal_handoff: if guided.is_some() && disk.is_some_and(Disk::hosts_staged_installer)
            {
                crate::internal_handoff::HANDOFF_PATH.to_string()
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

    fn bootstrap_crdt(&self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, USERNAME_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, USERNAME_PATH, ObjType::Text)
            && !self.username.is_empty()
        {
            let _ = state.splice_text(&obj, 0, 0, &self.username);
        }
        if state.get(root, FULL_NAME_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, FULL_NAME_PATH, ObjType::Text)
            && !self.full_name.is_empty()
        {
            let _ = state.splice_text(&obj, 0, 0, &self.full_name);
        }
        // Hostname / timezone seed with sensible defaults so the fields
        // aren't blank (and validation passes out of the box).
        if state.get(root, HOSTNAME_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, HOSTNAME_PATH, ObjType::Text)
        {
            let _ = state.splice_text(&obj, 0, 0, DEFAULT_HOSTNAME);
        }
        if state.get(root, TIMEZONE_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, TIMEZONE_PATH, ObjType::Text)
        {
            let _ = state.splice_text(&obj, 0, 0, DEFAULT_TIMEZONE);
        }
        if state.get(root, LOCALE_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, LOCALE_PATH, ObjType::Text)
        {
            let _ = state.splice_text(&obj, 0, 0, &self.locale);
        }
        // Keyboard layout seeds with the current selection (default or a
        // shot-injected one), so the picker + preview aren't blank.
        if state.get(root, KEYMAP_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, KEYMAP_PATH, ObjType::Text)
        {
            let _ = state.splice_text(&obj, 0, 0, &self.keymap);
        }
        if state.get(root, ERASE_CONFIRM_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, ERASE_CONFIRM_PATH, ObjType::Text);
        }
        // The TPM2 PIN is a Text object so the masked TextInput can splice
        // into it like every other field; it starts empty (no PIN).
        for path in [
            NEXT_PATH,
            BACK_PATH,
            BEGIN_PATH,
            REBOOT_PATH,
            POWEROFF_PATH,
            RECOVERY_COPY_PATH,
            RECOVERY_SAVED_PATH,
            RECOVERY_ACK_PATH,
            NETWORK_REFRESH_PATH,
            NETWORK_CONNECT_PATH,
            DISK_REFRESH_PATH,
        ] {
            if state.get(root, path).ok().flatten().is_none() {
                let _ = state.put(root, path, false);
            }
        }
        for i in 0..self.disks.len() {
            let p = disk_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }

        for i in 0..INSTALL_METHOD_LABELS.len() {
            let p = install_method_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }
        for i in 0..self.network.snapshot.networks.len() {
            let p = network_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }
        for i in 0..FILESYSTEMS.len() {
            let p = fs_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }
        for i in 0..FIDO2_LABELS.len() {
            let p = fido2_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }
        for i in 0..CODECS_LABELS.len() {
            let p = codecs_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }
    }

    fn clear_text(state: &mut AutoCommit, root: &ObjId, path: &str) {
        if let Ok(Some((Value::Object(ObjType::Text), object))) = state.get(root, path) {
            let delete = isize::try_from(state.length(&object)).unwrap_or(isize::MAX);
            let _ = state.splice_text(&object, 0, delete, "");
        }
    }

    fn validate_current_step_details(&self) -> Result<(), String> {
        if self.step == Step::Account && !valid_full_name(self.full_name.trim()) {
            return Err(
                "Use at most 100 characters for the name, without colons or control characters"
                    .into(),
            );
        }
        if self.step == Step::Account {
            validate_passwords(
                &secret_copy(&self.password),
                &secret_copy(&self.password_confirmation),
            )?;
            let separate = secret_copy(&self.disk_password);
            if !separate.is_empty() && separate.as_str() == secret_copy(&self.password).as_str() {
                return Err("Choose a disk secret different from the account password".into());
            }
            validate_startup_secret_for(
                &self.startup_secret(),
                matches!(
                    self.verified_boot_state(),
                    VerifiedBootState::TargetVerified
                ),
            )
            .map_err(str::to_string)?;
        }
        if self.step == Step::Options {
            if !self.filesystem_policy_ready() {
                return Err(self.filesystem_policy_message());
            }
            if !valid_locale(self.locale.trim()) {
                return Err("Choose a supported language and locale from the list".into());
            }
            if !valid_timezone_name(self.timezone.trim())
                || !self
                    .timezones
                    .iter()
                    .any(|zone| zone == self.timezone.trim())
            {
                return Err("Choose a valid time zone from the list".into());
            }
        }
        Ok(())
    }

    /// Advance to the next step if the current one validates.
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
            self.set_status("Alongside needs either one unallocated GPT extent of at least 35 GiB, or a prepared empty Linux root (32 GiB+) and a 1 GiB boot plus 2 GiB EFI partition");
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
            &self.username,
            &self.hostname,
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
                        Step::Network | Step::Disk | Step::Review | Step::Install => {}
                    }
                }
                let destination = if self.step == Step::Account
                    && self
                        .profile_completion
                        .is_some_and(ProfileCompletion::options_complete)
                {
                    Some(Step::Review)
                } else {
                    next_step(self.step)
                };
                if let Some(n) = destination {
                    // Never auto-advance into Install via Next; the Begin
                    // button (begin flag) is the only entry into install.
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
        let Some(proxy) = self.event_proxy() else {
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
                    format!("Could not launch the prepared-storage check: {error}"),
                ),
            };
            let _ = proxy.send_event(UserEvent::External(format!("{prefix}{request}:{message}")));
        });
    }

    /// Step backward. A failed launch may already have formatted the selected
    /// root. On-disk metadata is not authenticated ownership proof, so never
    /// steer a failure directly into a destructive retry. Return to storage,
    /// clear the selection and require a fresh probe/review instead.
    fn go_back(&mut self) {
        if self.step == Step::Install {
            if self.install_state == InstallState::Failed {
                self.step = Step::Disk;
                self.selected_disk = None;
                self.selected_guided_extent = None;
                self.erase_confirmation.clear();
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
        // Back always follows the visible breadcrumb. A shortened forward path
        // must not trap the owner between Account and Review: Options (including
        // FIDO2) and Network remain available for deliberate changes.
        let destination = prev_step(self.step);
        if let Some(p) = destination {
            self.step = p;
            self.reset_viewport();
            self.status.clear();
            self.dirty_structure = true;
            self.focus_reset = true;
        }
    }

    /// Enter the Install step and kick off the backend. Prefers a worker
    /// thread (live streaming via the event-loop proxy); falls back to a
    /// blocking run when no proxy is wired (tests / no event loop).
    #[expect(
        clippy::too_many_lines,
        reason = "installer preflight and worker launch are one ordered operation"
    )]
    fn begin_install(&mut self, state: &AutoCommit, root: &ObjId) {
        if self.install_launched {
            return;
        }
        if !self.filesystem_policy_ready() {
            self.set_status(&self.filesystem_policy_message());
            return;
        }
        let Some(disk) = self.selected_dev() else {
            self.set_status("Select a disk to continue");
            return;
        };
        // Check immediately before consuming secrets or starting the worker.
        // The Review warning is advisory because plugging in does not rebuild
        // this static wizard page; pressing Begin probes again and proceeds as
        // soon as the machine is safely powered.
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
        // depth — the Begin button and Enter both gate on this too).
        if !self.recovery_ack {
            self.set_status("Confirm you'll save your recovery key first");
            return;
        }
        if let Err(message) = validate_startup_secret_for(
            &self.startup_secret(),
            matches!(
                self.verified_boot_state(),
                VerifiedBootState::TargetVerified
            ),
        ) {
            self.set_status(message);
            return;
        }
        let user = read_text(state, root, USERNAME_PATH).unwrap_or_default();
        let password = secret_copy(&self.password);
        let confirmation = secret_copy(&self.password_confirmation);
        if let Err(message) = validate_passwords(&password, &confirmation) {
            self.set_status(&message);
            return;
        }
        if secret_copy(&self.disk_password).as_str() == password.as_str() {
            self.set_status("Choose a disk secret different from the account password");
            return;
        }
        let opts = self.install_options();

        // The worker owns the only remaining password buffer. Remove both UI
        // fields before launch so a failed install cannot leave account
        // secrets sitting in the live session's editable document.
        clear_secret_buffer(&self.password);
        clear_secret_buffer(&self.password_confirmation);
        clear_secret_buffer(&self.disk_password);

        self.step = Step::Install;
        self.reset_viewport();
        self.install_state = InstallState::Running;
        self.install_launched = true;
        self.status_lines = vec![format!(
            "Starting install to {disk} for user {user} ({}).",
            opts.filesystem
        )];
        self.recovery_key = None;
        self.recovery_key_copied = false;
        self.recovery_key_saved = false;
        self.dirty_structure = true;
        self.focus_reset = true;

        let proxy = self
            .proxy_slot
            .as_ref()
            .and_then(|s| s.lock().ok().and_then(|g| g.clone()));

        let backend = Arc::clone(&self.backend);
        if let Some(proxy) = proxy {
            // Worker thread: stream each line back as an External event.
            std::thread::spawn(move || {
                let result = backend.run_install(&disk, &user, &password, &opts, &mut |line| {
                    let _ = proxy.send_event(UserEvent::External(line));
                });
                let terminal = match result {
                    Ok(true) => CTRL_DONE.to_string(),
                    Ok(false) => CTRL_FAIL.to_string(),
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::External(format!(
                            "error: failed to launch installer: {e}"
                        )));
                        CTRL_FAIL.to_string()
                    }
                };
                let _ = proxy.send_event(UserEvent::External(terminal));
            });
        } else {
            // Synchronous fallback (blocks the caller). Lines take the same
            // route as streamed ones, so the recovery-key marker becomes the
            // recovery card here too and never a log line.
            let mut lines = Vec::new();
            let result = backend.run_install(&disk, &user, &password, &opts, &mut |line| {
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
        self.status_lines.push(if self.internal_host().is_some() {
            "Installed — restart to start Tilo.".to_string()
        } else {
            "Installed — remove the medium and reboot.".to_string()
        });
        // Completion replaces an unfocusable progress surface with recovery
        // and power controls. Put keyboard users on the first recovery action
        // deterministically instead of retaining a stale node from Review.
        self.focus_reset = true;
        self.dirty_structure = true;
    }

    fn mark_install_failed(&mut self, extra: Option<String>) {
        if let Some(line) = extra {
            self.status_lines.push(line);
        }
        self.install_state = InstallState::Failed;
        self.reset_viewport();
        // Running progress has no focusable controls. Failure replaces it
        // with recovery-key actions and Review storage, so keyboard and
        // assistive-technology users need a fresh focus target just as they
        // do on successful completion.
        self.focus_reset = true;
        self.dirty_structure = true;
    }

    /// Run a terminal power action (Reboot / Power off). On success the
    /// machine goes down before this returns; a failure surfaces a status.
    fn run_power(&mut self, action: PowerAction) -> bool {
        // Do not deliberately leave a disk-unlock factor in the live
        // session's clipboard after handing off to the installed system. If
        // the person copied something else since, preserve their newer value.
        if let Some(key) = &self.recovery_key
            && ui::clipboard_paste().as_deref() == Some(key.as_str())
        {
            ui::clipboard_copy("");
        }
        if matches!(self.backend.run_power(action), Ok(true)) {
            true
        } else {
            self.set_status("Could not run power action — remove the medium and reboot manually.");
            false
        }
    }

    /// Build the composed card subtree for the current step.
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut children: Vec<Box<dyn Widget>> = vec![Box::new(StepIndicator::new(
            STEP_LABELS
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            step_index(self.step),
        ))];

        match self.step {
            Step::Network => self.compose_network_step(theme, &mut children),
            Step::Disk => self.compose_disk_step(theme, &mut children),
            Step::Account => self.compose_account_step(theme, &mut children),
            Step::Keyboard => Self::compose_keyboard_step(theme, &mut children),
            Step::Options => self.compose_options_step(theme, &mut children),
            Step::Review => self.compose_review_step(theme, &mut children),
            Step::Install => self.compose_install_step(theme, &mut children),
        }

        // A staged install has no medium to pull: offer the way back to the
        // retained OS on every screen except while installing or once done.
        if let Some(host) = self.internal_host()
            && !matches!(
                self.install_state,
                InstallState::Running | InstallState::Done
            )
        {
            children.push(Box::new(
                ActionButton::new(
                    &format!("Return to {}", host.display_name()),
                    RETURN_TO_HOST_PATH,
                )
                .with_test_id("return_to_host"),
            ));
        }

        // A transient validation/error line under every step's controls.
        if !self.status.is_empty() {
            children.push(Box::new(
                Text::new(&self.status)
                    .with_size(theme.tokens.typography.scale.sm)
                    .with_color(theme.tokens.colors.semantic.danger)
                    .with_live(Politeness::Assertive),
            ));
        }

        let card: Box<dyn Widget> = Box::new(Card::new(children));
        Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_content_gutter(SCROLLBAR_GUTTER)
                .with_persisted_offset(Rc::clone(&self.viewport_offset))
                .always_visible()
                .with_child(Box::new(ViewportStage::new(card))),
        )
    }

    fn compose_network_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Connect to a network").with_size(xl)));
        children.push(Box::new(
            Text::new(
                "Optional — Tilo installs from this medium and does not require internet access.",
            )
            .with_size(sm)
            .with_color(dim),
        ));
        children.push(Box::new(
            Text::new(&self.network.notice)
                .with_size(base)
                .with_color(theme.text_primary),
        ));

        if self.network.is_busy() {
            let operation = match &self.network.phase {
                NetworkPhase::Connecting(ssid) => format!("Connecting to {ssid}…"),
                _ => "Checking networks…".to_string(),
            };
            children.push(Box::new(Row::new().push(Box::new(Spinner::new())).push(
                Box::new(Text::new(&operation).with_size(sm).with_color(dim)),
            )));
        }

        self.compose_network_choices(theme, children);

        if let Some(selected) = self.network.selected_network()
            && !selected.connected
        {
            if selected.secure {
                children.push(Box::new(Text::new("Wi-Fi password").with_size(base)));
                children.push(Box::new(
                    TextInput::new(Vec::new())
                        .with_id("installer.wifi_password")
                        .with_external_buffer(Arc::clone(&self.wifi_password))
                        .with_secret(true),
                ));
            }
            children.push(Box::new(
                ActionButton::new("Connect", NETWORK_CONNECT_PATH)
                    .primary()
                    .disabled(self.network.is_busy()),
            ));
        }

        if let Some(uri) = self.network.snapshot.portal_uri.as_deref() {
            children.push(Box::new(
                Text::new(&format!("Network sign-in address: {uri}"))
                    .with_size(sm)
                    .with_color(theme.text_primary),
            ));
            children.push(Box::new(
                Text::new("For safety, the privileged live installer does not open portal web content. Continue offline, then open this address after first boot.")
                    .with_size(sm)
                    .with_color(dim),
            ));
        } else if let Some(error) = self.network.snapshot.portal_error.as_deref() {
            children.push(Box::new(
                Text::new(&format!(
                    "A sign-in page was detected but cannot be opened: {error}"
                ))
                .with_size(sm)
                .with_color(dim),
            ));
        }

        children.push(Box::new(
            Text::new("Wi-Fi connections made here are saved for first boot. You can also continue without one.")
                .with_size(sm)
                .with_color(dim),
        ));
        children.push(Box::new(
            ActionButton::new("Refresh networks", NETWORK_REFRESH_PATH)
                .disabled(self.network.is_busy()),
        ));
        let continue_label =
            if self.network.snapshot.connectivity == os_actions::ConnectivityState::Full {
                "Continue"
            } else {
                "Continue offline"
            };
        children.push(nav_row(Step::Network, NEXT_PATH, continue_label, false));
    }

    /// Add the Wi-Fi error or the currently visible network choices.
    fn compose_network_choices(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let sm = theme.tokens.typography.scale.sm;

        if let Some(error) = self.network.snapshot.wifi_error.as_deref() {
            children.push(Box::new(
                Text::new(&format!("Wi-Fi controls unavailable: {error}"))
                    .with_size(sm)
                    .with_color(dim),
            ));
        } else {
            for (i, network) in self.network.snapshot.networks.iter().take(5).enumerate() {
                let marker = if network.connected {
                    "Connected"
                } else if network.secure {
                    "Secured"
                } else {
                    "Open"
                };
                let selected = self.network.selected_ssid.as_deref() == Some(network.ssid.as_str());
                let label = format!(
                    "{}{} · {marker} · signal {}/4",
                    if selected { "✓ " } else { "" },
                    network.ssid,
                    network.signal
                );
                children.push(Box::new(
                    ActionButton::new(&label, &network_path(i))
                        .with_test_id(&format!("wifi_network_{i}"))
                        .disabled(self.network.is_busy()),
                ));
            }
        }
    }

    fn compose_disk_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let danger = theme.tokens.colors.semantic.danger;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Install Tilo").with_size(xl)));
        self.compose_install_method(theme, children);
        children.push(Box::new(
            ActionButton::new("Refresh disks", DISK_REFRESH_PATH).with_test_id("disk_refresh"),
        ));
        if self.disks.is_empty() {
            children.push(Box::new(
                Text::new("No disks found — is this running on real hardware?")
                    .with_size(base)
                    .with_color(danger),
            ));
        } else {
            for (i, disk) in self.disks.iter().enumerate() {
                children.push(Box::new(DiskRow::new(
                    disk.clone(),
                    &disk_path(i),
                    self.selected_disk == Some(i),
                )));
            }
        }
        children.extend(self.windows_alongside_preparation(theme));
        let read_only = self.selected_disk_ref().is_some_and(|disk| disk.read_only);
        let guided_plans = self.guided_alongside_plans();
        if self.install_method == InstallMethod::Alongside && guided_plans.len() > 1 {
            self.compose_guided_extent_choices(theme, children, &guided_plans);
        }
        let unavailable = read_only
            || (self.install_method == InstallMethod::Alongside
                && !self.selected_disk_can_install_alongside());
        if read_only {
            children.push(Box::new(
                Text::new("This disk is hardware or policy write-protected.")
                    .with_size(sm)
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
                Text::new(&message).with_size(sm).with_color(danger),
            ));
        } else if self.install_method == InstallMethod::Alongside
            && self.uses_guided_alongside()
            && let Some(plan) = self.guided_alongside_plan()
        {
            children.push(Box::new(
                Text::new(&format!(
                    "Ready to use {} GiB of unallocated space: a new 2 GiB Tilo EFI partition, a new 1 GiB Tilo boot partition, and a new {} GiB encrypted root. Existing partitions will not be changed.",
                    plan.free_size_bytes / (1024 * 1024 * 1024),
                    plan.root_size_bytes / (1024 * 1024 * 1024)
                ))
                    .with_size(sm)
                    .with_color(theme.status_success),
            ));
        }
        children.push(nav_row(
            Step::Disk,
            NEXT_PATH,
            "Next",
            self.selected_disk.is_none() || unavailable,
        ));
    }

    /// The Erase/Alongside choice, or — on the disk that holds a staged
    /// installer — the one plan that disk allows and what it leaves untouched.
    fn compose_install_method(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
        let sm = theme.tokens.typography.scale.sm;
        if let Some(host) = self
            .internal_host()
            .filter(|_| self.selected_disk_hosts_installer())
        {
            children.push(Box::new(
                Text::new(&format!(
                    "Tilo will be installed alongside {} in the space Tilo Imager prepared. This disk also holds the running installer; {} and its files are not changed.",
                    host.display_name(),
                    host.display_name()
                ))
                .with_size(sm)
                .with_color(dim),
            ));
        } else {
            children.push(Box::new(
                SegChoice::new(
                    INSTALL_METHOD_LABELS
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                    (0..INSTALL_METHOD_LABELS.len())
                        .map(install_method_path)
                        .collect(),
                    usize::from(self.install_method == InstallMethod::Alongside),
                )
                .with_test_id("install_method"),
            ));
        }
        let guidance = if self.install_method == InstallMethod::Erase {
            "Erase disk deletes every partition and operating system on the selected disk."
        } else {
            "Alongside preserves existing partitions. Tilo can use one unallocated GPT extent (35 GiB+) or prepared Tilo partitions. It never shrinks, moves, or deletes an existing partition."
        };
        children.push(Box::new(Text::new(guidance).with_size(sm).with_color(
            if self.install_method == InstallMethod::Erase {
                danger
            } else {
                dim
            },
        )));
    }

    fn windows_alongside_preparation(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        (self.install_method == InstallMethod::Alongside
            && self
                .selected_disk_ref()
                .is_some_and(Disk::has_windows_storage))
        .then(|| {
            Box::new(
                Text::new(WINDOWS_ALONGSIDE_PREPARATION)
                    .with_size(theme.tokens.typography.scale.sm)
                    .with_color(theme.tokens.colors.semantic.warning),
            ) as Box<dyn Widget>
        })
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
        "No safe alongside space was found. Shrink the existing OS from its own disk manager and leave at least 35 GiB unallocated, or prepare an empty Linux root and 1 GiB boot plus 2 GiB EFI partition, then refresh disks.".to_string()
    }

    fn compose_guided_extent_choices(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        plans: &[GuidedAlongsidePlan],
    ) {
        children.push(Box::new(
            Text::new("Choose the exact unallocated region for Tilo")
                .with_size(theme.tokens.typography.scale.base),
        ));
        for (index, plan) in plans.iter().enumerate() {
            let start_gib = plan.free_start_bytes / (1024 * 1024 * 1024);
            let size_gib = plan.free_size_bytes / (1024 * 1024 * 1024);
            let label = if self.selected_guided_extent == Some(index) {
                format!("✓ Selected · {size_gib} GiB free at {start_gib} GiB")
            } else {
                format!("Use {size_gib} GiB free at {start_gib} GiB")
            };
            children.push(Box::new(
                ActionButton::new(&label, &guided_extent_path(index))
                    .with_test_id(&format!("guided_extent_{index}")),
            ));
        }
        children.push(Box::new(
            Text::new("The location is measured from the start of the selected disk. Existing partitions between free regions remain untouched.")
                .with_size(theme.tokens.typography.scale.sm)
                .with_color(theme.text_dim),
        ));
    }

    fn compose_account_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        let completion = self.profile_completion.unwrap_or_default();
        let identity_complete = self
            .profile_completion
            .is_some_and(ProfileCompletion::identity_complete);
        children.push(Box::new(
            Text::new(if identity_complete {
                "Secure your account"
            } else {
                "Create your account"
            })
            .with_size(xl),
        ));
        if self.profile_completion.is_none() || !completion.username.is_complete() {
            children.push(Box::new(Text::new("Username").with_size(base)));
            children.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("installer.username")
                    .with_on_change_path(USERNAME_PATH),
            ));
            children.push(Box::new(
                Text::new("Used to sign in and for your home directory.")
                    .with_size(sm)
                    .with_color(dim),
            ));
        }
        if self.profile_completion.is_none() || !completion.full_name.is_complete() {
            children.push(Box::new(Text::new("Full name (optional)").with_size(base)));
            children.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("installer.full_name")
                    .with_on_change_path(FULL_NAME_PATH),
            ));
        }
        let tpm_pin = matches!(
            self.verified_boot_state(),
            VerifiedBootState::TargetVerified
        );
        children.push(Box::new(
            Text::new(if tpm_pin {
                "TPM-protected startup PIN"
            } else {
                "Startup and unlock passphrase"
            })
            .with_size(base),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password")
                .with_external_buffer(Arc::clone(&self.password))
                .with_secret(true),
        ));
        children.push(Box::new(
            Text::new(if tpm_pin {
                "Use at least 6 digits. The TPM rate-limits guesses; keep the recovery key for hardware changes."
            } else {
                "Choose at least 15 characters, ideally several unrelated words. It decrypts the disk at startup and unlocks your session later."
            })
            .with_size(sm)
            .with_color(dim),
        ));
        children.push(Box::new(
            Text::new(if tpm_pin {
                "Confirm TPM-protected startup PIN"
            } else {
                "Confirm startup and unlock passphrase"
            })
            .with_size(base),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password_confirm")
                .with_external_buffer(Arc::clone(&self.password_confirmation))
                .with_secret(true),
        ));
        children.push(nav_row(Step::Account, NEXT_PATH, "Next", false));
    }

    fn compose_keyboard_step(theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let xl = theme.tokens.typography.scale.xl;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Keyboard layout").with_size(xl)));
        children.push(Box::new(
            Text::new("Pick the layout that matches your keyboard. The preview below shows exactly what each key will type.")
                .with_size(sm)
                .with_color(dim),
        ));
        children.push(Box::new(
            Dropdown::new(layouts::display_names(), KEYMAP_PATH)
                .with_id("installer.keymap")
                .with_placeholder("Select a keyboard layout"),
        ));
        children.push(Box::new(KeyboardView::new(KEYMAP_PATH)));
        children.push(Box::new(
            Text::new("Need files from this computer, or removing an alongside Tilo install? Press Ctrl+Alt+F2 for recovery tools; Ctrl+Alt+F1 returns here.")
                .with_size(sm)
                .with_color(dim),
        ));
        children.push(nav_row(Step::Keyboard, NEXT_PATH, "Next", false));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one linear list of the Options step's choices, each with its explanation"
    )]
    fn compose_options_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;
        let completion = self.profile_completion.unwrap_or_default();

        children.push(Box::new(Text::new("Installation options").with_size(xl)));

        if self.profile_completion.is_none() || !completion.filesystem.is_complete() {
            self.compose_filesystem_choice(theme, children);
        }

        children.push(Box::new(Text::new("Disk encryption").with_size(base)));
        children.push(Box::new(
            Text::new("Always on · LUKS2")
                .with_size(base)
                .with_color(theme.status_success),
        ));
        children.push(Box::new(
            Text::new(encrypt_description(self.verified_boot_state()))
                .with_size(sm)
                .with_color(dim),
        ));

        children.push(Box::new(Text::new("Security key").with_size(base)));
        children.push(Box::new(
            SegChoice::new(
                FIDO2_LABELS
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                (0..FIDO2_LABELS.len()).map(fido2_path).collect(),
                usize::from(self.fido2),
            )
            .with_test_id("fido2_choice"),
        ));
        children.push(Box::new(
            Text::new("Optionally enroll a FIDO2 security key (e.g. a YubiKey) as another way to unlock. Plug it in before installing.")
                .with_size(sm)
                .with_color(dim),
        ));

        if self.profile_completion.is_none() || !completion.hostname.is_complete() {
            children.push(Box::new(Text::new("Hostname").with_size(base)));
            children.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("installer.hostname")
                    .with_on_change_path(HOSTNAME_PATH),
            ));
            children.push(Box::new(
                Text::new("The name your computer uses on the network.")
                    .with_size(sm)
                    .with_color(dim),
            ));
        }

        if self.profile_completion.is_none() || !completion.locale.is_complete() {
            children.push(Box::new(Text::new("Language & locale").with_size(base)));
            children.push(Box::new(
                Dropdown::new(crate::locales::list(), LOCALE_PATH)
                    .with_id("installer.locale")
                    .with_accessible_name("Language & locale")
                    .with_placeholder("Choose a language and locale"),
            ));
            children.push(Box::new(
                Text::new("This controls language, formatting, and regional conventions after installation.")
                    .with_size(sm)
                    .with_color(dim),
            ));
        }

        if self.profile_completion.is_none() || !completion.timezone.is_complete() {
            children.push(Box::new(Text::new("Time zone").with_size(base)));
            children.push(Box::new(
                Dropdown::new(self.timezones.clone(), TIMEZONE_PATH)
                    .with_id("installer.timezone")
                    .with_placeholder("Select a time zone")
                    .open_if(self.tz_open_for_shot),
            ));
            children.push(Box::new(
                Text::new("Pick from the list — start typing to filter (IANA name, e.g. Europe/Amsterdam or UTC).")
                    .with_size(sm)
                    .with_color(dim),
            ));
        }

        children.push(Box::new(Text::new("Media codecs").with_size(base)));
        children.push(Box::new(
            SegChoice::new(
                CODECS_LABELS
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                (0..CODECS_LABELS.len()).map(codecs_path).collect(),
                usize::from(self.codecs),
            )
            .with_test_id("codecs_choice"),
        ));
        children.push(Box::new(
            Text::new(CODECS_DESCRIPTION).with_size(sm).with_color(dim),
        ));

        children.push(nav_row(
            Step::Options,
            NEXT_PATH,
            "Next",
            !self.filesystem_policy_ready(),
        ));
    }

    fn compose_filesystem_choice(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;
        children.push(Box::new(Text::new("Filesystem").with_size(base)));
        let choices = self.available_filesystem_indices();
        if choices.is_empty() {
            children.push(Box::new(
                Text::new(&self.filesystem_policy_message())
                    .with_size(sm)
                    .with_color(theme.tokens.colors.semantic.danger),
            ));
            return;
        }
        let selected = choices
            .iter()
            .position(|index| *index == self.selected_fs)
            .unwrap_or(0);
        children.push(Box::new(SegChoice::new(
            choices
                .iter()
                .filter_map(|index| FILESYSTEMS.get(*index).map(|name| (*name).to_string()))
                .collect(),
            choices.iter().map(|index| fs_path(*index)).collect(),
            selected,
        )));
        children.push(Box::new(
            Text::new(fs_description(self.selected_fs))
                .with_size(sm)
                .with_color(theme.text_dim),
        ));
    }

    fn compose_destructive_confirmation(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        disk: &str,
    ) {
        let (expected, operation) = match self.install_method {
            InstallMethod::Erase => (disk, "whole-disk erase"),
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
            Text::new(&format!("Type {expected} to confirm the {operation}"))
                .with_size(theme.tokens.typography.scale.sm)
                .with_color(theme.text_dim),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.erase_confirmation")
                .with_placeholder(expected)
                .with_on_change_path(ERASE_CONFIRM_PATH),
        ));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the review is one linear visual composition and splitting it would obscure its exact destructive-plan ordering"
    )]
    fn compose_review_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Review your choices").with_size(xl)));

        let dev = self
            .selected_dev()
            .unwrap_or_else(|| "<no disk>".to_string());
        let disk_value = match self.selected_disk_ref() {
            Some(d) if d.model.is_empty() => format!("{} ({})", d.name, d.size),
            Some(d) => format!("{} ({}, {})", d.name, d.size, d.model),
            None => "<no disk>".to_string(),
        };
        children.push(summary_row(theme, "Disk", &disk_value));
        children.push(summary_row(
            theme,
            "Install method",
            if self.install_method == InstallMethod::Alongside {
                "Alongside — preserve existing partitions"
            } else {
                "Erase the entire disk"
            },
        ));
        if self.install_method == InstallMethod::Alongside
            && let Some(disk) = self.selected_disk_ref()
        {
            if let Some(plan) = self.guided_alongside_plan() {
                children.push(summary_row(
                    theme,
                    "Partition plan",
                    &format!(
                        "Create 2 GiB Tilo EFI + {} GiB Tilo root in {} GiB unallocated space",
                        plan.root_size_bytes / (1024 * 1024 * 1024),
                        plan.free_size_bytes / (1024 * 1024 * 1024)
                    ),
                ));
                children.push(summary_row(
                    theme,
                    "Existing partitions",
                    "Preserved byte-for-byte; none resized, moved, or deleted",
                ));
                if let Some(host) = self
                    .internal_host()
                    .filter(|_| self.selected_disk_hosts_installer())
                {
                    let host = host.display_name();
                    children.push(summary_row(
                        theme,
                        "This disk",
                        &format!(
                            "Also holds the running installer and your existing {host} system. {host} stays untouched."
                        ),
                    ));
                    children.push(summary_row(
                        theme,
                        "Installer space",
                        "Returned to Tilo automatically after you first sign in",
                    ));
                    children.push(summary_row(
                        theme,
                        "Choose an OS",
                        &format!(
                            "The next restart starts Tilo once; {host} stays the firmware default"
                        ),
                    ));
                } else {
                    children.push(summary_row(
                        theme,
                        "Choose an OS",
                        "Use the firmware boot picker; its current default and existing entries are preserved",
                    ));
                }
            } else {
                children.push(summary_row(
                    theme,
                    "Tilo root",
                    disk.alongside_root()
                        .map_or("<missing>", |p| p.name.as_str()),
                ));
                children.push(summary_row(
                    theme,
                    "Tilo EFI partition",
                    disk.efi_partition()
                        .map_or("<missing>", |p| p.name.as_str()),
                ));
                children.push(summary_row(
                    theme,
                    "Choose an OS",
                    "Tilo adds safe menu shortcuts for unambiguous Windows/Linux loaders on this shared EFI partition",
                ));
            }
        }
        children.push(summary_row(theme, "Filesystem", self.filesystem()));
        children.push(summary_row(theme, "Encryption", &self.encryption_summary()));
        children.push(summary_row(theme, "Hostname", self.hostname.trim()));
        children.push(summary_row(theme, "Time zone", self.timezone.trim()));
        children.push(summary_row(theme, "Keyboard layout", self.keymap.trim()));
        children.push(summary_row(
            theme,
            "Media codecs",
            if self.codecs {
                "Download from RPM Fusion and Cisco once the new system is online"
            } else {
                "Not installed (available later in Settings, Updates)"
            },
        ));
        children.push(summary_row(theme, "Username", self.username.trim()));
        if !self.full_name.trim().is_empty() {
            children.push(summary_row(theme, "Name", self.full_name.trim()));
        }
        children.push(summary_row(
            theme,
            "Network",
            &self.network.connection_summary(),
        ));
        children.push(Box::new(
            Text::new("This install uses the Tilo image on this medium; it does not download an update. Future system updates come from the signature-required Tilo repository and are checked automatically after first boot whenever the computer is online.")
                .with_size(sm)
                .with_color(dim),
        ));

        if let Some(percent) = (self.power_probe)() {
            children.push(Box::new(
                Text::new(&connect_power_message(percent))
                    .with_size(base)
                    .with_color(theme.tokens.colors.semantic.warning),
            ));
        }

        let impact = if self.install_method == InstallMethod::Alongside {
            alongside_install_impact(self.uses_guided_alongside(), &dev)
        } else {
            format!("This will ERASE {dev} and all data on it.")
        };
        children.push(Box::new(
            Text::new(&impact).with_size(base).with_color(danger),
        ));
        self.compose_destructive_confirmation(theme, children, &dev);

        // Encryption generates a recovery key shown ONCE during the
        // install. Make the user explicitly acknowledge they'll save
        // it before they can proceed — otherwise a lost TPM = lost
        // data with no warning.
        children.push(Box::new(
            Text::new("A recovery key will be shown during install — it is your only backup if the TPM is reset or fails. Save it somewhere safe.")
                .with_size(sm)
                .with_color(dim),
        ));
        let ack_label = if self.recovery_ack {
            "[x] I will save my recovery key"
        } else {
            "[ ] I will save my recovery key"
        };
        children.push(Box::new(
            ActionButton::new(ack_label, RECOVERY_ACK_PATH)
                .with_test_id("recovery_ack")
                .checkbox(self.recovery_ack),
        ));

        // Begin is gated on the recovery-key acknowledgement when
        // encrypting (otherwise always enabled).
        let begin_disabled = !self.encryption_ready()
            || !self.destructive_confirmation_ready()
            || !self.filesystem_policy_ready();
        children.push(nav_row(
            Step::Review,
            BEGIN_PATH,
            "Begin Install",
            begin_disabled,
        ));
    }

    fn compose_install_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
        let xl = theme.tokens.typography.scale.xl;
        let sm = theme.tokens.typography.scale.sm;
        let friendly_status = friendly_install_status(&self.status_lines);

        match self.install_state {
            InstallState::Done => {
                children.push(Box::new(
                    Text::new("Tilo is installed")
                        .with_size(xl)
                        .with_color(theme.status_success)
                        .with_role(TextRole::Title)
                        .with_live(Politeness::Polite),
                ));
                let completion = match self.internal_host() {
                    Some(host) => format!(
                        "Installation complete. Restart to start Tilo; {} stays the default in the firmware boot order. The temporary installer space is returned to Tilo after you first sign in. Signed system updates will be checked automatically after first boot when online; no update was downloaded during this install.",
                        host.display_name()
                    ),
                    None => "Installation complete. Remove the installer medium, then reboot. Signed system updates will be checked automatically after first boot when online; no update was downloaded during this install.".to_string(),
                };
                children.push(Box::new(
                    Text::new(&completion).with_size(sm).with_color(dim),
                ));
                if self.install_method == InstallMethod::Alongside {
                    children.push(Box::new(
                        Text::new(if self.uses_guided_alongside() {
                            "To choose Tilo or your existing system, open this computer's firmware boot picker. The installer preserved its previous default."
                        } else {
                            "The Tilo boot menu includes each unambiguous Windows or Linux loader found on the shared EFI partition; firmware choices remain available too."
                        })
                        .with_size(sm)
                        .with_color(dim),
                    ));
                }
                self.compose_recovery_key(theme, children);
                children.push(Box::new(StatusLog::new(friendly_status, STATUS_LOG_HEIGHT)));
                if !self.status.is_empty() {
                    children.push(Box::new(
                        Text::new(&self.status)
                            .with_size(sm)
                            .with_color(theme.tokens.colors.semantic.warning)
                            .with_live(Politeness::Assertive),
                    ));
                }
                children.push(finish_row(!self.recovery_key_saved));
            }
            InstallState::Failed => {
                children.push(Box::new(
                    Text::new("Installation failed")
                        .with_size(xl)
                        .with_color(danger)
                        .with_role(TextRole::Title)
                        .with_live(Politeness::Assertive),
                ));
                children.push(Box::new(
                    Text::new("The installer stopped before it could confirm a bootable system. The destination may already contain a partial encrypted installation. Tilo will not treat on-disk names, labels, or metadata as permission to erase it. Save the recovery key below if one was generated, then review storage or use Ctrl+Alt+F2 for recovery tools.")
                        .with_size(sm)
                        .with_color(dim),
                ));
                children.push(Box::new(Text::new("Details").with_size(sm).with_color(dim)));
                self.compose_recovery_key(theme, children);
                children.push(Box::new(StatusLog::new(
                    self.status_lines.clone(),
                    STATUS_LOG_HEIGHT,
                )));
                children.push(Box::new(ActionButton::new("Review storage", BACK_PATH)));
            }
            _ => {
                children.push(Box::new(
                    Text::new("Installing Tilo…")
                        .with_size(xl)
                        .with_role(TextRole::Title),
                ));
                children.push(Box::new(
                    Row::new().push(Box::new(Spinner::new())).push(Box::new(
                        Text::new("Working — this can take a few minutes. Don't power off.")
                            .with_size(sm)
                            .with_color(dim),
                    )),
                ));
                self.compose_recovery_key(theme, children);
                children.push(Box::new(StatusLog::new(friendly_status, STATUS_LOG_HEIGHT)));
            }
        }
    }

    fn compose_recovery_key(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let Some(key) = &self.recovery_key else {
            return;
        };
        let sm = theme.tokens.typography.scale.sm;
        children.push(Box::new(
            Text::new("Recovery key — save this somewhere off-device")
                .with_size(sm)
                .with_color(theme.status_success),
        ));
        children.push(Box::new(Text::new(key).with_size(sm)));
        children.push(Box::new(
            ActionButton::new(
                if self.recovery_key_copied {
                    "Copy recovery key again"
                } else {
                    "Copy recovery key"
                },
                RECOVERY_COPY_PATH,
            )
            .with_test_id("recovery_key_copy"),
        ));
        children.push(Box::new(
            Text::new(if self.recovery_key_copied {
                "Copied. Paste it into a password manager or an encrypted note on another drive before rebooting."
            } else {
                "Copy it into a password manager or an encrypted note on another drive. The installer clears its clipboard copy when you reboot or power off."
            })
            .with_size(sm)
            .with_color(if self.recovery_key_copied {
                theme.status_success
            } else {
                theme.text_dim
            })
            .with_live(Politeness::Polite),
        ));
        let saved_label = if self.recovery_key_saved {
            "[x] I saved this key off this computer"
        } else {
            "[ ] I saved this key off this computer"
        };
        children.push(Box::new(
            ActionButton::new(saved_label, RECOVERY_SAVED_PATH)
                .with_test_id("recovery_key_saved")
                .checkbox(self.recovery_key_saved),
        ));
    }
}

/// A navigation row for `step`: `Back` (secondary) — shown only when the
/// step has a predecessor, so the first step has no dangling Back — plus the
/// forward action (`forward_label`, primary, firing `forward_path`, optionally
/// disabled). A real `ui::Row` so its buttons stay in the focusable order
/// (widget tree mirrors taffy).
fn nav_row(
    step: Step,
    forward_path: &str,
    forward_label: &str,
    forward_disabled: bool,
) -> Box<dyn Widget> {
    let mut row = Row::new();
    if prev_step(step).is_some() {
        row = row.push(Box::new(ActionButton::new("Back", BACK_PATH)));
    }
    row = row.push(Box::new(
        ActionButton::new(forward_label, forward_path)
            .primary()
            .disabled(forward_disabled),
    ));
    Box::new(row)
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

/// The Finish-screen action row: Reboot (primary) + Power off (secondary).
fn finish_row(disabled: bool) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(Box::new(
                ActionButton::new("Reboot now", REBOOT_PATH)
                    .primary()
                    .disabled(disabled),
            ))
            .push(Box::new(
                ActionButton::new("Power off", POWEROFF_PATH).disabled(disabled),
            )),
    )
}

/// One `label: value` review line (label dim, value primary).
fn summary_row(theme: &Theme, label: &str, value: &str) -> Box<dyn Widget> {
    let sm = theme.tokens.typography.scale.sm;
    let base = theme.tokens.typography.scale.base;
    Box::new(
        Row::new()
            .push(Box::new(
                Text::new(&format!("{label}:"))
                    .with_size(sm)
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(Text::new(value).with_size(base))),
    )
}

/// A small, deterministic time-zone list used by [`InstallerApp::with_disks`]
/// (tests + headless shots). [`InstallerApp::new`] replaces it with the full
/// system list. Long enough that the open dropdown scrolls in `--shots`.
fn default_timezones() -> Vec<String> {
    [
        "UTC",
        "Europe/Amsterdam",
        "Europe/Berlin",
        "Europe/London",
        "Europe/Madrid",
        "Europe/Paris",
        "America/Chicago",
        "America/Denver",
        "America/Los_Angeles",
        "America/New_York",
        "America/Sao_Paulo",
        "Asia/Dubai",
        "Asia/Kolkata",
        "Asia/Shanghai",
        "Asia/Tokyo",
        "Australia/Sydney",
        "Pacific/Auckland",
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect()
}

/// Two representative disks for headless `--shots` rendering.
fn sample_disks() -> Vec<Disk> {
    vec![
        Disk {
            name: "/dev/sda".to_string(),
            size: "238.5G".to_string(),
            model: "Samsung SSD 860 EVO".to_string(),
            size_bytes: 256_000_000_000,
            partition_table: "gpt".to_string(),
            partition_table_uuid: "11111111-1111-1111-1111-111111111111".to_string(),
            logical_sector_size: 512,
            major_minor: "8:0".to_string(),
            wwn: "0x5002538e40abcdef".to_string(),
            serial: "TILO-SAMPLE-SATA".to_string(),
            removable: false,
            read_only: false,
            is_system: false,
            live_system_nodes: Vec::new(),
            internal_plan: None,
            partitions: vec![
                DiskPartition {
                    name: "/dev/sda1".to_string(),
                    start_bytes: 1_048_576,
                    size_bytes: 536_870_912,
                    part_uuid: "00000000-0000-0000-0000-000000000001".to_string(),
                    filesystem: "vfat".to_string(),
                    label: "EFI".to_string(),
                    mountpoint: String::new(),
                    part_type: EFI_SYSTEM_GUID.to_string(),
                },
                DiskPartition {
                    name: "/dev/sda2".to_string(),
                    start_bytes: 537_919_488,
                    size_bytes: 160_000_000_000,
                    part_uuid: "00000000-0000-0000-0000-000000000002".to_string(),
                    filesystem: "ntfs".to_string(),
                    label: "Windows".to_string(),
                    mountpoint: String::new(),
                    part_type: "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7".to_string(),
                },
                DiskPartition {
                    name: "/dev/sda3".to_string(),
                    start_bytes: 160_537_919_488,
                    size_bytes: 64_000_000_000,
                    part_uuid: "00000000-0000-0000-0000-000000000003".to_string(),
                    filesystem: String::new(),
                    label: String::new(),
                    mountpoint: String::new(),
                    part_type: LINUX_FILESYSTEM_GUID.to_string(),
                },
                DiskPartition {
                    name: "/dev/sda4".to_string(),
                    start_bytes: 224_537_919_488,
                    size_bytes: 2_147_483_648,
                    part_uuid: "00000000-0000-0000-0000-000000000004".to_string(),
                    filesystem: "vfat".to_string(),
                    label: "Tilo EFI".to_string(),
                    mountpoint: String::new(),
                    part_type: EFI_SYSTEM_GUID.to_string(),
                },
                DiskPartition {
                    name: "/dev/sda5".to_string(),
                    start_bytes: 226_685_403_136,
                    size_bytes: 1_073_741_824,
                    part_uuid: "00000000-0000-0000-0000-000000000005".to_string(),
                    filesystem: String::new(),
                    label: String::new(),
                    mountpoint: String::new(),
                    part_type: LINUX_FILESYSTEM_GUID.to_string(),
                },
            ],
        },
        Disk {
            name: "/dev/nvme0n1".to_string(),
            size: "1T".to_string(),
            model: "WD Black SN770".to_string(),
            size_bytes: 1_000_000_000_000,
            partition_table: "gpt".to_string(),
            partition_table_uuid: "22222222-2222-2222-2222-222222222222".to_string(),
            logical_sector_size: 512,
            major_minor: "259:0".to_string(),
            wwn: "eui.0000000000000001".to_string(),
            serial: "TILO-SAMPLE-NVME".to_string(),
            removable: false,
            read_only: false,
            is_system: false,
            live_system_nodes: Vec::new(),
            internal_plan: None,
            partitions: Vec::new(),
        },
    ]
}

/// Representative streamed backend output for `--shots`. Includes long
/// lines that wrap so the status log's wrap-aware tailing is exercised.
fn sample_install_log() -> Vec<String> {
    vec![
        "Starting install to /dev/sda for user quinten (btrfs).".to_string(),
        "==> Copying the running Tilo image into local container storage".to_string(),
        "Copying local image docker://localhost/tilo-installer:latest to containers-storage:localhost/bootc".to_string(),
        "==> Installing Tilo onto /dev/sda (this ERASES the disk)".to_string(),
        "  base system installed.".to_string(),
        "==> Preparing the 'quinten' account on the installed system".to_string(),
        "==> Setting hostname 'tilo' and time zone 'UTC'".to_string(),
    ]
}

/// Turn noisy container/partitioning output into the small set of milestones a
/// person needs while the install is running. The complete transcript is still
/// retained in `status_lines` and is shown unchanged on failure.
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
            add("✓ Installation choices verified");
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
            // This is not background progress: systemd-cryptenroll is blocked
            // until the person touches their selected hardware token. Keep it
            // as an explicit stage so the visible log and its assertive live
            // region both ask for the required action.
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
        } else if lower.contains("installed — remove the medium") {
            add("✓ Tilo is ready");
        }
    }

    if stages.is_empty() {
        stages.push("Preparing installation…".to_string());
    }
    let keep_from = stages.len().saturating_sub(5);
    stages.split_off(keep_from)
}

impl InstallerApp {
    /// Cache the editable text fields for display / backend argv.
    fn sync_cached_fields(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(u) = read_text(state, root, USERNAME_PATH) {
            self.username = u;
        }
        if let Some(name) = read_text(state, root, FULL_NAME_PATH) {
            self.full_name = name;
        }
        if let Some(h) = read_text(state, root, HOSTNAME_PATH) {
            self.hostname = h;
        }
        if let Some(t) = read_text(state, root, TIMEZONE_PATH) {
            self.timezone = t;
        }
        if let Some(locale) = read_text(state, root, LOCALE_PATH) {
            self.locale = locale;
        }
        if let Some(k) = read_text(state, root, KEYMAP_PATH) {
            self.keymap = k;
        }
        if let Some(confirmation) = read_text(state, root, ERASE_CONFIRM_PATH)
            && self.erase_confirmation != confirmation
        {
            let was_ready = self.destructive_confirmation_ready();
            let had_status = !self.status.is_empty();
            self.erase_confirmation = confirmation;
            self.status.clear();
            // TextInput already owns the edited text. Rebuilding its widget
            // on every keystroke discards the live caret/focus state; only
            // rebuild when the Begin button's enabled state or the status
            // line actually changes.
            self.dirty_structure |=
                had_status || was_ready != self.destructive_confirmation_ready();
        }
    }

    fn event_proxy(&self) -> Option<EventLoopProxy<UserEvent>> {
        self.proxy_slot
            .as_ref()
            .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
    }

    fn ensure_network_paths(&self, state: &mut AutoCommit, root: &ObjId) {
        for i in 0..self.network.snapshot.networks.len() {
            let path = network_path(i);
            if state.get(root, path.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, path.as_str(), false);
            }
        }
    }

    fn ensure_disk_paths(&self, state: &mut AutoCommit, root: &ObjId) {
        for i in 0..self.disks.len() {
            let path = disk_path(i);
            if state.get(root, path.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, path.as_str(), false);
            }
        }
        let extent_count = self
            .disks
            .iter()
            .map(|disk| disk.guided_alongside_plans().len())
            .max()
            .unwrap_or(0);
        for i in 0..extent_count {
            let path = guided_extent_path(i);
            if state.get(root, path.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, path.as_str(), false);
            }
        }
    }

    fn refresh_disks_with(&mut self, disks: Vec<Disk>) {
        self.disks = disks;
        self.selected_disk = None;
        self.selected_guided_extent = None;
        self.guided_efi_partuuid = uuid::Uuid::new_v4().to_string();
        self.guided_boot_partuuid = uuid::Uuid::new_v4().to_string();
        self.guided_root_partuuid = uuid::Uuid::new_v4().to_string();
        self.erase_confirmation.clear();
        self.storage_preflight_request = None;
        self.storage_preflight_passed = false;
        if self.internal_handoff.is_some() && !self.bind_internal_handoff() {
            // Discovery no longer matches the staged plan: fall back to the
            // ordinary rules, which keep the running-system disk unavailable.
            self.internal_handoff = None;
        }
        self.status = if self.disks.is_empty() {
            "Disk list refreshed; no installable disks were found.".to_string()
        } else {
            "Disk list refreshed. Select a destination again.".to_string()
        };
        self.dirty_structure = true;
        self.focus_reset = true;
    }

    /// Drain one rising-edge control flag.
    fn take_flag(state: &mut AutoCommit, root: &ObjId, path: &str) -> bool {
        if read_bool(state, root, path).unwrap_or(false) {
            let _ = state.put(root, path, false);
            true
        } else {
            false
        }
    }

    fn sync_network_rising_edges(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let proxy = self.event_proxy();
        if self.network.ensure_initial_scan(proxy) {
            self.dirty_structure = true;
        }
        self.ensure_network_paths(state, root);

        for i in 0..self.network.snapshot.networks.len() {
            if Self::take_flag(state, root, network_path(i).as_str())
                && let Some(ssid) = self
                    .network
                    .snapshot
                    .networks
                    .get(i)
                    .map(|network| network.ssid.clone())
            {
                self.invoke_control(
                    state,
                    root,
                    op::NETWORK,
                    "select",
                    args(serde_json::json!({ "ssid": ssid })),
                    None,
                );
            }
        }

        if Self::take_flag(state, root, NETWORK_REFRESH_PATH) {
            self.invoke_control(state, root, op::NETWORK, "refresh", args_none(), None);
        }

        if Self::take_flag(state, root, NETWORK_CONNECT_PATH) {
            let password = take_secret(&self.wifi_password);
            let mut connect = serde_json::Map::new();
            if !password.is_empty() {
                connect.insert(
                    "password".into(),
                    serde_json::Value::String(password.to_string()),
                );
            }
            self.invoke_control(state, root, op::NETWORK, "connect", connect, None);
        }
    }

    /// Disk / method / region / filesystem / FIDO2 selector rising edges.
    /// Each control raises a per-option flag; draining it invokes the same
    /// published action an external caller would.
    fn sync_selection_rising_edges(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.ensure_disk_paths(state, root);
        if Self::take_flag(state, root, DISK_REFRESH_PATH) {
            for i in 0..self.disks.len() {
                let _ = state.put(root, disk_path(i).as_str(), false);
            }
            for i in 0..self
                .disks
                .iter()
                .map(|disk| disk.guided_alongside_plans().len())
                .max()
                .unwrap_or(0)
            {
                let _ = state.put(root, guided_extent_path(i).as_str(), false);
            }
            self.invoke_control(state, root, op::STORAGE, "refresh", args_none(), None);
            return;
        }

        for i in 0..self.disks.len() {
            if Self::take_flag(state, root, disk_path(i).as_str())
                && let Some(device) = self.disks.get(i).map(|disk| disk.name.clone())
            {
                self.invoke_control(
                    state,
                    root,
                    &op::disk_resource(&device),
                    "select",
                    args_none(),
                    None,
                );
            }
        }

        for i in 0..INSTALL_METHOD_LABELS.len() {
            if Self::take_flag(state, root, install_method_path(i).as_str()) {
                let method = if i == 1 { "alongside" } else { "erase" };
                self.invoke_control(
                    state,
                    root,
                    op::STORAGE,
                    "set-method",
                    args(serde_json::json!({ "method": method })),
                    None,
                );
            }
        }

        for i in 0..self.guided_alongside_plans().len() {
            if Self::take_flag(state, root, guided_extent_path(i).as_str()) {
                self.invoke_control(
                    state,
                    root,
                    &op::region_resource(i),
                    "select",
                    args_none(),
                    None,
                );
            }
        }

        for (i, filesystem) in FILESYSTEMS.iter().enumerate() {
            if Self::take_flag(state, root, fs_path(i).as_str()) {
                self.invoke_control(
                    state,
                    root,
                    op::OPTIONS,
                    "set-filesystem",
                    args(serde_json::json!({ "filesystem": filesystem })),
                    None,
                );
            }
        }

        for i in 0..FIDO2_LABELS.len() {
            if Self::take_flag(state, root, fido2_path(i).as_str()) {
                self.invoke_control(
                    state,
                    root,
                    op::OPTIONS,
                    "set-security-key",
                    args(serde_json::json!({ "enabled": i == 1 })),
                    None,
                );
            }
        }

        for i in 0..CODECS_LABELS.len() {
            if Self::take_flag(state, root, codecs_path(i).as_str()) {
                self.invoke_control(
                    state,
                    root,
                    op::OPTIONS,
                    "set-media-codecs",
                    args(serde_json::json!({ "enabled": i == 1 })),
                    None,
                );
            }
        }
    }

    /// Recovery-key, wizard-navigation (Back/Next/Begin) and Finish-screen
    /// (Reboot/Power-off) rising edges, each through its published action.
    fn sync_navigation_rising_edges(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if Self::take_flag(state, root, RECOVERY_ACK_PATH) {
            let acknowledged = !self.recovery_ack;
            self.invoke_control(
                state,
                root,
                op::REVIEW,
                "set-recovery-promise",
                args(serde_json::json!({ "acknowledged": acknowledged })),
                None,
            );
        }

        if Self::take_flag(state, root, RECOVERY_COPY_PATH) && self.recovery_key.is_some() {
            self.invoke_control(state, root, op::RECOVERY, "copy", args_none(), None);
        }

        if Self::take_flag(state, root, RECOVERY_SAVED_PATH) && self.recovery_key.is_some() {
            let saved = !self.recovery_key_saved;
            self.invoke_control(
                state,
                root,
                op::RECOVERY,
                "confirm-saved",
                args(serde_json::json!({ "saved": saved })),
                None,
            );
        }

        if Self::take_flag(state, root, BACK_PATH) {
            self.invoke_control(state, root, op::SESSION, "back", args_none(), None);
        }

        if Self::take_flag(state, root, NEXT_PATH) {
            self.invoke_control(state, root, op::SESSION, "next", args_none(), None);
        }

        // Begin is only a control on the Review step; elsewhere the flag is
        // consumed without effect, exactly as before.
        if Self::take_flag(state, root, BEGIN_PATH) && self.step == Step::Review {
            let typed = self.erase_confirmation.clone();
            self.invoke_control(state, root, op::REVIEW, "begin", args_none(), Some(&typed));
        }

        if Self::take_flag(state, root, RETURN_TO_HOST_PATH) && self.internal_host().is_some() {
            self.invoke_control(
                state,
                root,
                op::SESSION,
                "return-to-host",
                args_none(),
                None,
            );
        }

        let finished = self.step == Step::Install && self.install_state == InstallState::Done;
        if Self::take_flag(state, root, REBOOT_PATH) && finished {
            self.invoke_control(state, root, op::POWER, "reboot", args_none(), None);
        }
        if Self::take_flag(state, root, POWEROFF_PATH) && finished {
            self.invoke_control(state, root, op::POWER, "power-off", args_none(), None);
        }
    }

    /// One backend output line: a terminal sentinel, the recovery-key
    /// marker (which becomes the recovery card and never a log line), or a
    /// line of the install log.
    fn ingest_backend_line(&mut self, payload: &str) {
        if payload == CTRL_DONE {
            self.mark_install_done();
        } else if payload == CTRL_FAIL {
            self.mark_install_failed(None);
        } else if let Some(key) = payload.strip_prefix(RECOVERY_KEY_PREFIX) {
            if !key.trim().is_empty() {
                self.recovery_key = Some(key.trim().to_string());
                self.recovery_key_copied = false;
                self.recovery_key_saved = false;
                self.dirty_structure = true;
            }
        } else {
            self.status_lines.push(payload.to_string());
            self.dirty_structure = true;
        }
    }
}

fn args(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn args_none() -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::new()
}

impl Widget for InstallerApp {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::undo::UndoPoints
    );

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, root);
            self.bootstrap_done = true;
        }

        self.sync_cached_fields(state, root);
        self.sync_network_rising_edges(state, root);
        self.sync_selection_rising_edges(state, root);
        self.sync_navigation_rising_edges(state, root);

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition(theme);
        let card_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // `new_with_children` only errors for a child `NodeId` from a
        // different tree, which never happens here since `card_node` was
        // just built in this same tree by the call above.
        #[allow(
            clippy::unwrap_used,
            reason = "card_node was just built in this same tree"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    justify_content: Some(JustifyContent::Center),
                    align_items: Some(AlignItems::Center),
                    size: Size {
                        width: percent(1.0_f32),
                        height: percent(1.0_f32),
                    },
                    ..Default::default()
                },
                &[card_node],
            )
            .unwrap();

        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&card_node) = children.first()
        {
            c.apply_layout(taffy, card_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Forward to the composed card FIRST so a focused field can
        // consume the event before the wizard interprets it. This matters
        // for the Options-step time-zone `Dropdown`: when it's open, Enter
        // selects an option (and the Dropdown marks the event handled) —
        // without forwarding first we'd also raise `NEXT` and skip the
        // step out from under the user.
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&card_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, card_node));
        }

        // Enter advances only when no inner field consumed it: Begin on
        // Review, Next on Disk/Account/Options. We mark the event handled
        // so the App runs its post-event `sync` (which detects the rising
        // edge).
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
                    // Enter must honour the same gate as the Begin button:
                    // when encrypting, the recovery-key acknowledgement is
                    // required before the install can start.
                    if self.encryption_ready() && self.destructive_confirmation_ready() {
                        let _ = ctx.state.put(ctx.path, BEGIN_PATH, true);
                    }
                    *ctx.handled = true;
                }
                Step::Network => {
                    if self
                        .network
                        .selected_network()
                        .is_some_and(|network| !network.connected)
                    {
                        let _ = ctx.state.put(ctx.path, NETWORK_CONNECT_PATH, true);
                    } else {
                        let _ = ctx.state.put(ctx.path, NEXT_PATH, true);
                    }
                    *ctx.handled = true;
                }
                Step::Disk | Step::Account | Step::Keyboard | Step::Options => {
                    let _ = ctx.state.put(ctx.path, NEXT_PATH, true);
                    *ctx.handled = true;
                }
                Step::Install => {}
            }
        }
    }

    fn on_external_event(&mut self, payload: &str, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if payload == CONSOLE_WAKE {
            self.serve_console(state, root);
        } else if payload == NETWORK_WAKE_EVENT {
            if self.network.drain_results() {
                self.ensure_network_paths(state, root);
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

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        op::HANDLERS
    }

    /// The host door (Ctrl+K palette, broker) admits an installer operation
    /// here, after its own resolution, permission and confirmation checks.
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped().filter(|r| r.type_id == op::OPERATION_TYPE) else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        let encoded = tilo_actions::EncodedAction {
            type_id: request.type_id.to_owned(),
            payload: request.payload.to_vec(),
        };
        self.apply_encoded(&encoded, doc, path)
            .map(|output| Some(ui::widget::ResourceOperationStart::Completed(output)))
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mine = self.dirty_structure;
        self.dirty_structure = false;
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        mine || child
    }

    fn wants_pointer_capture(&self) -> bool {
        self.composed
            .as_ref()
            .is_some_and(|c| c.wants_pointer_capture())
    }

    fn name(&self) -> &'static str {
        "InstallerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for InstallerApp {
    fn needs_commit(&self) -> bool {
        self.composed
            .as_ref()
            .is_some_and(|c| ui::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        if let Some(c) = &mut self.composed {
            ui::undo::on_commit(c.as_mut());
        }
    }
}

impl ui::resources::PublishesResources for InstallerApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.publish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Disk> {
        sample_disks()
    }

    fn disk_with_two_guided_extents() -> Disk {
        let gib = 1024_u64 * 1024 * 1024;
        let mut disk = sample_disks().remove(1);
        disk.partitions.push(DiskPartition {
            name: "/dev/nvme0n1p1".to_string(),
            start_bytes: 100 * gib,
            size_bytes: 20 * gib,
            part_uuid: "30000000-0000-4000-8000-000000000001".to_string(),
            filesystem: "ntfs".to_string(),
            label: "Existing OS".to_string(),
            mountpoint: String::new(),
            part_type: "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7".to_string(),
        });
        disk
    }

    /// Recursively assert the widget tree mirrors the taffy tree at every
    /// node — the structural invariant the App's focusable-order traversal
    /// depends on.
    fn assert_tree_mirrors(widget: &dyn Widget, taffy: &TaffyTree<()>, node: NodeId) {
        let wc = widget.children();
        let tc = taffy.children(node).unwrap();
        assert_eq!(
            wc.len(),
            tc.len(),
            "widget '{}' has {} children but its taffy node has {}",
            widget.name(),
            wc.len(),
            tc.len()
        );
        for (w, &n) in wc.iter().zip(tc.iter()) {
            assert_tree_mirrors(*w, taffy, n);
        }
    }

    fn find_named<'a>(widget: &'a dyn Widget, name: &str) -> Option<&'a dyn Widget> {
        if widget.name() == name {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find_named(child, name))
    }

    fn find_accessible(widget: &dyn Widget, name: &str) -> Option<ui::a11y::AccessNode> {
        if let Some(node) = widget.accessibility()
            && node.name.as_deref() == Some(name)
        {
            return Some(node);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find_accessible(child, name))
    }

    fn find_accessible_role(
        widget: &dyn Widget,
        name: &str,
        role: ui::a11y::Role,
    ) -> Option<ui::a11y::AccessNode> {
        if let Some(node) = widget.accessibility()
            && node.name.as_deref() == Some(name)
            && node.role == role
        {
            return Some(node);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find_accessible_role(child, name, role))
    }

    fn has_test_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id)
            || widget
                .children()
                .into_iter()
                .any(|child| has_test_id(child, id))
    }

    fn build(app: &mut InstallerApp) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = app.build_layout(&mut taffy, &theme, &mut node_map);
        (taffy, root)
    }

    fn layout_at(app: &mut InstallerApp, width: f32, height: f32) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let app_node = app.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: length(width),
                        height: length(height),
                    },
                    ..Default::default()
                },
                &[app_node],
            )
            .expect("viewport node");
        let measure_map = node_map.clone();
        let mut text_renderer = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                viewport,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(height),
                },
                |known, available, node_id, _, _| {
                    if let Some(&widget) = measure_map.get(&node_id) {
                        // SAFETY: every pointer belongs to `app`, which remains
                        // alive and stationary for this layout pass.
                        unsafe { (*widget).measure(known, available, &mut text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .expect("compute compact installer layout");
        app.apply_layout(&taffy, app_node, Point::ORIGIN);
    }

    // --- pure state-machine ---------------------------------------

    #[test]
    fn step_navigation_walks_forward_back_and_indexes_labels() {
        assert_eq!(STEP_LABELS.len(), 7);
        // (step, next, prev, index): forward stops at Install, back stops at
        // Keyboard (and Install has no way back), indices match the labels.
        let cases = [
            (Step::Keyboard, Some(Step::Network), None, 0),
            (Step::Network, Some(Step::Disk), Some(Step::Keyboard), 1),
            (Step::Disk, Some(Step::Account), Some(Step::Network), 2),
            (Step::Account, Some(Step::Options), Some(Step::Disk), 3),
            (Step::Options, Some(Step::Review), Some(Step::Account), 4),
            (Step::Review, Some(Step::Install), Some(Step::Options), 5),
            (Step::Install, None, None, 6),
        ];
        for (step, next, prev, index) in cases {
            assert_eq!(next_step(step), next, "next_step case {step:?}");
            assert_eq!(prev_step(step), prev, "prev_step case {step:?}");
            assert_eq!(step_index(step), index, "step_index case {step:?}");
        }
    }

    #[test]
    fn step_and_field_validators_match_backend_contract() {
        // validate_step: (step, disk, username, hostname, ok).
        let steps = [
            // The keyboard step never blocks forward progress.
            (Step::Keyboard, Some(0), "quinten", "tilo", true),
            // Disk requires a selection.
            (Step::Disk, None, "", "tilo", false),
            (Step::Disk, Some(0), "", "tilo", true),
            // Account requires a valid username.
            (Step::Account, Some(0), "", "tilo", false),
            (Step::Account, Some(0), "   ", "tilo", false),
            (Step::Account, Some(0), "quinten", "tilo", true),
            (Step::Account, Some(0), "lab-user_2", "tilo", true),
            (Step::Account, Some(0), "2bad", "tilo", false),
            (Step::Account, Some(0), "has space", "tilo", false),
            (Step::Account, Some(0), "bad:name", "tilo", false),
            // Options requires a valid hostname.
            (Step::Options, Some(0), "q", "tilo", true),
            (Step::Options, Some(0), "q", "", false),
            (Step::Options, Some(0), "q", "-bad", false),
            (Step::Options, Some(0), "q", "bad host", false),
        ];
        for (step, disk, username, hostname, ok) in steps {
            assert_eq!(
                validate_step(step, disk, username, hostname).is_ok(),
                ok,
                "validate_step case {step:?} {disk:?} {username:?} {hostname:?}"
            );
        }

        assert_eq!(
            validate_passwords("", "").unwrap_err(),
            "Enter an account password"
        );
        assert_eq!(
            validate_passwords("secret", "different").unwrap_err(),
            "The passwords do not match"
        );
        assert!(validate_passwords("secret", "secret").is_ok());

        for (username, ok) in [
            ("q", true),
            ("_service", true),
            ("lab-user_2", true),
            ("", false),
            ("Upper", false),
            ("2start", false),
            ("root", false),
            ("greetd", false),
            ("a_very_long_username_that_exceeds_32_chars", false),
        ] {
            assert_eq!(
                valid_username(username),
                ok,
                "valid_username case {username:?}"
            );
        }
        for (hostname, ok) in [
            ("tilo", true),
            ("lab-1", true),
            ("", false),
            ("-x", false),
            ("x-", false),
            ("has space", false),
            ("under_score", false),
        ] {
            assert_eq!(
                valid_hostname(hostname),
                ok,
                "valid_hostname case {hostname:?}"
            );
        }
        // The locale validator accepts only the installed catalog.
        for (locale, ok) in [
            ("en_US.UTF-8", true),
            ("de_DE.UTF-8", true),
            ("zz_ZZ.UTF-8", false),
            ("../../etc", false),
        ] {
            assert_eq!(valid_locale(locale), ok, "valid_locale case {locale:?}");
        }
        // The startup-secret policy always requires a strong passphrase.
        for (secret, ok) in [
            ("correct horse battery", true),
            ("correct horse", false),
            ("246810", false),
            ("hunter2", false),
            ("aaaaaaaaaaaa", false),
            ("password1234", false),
            ("abcabcabcabc", false),
            ("123456789012", false),
            ("12345678901234567890", true),
            (" correct horse battery", false),
        ] {
            assert_eq!(
                validate_startup_secret(secret).is_ok(),
                ok,
                "validate_startup_secret case {secret:?}"
            );
        }
    }

    #[test]
    fn account_step_keeps_secrets_out_of_the_crdt_and_gates_on_username() {
        // Phase `secret_fields_never_enter_automerge_history`.
        {
            let mut app = InstallerApp::with_disks(sample());
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            for path in [
                "installer.password",
                "installer.password_confirm",
                "installer.wifi_password",
            ] {
                assert!(doc.get(&automerge::ROOT, path).unwrap().is_none(), "{path}");
            }
        }
        // Phase `account_step_requires_username_before_advancing`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = false;
            app.selected_disk = Some(0);
            app.step = Step::Account;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            // Empty username → blocked.
            let _ = doc.put(&root, NEXT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Account);
            assert_eq!(app.status, "Enter a username");

            // Only the username enters the CRDT. Secrets use external buffers.
            let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
            let _ = doc.splice_text(&uobj, 0, 0, "quinten");
            *app.password.lock().unwrap() = "correct horse battery staple".to_string();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".to_string();
            let _ = doc.put(&root, NEXT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Options);
            assert_eq!(app.username, "quinten");
        }
        // Phase `startup_secret_threads_into_install_options`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            *app.password.lock().unwrap() = "correct horse battery".to_string();
            app.sync(&mut doc, &root);
            assert_eq!(&*secret_copy(&app.password), "correct horse battery");
            assert_eq!(app.install_options().pin.as_str(), "correct horse battery");
        }
    }

    // --- app behaviour --------------------------------------------

    #[test]
    fn disk_step_gates_next_on_a_usable_selection() {
        // Phase `next_on_disk_without_selection_sets_status_and_stays`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Disk);

            let _ = doc.put(&root, NEXT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Disk, "must not advance without a disk");
            assert_eq!(app.status, "Select a disk to continue");
            assert!(!read_bool(&doc, &root, NEXT_PATH).unwrap_or(false));
        }
        // Phase `selecting_disk_then_next_advances_to_account`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            // Pick the second disk via its flag.
            let _ = doc.put(&root, disk_path(1).as_str(), true);
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_disk, Some(1));
            assert!(!read_bool(&doc, &root, disk_path(1).as_str()).unwrap_or(false));

            // Now Next advances and requests a focus reset for the new step.
            let _ = doc.put(&root, NEXT_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Account);
            assert!(app.take_focus_reset(), "step change requests focus reset");
        }
        // Phase `read_only_disk_cannot_advance`.
        {
            let mut disks = sample();
            disks[0].read_only = true;
            let mut app = InstallerApp::with_disks(disks);
            app.step = Step::Disk;
            app.selected_disk = Some(0);
            app.go_next();
            assert_eq!(app.step, Step::Disk);
            assert_eq!(
                app.status,
                "This disk is read-only and cannot be used for installation"
            );
        }
        // Phase `begin_only_runs_install_from_review`.
        {
            // On the Disk step, raising `begin` must NOT trigger an install
            // (would shell out to a missing backend). It's simply consumed.
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let _ = doc.put(&root, BEGIN_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!read_bool(&doc, &root, BEGIN_PATH).unwrap_or(false));
        }
        // Phase `confirm_shows_selected_device`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(1);
            assert_eq!(app.selected_dev().as_deref(), Some("/dev/nvme0n1"));
        }
        // Phase `step_changes_start_at_the_top_without_losing_incidental_scroll`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            app.selected_disk = Some(0);
            app.viewport_offset.set((0.0, 144.0));
            app.set_status("A transient message");
            assert_eq!(app.viewport_offset.get(), (0.0, 144.0));

            app.go_next();
            assert_eq!(app.step, Step::Account);
            assert_eq!(app.viewport_offset.get(), (0.0, 0.0));
        }
    }

    #[test]
    fn options_choices_thread_into_install_options() {
        // Phase `keyboard_selection_threads_layout_into_install_options`.
        {
            let mut app = InstallerApp::with_disks(sample());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            // Default seeds English (US) → xkb us, no variant.
            assert_eq!(app.keymap, "English (US)");
            let opts = app.install_options();
            assert_eq!(opts.keymap, "us");
            assert!(opts.keyvariant.is_empty());

            // Select Dvorak through the dropdown's CRDT Text path.
            let (_, kobj) = doc.get(&root, KEYMAP_PATH).unwrap().unwrap();
            let cur = doc.text(&kobj).unwrap();
            let _ = doc.splice_text(
                &kobj,
                0,
                ui::num::isize_of_usize(cur.chars().count()),
                "Dvorak",
            );
            app.sync(&mut doc, &root);
            assert_eq!(app.keymap, "Dvorak");
            let opts = app.install_options();
            assert_eq!(opts.keymap, "us", "Dvorak is an xkb us variant");
            assert_eq!(opts.keyvariant, "dvorak");
        }
        // Phase `filesystem_flag_updates_selection`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_fs, 0);
            assert_eq!(app.filesystem(), "btrfs");

            let _ = doc.put(&root, fs_path(2).as_str(), true);
            app.sync(&mut doc, &root);
            assert_eq!(app.selected_fs, 2);
            assert_eq!(app.filesystem(), "xfs");
            assert!(!read_bool(&doc, &root, fs_path(2).as_str()).unwrap_or(false));
        }
        // Phase `fido2_flag_threads_into_mandatory_encryption_options`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let _ = doc.put(&root, fido2_path(1).as_str(), true);
            app.sync(&mut doc, &root);
            assert!(app.fido2);
            assert!(app.install_options().fido2);
        }
        // Phase `options_step_uses_seeded_hostname_and_timezone`.
        {
            let mut app = InstallerApp::with_disks(sample());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            assert_eq!(app.hostname, "tilo");
            assert_eq!(app.timezone, "UTC");
            let opts = app.install_options();
            assert_eq!(opts.hostname, "tilo");
            assert_eq!(opts.timezone, "UTC");
            assert_eq!(opts.filesystem, "btrfs");
        }
        // Phase `install_options_bind_selected_disk_and_alongside_partition_identities`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.install_method = InstallMethod::Alongside;
            let opts = app.install_options();
            assert_eq!(opts.disk_size_bytes, 256_000_000_000);
            assert_eq!(opts.disk_major_minor, "8:0");
            assert_eq!(opts.disk_wwn, "0x5002538e40abcdef");
            assert_eq!(opts.disk_serial, "TILO-SAMPLE-SATA");
            assert_eq!(opts.alongside_root, "/dev/sda3");
            assert_eq!(
                opts.alongside_root_partuuid,
                "00000000-0000-0000-0000-000000000003"
            );
            assert_eq!(opts.alongside_root_size_bytes, 64_000_000_000);
            assert_eq!(opts.alongside_boot, "/dev/sda5");
            assert_eq!(
                opts.alongside_boot_partuuid,
                "00000000-0000-0000-0000-000000000005"
            );
            assert_eq!(opts.alongside_boot_size_bytes, 1024 * 1024 * 1024);
            assert_eq!(opts.alongside_efi, "/dev/sda4");
            assert_eq!(
                opts.alongside_efi_partuuid,
                "00000000-0000-0000-0000-000000000004"
            );
            assert_eq!(opts.alongside_efi_size_bytes, 2 * 1024 * 1024 * 1024);
        }
    }

    #[test]
    fn media_codecs_are_an_explicit_opt_in_that_threads_into_install_options() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Options;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(!app.codecs, "media codecs are not selected by default");
        assert!(!app.install_options().codecs);
        let _ = doc.put(&root, codecs_path(1).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(app.codecs);
        assert!(app.install_options().codecs);
        let _ = doc.put(&root, codecs_path(0).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(!app.codecs, "the choice can be withdrawn");
    }

    #[test]
    fn options_step_explains_the_media_codecs_choice_and_review_repeats_it() {
        let mut app = InstallerApp::for_shot(Step::Options);
        let _ = build(&mut app);
        let composed = app.composed.as_deref().unwrap();
        assert!(has_test_id(composed, "codecs_choice"));
        for needle in [
            "RPM Fusion",
            "Cisco",
            "AMD and Intel",
            "screen sharing and calls",
        ] {
            assert!(CODECS_DESCRIPTION.contains(needle), "{needle}");
        }
        app.codecs = true;
        app.step = Step::Review;
        app.dirty_structure = true;
        let _ = build(&mut app);
        let review = app.composed.as_deref().unwrap();
        assert!(
            find_accessible(
                review,
                "Download from RPM Fusion and Cisco once the new system is online"
            )
            .is_some(),
            "Review repeats the codecs choice"
        );
    }

    #[test]
    fn image_filesystem_manifest_bounds_the_choice() {
        // Phase `image_driver_manifest_filters_choices_and_rejects_stale_selection`.
        {
            assert_eq!(
                filesystem_support_from_manifest("ext4\nxfs\n").unwrap(),
                [true, false, false]
            );
            assert_eq!(
                filesystem_support_from_manifest("xfs\n").unwrap(),
                [true, true, false]
            );
            assert_eq!(
                filesystem_support_from_manifest("amdgpu\nnouveau\nxe\nxfs\n").unwrap(),
                [true, true, false],
                "board GPU omissions must not hide supported filesystems"
            );
            assert!(filesystem_support_from_manifest("unknown\n").is_err());
            assert!(filesystem_support_from_manifest("amdgpu\namdgpu\n").is_err());

            let mut app = InstallerApp::with_disks(sample());
            app.set_filesystem_support(filesystem_support_from_manifest("ext4\nxfs\n"));
            assert_eq!(app.available_filesystem_indices(), vec![0]);
            app.set_filesystem_support(filesystem_support_from_manifest("xfs\n"));
            assert_eq!(app.available_filesystem_indices(), vec![0, 1]);
            app.set_filesystem_support(filesystem_support_from_manifest("ext4\nxfs\n"));
            app.step = Step::Options;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            let _ = doc.put(&root, fs_path(1).as_str(), true);
            app.sync(&mut doc, &root);
            assert_eq!(
                app.filesystem(),
                "btrfs",
                "hidden ext4 CRDT actions must be rejected"
            );
            assert!(!read_bool(&doc, &root, fs_path(1).as_str()).unwrap_or(false));

            app.selected_fs = 1; // stale selection from a media profile or UI rebuild
            assert!(!app.filesystem_policy_ready());
            let mut review = InstallerApp::for_shot(Step::Review);
            review.set_filesystem_support(filesystem_support_from_manifest("ext4\nxfs\n"));
            review.selected_fs = 1;
            review.begin_install(&doc, &root);
            assert!(!review.install_launched);
            assert_eq!(review.step, Step::Review);
        }
        // Phase `unsupported_profile_filesystem_requires_repair_and_missing_manifest_blocks_install`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                hostname: Some("studio".into()),
                username: Some("quinten".into()),
                full_name: None,
                locale: Some("en_US.UTF-8".into()),
                keyboard: Some("us".into()),
                timezone: Some("UTC".into()),
                filesystem: Some("ext4".into()),
                repairs: Vec::new(),
            });
            app.set_filesystem_support(filesystem_support_from_manifest("ext4\nxfs\n"));
            assert_eq!(app.filesystem(), "btrfs");
            assert!(!app.profile_completion.unwrap().options_complete());

            app.set_filesystem_support(Err("manifest missing".into()));
            assert!(!app.filesystem_policy_ready());
            app.step = Step::Options;
            assert!(app.validate_current_step_details().is_err());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn review_begin_is_gated_on_ack_confirmation_and_power() {
        // Phase `recovery_ack_toggles_and_gates_begin`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            assert!(!app.recovery_ack);

            // An unacknowledged recovery key must block every native install.
            let _ = doc.put(&root, BEGIN_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.install_state, InstallState::Idle);
            assert_eq!(
                app.step,
                Step::Review,
                "blocked until the key is acknowledged"
            );

            // Toggle the acknowledgement on.
            let _ = doc.put(&root, RECOVERY_ACK_PATH, true);
            app.sync(&mut doc, &root);
            assert!(app.recovery_ack, "ack flips on");
            assert!(!read_bool(&doc, &root, RECOVERY_ACK_PATH).unwrap_or(false));

            // Toggle off again (it's a checkbox-style flip).
            let _ = doc.put(&root, RECOVERY_ACK_PATH, true);
            app.sync(&mut doc, &root);
            assert!(!app.recovery_ack, "ack flips back off");
        }
        // Phase `review_begin_disabled_until_ack_when_encrypting`.
        {
            // Encrypting + un-acknowledged: the Begin button (nav row's primary,
            // its last child) must be disabled (non-focusable).
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            let _ = build(&mut app);
            let card = find_named(&app, "InstallerCard").expect("review card");
            let begin_disabled = card
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| !b.is_focusable()));
            assert_eq!(begin_disabled, Some(true), "Begin disabled until ack");

            // Acknowledgement alone is insufficient because the passphrase is mandatory.
            let mut app2 = InstallerApp::with_disks(sample());
            app2.selected_disk = Some(0);
            app2.step = Step::Review;
            app2.tpm2 = true;
            app2.secure_boot = true;
            app2.kernel_lockdown = true;
            app2.target_verified_boot = true;
            app2.recovery_ack = true;
            app2.erase_confirmation = "/dev/sda".to_string();
            *app2.password.lock().unwrap() = "804927".to_string();
            let _ = build(&mut app2);
            let card2 = find_named(&app2, "InstallerCard").expect("review card");
            let begin_enabled = card2
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| b.is_focusable()));
            assert_eq!(begin_enabled, Some(true), "Begin enabled once acknowledged");
        }
        // Phase `whole_disk_install_requires_exact_typed_device_confirmation`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            app.recovery_ack = true;
            *app.password.lock().unwrap() = "804927".to_string();
            *app.password_confirmation.lock().unwrap() = "804927".to_string();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let _ = doc.put(&root, BEGIN_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Review);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(app.status.contains("Type /dev/sda exactly"));

            let object = match doc.get(&root, ERASE_CONFIRM_PATH).unwrap() {
                Some((Value::Object(ObjType::Text), object)) => object,
                other => panic!("expected confirmation text object, got {other:?}"),
            };
            let _ = doc.splice_text(&object, 0, 0, "/dev/sda");
            app.sync(&mut doc, &root);
            assert!(app.destructive_confirmation_ready());
            assert!(
                app.take_structure_dirty(),
                "typing the confirmation must rebuild the disabled Begin button"
            );
            let _ = build(&mut app);
            let card = find_named(&app, "InstallerCard").expect("review card");
            let begin_enabled = card
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|button| button.is_focusable()));
            assert_eq!(begin_enabled, Some(true));
        }
        // Phase `confirmation_keeps_live_input_until_begin_gate_changes`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            let _ = build(&mut app);
            assert!(!app.take_structure_dirty());
            let object = match doc.get(&root, ERASE_CONFIRM_PATH).unwrap() {
                Some((Value::Object(ObjType::Text), object)) => object,
                other => panic!("expected confirmation text object, got {other:?}"),
            };
            for (index, character) in "/dev/sd".chars().enumerate() {
                let _ = doc.splice_text(&object, index, 0, &character.to_string());
                app.sync(&mut doc, &root);
                assert!(
                    !app.take_structure_dirty(),
                    "partial input rebuilt at {index}"
                );
            }
            let _ = doc.splice_text(&object, 7, 0, "a");
            app.sync(&mut doc, &root);
            assert!(
                app.take_structure_dirty(),
                "exact confirmation must enable Begin"
            );
        }
        // Phase `alongside_install_requires_exact_typed_root_confirmation`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.install_method = InstallMethod::Alongside;
            let root = app
                .selected_disk_ref()
                .and_then(|disk| disk.alongside_root())
                .map(|partition| partition.name.clone())
                .expect("sample disk has one prepared alongside root");

            assert!(!app.destructive_confirmation_ready());
            app.erase_confirmation = app.selected_dev().expect("selected disk");
            assert!(!app.destructive_confirmation_ready());
            app.erase_confirmation = root;
            assert!(app.destructive_confirmation_ready());
        }
        // Phase `low_discharging_battery_is_visible_and_blocks_launch_before_secrets_move`.
        {
            let mut app = InstallerApp::for_shot(Step::Review);
            app.power_probe = || Some(7);
            let (_taffy, _root) = build(&mut app);
            let message = connect_power_message(7);
            assert!(
                find_accessible(&app, &message).is_some(),
                "Review must explain why destructive work cannot start"
            );

            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.begin_install(&doc, &root);
            assert_eq!(app.step, Step::Review);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!app.install_launched);
            assert_eq!(app.status, message);
            assert_eq!(
                &*secret_copy(&app.password),
                "correct horse battery staple",
                "the account secret stays editable because installation never started"
            );
        }
    }

    #[test]
    fn encryption_summary_names_the_enrolled_factors() {
        // Phase `encryption_summary_reflects_factors`.
        {
            let mut app = InstallerApp::with_disks(sample());
            // Pin the hardware so the Review copy must describe measured-boot PIN
            // mode rather than the non-TPM passphrase fallback.
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            assert_eq!(app.encryption_summary(), "On (LUKS — recovery key)");

            *app.password.lock().unwrap() = "804927".to_string();
            assert_eq!(
                app.encryption_summary(),
                "On (LUKS — recovery key, TPM-protected startup PIN)"
            );

            app.fido2 = true;
            assert_eq!(
                app.encryption_summary(),
                "On (LUKS — recovery key, TPM-protected startup PIN, FIDO2)"
            );
        }
        // Phase `summary_and_description_name_the_passphrase_when_there_is_no_tpm`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = false;
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert_eq!(
                app.encryption_summary(),
                "On (LUKS — recovery key, boot passphrase)",
                "the review step must not claim a TPM factor this machine cannot provide"
            );

            clear_secret_buffer(&app.password);
            assert_eq!(app.encryption_summary(), "On (LUKS — recovery key)");

            assert!(
                encrypt_description(VerifiedBootState::NoTpm).contains("no TPM2 chip"),
                "the chooser has to say the machine cannot unlock itself"
            );
            assert!(
                !encrypt_description(VerifiedBootState::TargetVerified)
                    .contains("Requires a TPM2 device"),
                "encryption no longer requires a TPM, and the copy must not claim it does"
            );
        }
        // Phase `options_step_always_shows_encryption_factors`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let _ = build(&mut app);
            let card = find_named(&app, "InstallerCard").expect("options card");
            let has_fido2 = card
                .children()
                .iter()
                .any(|c| c.test_id() == Some("fido2_choice"));
            assert!(
                has_fido2,
                "mandatory encryption exposes its optional extra factor"
            );
        }
    }

    #[test]
    fn encryption_readiness_follows_the_verified_boot_policy() {
        // Phase `without_a_tpm_the_unlock_secret_is_required`.
        {
            // A machine with no TPM cannot unlock itself. Encryption still works —
            // that is the point of building the volume around a recovery key — but
            // with no passphrase enrolled, the recovery key is what gets typed at
            // every boot. Requiring the secret is what keeps the degradation
            // usable instead of merely correct.
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.tpm2 = false;
            app.recovery_ack = true;
            assert!(
                !app.encryption_ready(),
                "no TPM and no passphrase must not be installable"
            );

            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready(), "a passphrase unblocks it");

            // Whitespace is not a passphrase.
            *app.password.lock().unwrap() = "   ".to_string();
            assert!(
                !app.encryption_ready(),
                "blank passphrase is still no passphrase"
            );
        }
        // Phase `verified_tpm_requires_a_nontrivial_pin`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            app.recovery_ack = true;
            assert!(
                !app.encryption_ready(),
                "silent TPM-only unlock is not a hardened startup boundary"
            );
            *app.password.lock().unwrap() = "123456".to_string();
            assert!(
                !app.encryption_ready(),
                "a trivial sequence must not become a TPM PIN"
            );
            *app.password.lock().unwrap() = "804927".to_string();
            assert!(app.encryption_ready());
        }
        // Phase `tpm_without_verified_boot_uses_the_passphrase_policy`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = false;
            app.kernel_lockdown = false;
            app.recovery_ack = true;

            *app.password.lock().unwrap() = "246810".to_string();
            assert!(
                !app.encryption_ready(),
                "an unverified TPM must not enable short-PIN mode"
            );
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready());
            assert!(
                encrypt_description(VerifiedBootState::SecureBootInactive)
                    .contains("Secure Boot is not active")
            );
        }
        // Phase `secure_boot_without_kernel_lockdown_uses_the_passphrase_policy`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = false;
            app.recovery_ack = true;

            *app.password.lock().unwrap() = "246810".to_string();
            assert!(!app.encryption_ready());
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready());
            assert!(
                encrypt_description(VerifiedBootState::KernelLockdownInactive)
                    .contains("kernel lockdown is not")
            );
        }
        // Phase `live_verified_boot_without_target_preflight_uses_a_passphrase`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = false;
            app.recovery_ack = true;

            *app.password.lock().unwrap() = "246810".to_string();
            assert!(
                !app.encryption_ready(),
                "the live medium's boundary cannot vouch for the installed target"
            );
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready());
            assert!(
                encrypt_description(VerifiedBootState::TargetUnverified)
                    .contains("Verified boot is unavailable for this installation")
            );
        }
    }

    #[test]
    fn alongside_copy_explains_what_changes_before_leaving_storage() {
        // Phase `alongside_review_describes_each_partition_it_changes`.
        {
            let guided = alongside_install_impact(true, "/dev/nvme0n1");
            assert!(guided.contains("Three new Tilo partitions (EFI, boot, and encrypted root)"));
            assert!(
                guided.contains("created and formatted only in the reviewed unallocated space")
            );
            assert!(guided.contains("BitLocker recovery prompt"));

            let prepared = alongside_install_impact(false, "/dev/nvme0n1");
            assert!(prepared.contains("prepared Tilo boot and root partitions"));
            assert!(prepared.contains("will be formatted"));
            assert!(
                prepared.contains("existing EFI partition and other OS partitions are preserved")
            );
            assert!(prepared.contains("BitLocker recovery prompt"));
            assert_eq!(
                alongside_confirmation_operation(false),
                "Tilo boot and root format"
            );
            assert_eq!(
                alongside_confirmation_operation(true),
                "guided free-space partition plan"
            );
        }
        // Phase `guided_alongside_explains_the_firmware_picker_before_and_after_install`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(1);
            app.step = Step::Review;
            assert!(app.uses_guided_alongside());
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    "Use the firmware boot picker; its current default and existing entries are preserved"
                )
                .is_some()
            );

            app.install_state = InstallState::Done;
            app.step = Step::Install;
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    "To choose Tilo or your existing system, open this computer's firmware boot picker. The installer preserved its previous default."
                )
                .is_some()
            );
        }
        // Phase `alongside_surfaces_windows_preparation_before_leaving_storage`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;
            let _ = build(&mut app);

            assert!(
                find_accessible(&app, WINDOWS_ALONGSIDE_PREPARATION).is_some(),
                "Windows preparation must be actionable before the user leaves storage selection"
            );

            app.disks[0].partitions[1].filesystem = "ext4".into();
            app.disks[0].partitions[1].label = "Existing Linux".into();
            let _ = build(&mut app);
            assert!(
                find_accessible(&app, WINDOWS_ALONGSIDE_PREPARATION).is_none(),
                "Linux-only alongside disks must not receive Windows-specific instructions"
            );
        }
        // Phase `alongside_explains_a_determinable_prepared_efi_failure`.
        {
            let mut disk = sample_disks().remove(0);
            disk.partitions
                .retain(|partition| partition.name != "/dev/sda4"); // remove the valid 2 GiB Tilo ESP, retain prepared boot
            let mut app = InstallerApp::with_disks(vec![disk]);
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;
            let _ = build(&mut app);

            assert!(
                find_accessible(
                    &app,
                    "/dev/sda1 is too small for Tilo boot images; a prepared EFI partition must be at least 2 GiB. The privileged install check will also verify FAT health and at least 1 GiB of free space before formatting anything."
                )
                .is_some(),
                "known ESP failures should be explained before the user leaves storage selection"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn guided_alongside_binds_exactly_the_reviewed_extent() {
        // Phase `guided_alongside_advances_and_binds_reviewed_free_extent`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.step = Step::Disk;
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(1); // empty 1 TB GPT sample
            assert!(app.uses_guided_alongside());
            assert!(app.selected_disk_can_install_alongside());

            let plan = app.guided_alongside_plan().unwrap();
            let opts = app.install_options();
            assert_eq!(opts.guided_free_start_bytes, plan.free_start_bytes);
            assert_eq!(opts.guided_free_size_bytes, plan.free_size_bytes);
            assert_eq!(opts.guided_efi_size_bytes, 2 * 1024 * 1024 * 1024);
            assert_eq!(opts.guided_boot_size_bytes, 1024 * 1024 * 1024);
            assert_eq!(opts.guided_boot_start_bytes, plan.boot_start_bytes);
            assert!(opts.guided_root_size_bytes >= 32 * 1024 * 1024 * 1024);
            assert_eq!(
                opts.disk_partition_table_uuid,
                "22222222-2222-2222-2222-222222222222"
            );
            assert!(opts.expected_partitions.is_empty());
            assert_eq!(
                app.destructive_confirmation_target().as_deref(),
                Some("/dev/nvme0n1")
            );

            app.go_next();
            assert_eq!(app.step, Step::Account);
        }
        // Phase `multiple_guided_extents_require_and_bind_an_explicit_choice`.
        {
            let disk = disk_with_two_guided_extents();
            let plans = disk.guided_alongside_plans();
            assert_eq!(plans.len(), 2);
            let mut app = InstallerApp::with_disks(vec![disk]);
            app.step = Step::Disk;
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);

            assert!(app.guided_alongside_plan().is_none());
            assert!(!app.selected_disk_can_install_alongside());
            app.go_next();
            assert_eq!(app.step, Step::Disk, "ambiguous free space must block Next");

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            let _ = doc.put(&root, guided_extent_path(1).as_str(), true);
            app.sync(&mut doc, &root);
            assert_eq!(app.guided_alongside_plan(), Some(plans[1].clone()));
            let options = app.install_options();
            assert_eq!(options.guided_free_start_bytes, plans[1].free_start_bytes);
            assert_eq!(options.guided_free_size_bytes, plans[1].free_size_bytes);

            let _ = build(&mut app);
            assert!(has_test_id(&app, "disk_refresh"));
            assert!(has_test_id(&app, "guided_extent_0"));
            assert!(has_test_id(&app, "guided_extent_1"));
        }
        // Phase `disk_refresh_invalidates_every_reviewed_guided_identity`.
        {
            let disk = disk_with_two_guided_extents();
            let mut app = InstallerApp::with_disks(vec![disk.clone()]);
            app.step = Step::Disk;
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.selected_guided_extent = Some(1);
            app.erase_confirmation = disk.name.clone();
            let old_efi_uuid = app.guided_efi_partuuid.clone();
            let old_root_uuid = app.guided_root_partuuid.clone();
            assert!(app.guided_alongside_plan().is_some());

            app.refresh_disks_with(vec![disk]);
            assert_eq!(app.selected_disk, None);
            assert_eq!(app.selected_guided_extent, None);
            assert!(app.erase_confirmation.is_empty());
            assert_ne!(app.guided_efi_partuuid, old_efi_uuid);
            assert_ne!(app.guided_root_partuuid, old_root_uuid);

            app.selected_disk = Some(0);
            assert!(
                app.guided_alongside_plan().is_none(),
                "the pre-refresh extent index must not survive rediscovery"
            );
            app.go_next();
            assert_eq!(app.step, Step::Disk, "a stale choice must remain blocked");
        }
        // Phase `restarted_ui_does_not_reinitialize_an_unauthenticated_partial_pair`.
        {
            let mut disk = sample_disks().remove(1);
            let gib = 1024_u64 * 1024 * 1024;
            disk.partitions = vec![
                DiskPartition {
                    name: "/dev/nvme0n1p1".into(),
                    start_bytes: 1024 * 1024,
                    size_bytes: 3 * gib,
                    part_uuid: "00000000-0000-4000-8000-000000000001".into(),
                    filesystem: "ntfs".into(),
                    label: "existing OS".into(),
                    mountpoint: String::new(),
                    part_type: LINUX_FILESYSTEM_GUID.into(),
                },
                DiskPartition {
                    name: "/dev/nvme0n1p2".into(),
                    start_bytes: 4 * gib,
                    size_bytes: 2 * gib,
                    part_uuid: "10000000-0000-4000-8000-000000000002".into(),
                    filesystem: String::new(),
                    label: "Tilo EFI".into(),
                    mountpoint: String::new(),
                    part_type: EFI_SYSTEM_GUID.into(),
                },
                DiskPartition {
                    name: "/dev/nvme0n1p3".into(),
                    start_bytes: 6 * gib,
                    size_bytes: disk.size_bytes - 7 * gib,
                    part_uuid: "20000000-0000-4000-8000-000000000003".into(),
                    filesystem: String::new(),
                    label: "Tilo root".into(),
                    mountpoint: String::new(),
                    part_type: LINUX_FILESYSTEM_GUID.into(),
                },
            ];
            let mut restarted = InstallerApp::with_disks(vec![disk]);
            restarted.step = Step::Disk;
            restarted.install_method = InstallMethod::Alongside;
            restarted.selected_disk = Some(0);

            assert!(!restarted.selected_disk_can_install_alongside());
            assert!(!restarted.uses_guided_alongside());
            restarted.erase_confirmation = "/dev/nvme0n1".into();
            assert!(!restarted.destructive_confirmation_ready());
            restarted.go_next();
            assert_eq!(restarted.step, Step::Disk);
        }
    }

    #[test]
    fn prepared_preflight_gates_leaving_storage() {
        // Phase `prepared_preflight_ignores_stale_results_and_advances_only_on_success`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;
            app.storage_preflight_request = Some("current".into());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            app.on_external_event(
                &format!("{PREFLIGHT_OK_PREFIX}stale:stale result"),
                &mut doc,
                &root,
            );
            assert_eq!(app.step, Step::Disk);
            assert!(!app.storage_preflight_passed);

            app.on_external_event(
                &format!("{PREFLIGHT_FAIL_PREFIX}current:EFI has less than 1 GiB free"),
                &mut doc,
                &root,
            );
            assert_eq!(app.step, Step::Disk);
            assert!(app.status.contains("less than 1 GiB"));

            app.storage_preflight_request = Some("retry".into());
            app.on_external_event(
                &format!("{PREFLIGHT_OK_PREFIX}retry:prepared storage check passed"),
                &mut doc,
                &root,
            );
            assert_eq!(app.step, Step::Account);
            assert!(app.storage_preflight_passed);
        }
        // Phase `prepared_preflight_fails_closed_without_an_event_channel`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;

            app.go_next();

            assert_eq!(app.step, Step::Disk);
            assert!(!app.storage_preflight_passed);
            assert!(app.status.contains("prepared-storage check is unavailable"));
        }
    }

    fn staged_app() -> InstallerApp {
        let disk =
            crate::disks::parse_lsblk(&crate::internal_handoff::tests::lsblk_json().to_string());
        let mut app = InstallerApp::with_disks(disk);
        app.apply_internal_handoff(crate::internal_handoff::tests::handoff());
        app
    }

    #[test]
    fn staged_handoff_preselects_the_plan_and_offers_a_way_back_only_when_valid() {
        // Phase `staged_installation_preselects_the_prepared_plan_and_profile`.
        {
            use crate::internal_handoff::tests::TARGET;
            let mut app = staged_app();
            assert_eq!(app.selected_disk, Some(0));
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert_eq!(app.username, "quinten");
            assert_eq!(app.hostname, "studio");
            assert!(
                app.status
                    .starts_with("Tilo Imager prepared space for Tilo next to Windows.")
            );
            assert!(app.uses_guided_alongside());
            let opts = app.install_options();
            assert_eq!(opts.guided_free_start_bytes, TARGET.0 * 512);
            assert_eq!(
                opts.guided_root_start_bytes + opts.guided_root_size_bytes,
                (TARGET.1 + 1) * 512
            );
            assert_eq!(opts.internal_handoff, crate::internal_handoff::HANDOFF_PATH);
            assert_eq!(
                opts.expected_partitions.len(),
                4,
                "staging partitions stay in the reviewed map"
            );
            assert!(opts.alongside_root.is_empty());

            // The running-installer disk can never be switched to Erase.
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            let _ = doc.put(&root, install_method_path(0).as_str(), true);
            app.sync(&mut doc, &root);
            assert_eq!(app.install_method, InstallMethod::Alongside);

            app.step = Step::Disk;
            let _ = build(&mut app);
            assert!(!has_test_id(&app, "install_method"));
            assert!(has_test_id(&app, "return_to_host"));
            assert!(find_accessible(&app, "Return to Windows").is_some());
            app.go_next();
            assert_eq!(
                app.step,
                Step::Account,
                "the staged disk is a valid destination"
            );

            app.step = Step::Review;
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    "Also holds the running installer and your existing Windows system. Windows stays untouched."
                )
                .is_some()
            );
        }
        // Phase `staged_handoff_that_no_longer_matches_keeps_the_disk_unavailable`.
        {
            let mut json = crate::internal_handoff::tests::lsblk_json();
            json["blockdevices"][0]["ptuuid"] = "99999999-2222-4333-8444-555555555555".into();
            let mut app = InstallerApp::with_disks(crate::disks::parse_lsblk(&json.to_string()));
            app.apply_internal_handoff(crate::internal_handoff::tests::handoff());
            assert!(app.internal_handoff.is_none());
            assert!(app.internal_host().is_none());
            assert_eq!(app.selected_disk, None);
            assert!(app.disks[0].blocks_install());
            assert!(app.status.contains("no longer matches"));
            assert!(app.install_options().internal_handoff.is_empty());
            app.step = Step::Disk;
            let _ = build(&mut app);
            assert!(!has_test_id(&app, "return_to_host"));
        }
        // Phase `return_to_host_is_withheld_while_installing`.
        {
            let mut app = staged_app();
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let _ = build(&mut app);
            assert!(!has_test_id(&app, "return_to_host"));
            app.install_state = InstallState::Failed;
            let _ = build(&mut app);
            assert!(has_test_id(&app, "return_to_host"));
        }
    }

    #[test]
    fn back_walks_in_reverse_and_a_failed_install_returns_to_storage() {
        // Phase `back_walks_the_wizard_in_reverse`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Options);

            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Account);

            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Disk);

            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Network);

            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Keyboard);

            // Back from the first step (Keyboard) is a no-op.
            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Keyboard);
        }
        // Phase `failed_install_recovery_returns_to_unselected_storage_review`.
        {
            let mut app = InstallerApp::with_disks(sample());
            let old_efi_partuuid = app.guided_efi_partuuid.clone();
            let old_root_partuuid = app.guided_root_partuuid.clone();
            app.selected_disk = Some(0);
            app.step = Step::Install;
            app.install_state = InstallState::Failed;
            app.install_launched = true;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            let _ = doc.put(&root, BACK_PATH, true);
            app.sync(&mut doc, &root);
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.selected_disk, None);
            assert_ne!(app.guided_efi_partuuid, old_efi_partuuid);
            assert_ne!(app.guided_root_partuuid, old_root_partuuid);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!app.install_launched, "retry must re-arm the launch guard");
            assert!(app.status.contains("will not reinitialize a target"));
        }
    }

    #[test]
    fn install_events_stream_human_progress_and_set_terminal_state() {
        // Phase `external_lines_stream_into_log_and_sentinels_set_state`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            app.on_external_event("==> step one", &mut doc, &root);
            app.on_external_event("==> step two", &mut doc, &root);
            assert!(app.status_lines.iter().any(|l| l == "==> step one"));
            assert!(app.status_lines.iter().any(|l| l == "==> step two"));
            assert_eq!(app.install_state, InstallState::Running);

            app.on_external_event(CTRL_DONE, &mut doc, &root);
            assert_eq!(app.install_state, InstallState::Done);
            assert!(
                app.take_focus_reset(),
                "completion must focus the first recovery action on the new Finish surface"
            );
        }
        // Phase `external_fail_sentinel_marks_failed`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.on_external_event("error: no space left on device", &mut doc, &root);
            app.on_external_event(CTRL_FAIL, &mut doc, &root);
            assert_eq!(app.install_state, InstallState::Failed);
            assert_eq!(
                app.status_lines.last().map(String::as_str),
                Some("error: no space left on device"),
                "the useful backend failure must remain the newest visible log line"
            );
            assert!(
                app.take_focus_reset(),
                "failure must focus the first recovery action on the new error surface"
            );
        }
        // Phase `install_progress_hides_container_noise_behind_human_milestones`.
        {
            let lines = vec![
                "Starting install to /dev/vdb for user tilotest (btrfs).".to_string(),
                "Copying blob 0123456789abcdef".to_string(),
                "Copying blob fedcba9876543210".to_string(),
                "==> Preparing an encrypted root on /dev/vdb".to_string(),
                "Formatting LUKS2 with argon2id".to_string(),
            ];
            assert_eq!(
                friendly_install_status(&lines),
                vec![
                    "✓ Installation choices verified",
                    "Copying verified Tilo system files…",
                    "Preparing encrypted storage…",
                ]
            );
        }
        // Phase `hardware_token_touch_is_an_explicit_human_milestone`.
        {
            let lines = vec![
                "==> Enrolling disk-encryption unlock factors".to_string(),
                "  enrolling a FIDO2 security key (touch it now if prompted)…".to_string(),
            ];
            assert_eq!(
                friendly_install_status(&lines),
                vec![
                    "Securing disk unlock and recovery…",
                    "Touch your security key to continue…",
                ]
            );
        }
    }

    #[test]
    fn recovery_key_stays_out_of_shared_state_and_gates_power() {
        // Phase `recovery_key_marker_becomes_persistent_card_not_log_noise`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            app.on_external_event("::tilo-recovery-key::abcd-efgh-ijkl-mnop", &mut doc, &root);
            assert_eq!(app.recovery_key.as_deref(), Some("abcd-efgh-ijkl-mnop"));
            assert!(
                app.status_lines.is_empty(),
                "secret marker is not a log line"
            );
        }
        // Phase `visible_recovery_key_can_be_copied_without_entering_shared_state`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            assert!(
                !has_test_id(
                    app.build_composition(&Theme::dark()).as_ref(),
                    "recovery_key_copy"
                ),
                "the action must not exist before a key has been received"
            );

            let key = "abcd-efgh-ijkl-mnop";
            app.on_external_event(&format!("{RECOVERY_KEY_PREFIX}{key}"), &mut doc, &root);
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_copy"
            ));

            let _ = doc.put(&root, RECOVERY_COPY_PATH, true);
            app.sync(&mut doc, &root);
            assert!(app.recovery_key_copied);
            assert!(!read_bool(&doc, &root, RECOVERY_COPY_PATH).unwrap_or(false));
            assert_eq!(ui::clipboard_paste().as_deref(), Some(key));
            assert!(
                !String::from_utf8_lossy(&doc.save()).contains(key),
                "the recovery secret must never be replicated through the CRDT"
            );

            app.on_external_event(
                &format!("{RECOVERY_KEY_PREFIX}replacement-key"),
                &mut doc,
                &root,
            );
            assert!(
                !app.recovery_key_copied,
                "a newly emitted key needs its own copy confirmation"
            );
            assert!(
                !app.recovery_key_saved,
                "a newly emitted key needs its own post-generation saved confirmation"
            );
        }
        // Phase `generated_recovery_key_survives_failure_and_power_requires_saved_ack`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);

            app.on_external_event(
                &format!("{RECOVERY_KEY_PREFIX}failure-safe-key"),
                &mut doc,
                &root,
            );
            app.on_external_event(CTRL_FAIL, &mut doc, &root);
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_copy"
            ));
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_saved"
            ));

            app.install_state = InstallState::Done;
            let _ = doc.put(&root, REBOOT_PATH, true);
            app.sync(&mut doc, &root);
            assert!(app.status.contains("Save the recovery key"));

            let _ = doc.put(&root, RECOVERY_SAVED_PATH, true);
            app.sync(&mut doc, &root);
            assert!(app.recovery_key_saved);
            assert!(
                !app.recovery_key_copied,
                "explicit saved acknowledgement must not be inferred from clipboard state"
            );
        }
    }

    #[test]
    fn status_and_terminal_surfaces_have_live_accessible_semantics() {
        // Phase `terminal_states_and_recovery_actions_have_live_accessible_semantics`.
        {
            let mut done = InstallerApp::for_shot(Step::Install);
            done.shot_finished();
            done.recovery_key = Some("test-recovery-key".to_string());
            let _ = build(&mut done);
            let success = find_accessible(&done, "Tilo is installed").expect("success title");
            assert_eq!(success.live, Some(Politeness::Polite));
            let reboot = find_accessible(&done, "Reboot now").expect("reboot action");
            assert_eq!(reboot.role, ui::a11y::Role::Button);
            assert!(
                !reboot.state.focusable,
                "power stays gated until the key is saved"
            );
            assert!(find_accessible(&done, "Power off").is_some());
            done.recovery_key_saved = true;
            let _ = build(&mut done);
            assert!(
                find_accessible(&done, "Reboot now")
                    .expect("enabled reboot action")
                    .state
                    .focusable
            );

            let mut failed = InstallerApp::for_shot(Step::Install);
            failed.shot_failed();
            let _ = build(&mut failed);
            let failure = find_accessible(&failed, "Installation failed").expect("failure title");
            assert_eq!(failure.live, Some(Politeness::Assertive));
            assert!(find_accessible(&failed, "Review storage").is_some());
        }
        // Phase `validation_and_recovery_ack_announce_without_focus_reset`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Review;
            app.selected_disk = Some(0);
            app.set_status("Type /dev/sda exactly to confirm formatting it");
            let _ = build(&mut app);

            let validation =
                find_accessible(&app, "Type /dev/sda exactly to confirm formatting it")
                    .expect("validation status");
            assert_eq!(validation.live, Some(Politeness::Assertive));
            assert!(!app.focus_reset);

            let ack = find_accessible(&app, "I will save my recovery key")
                .expect("recovery acknowledgement");
            assert_eq!(ack.role, ui::a11y::Role::Checkbox);
            assert_eq!(ack.state.checked, Some(false));
            assert!(ack.state.focusable);
        }
    }

    // --- structural invariants (widget tree mirrors taffy) --------

    #[test]
    fn step_layouts_mirror_taffy_and_fit_compact_viewports() {
        // Phase `every_step_widget_tree_mirrors_taffy`.
        {
            for step in [
                Step::Network,
                Step::Disk,
                Step::Account,
                Step::Keyboard,
                Step::Options,
                Step::Review,
                Step::Install,
            ] {
                let mut app = InstallerApp::for_shot(step);
                let (taffy, root) = build(&mut app);
                assert_tree_mirrors(&app, &taffy, root);
            }
            // The two terminal Install variants too.
            let mut done = InstallerApp::for_shot(Step::Install);
            done.shot_finished();
            let (t, r) = build(&mut done);
            assert_tree_mirrors(&done, &t, r);

            let mut failed = InstallerApp::for_shot(Step::Install);
            failed.shot_failed();
            let (t, r) = build(&mut failed);
            assert_tree_mirrors(&failed, &t, r);
        }
        // Phase `compact_review_narrows_the_card_and_exposes_keyboard_scroll`.
        {
            let mut app = InstallerApp::for_shot(Step::Review);
            layout_at(&mut app, 320.0, 480.0);

            let card = find_named(&app, "InstallerCard").expect("review card");
            assert!(
                card.get_rect().width() <= 320.0 - 2.0 * f64::from(VIEWPORT_PADDING),
                "responsive card escaped compact viewport: {:?}",
                card.get_rect()
            );
            assert!(card.get_rect().x0 >= f64::from(VIEWPORT_PADDING));
            assert!(
                card.get_rect().height() > 480.0,
                "representative Review must exercise vertical overflow"
            );
            let stage = find_named(&app, "InstallerViewportStage").expect("review viewport stage");
            assert!(
                stage.get_rect().y1 >= card.get_rect().y1,
                "scroll extent stops before the Review card ends"
            );

            let scroll = find_named(&app, "Scrollable").expect("installer scroll viewport");
            assert!(
                scroll.is_focusable(),
                "overflowing installer must expose Page Up/Down, Home/End and arrow scrolling"
            );
        }
        // Phase `compact_review_keeps_confirmation_controls_at_full_height`.
        {
            let mut app = InstallerApp::for_shot(Step::Review);
            layout_at(&mut app, 1280.0, 768.0);
            let card = find_named(&app, "InstallerCard").expect("review card");
            let input = find_named(&app, "TextInput").expect("destructive confirmation input");
            let controls = card.children();
            let ack = controls
                .iter()
                .find(|child| child.test_id() == Some("recovery_ack"))
                .expect("recovery-key acknowledgement");
            let nav = controls.last().expect("review navigation");
            assert!(
                input.get_rect().height() >= 38.0,
                "confirmation input collapsed: {:?}",
                input.get_rect()
            );
            assert!(
                card.get_rect().y1 >= input.get_rect().y1,
                "confirmation input escaped the review card"
            );
            assert!(
                ack.get_rect().height() >= 40.0,
                "acknowledgement collapsed: {:?}",
                ack.get_rect()
            );
            assert!(
                ack.get_rect().y0 >= input.get_rect().y1,
                "acknowledgement overlaps confirmation input"
            );
            assert!(
                nav.get_rect().y0 >= ack.get_rect().y1,
                "navigation overlaps acknowledgement"
            );
            assert!(
                card.get_rect().y1 >= nav.get_rect().y1,
                "navigation escaped review card"
            );
        }
    }

    /// Count focusable widgets anywhere in the subtree (the Account step's
    /// nav buttons live nested inside a Row, not as direct card children).
    fn count_focusable(widget: &dyn Widget) -> usize {
        let mut n = usize::from(widget.is_focusable());
        for c in widget.children() {
            n = n.saturating_add(count_focusable(c));
        }
        n
    }

    #[test]
    fn step_controls_are_focusable_only_when_usable() {
        // Phase `account_step_inputs_focusable`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Account;
            let (_taffy, _root) = build(&mut app);

            // Username + optional full name + password + confirmation + Back + Next.
            assert_eq!(
                count_focusable(&app),
                6,
                "4 inputs + 2 nav buttons must be focusable"
            );
        }
        // Phase `keyboard_step_has_dropdown_and_nav_and_a_preview`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Keyboard;
            let (_taffy, _root) = build(&mut app);

            // Keyboard is the FIRST step, so there is no Back: Dropdown + Next =
            // 2 focusable widgets (the KeyboardView preview is a non-focusable
            // leaf and must not count).
            assert_eq!(
                count_focusable(&app),
                2,
                "layout dropdown + Next must be focusable (no Back on the first step)"
            );

            // The visual preview is present as a card child.
            let card = find_named(&app, "InstallerCard").expect("keyboard card");
            assert!(
                card.children().iter().any(|c| c.name() == "KeyboardView"),
                "the Keyboard step renders the visual keyboard preview"
            );
        }
        // Phase `disk_step_first_button_disabled_until_selection`.
        {
            // Build without a selection: the nav row's primary ("Next", its last
            // child after "Back") must be non-focusable (disabled). After
            // selecting a disk it enables.
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let _ = build(&mut app);
            let card = find_named(&app, "InstallerCard").expect("disk card");
            let next_disabled = card
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| !b.is_focusable()));
            assert_eq!(next_disabled, Some(true), "Next disabled with no disk");

            let mut app2 = InstallerApp::with_disks(sample());
            app2.step = Step::Disk;
            app2.selected_disk = Some(0);
            let _ = build(&mut app2);
            let card2 = find_named(&app2, "InstallerCard").expect("disk card");
            let next_enabled = card2
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| b.is_focusable()));
            assert_eq!(
                next_enabled,
                Some(true),
                "Next enabled once a disk is selected"
            );
        }
        // Phase `disk_step_rows_are_focusable`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let _ = build(&mut app);
            let card = find_named(&app, "InstallerCard").expect("disk card");
            let fields = card.children();
            let disk_rows = fields.iter().filter(|c| c.name() == "DiskRow").count();
            assert_eq!(disk_rows, 2, "both sample disks render as rows");
            assert!(
                fields
                    .iter()
                    .any(|c| c.name() == "DiskRow" && c.is_focusable())
            );
        }
        // Phase `running_system_disk_is_visible_but_not_focusable`.
        {
            let mut disks = sample();
            disks[0].is_system = true;
            let mut app = InstallerApp::with_disks(disks);
            app.step = Step::Disk;
            let _ = build(&mut app);
            let card = find_named(&app, "InstallerCard").expect("disk card");
            let disk_rows: Vec<_> = card
                .children()
                .into_iter()
                .filter(|child| child.name() == "DiskRow")
                .collect();
            assert_eq!(disk_rows.len(), 2);
            assert!(!disk_rows[0].is_focusable());
            assert!(disk_rows[1].is_focusable());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn media_profile_skips_configured_screens_and_routes_repairs() {
        // Phase `validated_media_profile_skips_already_configured_screens`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                hostname: Some("studio".into()),
                username: Some("quinten".into()),
                full_name: Some("Quinten Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: Vec::new(),
            });
            app.tpm2 = false;
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.hostname, "studio");
            assert_eq!(app.username, "quinten");
            assert_eq!(app.keymap, "German (QWERTZ)");
            assert_eq!(app.filesystem(), "ext4");

            app.selected_disk = Some(0);
            app.go_next();
            assert_eq!(app.step, Step::Account);
            let _ = build(&mut app);
            assert!(find_accessible(&app, "Secure your account").is_some());
            assert!(find_accessible(&app, "Username").is_none());
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            app.go_next();
            assert_eq!(app.step, Step::Review, "configured Options is not repeated");
            app.go_back();
            assert_eq!(app.step, Step::Options);
            let _ = build(&mut app);
            assert!(
                has_test_id(app.composed.as_deref().unwrap(), "fido2_choice"),
                "Review Back must keep on-device security choices reachable"
            );
            app.go_back();
            assert_eq!(app.step, Step::Account);
            app.go_back();
            assert_eq!(app.step, Step::Disk);
            app.go_back();
            assert_eq!(app.step, Step::Network);
        }
        // Phase `partial_media_profile_routes_to_only_the_repair_screens`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                hostname: None,
                username: Some("quinten".into()),
                full_name: Some("Quinten Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![crate::media_profile::MediaProfileField::Hostname],
            });
            app.tpm2 = false;
            assert_eq!(app.step, Step::Disk, "sound keyboard is retained");
            assert_eq!(app.username, "quinten");
            assert_eq!(app.filesystem(), "ext4");

            app.selected_disk = Some(0);
            app.go_next();
            assert_eq!(app.step, Step::Account);
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            app.go_next();
            assert_eq!(app.step, Step::Options, "invalid hostname needs one repair");
            let _ = build(&mut app);
            assert!(find_accessible(&app, "Hostname").is_some());
            assert!(find_accessible(&app, "Filesystem").is_none());
            assert!(find_accessible(&app, "Time zone").is_none());
        }
        // Phase `unsupported_profile_locale_gets_one_bounded_on_device_chooser`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                hostname: Some("studio".into()),
                username: Some("quinten".into()),
                full_name: Some("Quinten Example".into()),
                locale: None,
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![crate::media_profile::MediaProfileField::Locale],
            });
            app.tpm2 = false;
            app.selected_disk = Some(0);
            app.go_next();
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            app.go_next();
            assert_eq!(app.step, Step::Options);
            let _ = build(&mut app);
            assert!(
                find_accessible_role(&app, "Language & locale", ui::a11y::Role::ComboBox).is_some(),
                "locale repair must be a bounded chooser"
            );
            assert!(find_accessible(&app, "Hostname").is_none());
            assert!(find_accessible(&app, "Filesystem").is_none());
            assert!(find_accessible(&app, "Time zone").is_none());
        }
        // Phase `invalid_keyboard_starts_at_keyboard_but_retains_other_profile_values`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                hostname: Some("studio".into()),
                username: Some("quinten".into()),
                full_name: Some("Quinten Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: None,
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![crate::media_profile::MediaProfileField::Keyboard],
            });
            assert_eq!(app.step, Step::Keyboard);
            assert_eq!(app.hostname, "studio");
            assert_eq!(app.username, "quinten");
            assert!(app.status.contains("keyboard layout"));
        }
    }
}
