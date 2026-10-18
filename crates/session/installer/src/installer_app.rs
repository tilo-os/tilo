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
//! The installer keeps nothing in its document. It runs once, on one
//! machine, as a privileged process whose secrets must never reach CRDT
//! history, and nothing else reads or syncs its state. Every choice of the
//! install (disk, method, filesystem, security key, codecs, recovery
//! promises) is wizard state written only by [`InstallerApp::apply`], the
//! typed operation every door ends in. The typed and picked fields
//! (username, full name, hostname, locale, time zone, keyboard layout, the
//! destructive confirmation) are view state: each `TextInput` or `Dropdown`
//! edits a [`ui::LocalState`] the wizard reads when it validates and
//! installs, and the same operations set them for every other caller.
//!
//! A control does not act by itself either: it names the action the
//! installer publishes for it (see the `surface` module) and queues one
//! [`crate::control::Press`]; the wizard runs it through the same door the
//! console and the host use, so a control can never do something its
//! published action would refuse.
//!
//! The install runs on a **worker thread**: on `begin` the
//! app spawns [`crate::install::run_install`] and streams each output line
//! back through the winit `EventLoopProxy` as a `ui::UserEvent::External`,
//! folded into the status log by [`InstallerApp::on_external_event`] — so
//! the UI stays live and the log animates. If no proxy is wired (tests /
//! no event loop) it falls back to a synchronous run.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use serde_json::Value;
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

use install_profile_model::{
    FILESYSTEMS, valid_full_name, valid_hostname, valid_locale, valid_timezone_name, valid_username,
};

use crate::action_button::ActionButton;
use crate::backend::{InstallBackend, SystemBackend};
use crate::control::{Control, Press, Presses};
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

/// Labels for the FIDO2 segmented selector, in display order. Index 0 (off)
/// is the default; index 1 enrolls a hardware security key as an extra
/// unlock factor. Only shown when encryption is on.
pub const FIDO2_LABELS: [&str; 2] = ["No security key", "Enroll security key"];

/// The heading of the media codecs opt-in.
pub const CODECS_TITLE: &str = "Play more video and audio formats";

/// What the Media step says once Install was pressed, above its Undo.
pub const CODECS_CHOSEN: &str = "Chosen: installed once the new system is online";

/// The one short line under the title. The installer only records the
/// choice; the installed system downloads the formats once it is online.
pub const CODECS_DESCRIPTION: &str = "Downloaded to this computer from the formats' publishers once \
it is online. You can turn it off in Settings.";

/// Default hostname seeded into the CRDT (and the installed system).
const DEFAULT_HOSTNAME: &str = "tilo";
/// Default time zone seeded into the CRDT.
const DEFAULT_TIMEZONE: &str = "UTC";
/// Default keyboard-layout display name seeded into the CRDT.
const DEFAULT_KEYMAP_NAME: &str = layouts::DEFAULT_DISPLAY_NAME;

/// Filesystem choices, in display order. Index 0 is the default.
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
const WINDOWS_ALONGSIDE_PREPARATION: &str = "installer-storage-windows-preparation";

const LINUX_ALONGSIDE_READY: &str = "installer-storage-linux-ready";
const LINUX_ALONGSIDE_PREPARATION: &str = "installer-storage-linux-preparation";
const ALONGSIDE_BOOT_GUIDANCE: &str = "installer-storage-boot-guidance";
const ALONGSIDE_FILES_GUIDANCE: &str = "installer-storage-files-guidance";

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

/// Bind intent as strictly as a destructive target: a reused device path or a
/// replaced partition table must not inherit a previous disk's erase choice.
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
/// Height of the install status log box.
const STATUS_LOG_HEIGHT: f32 = 200.0;

/// The wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Keyboard,
    Network,
    Accounts,
    Disk,
    Account,
    Options,
    /// The optional media formats: one Install button, nothing chosen in
    /// advance.
    Media,
    Review,
    Install,
}

/// Captions for the step-progress breadcrumb, index-aligned with
/// [`step_index`].
pub const STEP_LABELS: [&str; 9] = [
    "Keyboard", "Network", "Accounts", "Disk", "Account", "Options", "Media", "Review", "Install",
];

/// Position of `step` in the breadcrumb (0-based). Keyboard is first so the
/// layout is chosen BEFORE the user types anything (username/password).
pub fn step_index(step: Step) -> usize {
    match step {
        Step::Keyboard => 0,
        Step::Network => 1,
        Step::Accounts => 2,
        Step::Disk => 3,
        Step::Account => 4,
        Step::Options => 5,
        Step::Media => 6,
        Step::Review => 7,
        Step::Install => 8,
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
        Step::Network => Some(Step::Accounts),
        Step::Accounts => Some(Step::Disk),
        Step::Disk => Some(Step::Account),
        Step::Account => Some(Step::Options),
        Step::Options => Some(Step::Media),
        Step::Media => Some(Step::Review),
        Step::Review => Some(Step::Install),
        Step::Install => None,
    }
}

/// The step reached by pressing Back from `step`, if any.
pub fn prev_step(step: Step) -> Option<Step> {
    match step {
        Step::Network => Some(Step::Keyboard),
        Step::Accounts => Some(Step::Network),
        Step::Disk => Some(Step::Accounts),
        Step::Account => Some(Step::Disk),
        Step::Options => Some(Step::Account),
        Step::Media => Some(Step::Options),
        Step::Review => Some(Step::Media),
        // Keyboard is the first step; Back out of Install is handled
        // specially (only on failure) by the app — see `go_back`.
        Step::Keyboard | Step::Install => None,
    }
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
        // A layout is always selected (seeded to the default), the media
        // formats are optional, and Review/Install have nothing left to
        // validate, so these steps never block forward progress.
        Step::Network
        | Step::Accounts
        | Step::Keyboard
        | Step::Media
        | Step::Review
        | Step::Install => Ok(()),
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
        let style = Style {
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
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
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
        if let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) {
            ui::widget::route_to_children(&mut self.children, &child_nodes, event, ctx);
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
        let style = Style {
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
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        if let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) {
            ui::widget::route_to_children(&mut self.children, &child_nodes, event, ctx);
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

// ===================================================================
// InstallerApp
// ===================================================================

/// Shared slot for the winit `EventLoopProxy`. `main` fills it once the
/// event loop exists (via `run_with_setup`); the install worker thread
/// reads it to pump progress lines back as `UserEvent::External`.
pub type ProxySlot = Arc<Mutex<Option<EventLoopProxy<UserEvent>>>>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent wizard-state flag (encryption/FIDO2 opt-in, TPM presence, shot/dirty/focus one-shots), not a combinatorial state machine"
)]
pub struct InstallerApp {
    network: InstallerNetwork,
    // Online identities, commands and credentials never enter the document or profile.
    accounts_form: crate::online_accounts::view::AccountsForm,
    accounts_controller: Option<crate::online_accounts::AccountsController>,
    accounts_initialized: bool,
    windows_hints: crate::online_accounts::hints::WindowsReview,
    disks: Vec<Disk>,
    selected_disk: Option<usize>,
    /// Explicit choice when a disk has multiple safe unallocated extents.
    /// A sole candidate is selected implicitly; ambiguity never is.
    selected_guided_extent: Option<usize>,
    /// Bound to the exact hardware inventory and free extent, never an index alone.
    alongside_capacity: Option<(Disk, GuidedAlongsidePlan, u64)>,
    install_method: InstallMethod,
    /// Manual method choices are session-local and bound to discovered hardware,
    /// never a global preference that can carry Erase onto another disk.
    disk_method_choices: HashMap<DiskMethodIdentity, InstallMethod>,
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
    /// The recovery key's copy, whose outcome can arrive after it was
    /// shown as copied.
    recovery_key_copy: ui::CopyWatch,
    /// Explicit confirmation made after the concrete recovery key was shown.
    /// Copying to the live clipboard alone is not evidence of an off-device copy.
    recovery_key_saved: bool,
    /// Typed account/system fields, edited in place by their `TextInput`s
    /// and read for validation, the Review display and the backend argv.
    username: ui::LocalState<String>,
    full_name: ui::LocalState<String>,
    // Secrets deliberately bypass Automerge: deleting CRDT text leaves the
    // old characters in document history. These buffers are local to the
    // privileged installer process and are zeroized when consumed.
    password: Arc<Mutex<String>>,
    password_confirmation: Arc<Mutex<String>>,
    /// Optional action-supplied disk secret. Empty means the account password
    /// is also the startup secret, preserving the graphical wizard behavior.
    disk_password: Arc<Mutex<String>>,
    wifi_password: Arc<Mutex<String>>,
    hostname: ui::LocalState<String>,
    purpose: String,
    device_label: String,
    prepared_setup: Option<install_profile_model::PreparedSetupExpectation>,
    locale: ui::LocalState<String>,
    timezone: ui::LocalState<String>,
    /// Selected keyboard-layout *display name*. Resolved to xkb
    /// layout/variant via [`crate::layouts`] when building the install
    /// command.
    keymap: ui::LocalState<String>,
    /// Typed confirmation for the exact object that will be formatted: the
    /// whole disk for Erase, or the prepared root partition for Alongside.
    erase_confirmation: ui::LocalState<String>,
    /// The confirmation as of the last sync, so a user edit is told apart
    /// from the wizard's own resets.
    erase_confirmation_seen: String,
    /// Time-zone choices shown by the Options-step [`Dropdown`], sourced
    /// from the system at runtime (see [`crate::timezones::list_timezones`]).
    timezones: Vec<String>,
    /// Headless-shot only: build the Options-step time-zone dropdown in
    /// its open state so `--shots --scenario options-open` captures the popup.
    tz_open_for_shot: bool,
    /// Transient validation / error line under the controls.
    status: String,
    /// Presses the composed controls queued, run by [`Self::run_presses`].
    presses: Presses,
    dirty_structure: bool,
    /// One-shot: raised on a step transition so the App re-focuses the new
    /// step's first field. Drained via [`Widget::take_focus_reset`].
    focus_reset: bool,
    rect: Rect,
    composed: ui::widget::Composed,
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
        let profile = crate::internal_handoff::media_profile(&handoff);
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
            .map(|handoff| handoff.host_os)
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
            alongside_capacity: None,
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
            tpm2: has_tpm2(),
            secure_boot: secure_boot_enabled(),
            kernel_lockdown: kernel_lockdown_active(),
            target_verified_boot: target_verified_boot_ready(),
            step: Step::Keyboard,
            install_state: InstallState::Idle,
            status_lines: Vec::new(),
            recovery_key: None,
            recovery_key_copied: false,
            recovery_key_copy: ui::CopyWatch::default(),
            recovery_key_saved: false,
            username: ui::LocalState::new(String::new()),
            full_name: ui::LocalState::new(String::new()),
            password: Arc::new(Mutex::new(String::new())),
            password_confirmation: Arc::new(Mutex::new(String::new())),
            disk_password: Arc::new(Mutex::new(String::new())),
            wifi_password: Arc::new(Mutex::new(String::new())),
            accounts_form: crate::online_accounts::view::AccountsForm::new(),
            accounts_controller: None,
            accounts_initialized: false,
            windows_hints: crate::online_accounts::hints::WindowsReview::default(),
            hostname: ui::LocalState::new(DEFAULT_HOSTNAME.to_string()),
            locale: ui::LocalState::new("en_US.UTF-8".to_string()),
            timezone: ui::LocalState::new(DEFAULT_TIMEZONE.to_string()),
            purpose: install_profile_model::default_purpose(),
            device_label: String::new(),
            prepared_setup: None,
            keymap: ui::LocalState::new(DEFAULT_KEYMAP_NAME.to_string()),
            erase_confirmation: ui::LocalState::new(String::new()),
            erase_confirmation_seen: String::new(),
            timezones: default_timezones(),
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
        self.purpose.clone_from(&profile.purpose);
        self.device_label.clone_from(&profile.device_label);
        self.prepared_setup.clone_from(&profile.prepared_setup);
        self.windows_hints
            .set_profile(profile.windows_preparation.clone(), &self.accounts_form);
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
        app.username.set("ada".to_string());
        app.full_name.set("ada Example".to_string());
        app.password = Arc::new(Mutex::new("correct horse battery staple".to_string()));
        app.password_confirmation =
            Arc::new(Mutex::new("correct horse battery staple".to_string()));
        app.hostname.set(DEFAULT_HOSTNAME.to_string());
        app.timezone.set(DEFAULT_TIMEZONE.to_string());
        app.keymap.set(DEFAULT_KEYMAP_NAME.to_string());
        if !matches!(
            step,
            Step::Network | Step::Accounts | Step::Keyboard | Step::Disk
        ) {
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
    /// renders open — used by `--shots --scenario options-open` to capture the
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
            purpose: install_profile_model::default_purpose(),
            device_label: String::new(),
            prepared_setup: None,
            windows_preparation: None,
            hostname: Some("studio".into()),
            username: Some("ada".into()),
            full_name: Some("ada Example".into()),
            locale: None,
            keyboard: Some("de".into()),
            timezone: Some("Europe/Berlin".into()),
            filesystem: Some("ext4".into()),
            repairs: vec![crate::media_profile::MediaProfileField::Locale],
        });
        self.selected_disk = Some(0);
        self.step = Step::Options;
    }

    /// Pose the Media step after its Install button was pressed.
    pub fn shot_codecs_chosen(&mut self) {
        self.step = Step::Media;
        self.codecs = true;
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
    /// `--shots --scenario keyboard` render, so the visual preview clearly
    /// shows remapped keys.
    pub fn shot_keymap(&mut self, display_name: &str) {
        self.step = Step::Keyboard;
        self.keymap.set(display_name.to_string());
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

    /// Recompute the safe default, or restore this disk's explicit choice.
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

    /// The selected disk, if any.
    fn selected_disk_ref(&self) -> Option<&Disk> {
        self.selected_disk.and_then(|i| self.disks.get(i))
    }

    /// The selected disk's device path, if any.
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
        layouts::by_display_name(self.keymap().trim()).unwrap_or_else(layouts::default_layout)
    }

    /// The chosen keyboard layout's display name.
    fn keymap(&self) -> String {
        local_text(&self.keymap)
    }

    /// The chosen locale.
    fn locale(&self) -> String {
        local_text(&self.locale)
    }

    /// The chosen time zone.
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
            .is_some_and(|device| self.erase_confirmation.with(|typed| typed.trim() == device))
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
            hostname: local_text(&self.hostname).trim().to_string(),
            purpose: self.purpose.clone(),
            device_label: self.device_label.clone(),
            prepared_setup: self.prepared_setup.clone(),
            full_name: local_text(&self.full_name).trim().to_string(),
            locale: self.locale().trim().to_string(),
            timezone: self.timezone().trim().to_string(),
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
            online_accounts: None,
            account_operation: String::new(),
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

    /// Reset the typed confirmation. A reset is the wizard's own change,
    /// not a user edit, so the next sync leaves the status line alone.
    fn clear_erase_confirmation(&mut self) {
        self.set_erase_confirmation(String::new());
    }

    fn set_erase_confirmation(&mut self, value: String) {
        self.erase_confirmation.set(value.clone());
        self.erase_confirmation_seen = value;
    }

    fn validate_account_details(&self) -> Result<(), String> {
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
        validate_startup_secret_for(
            &self.startup_secret(),
            matches!(
                self.verified_boot_state(),
                VerifiedBootState::TargetVerified
            ),
        )
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

    /// Create custody only on entry, below the owned 0700 runtime directory.
    /// `live` validates the directory and refuses both real and effective root
    /// before creating its private session child or launching any OAuth flow.
    fn enter_accounts(&mut self) {
        if self.step != Step::Accounts || self.accounts_initialized {
            return;
        }
        self.accounts_initialized = true;
        self.accounts_controller = std::env::var_os("XDG_RUNTIME_DIR").and_then(|root| {
            crate::online_accounts::AccountsController::live(std::path::Path::new(&root)).ok()
        });
        if self.accounts_controller.is_none() {
            self.accounts_form
                .set_status(crate::online_accounts::view::AccountsStatus::Failed);
        }
    }

    fn leave_accounts(&mut self) {
        use crate::online_accounts::view::{AccountsCommand, AccountsStatus};
        if self.step != Step::Accounts {
            return;
        }
        // Discard pending starts before cancelling the actor; its generation is
        // invalidated even when a successful result has not yet been polled.
        self.accounts_form.drain_commands();
        self.windows_hints.cancel(&self.accounts_form);
        if let Some(controller) = self.accounts_controller.as_mut() {
            let _ = controller.command(AccountsCommand::Cancel);
        }
        self.accounts_form.set_status(AccountsStatus::Ready);
        let _ = self.accounts_form.select_source(None);
    }

    fn sync_accounts(&mut self) {
        use crate::online_accounts::view::{AccountsCommand, AccountsStatus};
        if self.step != Step::Accounts {
            return;
        }
        self.enter_accounts();
        self.windows_hints.sync(&self.accounts_form);
        for command in self.accounts_form.drain_commands() {
            let skip = matches!(command, AccountsCommand::SkipOffline);
            let escape = matches!(
                command,
                AccountsCommand::Cancel | AccountsCommand::SkipOffline
            );
            let status = match self.accounts_controller.as_mut() {
                Some(controller) => {
                    if controller.command(command).is_ok() {
                        controller.status()
                    } else {
                        AccountsStatus::Failed
                    }
                }
                None if escape => AccountsStatus::Ready,
                None => AccountsStatus::Failed,
            };
            self.accounts_form.set_status(status);
            self.dirty_structure = true;
            if skip {
                self.go_next();
                return;
            }
        }
        if let Some(controller) = self.accounts_controller.as_mut() {
            let status = controller.poll();
            self.accounts_form.set_connected(controller.connected());
            if status != self.accounts_form.status() {
                self.accounts_form.set_status(status);
                self.dirty_structure = true;
            }
            if status == AccountsStatus::ReviewCalendars {
                self.accounts_form.set_calendars(controller.calendars());
            }
        }
        self.dirty_structure |= self.accounts_form.take_structure_changed();
    }

    /// Seal this session's explicitly selected accounts for an authenticated
    /// install operation. The caller must persist the ciphertext in the target
    /// user's handoff and arrange adoption at login, never in a public profile.
    /// This password-based API remains available for portable-media callers;
    /// live installation uses a fresh private transport key instead.
    ///
    /// # Errors
    /// Returns a fixed error if custody cannot seal the selected accounts.
    pub fn stage_online_accounts(
        &mut self,
        password: &[u8],
        binding: &account_custody::portable::PortableBinding,
    ) -> Result<
        Option<account_custody::portable::PasswordSealedAccounts>,
        crate::online_accounts::AccountsError,
    > {
        let Some(controller) = self.accounts_controller.as_mut() else {
            return Ok(None);
        };
        let selected = controller.selected().to_vec();
        if selected.is_empty() {
            return Ok(None);
        }
        controller
            .stage_selected(&selected, password, binding)
            .map(Some)
    }

    fn prepare_live_accounts(
        &mut self,
        opts: &mut InstallOptions,
        user: &str,
    ) -> Result<(), crate::online_accounts::AccountsError> {
        use crate::online_accounts::AccountsError;
        let Some(controller) = self.accounts_controller.as_mut() else {
            return Ok(());
        };
        let selected = controller.selected().to_vec();
        if selected.is_empty() {
            return Ok(());
        }
        // These are the current reviewed options, not a caller-supplied profile.
        // The backend revalidates each disk field before computing this binding.
        if opts.disk_size_bytes == 0 || opts.disk_major_minor.is_empty() {
            return Err(AccountsError::InvalidRequest);
        }
        let operation = uuid::Uuid::new_v4().to_string();
        let target = crate::account_handoff::target_fingerprint(
            opts.disk_size_bytes,
            &opts.disk_major_minor,
            &opts.disk_wwn,
            &opts.disk_serial,
        );
        let binding =
            account_custody::portable::PortableBinding::new(&operation, &target, user, 1000)
                .map_err(|_| AccountsError::InvalidRequest)?;
        let key = tilo_seal::SymmetricKey::generate().map_err(|_| AccountsError::Storage)?;
        let sealed = controller.stage_selected_with_key(&selected, &key, &binding)?;
        opts.online_accounts = Some(Arc::new(crate::account_handoff::InstallAccounts::new(
            &sealed, key,
        )));
        opts.account_operation = operation;
        Ok(())
    }

    fn next_setup_step(&self) -> Option<Step> {
        // Storage revalidation must not repeat a completed account. A public
        // prepared-setup binding alone is not a credential: reuse only secrets
        // supplied in this session, checked against the current boot policy.
        let account_ready = self.step == Step::Disk
            && self
                .profile_completion
                .is_some_and(ProfileCompletion::identity_complete)
            && valid_username(&local_text(&self.username))
            && self.validate_account_details().is_ok();
        // Never skip media consent or Review. Unsupported filesystems still
        // need repair, even if the original profile was complete.
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
                        Step::Network
                        | Step::Accounts
                        | Step::Disk
                        | Step::Media
                        | Step::Review
                        | Step::Install => {}
                    }
                }
                if let Some(n) = self.next_setup_step() {
                    // Never auto-advance into Install via Next; the Begin
                    // button (begin flag) is the only entry into install.
                    if n != Step::Install {
                        self.leave_accounts();
                        self.step = n;
                        self.enter_accounts();
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
        // Back always follows the visible breadcrumb. A shortened forward path
        // must not trap the owner between Account and Review: Options (including
        // FIDO2) and Network remain available for deliberate changes.
        let destination = prev_step(self.step);
        if let Some(p) = destination {
            self.leave_accounts();
            self.step = p;
            self.enter_accounts();
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
    fn begin_install(&mut self) {
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
        let user = local_text(&self.username);
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
        let mut opts = self.install_options();
        if self.prepare_live_accounts(&mut opts, &user).is_err() {
            self.set_status("Could not prepare your connected accounts; retry before installing");
            return;
        }

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
        self.recovery_key_copy = ui::CopyWatch::default();
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

    /// A control running `action` on `resource` with `args`.
    fn control(&self, resource: &str, action: &str, args: Value) -> Control {
        self.presses.control(Press::new(resource, action, args))
    }

    fn next_control(&self) -> Control {
        self.control(op::SESSION, "next", Value::Null)
    }

    fn back_control(&self) -> Control {
        self.control(op::SESSION, "back", Value::Null)
    }

    /// A navigation row for `step`: `Back` (secondary) — shown only when the
    /// step has a predecessor, so the first step has no dangling Back — plus
    /// the forward action (`forward_label`, primary, running `forward`,
    /// optionally disabled). A real `ui::Row` so its buttons stay in the
    /// focusable order (widget tree mirrors taffy).
    fn nav_row(
        &self,
        step: Step,
        forward: Control,
        forward_label: &str,
        forward_disabled: bool,
    ) -> Box<dyn Widget> {
        let mut row = Row::new();
        if prev_step(step).is_some() {
            row = row.push(Box::new(ActionButton::new("Back", self.back_control())));
        }
        row = row.push(Box::new(
            ActionButton::new(forward_label, forward)
                .primary()
                .disabled(forward_disabled),
        ));
        Box::new(row)
    }

    /// The Finish-screen action row: Reboot (primary) + Power off (secondary).
    fn finish_row(&self, disabled: bool) -> Box<dyn Widget> {
        Box::new(
            Row::new()
                .push(Box::new(
                    ActionButton::new("Reboot now", self.control(op::POWER, "reboot", Value::Null))
                        .primary()
                        .disabled(disabled),
                ))
                .push(Box::new(
                    ActionButton::new(
                        "Power off",
                        self.control(op::POWER, "power-off", Value::Null),
                    )
                    .disabled(disabled),
                )),
        )
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

        let mut actions: Vec<Box<dyn Widget>> = Vec::new();
        match self.step {
            Step::Network => self.compose_network_step(theme, &mut children, &mut actions),
            Step::Accounts => {
                children.push(self.accounts_form.view());
                actions.push(self.nav_row(Step::Accounts, self.next_control(), "Next", false));
            }
            Step::Disk => self.compose_disk_step(theme, &mut children, &mut actions),
            Step::Account => self.compose_account_step(theme, &mut children, &mut actions),
            Step::Keyboard => self.compose_keyboard_step(theme, &mut children, &mut actions),
            Step::Options => self.compose_options_step(theme, &mut children, &mut actions),
            Step::Media => self.compose_media_step(theme, &mut children, &mut actions),
            Step::Review => self.compose_review_step(theme, &mut children, &mut actions),
            Step::Install => self.compose_install_step(theme, &mut children, &mut actions),
        }

        // A staged install has no medium to pull: offer the way back to the
        // retained OS on every screen except while installing or once done.
        if let Some(host) = self.internal_host()
            && !matches!(
                self.install_state,
                InstallState::Running | InstallState::Done
            )
        {
            actions.push(Box::new(
                ActionButton::new(
                    &format!("Return to {}", host.display_name()),
                    self.control(op::SESSION, "return-to-host", Value::Null),
                )
                .with_test_id("return_to_host"),
            ));
        }

        // Validation stays alongside the actions, outside the content viewport.
        if !self.status.is_empty() {
            actions.push(Box::new(
                Text::new(&self.status)
                    .with_id("installer.validation")
                    .with_role(TextRole::Caption)
                    .with_color(theme.tokens.colors.semantic.danger)
                    .with_live(Politeness::Assertive),
            ));
        }

        let card: Box<dyn Widget> = Box::new(Card::new(children));
        let scroll = Scrollable::new_vertical()
            .fill_parent_height()
            .with_focus_reveal()
            .with_persisted_offset(Rc::clone(&self.viewport_offset))
            .always_visible()
            .with_child(Box::new(ViewportStage::new(card)));
        let mut page = ui::widget::Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(scroll));
        if !actions.is_empty() {
            let mut footer = ui::widget::Column::new()
                .with_id("installer.actions")
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s2);
            for action in actions {
                footer = footer.push(action);
            }
            page = page.push(Box::new(
                ui::widget::Container::new()
                    .with_no_padding()
                    .with_centered_max_width(theme.tokens.metrics.readable_width)
                    .with_child(Box::new(footer)),
            ));
        }
        Box::new(page)
    }

    fn compose_network_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;

        children.push(Box::new(
            Text::new("Connect to a network").with_role(TextRole::Title),
        ));
        children.push(Box::new(
            Text::new(
                "Optional — Tilo installs from this medium and does not require internet access.",
            )
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
                NetworkPhase::Connecting(ssid) => format!("Connecting to {ssid}…"),
                _ => "Checking networks…".to_string(),
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
                    Text::new("Wi-Fi password").with_role(TextRole::Body),
                ));
                children.push(Box::new(
                    TextInput::new(Vec::new())
                        .with_id("installer.wifi_password")
                        .with_external_buffer(Arc::clone(&self.wifi_password))
                        .with_secret(true),
                ));
            }
            children.push(Box::new(
                ActionButton::new("Connect", self.control(op::NETWORK, "connect", Value::Null))
                    .primary()
                    .disabled(self.network.is_busy()),
            ));
        }

        if let Some(uri) = self.network.snapshot.portal_uri.as_deref() {
            children.push(Box::new(
                Text::new(&format!("Network sign-in address: {uri}"))
                    .with_role(TextRole::Caption)
                    .with_color(theme.text_primary),
            ));
            children.push(Box::new(
                Text::new("For safety, the privileged live installer does not open portal web content. Continue offline, then open this address after first boot.")
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        } else if let Some(error) = self.network.snapshot.portal_error.as_deref() {
            children.push(Box::new(
                Text::new(&format!(
                    "A sign-in page was detected but cannot be opened: {error}"
                ))
                .with_role(TextRole::Caption)
                .with_color(dim),
            ));
        }

        children.push(Box::new(
            Text::new("Wi-Fi connections made here are saved for first boot. You can also continue without one.")
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            ActionButton::new(
                "Refresh networks",
                self.control(op::NETWORK, "refresh", Value::Null),
            )
            .disabled(self.network.is_busy()),
        ));
        let continue_label =
            if self.network.snapshot.connectivity == os_actions::ConnectivityState::Full {
                "Continue"
            } else {
                "Continue offline"
            };
        actions.push(self.nav_row(Step::Network, self.next_control(), continue_label, false));
    }

    /// Add the Wi-Fi error or the currently visible network choices.
    fn compose_network_choices(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;

        if let Some(error) = self.network.snapshot.wifi_error.as_deref() {
            children.push(Box::new(
                Text::new(&format!("Wi-Fi controls unavailable: {error}"))
                    .with_role(TextRole::Caption)
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
                    ActionButton::new(
                        &label,
                        self.control(
                            op::NETWORK,
                            "select",
                            serde_json::json!({ "ssid": network.ssid }),
                        ),
                    )
                    .with_test_id(&format!("wifi_network_{i}"))
                    .disabled(self.network.is_busy()),
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
            Text::new("Install Tilo").with_role(TextRole::Title),
        ));
        self.compose_install_method(theme, children);
        children.push(Box::new(
            ActionButton::new(
                "Refresh disks",
                self.control(op::STORAGE, "refresh", Value::Null),
            )
            .with_test_id("disk_refresh"),
        ));
        if self.disks.is_empty() {
            children.push(Box::new(
                Text::new(&crate::i18n::t("installer-storage-no-drives"))
                    .with_role(TextRole::Body)
                    .with_color(danger),
            ));
        } else {
            for (i, disk) in self.disks.iter().enumerate() {
                children.push(Box::new(DiskRow::new(
                    disk.clone(),
                    self.control(&op::disk_resource(&disk.name), "select", Value::Null),
                    self.selected_disk == Some(i),
                )));
            }
        }
        children.extend(self.windows_alongside_preparation(theme));
        if self.install_method == InstallMethod::Alongside
            && self
                .selected_disk_ref()
                .is_some_and(Disk::has_linux_storage)
            && !self.guided_alongside_plans().is_empty()
        {
            children.push(Box::new(
                Text::new(&crate::i18n::t(LINUX_ALONGSIDE_READY))
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
                Text::new("This disk is hardware or policy write-protected.")
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
            self.next_control(),
            "Next",
            self.selected_disk.is_none() || unavailable,
        ));
    }

    /// The Erase/Alongside choice, or — on the disk that holds a staged
    /// installer — the one plan that disk allows and what it leaves untouched.
    fn compose_install_method(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
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
                .with_role(TextRole::Caption)
                .with_color(dim),
            ));
        } else {
            children.push(Box::new(
                SegChoice::new(
                    INSTALL_METHOD_LABELS
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                    ["erase", "alongside"]
                        .iter()
                        .map(|method| {
                            self.control(
                                op::STORAGE,
                                "set-method",
                                serde_json::json!({ "method": method }),
                            )
                        })
                        .collect(),
                    usize::from(self.install_method == InstallMethod::Alongside),
                )
                .with_test_id("install_method"),
            ));
        }
        let guidance = crate::i18n::t(if self.install_method == InstallMethod::Erase {
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
                Text::new(&crate::i18n::t(WINDOWS_ALONGSIDE_PREPARATION))
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
        let Some(binding) = self.alongside_capacity_binding() else {
            return;
        };
        children.push(Box::new(Text::new(&crate::i18n::t(
            "installer-storage-use-capacity",
        ))));
        children.push(Box::new(
            ui::widget::NumberInput::new(ui::num::f64_of_u64(plan.free_size_bytes / (1 << 30)))
                .with_range(35.0, ui::num::f64_of_u64(full.free_size_bytes / (1 << 30)))
                .with_step(1.0)
                .with_suffix("GiB")
                .with_id("installer.alongside-capacity")
                .with_accessible_name(crate::i18n::t("installer-storage-use-capacity"))
                .on_action(binding),
        ));
    }

    fn alongside_allocation_summary(&self, plan: &GuidedAlongsidePlan) -> String {
        let remaining = self.selected_full_guided_plan().map_or(0, |full| {
            full.free_size_bytes.saturating_sub(plan.free_size_bytes)
        });
        crate::i18n::t_args(
            "installer-storage-allocation-summary",
            &[
                ("capacity", ui::format::memory(plan.free_size_bytes).into()),
                ("remaining", ui::format::memory(remaining).into()),
                ("details", Self::alongside_capacity_summary(plan).into()),
            ],
        )
    }

    fn alongside_capacity_summary(plan: &GuidedAlongsidePlan) -> String {
        crate::i18n::t_args(
            "installer-storage-alongside-capacity",
            &[
                ("root", ui::format::memory(plan.root_size_bytes).into()),
                ("total", ui::format::memory(plan.free_size_bytes).into()),
            ],
        )
    }

    fn alongside_storage_guidance(&self) -> String {
        let plans = self.guided_alongside_plans();
        let mut message = if self.selected_disk_ref().is_none() {
            "Select a disk to see its alongside options.".to_string()
        } else if !self.selected_disk_can_install_alongside() {
            self.alongside_unavailable_message(&plans)
        } else if self.uses_guided_alongside() {
            self.guided_alongside_plan()
                .as_ref()
                .map(|plan| self.alongside_allocation_summary(plan))
                .unwrap_or_default()
        } else {
            "Format only the prepared Tilo boot and root partitions; preserve the existing EFI and other OS partitions.".to_string()
        };
        if self
            .selected_disk_ref()
            .is_some_and(Disk::has_linux_storage)
            && !plans.is_empty()
        {
            message.push(' ');
            message.push_str(&crate::i18n::t(LINUX_ALONGSIDE_READY));
        }
        if self
            .selected_disk_ref()
            .is_some_and(Disk::has_windows_storage)
        {
            message.push(' ');
            message.push_str(&crate::i18n::t(WINDOWS_ALONGSIDE_PREPARATION));
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
            return crate::i18n::t_args(
                "installer-storage-linux-no-space",
                &[(
                    "preparation",
                    crate::i18n::t(LINUX_ALONGSIDE_PREPARATION).into(),
                )],
            );
        }
        "No safe alongside space was found. Back up your files and use the existing OS's supported partitioning tools to leave at least 35 GiB contiguous unallocated, or prepare an empty Linux root and 1 GiB boot plus 2 GiB EFI partition, then refresh disks. Tilo will not resize existing partitions or switch to Erase automatically.".to_string()
    }

    fn compose_guided_extent_choices(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        plans: &[GuidedAlongsidePlan],
    ) {
        children.push(Box::new(
            Text::new("Choose the exact unallocated region for Tilo").with_role(TextRole::Body),
        ));
        for (index, plan) in plans.iter().enumerate() {
            // The installer states its partition sizes in binary units
            // throughout, so they compare with its GiB minimums.
            let start = ui::format::memory(plan.free_start_bytes);
            let size = ui::format::memory(plan.free_size_bytes);
            let label = if self.selected_guided_extent == Some(index) {
                format!("✓ Selected · {size} free at {start}")
            } else {
                format!("Use {size} free at {start}")
            };
            children.push(Box::new(
                ActionButton::new(
                    &label,
                    self.control(&op::region_resource(index), "select", Value::Null),
                )
                .with_test_id(&format!("guided_extent_{index}")),
            ));
        }
        children.push(Box::new(
            Text::new("The location is measured from the start of the selected disk. Existing partitions between free regions remain untouched.")
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
        children.push(Box::new(
            Text::new(if identity_complete {
                "Secure your account"
            } else {
                "Create your account"
            })
            .with_role(TextRole::Title),
        ));
        if self.profile_completion.is_none() || !completion.full_name.is_complete() {
            children.push(Box::new(
                Text::new(&crate::i18n::t("installer-account-name")).with_role(TextRole::Body),
            ));
            children.push(Box::new(
                TextInput::from_state(&self.full_name)
                    .with_id("installer.full_name")
                    .with_accessible_name(crate::i18n::t("installer-account-name")),
            ));
            children.push(Box::new(
                Text::new(&crate::i18n::t("installer-account-name-note"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }
        if self.profile_completion.is_none() || !completion.username.is_complete() {
            children.push(Box::new(
                Text::new(&crate::i18n::t("installer-account-username")).with_role(TextRole::Body),
            ));
            children.push(Box::new(
                TextInput::from_state(&self.username)
                    .with_id("installer.username")
                    .with_accessible_name(crate::i18n::t("installer-account-username")),
            ));
            children.push(Box::new(
                Text::new(&crate::i18n::t("installer-account-username-note"))
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }
        let tpm_pin = matches!(
            self.verified_boot_state(),
            VerifiedBootState::TargetVerified
        );
        children.push(Box::new(
            Text::new(&crate::i18n::t(if tpm_pin {
                "installer-account-pin"
            } else {
                "installer-account-password"
            }))
            .with_role(TextRole::Body),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password")
                .with_accessible_name(crate::i18n::t(if tpm_pin {
                    "installer-account-pin"
                } else {
                    "installer-account-password"
                }))
                .with_external_buffer(Arc::clone(&self.password))
                .with_secret(true),
        ));
        children.push(Box::new(
            Text::new(&crate::i18n::t(if tpm_pin {
                "installer-account-pin-note"
            } else {
                "installer-account-password-note"
            }))
            .with_role(TextRole::Caption)
            .with_color(dim),
        ));
        children.push(Box::new(
            Text::new(&crate::i18n::t(if tpm_pin {
                "installer-account-pin-confirm"
            } else {
                "installer-account-password-confirm"
            }))
            .with_role(TextRole::Body),
        ));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password_confirm")
                .with_accessible_name(crate::i18n::t(if tpm_pin {
                    "installer-account-pin-confirm"
                } else {
                    "installer-account-password-confirm"
                }))
                .with_external_buffer(Arc::clone(&self.password_confirmation))
                .with_secret(true),
        ));
        actions.push(self.nav_row(Step::Account, self.next_control(), "Next", false));
    }

    fn compose_keyboard_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;

        children.push(Box::new(
            Text::new(&crate::i18n::t("installer-welcome-title")).with_role(TextRole::Title),
        ));
        children.push(Box::new(
            Text::new(&crate::i18n::t("installer-welcome-note")).with_role(TextRole::Body),
        ));
        children.push(Box::new(
            Text::new("Keyboard layout").with_role(TextRole::Body),
        ));
        children.push(Box::new(
            Text::new(&crate::i18n::t("installer-keyboard-note"))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        children.push(Box::new(
            Dropdown::from_state(layouts::display_names(), &self.keymap)
                .with_id("installer.keymap")
                .with_placeholder("Select a keyboard layout"),
        ));
        children.push(Box::new(KeyboardView::new(&self.keymap)));
        actions.push(self.nav_row(Step::Keyboard, self.next_control(), "Next", false));
        children.push(Box::new(
            Text::new("Need files from this computer, or removing an alongside Tilo install? Press Ctrl+Alt+F2 for recovery tools; Ctrl+Alt+F1 returns here.")
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
            Text::new("Installation options").with_role(TextRole::Title),
        ));

        if self.profile_completion.is_none() || !completion.filesystem.is_complete() {
            self.compose_filesystem_choice(theme, children);
        }

        children.push(Box::new(
            Text::new("Disk encryption").with_role(TextRole::Body),
        ));
        children.push(Box::new(
            Text::new("Always on · LUKS2")
                .with_role(TextRole::Body)
                .with_color(theme.status_success),
        ));
        children.push(Box::new(
            Text::new(encrypt_description(self.verified_boot_state()))
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));

        children.push(Box::new(
            Text::new("Security key").with_role(TextRole::Body),
        ));
        children.push(Box::new(
            SegChoice::new(
                FIDO2_LABELS
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                (0..FIDO2_LABELS.len())
                    .map(|i| {
                        self.control(
                            op::OPTIONS,
                            "set-security-key",
                            serde_json::json!({ "enabled": i == 1 }),
                        )
                    })
                    .collect(),
                usize::from(self.fido2),
            )
            .with_test_id("fido2_choice"),
        ));
        children.push(Box::new(
            Text::new("Optionally enroll a FIDO2 security key (e.g. a YubiKey) as another way to unlock. Plug it in before installing.")
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));

        if self.profile_completion.is_none() || !completion.hostname.is_complete() {
            children.push(Box::new(Text::new("Hostname").with_role(TextRole::Body)));
            children.push(Box::new(
                TextInput::from_state(&self.hostname).with_id("installer.hostname"),
            ));
            children.push(Box::new(
                Text::new("The name your computer uses on the network.")
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }

        if self.profile_completion.is_none() || !completion.locale.is_complete() {
            children.push(Box::new(
                Text::new("Language & locale").with_role(TextRole::Body),
            ));
            children.push(Box::new(
                Dropdown::from_state(
                    install_profile_model::locales()
                        .map(str::to_owned)
                        .collect(),
                    &self.locale,
                )
                .with_id("installer.locale")
                .with_accessible_name("Language & locale")
                .with_placeholder("Choose a language and locale"),
            ));
            children.push(Box::new(
                Text::new("This controls language, formatting, and regional conventions after installation.")
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }

        if self.profile_completion.is_none() || !completion.timezone.is_complete() {
            children.push(Box::new(Text::new("Time zone").with_role(TextRole::Body)));
            children.push(Box::new(
                Dropdown::from_state(self.timezones.clone(), &self.timezone)
                    .with_id("installer.timezone")
                    .with_placeholder("Select a time zone")
                    .open_if(self.tz_open_for_shot),
            ));
            children.push(Box::new(
                Text::new("Pick from the list — start typing to filter (IANA name, e.g. Europe/Amsterdam or UTC).")
                    .with_role(TextRole::Caption)
                    .with_color(dim),
            ));
        }

        actions.push(self.nav_row(
            Step::Options,
            self.next_control(),
            "Next",
            !self.filesystem_policy_ready(),
        ));
    }

    /// The Media step: a centred title and one line over a single large
    /// Install button, with a quiet way past it. Nothing is chosen until
    /// Install is pressed; afterwards the step says so and offers Undo until
    /// the install begins.
    fn compose_media_step(
        &self,
        theme: &Theme,
        children: &mut Vec<Box<dyn Widget>>,
        actions: &mut Vec<Box<dyn Widget>>,
    ) {
        let dim = theme.text_dim;
        children.push(Box::new(Spacer::fixed(theme.tokens.spacing.s6)));
        children.push(centered_media_control(Box::new(
            ui::widget::IconWidget::new(ui::icons::Icon::Film)
                .sized(ui::widget::IconSize::Large)
                .accent(),
        )));
        children.push(Box::new(
            Text::new(&crate::i18n::t("installer-media-title"))
                .with_role(TextRole::LargeTitle)
                .with_align(ui::ParagraphAlign::Center),
        ));
        children.push(Box::new(
            Text::new(&crate::i18n::t("installer-media-note"))
                .with_role(TextRole::Caption)
                .with_color(dim)
                .with_align(ui::ParagraphAlign::Center),
        ));
        // Media selection and its return/continue routes share the pinned region.
        let children = actions;
        if self.codecs {
            children.push(centered_media_control(Box::new(
                Row::new()
                    .with_padding(0.0, 0.0)
                    .no_wrap()
                    .with_gap(theme.tokens.spacing.s2)
                    .push(Box::new(
                        ui::widget::IconWidget::new(ui::icons::Icon::Check)
                            .with_color(theme.status_success),
                    ))
                    .push(Box::new(
                        Text::new(&crate::i18n::t("installer-media-chosen"))
                            .with_role(TextRole::Body)
                            .with_color(theme.status_success)
                            .with_id("codecs_chosen"),
                    )),
            )));
            children.push(centered_media_control(Box::new(
                ActionButton::new(
                    &crate::i18n::t("installer-media-undo"),
                    self.control(
                        op::MEDIA,
                        "set-media-codecs",
                        serde_json::json!({"enabled":false}),
                    ),
                )
                .quiet()
                .with_width(theme.tokens.spacing.s16 * 2.0 + theme.tokens.spacing.s8)
                .with_test_id("codecs_undo"),
            )));
            children.push(Box::new(Spacer::fixed(theme.tokens.spacing.s6)));
            children.push(self.nav_row(
                Step::Media,
                self.next_control(),
                &crate::i18n::t("installer-media-next"),
                false,
            ));
        } else {
            children.push(centered_media_control(Box::new(
                ActionButton::new(
                    &crate::i18n::t("installer-media-install"),
                    self.control(
                        op::MEDIA,
                        "set-media-codecs",
                        serde_json::json!({"enabled":true}),
                    ),
                )
                .primary()
                .large()
                .with_width(theme.tokens.spacing.s16 * 4.0 + theme.tokens.spacing.s12)
                .with_test_id("codecs_install"),
            )));
            children.push(centered_media_control(Box::new(
                ActionButton::new(&crate::i18n::t("installer-media-skip"), self.next_control())
                    .quiet()
                    .with_width(theme.tokens.spacing.s16 * 2.0 + theme.tokens.spacing.s8)
                    .with_test_id("codecs_skip"),
            )));
            children.push(Box::new(Spacer::fixed(theme.tokens.spacing.s6)));
            children.push(Box::new(
                Row::new()
                    .with_padding(0.0, 0.0)
                    .push(Box::new(
                        ActionButton::new(
                            &crate::i18n::t("installer-media-back"),
                            self.back_control(),
                        )
                        .with_width(theme.tokens.spacing.s16 * 2.0 + theme.tokens.spacing.s3),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
        }
    }

    fn compose_filesystem_choice(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        children.push(Box::new(Text::new("Filesystem").with_role(TextRole::Body)));
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
        children.push(Box::new(SegChoice::new(
            choices
                .iter()
                .filter_map(|index| FILESYSTEMS.get(*index).map(|name| (*name).to_string()))
                .collect(),
            choices
                .iter()
                .filter_map(|index| FILESYSTEMS.get(*index))
                .map(|filesystem| {
                    self.control(
                        op::OPTIONS,
                        "set-filesystem",
                        serde_json::json!({ "filesystem": filesystem }),
                    )
                })
                .collect(),
            selected,
        )));
        children.push(Box::new(
            Text::new(fs_description(self.selected_fs))
                .with_role(TextRole::Caption)
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
            Text::new("Review your choices").with_role(TextRole::Title),
        ));

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
                    "Tilo storage",
                    &self.alongside_allocation_summary(&plan),
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
        if self.install_method == InstallMethod::Alongside {
            children.push(summary_row(
                theme,
                "Boot choice",
                &crate::i18n::t(ALONGSIDE_BOOT_GUIDANCE),
            ));
            children.push(summary_row(
                theme,
                "Existing files",
                &crate::i18n::t(ALONGSIDE_FILES_GUIDANCE),
            ));
        }
        children.push(summary_row(theme, "Filesystem", self.filesystem()));
        children.push(summary_row(theme, "Encryption", &self.encryption_summary()));
        children.push(summary_row(
            theme,
            "Hostname",
            local_text(&self.hostname).trim(),
        ));
        children.push(summary_row(theme, "Time zone", self.timezone().trim()));
        children.push(summary_row(theme, "Keyboard layout", self.keymap().trim()));
        children.push(summary_row(
            theme,
            "More video and audio formats",
            if self.codecs {
                "Install once the new system is online"
            } else {
                "Not now (available later in Settings, Updates)"
            },
        ));
        children.push(summary_row(
            theme,
            "Username",
            local_text(&self.username).trim(),
        ));
        let full_name = local_text(&self.full_name);
        if !full_name.trim().is_empty() {
            children.push(summary_row(theme, "Name", full_name.trim()));
        }
        children.push(summary_row(
            theme,
            "Network",
            &self.network.connection_summary(),
        ));
        children.push(Box::new(
            Text::new("This install uses the Tilo image on this medium; it does not download an update. Future system updates come from the signature-required Tilo repository and are checked automatically after first boot whenever the computer is online.")
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

        let impact = if self.install_method == InstallMethod::Alongside {
            alongside_install_impact(self.uses_guided_alongside(), &dev)
        } else {
            format!("This will ERASE {dev} and all data on it.")
        };
        children.push(Box::new(
            Text::new(&impact)
                .with_role(TextRole::Body)
                .with_color(danger),
        ));
        self.compose_destructive_confirmation(theme, children, &dev);

        // Encryption generates a recovery key shown ONCE during the
        // install. Make the user explicitly acknowledge they'll save
        // it before they can proceed — otherwise a lost TPM = lost
        // data with no warning.
        children.push(Box::new(
            Text::new("A recovery key will be shown during install — it is your only backup if the TPM is reset or fails. Save it somewhere safe.")
                .with_role(TextRole::Caption)
                .with_color(dim),
        ));
        let ack_label = if self.recovery_ack {
            "[x] I will save my recovery key"
        } else {
            "[ ] I will save my recovery key"
        };
        children.push(Box::new(
            ActionButton::new(
                ack_label,
                self.control(
                    op::REVIEW,
                    "set-recovery-promise",
                    serde_json::json!({ "acknowledged": !self.recovery_ack }),
                ),
            )
            .with_test_id("recovery_ack")
            .checkbox(self.recovery_ack),
        ));

        // Begin is gated on the recovery-key acknowledgement when
        // encrypting (otherwise always enabled).
        let begin_disabled = !self.encryption_ready()
            || !self.destructive_confirmation_ready()
            || !self.filesystem_policy_ready();
        actions.push(self.nav_row(
            Step::Review,
            self.control(op::REVIEW, "begin", Value::Null),
            "Begin Install",
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
                    Text::new("Tilo is installed")
                        .with_role(TextRole::Title)
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
                    Text::new(&completion)
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                ));
                if self.install_method == InstallMethod::Alongside {
                    children.push(Box::new(
                        Text::new(if self.uses_guided_alongside() {
                            "To choose Tilo or your existing system, open this computer's firmware boot picker. The installer preserved its previous default."
                        } else {
                            "Tilo can add menu shortcuts for unambiguous loaders on the shared EFI partition. If Linux is not listed, use the computer's firmware boot picker; loaders on other EFI partitions are not guaranteed to appear here."
                        })
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                    ));
                }
                self.compose_recovery_key(theme, children);
                children.push(Box::new(StatusLog::new(friendly_status, STATUS_LOG_HEIGHT)));
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
                    Text::new("Installation failed")
                        .with_role(TextRole::Title)
                        .with_color(danger)
                        .with_role(TextRole::Title)
                        .with_live(Politeness::Assertive),
                ));
                children.push(Box::new(
                    Text::new("The installer stopped before it could confirm a bootable system. The destination may already contain a partial encrypted installation. Tilo will not treat on-disk names, labels, or metadata as permission to erase it. Save the recovery key below if one was generated, then review storage or use Ctrl+Alt+F2 for recovery tools.")
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                ));
                children.push(Box::new(
                    Text::new("Details")
                        .with_role(TextRole::Caption)
                        .with_color(dim),
                ));
                self.compose_recovery_key(theme, children);
                children.push(Box::new(StatusLog::new(
                    self.status_lines.clone(),
                    STATUS_LOG_HEIGHT,
                )));
                actions.push(Box::new(ActionButton::new(
                    "Review storage",
                    self.back_control(),
                )));
            }
            _ => {
                children.push(Box::new(
                    Text::new("Installing Tilo…")
                        .with_role(TextRole::Title)
                        .with_role(TextRole::Title),
                ));
                children.push(Box::new(
                    Row::new().push(Box::new(Spinner::new())).push(Box::new(
                        Text::new("Working — this can take a few minutes. Don't power off.")
                            .with_role(TextRole::Caption)
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
        children.push(Box::new(
            Text::new("Recovery key — save this somewhere off-device")
                .with_role(TextRole::Caption)
                .with_color(theme.status_success),
        ));
        children.push(Box::new(Text::new(key).with_role(TextRole::Caption)));
        children.push(Box::new(
            ActionButton::new(
                if self.recovery_key_copied {
                    "Copy recovery key again"
                } else {
                    "Copy recovery key"
                },
                self.control(op::RECOVERY, "copy", Value::Null),
            )
            .with_test_id("recovery_key_copy"),
        ));
        children.push(Box::new(
            Text::new(if self.recovery_key_copied {
                "Copied. Paste it into a password manager or an encrypted note on another drive before rebooting."
            } else {
                "Copy it into a password manager or an encrypted note on another drive. The installer clears its clipboard copy after 5 minutes, or when you reboot or power off."
            })
            .with_role(TextRole::Caption)
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
            ActionButton::new(
                saved_label,
                self.control(
                    op::RECOVERY,
                    "confirm-saved",
                    serde_json::json!({ "saved": !self.recovery_key_saved }),
                ),
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

fn local_text(state: &ui::LocalState<String>) -> String {
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

/// One `label: value` review line (label dim, value primary).
fn summary_row(theme: &Theme, label: &str, value: &str) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(Box::new(
                Text::new(&format!("{label}:"))
                    .with_role(TextRole::Caption)
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(Text::new(value).with_role(TextRole::Body))),
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
        "Starting install to /dev/sda for user ada (btrfs).".to_string(),
        "==> Copying the running Tilo image into local container storage".to_string(),
        "Copying local image docker://localhost/tilo-installer:latest to containers-storage:localhost/bootc".to_string(),
        "==> Installing Tilo onto /dev/sda (this ERASES the disk)".to_string(),
        "  base system installed.".to_string(),
        "==> Preparing the 'ada' account on the installed system".to_string(),
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
    /// React to an edit of the typed confirmation.
    fn notice_confirmation_edit(&mut self) {
        let confirmation = local_text(&self.erase_confirmation);
        if self.erase_confirmation_seen != confirmation {
            let was_ready = self
                .destructive_confirmation_target()
                .is_some_and(|device| self.erase_confirmation_seen.trim() == device);
            let had_status = !self.status.is_empty();
            self.erase_confirmation_seen = confirmation;
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

    fn refresh_disks_with(&mut self, disks: Vec<Disk>) {
        let previous = self.selected_disk_ref().and_then(DiskMethodIdentity::of);
        self.disks = disks;
        self.selected_disk = previous.and_then(|identity| {
            self.disks.iter().position(|disk| {
                DiskMethodIdentity::of(disk).as_ref() == Some(&identity)
                    && !disk.blocks_install()
                    && !disk.read_only
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
            // Discovery no longer matches the staged plan: fall back to the
            // ordinary rules, which keep the running-system disk unavailable.
            self.internal_handoff = None;
        }
        self.status = if self.disks.is_empty() {
            "Disk list refreshed; no installable disks were found.".to_string()
        } else if self.selected_disk.is_some() {
            "Disk list refreshed. Review the destination before continuing.".to_string()
        } else {
            "Disk list refreshed. Select a destination again.".to_string()
        };
        self.dirty_structure = true;
        self.focus_reset = true;
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
                self.recovery_key_copy = ui::CopyWatch::default();
                self.recovery_key_saved = false;
                self.dirty_structure = true;
            }
        } else {
            self.status_lines.push(payload.to_string());
            self.dirty_structure = true;
        }
    }

    /// Run every press the controls queued, in order.
    fn run_presses(&mut self) {
        while let Some(press) = self.presses.take() {
            self.run_press(press);
        }
    }

    /// Run one press through the action door. The Review field's typed text
    /// is the phrase a destructive action asks for, and the Wi-Fi password is
    /// taken from its secret buffer only now, so a queued press never holds
    /// either.
    fn run_press(&mut self, press: Press) {
        let Press { address, mut args } = press;
        if address.resource == op::NETWORK && address.action == "connect" {
            let password = take_secret(&self.wifi_password);
            if !password.is_empty() {
                args.insert("password".into(), Value::String(password.to_string()));
            }
        }
        let phrase = local_text(&self.erase_confirmation);
        self.invoke_control(&address.resource, &address.action, args, Some(&phrase));
    }
}

impl Widget for InstallerApp {
    ui::provides!(dyn ui::resources::PublishesResources);

    /// Reads nothing from and writes nothing to the document: the presses an
    /// assistive activation queued since the last event run here, then the
    /// view state is noticed and the composed controls sync.
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.recovery_key_copy.failed() {
            self.recovery_key_copied = false;
            self.dirty_structure = true;
            self.set_status("Could not copy the recovery key to the clipboard");
        }
        self.run_presses();
        self.sync_accounts();
        self.notice_confirmation_edit();
        if self.network.ensure_initial_scan(self.event_proxy()) {
            self.dirty_structure = true;
        }
        self.composed.sync(state, root);
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

        // `new_with_children` only errors for a child `NodeId` from a
        // different tree, which never happens here since `card_node` was
        // just built in this same tree by the call above.
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
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(c) = self.composed.element_mut()
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
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.rect,
        );
        if let Some(c) = self.composed.element_mut() {
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
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&card_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, card_node));
        }
        // A control the event used queued its press; run it now.
        self.run_presses();

        // Enter advances only when no inner field consumed it: Begin on
        // Review, Next on Disk/Account/Options — the same actions their
        // buttons run.
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
                        self.run_press(Press::new(op::REVIEW, "begin", Value::Null));
                    }
                    *ctx.handled = true;
                }
                Step::Network => {
                    if self
                        .network
                        .selected_network()
                        .is_some_and(|network| !network.connected)
                    {
                        self.run_press(Press::new(op::NETWORK, "connect", Value::Null));
                    } else {
                        self.run_press(Press::new(op::SESSION, "next", Value::Null));
                    }
                    *ctx.handled = true;
                }
                // On Media a focused Install button takes Enter itself (the
                // same deliberate press as a click); otherwise Enter moves on
                // without choosing anything.
                Step::Disk | Step::Account | Step::Keyboard | Step::Options | Step::Media => {
                    self.run_press(Press::new(op::SESSION, "next", Value::Null));
                    *ctx.handled = true;
                }
                // Enter in a private account field must not skip sign-in.
                Step::Accounts | Step::Install => {}
            }
        }
    }

    fn needs_idle_sync(&self) -> bool {
        self.step == Step::Accounts
            && (self.windows_hints.is_pending()
                || self.accounts_controller.as_ref().is_some_and(|controller| {
                    controller.status() == crate::online_accounts::view::AccountsStatus::SigningIn
                }))
    }

    fn on_external_event(
        &mut self,
        payload: &str,
        _state: &mut ui::widget::Edits<'_>,
        _root: &ModelPath,
    ) {
        if payload == CONSOLE_WAKE {
            self.serve_console();
        } else if payload == NETWORK_WAKE_EVENT {
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

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        op::HANDLERS
    }

    /// The host door (Ctrl+K palette, broker) admits an installer operation
    /// here, after its own resolution, permission and confirmation checks.
    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped().filter(|r| op::HANDLERS.contains(&r.type_id)) else {
            return ui::host_operation::start_in_children(self, op, doc, path);
        };
        // The operation changes wizard state only; the document is untouched.
        let encoded = tilo_actions::EncodedAction {
            type_id: request.type_id.to_owned(),
            payload: request.payload.to_vec(),
        };
        self.apply_encoded(&encoded).map(|output| {
            Some(ui::host_operation::ResourceOperationStart::Completed(
                output,
            ))
        })
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

    fn name(&self) -> &'static str {
        "InstallerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
    use automerge::ReadDoc;
    use ui::widget::find_test_id;

    fn find_test_id_mut<'a>(widget: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
        if widget.test_id() == Some(id) {
            return Some(widget);
        }
        widget
            .children_mut()
            .into_iter()
            .find_map(|child| find_test_id_mut(child, id))
    }

    /// Use the composed control with test id `id` the way assistive
    /// technology does, then let the app sync. Nothing reaches the document.
    fn perform(app: &mut InstallerApp, doc: &mut AutoCommit, id: &str, action: &ui::a11y::Action) {
        let _ = build(app);
        let composed = app.composed.element_mut().expect("composed step");
        let control = find_test_id_mut(composed, id).unwrap_or_else(|| panic!("no control {id}"));
        assert!(
            control.perform_action(
                action,
                &[],
                &mut ui::widget::Edits::lend(doc),
                &automerge::ROOT
            ),
            "{id} refused {action:?}"
        );
        app.sync(doc, &automerge::ROOT);
        assert_eq!(
            doc.length(automerge::ROOT),
            0,
            "a control wrote the document"
        );
    }

    pub(super) fn activate(app: &mut InstallerApp, doc: &mut AutoCommit, id: &str) {
        perform(app, doc, id, &ui::a11y::Action::Activate);
    }

    /// Run the press a control or the Enter key queues, then let the app
    /// sync. Nothing reaches the document.
    fn run(app: &mut InstallerApp, doc: &AutoCommit, resource: &str, action: &str, args: Value) {
        app.run_press(Press::new(resource, action, args));
        app.sync(doc, &automerge::ROOT);
        assert_eq!(doc.length(automerge::ROOT), 0, "a press wrote the document");
    }

    fn next(app: &mut InstallerApp, doc: &AutoCommit) {
        run(app, doc, op::SESSION, "next", Value::Null);
    }

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
        find_test_id(widget, id).is_some()
    }

    fn build(app: &mut InstallerApp) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = app.build_layout(&mut taffy, &theme, &mut node_map);
        (taffy, root)
    }

    fn layout_at(app: &mut InstallerApp, width: f32, height: f32) {
        layout_with_theme(app, width, height, &Theme::dark());
    }

    fn layout_with_theme(
        app: &mut InstallerApp,
        width: f32,
        height: f32,
        theme: &Theme,
    ) -> HashMap<NodeId, *mut dyn Widget> {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let app_node = app.build_layout(&mut taffy, theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(width, height),
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
        node_map
    }

    fn invoke_provider(app: &mut InstallerApp, doc: &mut AutoCommit, id: &str) {
        let payload = serde_json::to_vec(id).unwrap();
        let completed = ui::host_operation::ResourceOperation::local(
            tilo_actions::InvocationId::new(),
            tilo_actions::OperationKind::View,
            "tilo.ui.control.invoke",
            &payload,
        )
        .start(app, &mut ui::widget::Edits::lend(doc), &automerge::ROOT)
        .unwrap()
        .and_then(ui::host_operation::ResourceOperationStart::completed);
        assert!(
            completed.is_some(),
            "provider control must execute its view action"
        );
        app.sync(doc, &automerge::ROOT);
        assert_eq!(doc.length(automerge::ROOT), 0);
    }

    #[test]
    fn accounts_provider_buttons_rebuild_server_field() {
        let mut app = InstallerApp::with_disks(Vec::new());
        let mut doc = AutoCommit::new();
        app.step = Step::Accounts;
        // Exercise the form without starting live credential custody.
        app.accounts_initialized = true;
        app.dirty_structure = true;
        app.sync(&doc, &automerge::ROOT);
        let _ = build(&mut app);
        assert!(!has_test_id(&app, "installer.online.server"));
        app.take_structure_dirty();

        invoke_provider(&mut app, &mut doc, "installer-online-nextcloud");
        assert!(app.dirty_structure, "provider change must request layout");
        app.take_structure_dirty();
        let _ = build(&mut app);
        assert!(has_test_id(&app, "installer.online.server"));

        invoke_provider(&mut app, &mut doc, "installer-online-google");
        assert!(app.dirty_structure, "provider change must request layout");
        app.take_structure_dirty();
        let _ = build(&mut app);
        assert!(!has_test_id(&app, "installer.online.server"));
    }

    #[test]
    fn welcome_and_account_explain_choices_in_person_first_order() {
        let mut app = InstallerApp::with_disks(Vec::new());
        layout_at(&mut app, 800.0, 600.0);
        assert!(find_accessible(&app, &crate::i18n::t("installer-welcome-title")).is_some());
        assert!(find_accessible(&app, &crate::i18n::t("installer-welcome-note")).is_some());
        let next = find_test_id(&app, "Next").expect("first-page continuation");
        assert!(next.get_rect().height() > 0.0);
        assert!(next.get_rect().y0 >= 0.0);
        assert!(
            next.get_rect().y1 <= 600.0,
            "continue must not begin below the fold"
        );

        app.step = Step::Account;
        app.dirty_structure = true;
        layout_at(&mut app, 800.0, 600.0);
        let name = find_test_id(&app, "installer.full_name").expect("display name field");
        let username = find_test_id(&app, "installer.username").expect("sign-in field");
        assert!(name.get_rect().height() > 0.0);
        assert!(username.get_rect().y0 >= name.get_rect().y1);
        assert!(find_accessible(&app, &crate::i18n::t("installer-account-name-note")).is_some());
        let next = find_test_id(&app, "Next").expect("account continuation");
        assert!(
            next.get_rect().y1 <= 600.0,
            "continue must remain visible at normal text size"
        );
    }

    #[test]
    fn account_fields_have_localized_accessible_names_without_exposing_secrets() {
        for pin in [false, true] {
            let mut app = InstallerApp::with_disks(Vec::new());
            app.step = Step::Account;
            app.tpm2 = pin;
            app.secure_boot = pin;
            app.kernel_lockdown = pin;
            app.target_verified_boot = pin;
            *app.password.lock().unwrap() = "private-startup-secret".into();
            *app.password_confirmation.lock().unwrap() = "private-startup-secret".into();
            let _ = build(&mut app);
            for (id, key, protected) in [
                ("installer.full_name", "installer-account-name", false),
                ("installer.username", "installer-account-username", false),
                (
                    "installer.password",
                    if pin {
                        "installer-account-pin"
                    } else {
                        "installer-account-password"
                    },
                    true,
                ),
                (
                    "installer.password_confirm",
                    if pin {
                        "installer-account-pin-confirm"
                    } else {
                        "installer-account-password-confirm"
                    },
                    true,
                ),
            ] {
                let access = find_test_id(&app, id).unwrap().accessibility().unwrap();
                assert_eq!(
                    access.name.as_deref(),
                    Some(crate::i18n::t(key).as_str()),
                    "{id}"
                );
                assert_eq!(access.state.protected, protected, "{id}");
                if protected {
                    assert!(access.value.is_none(), "a named secret must remain private");
                }
            }
        }
    }

    #[test]
    fn storage_explains_missing_drives_and_the_confirmation_boundary() {
        let mut app = InstallerApp::with_disks(Vec::new());
        app.step = Step::Disk;
        let _ = build(&mut app);
        assert!(find_accessible(&app, &crate::i18n::t("installer-storage-no-drives")).is_some());
        assert!(
            find_accessible(&app, &crate::i18n::t("installer-storage-erase-warning")).is_some()
        );
        app.install_method = InstallMethod::Alongside;
        app.dirty_structure = true;
        let _ = build(&mut app);
        assert!(
            find_accessible(&app, &crate::i18n::t("installer-storage-keep-guidance")).is_some()
        );
    }

    // --- pure state-machine ---------------------------------------

    #[test]
    fn step_navigation_walks_forward_back_and_indexes_labels() {
        assert_eq!(STEP_LABELS.len(), 9);
        // (step, next, prev, index): forward stops at Install, back stops at
        // Keyboard (and Install has no way back), indices match the labels.
        let cases = [
            (Step::Keyboard, Some(Step::Network), None, 0),
            (Step::Network, Some(Step::Accounts), Some(Step::Keyboard), 1),
            (Step::Accounts, Some(Step::Disk), Some(Step::Network), 2),
            (Step::Disk, Some(Step::Account), Some(Step::Accounts), 3),
            (Step::Account, Some(Step::Options), Some(Step::Disk), 4),
            (Step::Options, Some(Step::Media), Some(Step::Account), 5),
            (Step::Media, Some(Step::Review), Some(Step::Options), 6),
            (Step::Review, Some(Step::Install), Some(Step::Media), 7),
            (Step::Install, None, None, 8),
        ];
        for (step, next, prev, index) in cases {
            assert_eq!(next_step(step), next, "next_step case {step:?}");
            assert_eq!(prev_step(step), prev, "prev_step case {step:?}");
            assert_eq!(step_index(step), index, "step_index case {step:?}");
        }
    }

    #[test]
    fn online_accounts_skip_uses_semantic_door_without_sharing_form_fields() {
        let mut app = InstallerApp::with_disks(sample());
        // No live browser or runtime directory is involved in this test.
        app.accounts_initialized = true;
        app.step = Step::Network;
        let doc = AutoCommit::new();
        next(&mut app, &doc);
        assert_eq!(app.step, Step::Accounts);
        app.accounts_form.email.set("private@example.org".into());
        app.accounts_form
            .server
            .set("https://private.example.org".into());
        let public = app.console_state().to_string();
        assert!(!public.contains("private@example.org"));
        assert!(!public.contains("private.example.org"));
        run(
            &mut app,
            &doc,
            "installer.accounts",
            "skip-offline",
            Value::Null,
        );
        assert_eq!(app.step, Step::Disk);
        assert!(app.accounts_form.drain_commands().is_empty());
        assert!(app.accounts_form.email.with(String::is_empty));
        assert!(app.accounts_form.server.with(String::is_empty));
        assert_eq!(doc.length(automerge::ROOT), 0);
    }

    #[test]
    fn leaving_online_accounts_discards_unstarted_authorization() {
        use crate::online_accounts::view::AccountsCommand;
        let mut app = InstallerApp::with_disks(sample());
        app.accounts_initialized = true;
        app.step = Step::Accounts;
        app.accounts_form.submit(false).unwrap();
        app.go_back();
        assert_eq!(app.step, Step::Network);
        assert!(app.accounts_form.drain_commands().is_empty());
        // Escape controls must remain usable even without live custody.
        app.step = Step::Accounts;
        app.accounts_form.cancel();
        assert!(matches!(
            app.accounts_form.drain_commands().as_slice(),
            [AccountsCommand::Cancel]
        ));
    }

    #[test]
    fn step_and_field_validators_match_backend_contract() {
        // validate_step: (step, disk, username, hostname, ok).
        let steps = [
            // The keyboard step never blocks forward progress.
            (Step::Keyboard, Some(0), "ada", "tilo", true),
            // Disk requires a selection.
            (Step::Disk, None, "", "tilo", false),
            (Step::Disk, Some(0), "", "tilo", true),
            // Account requires a valid username.
            (Step::Account, Some(0), "", "tilo", false),
            (Step::Account, Some(0), "   ", "tilo", false),
            (Step::Account, Some(0), "ada", "tilo", true),
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
            let doc = AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            for path in [
                "installer.username",
                "installer.full_name",
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
            app.sync(&doc, &root);

            // Empty username → blocked.
            activate(&mut app, &mut doc, "Next");
            assert_eq!(app.step, Step::Account);
            assert_eq!(app.status, "Enter a username");

            // Typed fields stay in view state; secrets use external buffers.
            app.username.set("ada".to_string());
            *app.password.lock().unwrap() = "correct horse battery staple".to_string();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".to_string();
            activate(&mut app, &mut doc, "Next");
            assert_eq!(app.step, Step::Options);
            assert_eq!(local_text(&app.username), "ada");
        }
        // Phase `startup_secret_threads_into_install_options`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            *app.password.lock().unwrap() = "correct horse battery".to_string();
            app.sync(&doc, &root);
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
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(app.step, Step::Disk);

            // The Next button is disabled; Enter runs the same action.
            next(&mut app, &doc);
            assert_eq!(app.step, Step::Disk, "must not advance without a disk");
            assert_eq!(app.status, "Select a disk to continue");
        }
        // Phase `selecting_disk_then_next_advances_to_account`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            // Pick the second disk with its row.
            activate(&mut app, &mut doc, "/dev/nvme0n1");
            assert_eq!(app.selected_disk, Some(1));

            // Now Next advances and requests a focus reset for the new step.
            activate(&mut app, &mut doc, "Next");
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
            // On the Disk step, `begin` must NOT trigger an install (would
            // shell out to a missing backend): the action is unavailable.
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            run(&mut app, &doc, op::REVIEW, "begin", Value::Null);
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!app.install_launched);
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
            app.sync(&doc, &root);
            // Default is English (US) → xkb us, no variant.
            assert_eq!(app.keymap(), "English (US)");
            let opts = app.install_options();
            assert_eq!(opts.keymap, "us");
            assert!(opts.keyvariant.is_empty());

            // Select Dvorak with the layout dropdown.
            perform(
                &mut app,
                &mut doc,
                "installer.keymap",
                &ui::a11y::Action::SetValue("Dvorak".into()),
            );
            assert_eq!(app.keymap(), "Dvorak");
            let opts = app.install_options();
            assert_eq!(opts.keymap, "us", "Dvorak is an xkb us variant");
            assert_eq!(opts.keyvariant, "dvorak");
        }
        // Phase `filesystem_flag_updates_selection`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(app.selected_fs, 0);
            assert_eq!(app.filesystem(), "btrfs");

            run(
                &mut app,
                &doc,
                op::OPTIONS,
                "set-filesystem",
                serde_json::json!({ "filesystem": "xfs" }),
            );
            assert_eq!(app.selected_fs, 2);
            assert_eq!(app.filesystem(), "xfs");
        }
        // Phase `fido2_flag_threads_into_mandatory_encryption_options`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            run(
                &mut app,
                &doc,
                op::OPTIONS,
                "set-security-key",
                serde_json::json!({ "enabled": true }),
            );
            assert!(app.fido2);
            assert!(app.install_options().fido2);
        }
        // Phase `options_step_uses_seeded_hostname_and_timezone`.
        {
            let mut app = InstallerApp::with_disks(sample());
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(local_text(&app.hostname), "tilo");
            assert_eq!(app.timezone(), "UTC");
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
        app.step = Step::Media;
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        assert!(!app.codecs, "media codecs are not selected by default");
        assert!(!app.install_options().codecs);
        let codecs = |enabled: bool| serde_json::json!({ "enabled": enabled });
        run(&mut app, &doc, op::MEDIA, "set-media-codecs", codecs(true));
        assert!(app.codecs);
        assert!(app.install_options().codecs);
        assert_eq!(
            app.step,
            Step::Media,
            "Install records the choice and stays"
        );
        run(&mut app, &doc, op::MEDIA, "set-media-codecs", codecs(false));
        assert!(!app.codecs, "the choice can be withdrawn");
    }

    #[test]
    fn the_media_step_is_one_install_button_and_review_repeats_it() {
        let mut app = InstallerApp::for_shot(Step::Media);
        let _ = build(&mut app);
        let composed = app.composed.element().unwrap();
        assert!(has_test_id(composed, "codecs_install"));
        assert!(has_test_id(composed, "codecs_skip"));
        assert!(
            !has_test_id(composed, "codecs_undo"),
            "nothing is chosen yet"
        );
        for absent in ["RPM Fusion", "Cisco", "patent", "licence"] {
            assert!(!CODECS_DESCRIPTION.contains(absent), "{absent}");
        }
        assert!(
            find_accessible(composed, CODECS_DESCRIPTION).is_some(),
            "the rendered codec control must expose the formats source explanation"
        );
        let resources = app.publish();
        let options = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == op::MEDIA)
            .expect("published media choice");
        let install = options
            .actions
            .iter()
            .find(|action| action.id == "set-media-codecs")
            .expect("published codec choice");
        assert_eq!(install.description.as_deref(), Some(CODECS_DESCRIPTION));
        let mut options = InstallerApp::for_shot(Step::Options);
        let _ = build(&mut options);
        assert!(
            find_accessible(options.composed.element().unwrap(), CODECS_TITLE).is_none(),
            "Options no longer carries the formats"
        );

        app.codecs = true;
        app.dirty_structure = true;
        let _ = build(&mut app);
        let chosen = app.composed.element().unwrap();
        assert!(has_test_id(chosen, "codecs_undo"));
        assert!(!has_test_id(chosen, "codecs_install"));
        assert!(find_accessible(chosen, CODECS_CHOSEN).is_some());

        app.step = Step::Review;
        app.dirty_structure = true;
        let _ = build(&mut app);
        let review = app.composed.element().unwrap();
        assert!(
            find_accessible(review, "Install once the new system is online").is_some(),
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
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            run(
                &mut app,
                &doc,
                op::OPTIONS,
                "set-filesystem",
                serde_json::json!({ "filesystem": "ext4" }),
            );
            assert_eq!(
                app.filesystem(),
                "btrfs",
                "a hidden ext4 choice must be rejected"
            );

            app.selected_fs = 1; // stale selection from a media profile or UI rebuild
            assert!(!app.filesystem_policy_ready());
            let mut review = InstallerApp::for_shot(Step::Review);
            review.set_filesystem_support(filesystem_support_from_manifest("ext4\nxfs\n"));
            review.selected_fs = 1;
            review.begin_install();
            assert!(!review.install_launched);
            assert_eq!(review.step, Step::Review);
        }
        // Phase `unsupported_profile_filesystem_requires_repair_and_missing_manifest_blocks_install`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                prepared_setup: None,
                windows_preparation: None,
                hostname: Some("studio".into()),
                username: Some("ada".into()),
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
            app.sync(&doc, &root);
            assert!(!app.recovery_ack);

            // An unacknowledged recovery key must block every native install.
            run(&mut app, &doc, op::REVIEW, "begin", Value::Null);
            assert_eq!(app.install_state, InstallState::Idle);
            assert_eq!(
                app.step,
                Step::Review,
                "blocked until the key is acknowledged"
            );

            // Toggle the acknowledgement on.
            perform(
                &mut app,
                &mut doc,
                "recovery_ack",
                &ui::a11y::Action::ToggleCheck,
            );
            assert!(app.recovery_ack, "ack flips on");

            // Toggle off again (it's a checkbox).
            activate(&mut app, &mut doc, "recovery_ack");
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
            let begin_disabled = find_test_id(&app, "Begin Install").map(|b| !b.is_focusable());
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
            app2.erase_confirmation.set("/dev/sda".to_string());
            *app2.password.lock().unwrap() = "804927".to_string();
            let _ = build(&mut app2);
            let begin_enabled = find_test_id(&app2, "Begin Install").map(Widget::is_focusable);
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
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            run(&mut app, &doc, op::REVIEW, "begin", Value::Null);
            assert_eq!(app.step, Step::Review);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(app.status.contains("Type /dev/sda exactly"));

            app.erase_confirmation.set("/dev/sda".to_string());
            app.sync(&doc, &root);
            assert!(app.destructive_confirmation_ready());
            assert!(app.status.is_empty(), "typing clears the stale refusal");
            assert!(
                app.take_structure_dirty(),
                "typing the confirmation must rebuild the disabled Begin button"
            );
            let _ = build(&mut app);
            let begin_enabled = find_test_id(&app, "Begin Install").map(Widget::is_focusable);
            assert_eq!(begin_enabled, Some(true));
            let field = find_test_id(&app, "installer.erase_confirmation")
                .and_then(Widget::accessibility)
                .expect("confirmation field");
            assert_eq!(field.value.as_deref(), Some("/dev/sda"));
        }
        // Phase `confirmation_keeps_live_input_until_begin_gate_changes`.
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            let _ = build(&mut app);
            assert!(!app.take_structure_dirty());
            for (index, character) in "/dev/sd".chars().enumerate() {
                app.erase_confirmation.update(|typed| typed.push(character));
                app.sync(&doc, &root);
                assert!(
                    !app.take_structure_dirty(),
                    "partial input rebuilt at {index}"
                );
            }
            app.erase_confirmation.update(|typed| typed.push('a'));
            app.sync(&doc, &root);
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
            app.erase_confirmation
                .set(app.selected_dev().expect("selected disk"));
            assert!(!app.destructive_confirmation_ready());
            app.erase_confirmation.set(root);
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

            app.begin_install();
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
    fn linux_guidance_matches_visible_space_state() {
        let mut disk = sample_disks().remove(1);
        disk.partitions.clear();
        // Keep a Linux partition and leave a large contiguous extent after it.
        let mut linux = sample_disks().remove(0).partitions.remove(1);
        linux.filesystem = "ext4".into();
        linux.label = "Linux".into();
        linux.part_type = LINUX_FILESYSTEM_GUID.into();
        linux.size_bytes = 64 * 1024 * 1024 * 1024;
        disk.partitions.push(linux);
        let mut app = InstallerApp::with_disks(vec![disk]);
        app.selected_disk = Some(0);
        app.install_method = InstallMethod::Alongside;
        app.step = Step::Disk;
        let full = app
            .guided_alongside_plan()
            .expect("usable unallocated extent");
        for chosen_gib in [None, Some(48)] {
            app.step = Step::Disk;
            if let Some(gib) = chosen_gib {
                let invocation = app
                    .alongside_capacity_binding()
                    .unwrap()
                    .invoke(f64::from(gib))
                    .unwrap();
                let resources = app.publish();
                let spec = tilo_actions::door::resolve(&resources, &invocation.address).unwrap();
                app.apply_encoded(&spec.build_json(&invocation.arguments).unwrap())
                    .unwrap();
            }
            let _ = build(&mut app);
            let plan = app.guided_alongside_plan().unwrap();
            let expected_bytes = chosen_gib.map_or(full.free_size_bytes, |gib| {
                u64::try_from(gib).unwrap() << 30
            });
            assert_eq!(plan.free_size_bytes, expected_bytes);
            let capacity = app.alongside_allocation_summary(&plan);
            assert!(
                capacity.contains(&ui::format::memory(expected_bytes)),
                "missing selected capacity: {capacity}"
            );
            assert!(
                capacity.contains(&ui::format::memory(full.free_size_bytes - expected_bytes)),
                "missing unused space: {capacity}"
            );
            assert!(capacity.contains("remains unused"), "{capacity}");
            assert!(capacity.contains("unallocated space"), "{capacity}");
            assert!(find_accessible(&app, &crate::i18n::t(LINUX_ALONGSIDE_READY)).is_some());
            assert!(find_accessible(&app, &capacity).is_some());
            assert!(find_accessible(&app, &crate::i18n::t(LINUX_ALONGSIDE_PREPARATION)).is_none());
            assert!(find_accessible(&app, &crate::i18n::t(ALONGSIDE_BOOT_GUIDANCE)).is_none());
            let guidance = app.alongside_storage_guidance();
            assert!(guidance.contains(&crate::i18n::t(LINUX_ALONGSIDE_READY)));
            assert!(guidance.contains(&capacity));
            let published = app.publish();
            let storage = published[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == op::STORAGE)
                .unwrap();
            let published_guidance = storage
                .children
                .iter()
                .filter(|resource| resource.kind == "installer.guidance")
                .map(|resource| {
                    resource
                        .presentation
                        .as_ref()
                        .unwrap()
                        .preview
                        .as_ref()
                        .unwrap()
                        .as_str()
                })
                .collect::<Vec<_>>()
                .join(". ");
            assert_eq!(published_guidance, guidance);
            for hidden in ["35 GiB", "offline", "LUKS", "2 GiB", "1 GiB"] {
                assert!(!guidance.contains(hidden), "unexpected advice: {guidance}");
            }
            app.step = Step::Review;
            let _ = build(&mut app);
            assert!(find_accessible(&app, &capacity).is_some());
            assert!(find_accessible(&app, &crate::i18n::t(ALONGSIDE_BOOT_GUIDANCE)).is_some());
            assert!(find_accessible(&app, &crate::i18n::t(ALONGSIDE_FILES_GUIDANCE)).is_some());
            assert_eq!(app.install_options().guided_free_size_bytes, expected_bytes);
        }
        let capacity = app.alongside_allocation_summary(&app.guided_alongside_plan().unwrap());

        app.disks[0].partitions[0].size_bytes =
            app.disks[0].size_bytes - app.disks[0].partitions[0].start_bytes - 1_048_576;
        app.step = Step::Disk;
        let _ = build(&mut app);
        let guidance = app.alongside_storage_guidance();
        assert!(guidance.contains(&crate::i18n::t(LINUX_ALONGSIDE_PREPARATION)));
        assert!(!guidance.contains(&crate::i18n::t(LINUX_ALONGSIDE_READY)));
        assert!(find_accessible(&app, &guidance).is_some());
        assert!(find_accessible(&app, &crate::i18n::t(LINUX_ALONGSIDE_READY)).is_none());
        assert!(find_accessible(&app, &capacity).is_none());
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
                find_accessible(&app, &crate::i18n::t(WINDOWS_ALONGSIDE_PREPARATION)).is_some(),
                "Windows preparation must be actionable before the user leaves storage selection"
            );

            app.disks[0].partitions[1].filesystem = "ext4".into();
            app.disks[0].partitions[1].label = "Existing Linux".into();
            let _ = build(&mut app);
            assert!(
                find_accessible(&app, &crate::i18n::t(WINDOWS_ALONGSIDE_PREPARATION)).is_none(),
                "Linux-only alongside disks must not receive Windows-specific instructions"
            );
            assert!(find_accessible(&app, &crate::i18n::t(LINUX_ALONGSIDE_READY)).is_none());
            assert!(find_accessible(&app, &crate::i18n::t(LINUX_ALONGSIDE_PREPARATION)).is_none());
            app.step = Step::Review;
            let _ = build(&mut app);
            assert!(find_accessible(&app, &crate::i18n::t(ALONGSIDE_BOOT_GUIDANCE)).is_some());
            assert!(find_accessible(&app, &crate::i18n::t(ALONGSIDE_FILES_GUIDANCE)).is_some());
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
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert!(app.destructive_confirmation_target().is_none());

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            activate(&mut app, &mut doc, "guided_extent_1");
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
            app.erase_confirmation.set(disk.name.clone());
            let old_efi_uuid = app.guided_efi_partuuid.clone();
            let old_root_uuid = app.guided_root_partuuid.clone();
            assert!(app.guided_alongside_plan().is_some());

            app.refresh_disks_with(vec![disk]);
            // Rediscovery may retain the same hardware, never its reviewed
            // extent or authorization to format anything.
            assert_eq!(app.selected_disk, Some(0));
            assert_eq!(app.selected_guided_extent, None);
            assert!(!app.selected_disk_can_install_alongside());
            assert!(local_text(&app.erase_confirmation).is_empty());
            assert_ne!(app.guided_efi_partuuid, old_efi_uuid);
            assert_ne!(app.guided_root_partuuid, old_root_uuid);

            app.selected_disk = Some(0);
            assert!(
                app.guided_alongside_plan().is_none(),
                "the pre-refresh extent index must not survive rediscovery"
            );
            app.go_next();
            assert_eq!(app.step, Step::Disk, "a stale choice must remain blocked");
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert!(app.destructive_confirmation_target().is_none());
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
            restarted.erase_confirmation.set("/dev/nvme0n1".into());
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
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.step, Step::Disk);
            assert!(!app.storage_preflight_passed);

            app.on_external_event(
                &format!("{PREFLIGHT_FAIL_PREFIX}current:EFI has less than 1 GiB free"),
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.step, Step::Disk);
            assert!(app.status.contains("less than 1 GiB"));

            app.storage_preflight_request = Some("retry".into());
            app.on_external_event(
                &format!("{PREFLIGHT_OK_PREFIX}retry:prepared storage check passed"),
                &mut ui::widget::Edits::lend(&mut doc),
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
            assert_eq!(local_text(&app.username), "ada");
            assert_eq!(local_text(&app.hostname), "studio");
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
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            run(
                &mut app,
                &doc,
                op::STORAGE,
                "set-method",
                serde_json::json!({ "method": "erase" }),
            );
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
            app.sync(&doc, &root);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Media);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Options);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Account);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Disk);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Accounts);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Network);

            activate(&mut app, &mut doc, "Back");
            assert_eq!(app.step, Step::Keyboard);

            // The first step (Keyboard) offers no Back; the action is a no-op.
            assert!(!has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "Back"
            ));
            run(&mut app, &doc, op::SESSION, "back", Value::Null);
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
            app.sync(&doc, &root);

            activate(&mut app, &mut doc, "Review storage");
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

            app.on_external_event(
                "==> step one",
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            app.on_external_event(
                "==> step two",
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            assert!(app.status_lines.iter().any(|l| l == "==> step one"));
            assert!(app.status_lines.iter().any(|l| l == "==> step two"));
            assert_eq!(app.install_state, InstallState::Running);

            app.on_external_event(CTRL_DONE, &mut ui::widget::Edits::lend(&mut doc), &root);
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
            app.on_external_event(
                "error: no space left on device",
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            app.on_external_event(CTRL_FAIL, &mut ui::widget::Edits::lend(&mut doc), &root);
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

            app.on_external_event(
                "::tilo-recovery-key::abcd-efgh-ijkl-mnop",
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.recovery_key.as_deref(), Some("abcd-efgh-ijkl-mnop"));
            assert!(
                app.status_lines.is_empty(),
                "secret marker is not a log line"
            );
        }
        // Phase `visible_recovery_key_can_be_copied_without_entering_shared_state`.
        {
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            assert!(
                !has_test_id(
                    app.build_composition(&Theme::dark()).as_ref(),
                    "recovery_key_copy"
                ),
                "the action must not exist before a key has been received"
            );

            let key = "abcd-efgh-ijkl-mnop";
            app.on_external_event(
                &format!("{RECOVERY_KEY_PREFIX}{key}"),
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_copy"
            ));

            activate(&mut app, &mut doc, "recovery_key_copy");
            assert!(app.recovery_key_copied);
            assert_eq!(ui::clipboard_paste().as_deref(), Some(key));
            assert_eq!(
                recorder.secret_copies(),
                [key],
                "the key is copied as a secret, out of history and device sync"
            );
            assert!(
                !String::from_utf8_lossy(&doc.save()).contains(key),
                "the recovery secret must never be replicated through the CRDT"
            );

            app.on_external_event(
                &format!("{RECOVERY_KEY_PREFIX}replacement-key"),
                &mut ui::widget::Edits::lend(&mut doc),
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
            app.sync(&doc, &root);

            app.on_external_event(
                &format!("{RECOVERY_KEY_PREFIX}failure-safe-key"),
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            );
            app.on_external_event(CTRL_FAIL, &mut ui::widget::Edits::lend(&mut doc), &root);
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_copy"
            ));
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_saved"
            ));

            app.install_state = InstallState::Done;
            // Reboot stays disabled until the key is confirmed saved; the
            // action refuses the same way.
            run(&mut app, &doc, op::POWER, "reboot", Value::Null);
            assert!(app.status.contains("Save the recovery key"));

            activate(&mut app, &mut doc, "recovery_key_saved");
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
                Step::Accounts,
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
            let footer = find_test_id(&app, "installer.actions").expect("pinned review actions");
            let nav = footer
                .children()
                .into_iter()
                .next()
                .expect("review navigation");
            let scroll = find_named(&app, "Scrollable").expect("review content viewport");
            // Full height is the toolkit's regular control height, the
            // height every single-line field takes.
            let full = ui::widget::ControlSize::Regular.height(&Theme::dark());
            assert!(
                input.get_rect().height() >= full - 0.5,
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
                nav.get_rect().y0 >= scroll.get_rect().y1,
                "navigation overlaps the scrolling confirmation region"
            );
            assert!(
                nav.get_rect().y1 <= 768.0,
                "pinned navigation escaped the viewport"
            );
        }
    }

    #[test]
    fn pinned_actions_fit_large_text_and_focus_reveals_only_step_content() {
        for base in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for scale in [1.0, 2.0] {
                let theme = base.clone().with_text_scale(scale);
                for step in [
                    Step::Keyboard,
                    Step::Network,
                    Step::Accounts,
                    Step::Disk,
                    Step::Account,
                    Step::Options,
                    Step::Media,
                    Step::Review,
                    Step::Install,
                ] {
                    let mut app = InstallerApp::for_shot(step);
                    if step == Step::Install {
                        app.shot_finished();
                        app.recovery_key_saved = true;
                    }
                    app.set_status("Check your choices before continuing.");
                    layout_with_theme(&mut app, 800.0, 600.0, &theme);
                    let footer = find_test_id(&app, "installer.actions").expect("pinned actions");
                    let scroll = find_named(&app, "Scrollable").expect("content viewport");
                    assert!(
                        footer.get_rect().y0 >= scroll.get_rect().y1,
                        "{step:?}/{scale}: actions overlap content"
                    );
                    assert!(
                        footer.get_rect().y0 >= 0.0 && footer.get_rect().y1 <= 600.0,
                        "{step:?}/{scale}: actions outside viewport: {:?}",
                        footer.get_rect()
                    );
                    assert!(
                        scroll.get_rect().height() > 0.0,
                        "content viewport collapsed"
                    );
                    let status = find_accessible(&app, "Check your choices before continuing.")
                        .expect("validation line");
                    assert_eq!(status.live, Some(Politeness::Assertive));
                    let validation =
                        find_test_id(footer, "installer.validation").expect("pinned validation");
                    assert!(
                        validation.get_rect().height() > 0.0 && validation.get_rect().y1 <= 600.0
                    );
                    assert!(!has_test_id(scroll, "installer.actions"));
                }

                for variant in 0..3 {
                    let mut app = staged_app();
                    let action = match variant {
                        0 => {
                            app.step = Step::Account;
                            "return_to_host"
                        }
                        1 => {
                            app.shot_failed();
                            "Review storage"
                        }
                        _ => {
                            app.step = Step::Media;
                            app.codecs = true;
                            "Next"
                        }
                    };
                    layout_with_theme(&mut app, 800.0, 600.0, &theme);
                    let footer = find_test_id(&app, "installer.actions").expect("variant footer");
                    let button = find_test_id(footer, action).expect("retained pinned action");
                    assert!(
                        button.get_rect().height() > 0.0 && button.get_rect().y1 <= 600.0,
                        "{action}/{scale} escaped viewport: {:?}",
                        button.get_rect()
                    );
                }

                assert_account_focus_reveal(&theme);
            }
        }
    }

    fn assert_account_focus_reveal(theme: &Theme) {
        let mut account = InstallerApp::for_shot(Step::Account);
        let nodes = layout_with_theme(&mut account, 800.0, 600.0, theme);
        let next = find_test_id(&account, "Next").expect("Next").get_rect();
        let confirm =
            find_test_id(&account, "installer.password_confirm").expect("password confirmation");
        assert!(confirm.is_focusable());
        let target = nodes
            .values()
            .copied()
            .find(|&widget| {
                // SAFETY: the production node map belongs to the still-live,
                // unchanged app tree. No pointer survives the next rebuild.
                unsafe { (*widget).test_id() == Some("installer.password_confirm") }
            })
            .expect("confirmation layout node");
        assert!(
            account.reveal_descendant(target),
            "focus reveal must find the input"
        );
        let nodes = layout_with_theme(&mut account, 800.0, 600.0, theme);
        let confirm = find_test_id(&account, "installer.password_confirm")
            .unwrap()
            .get_rect();
        let viewport = find_named(&account, "Scrollable").unwrap().get_rect();
        assert!(
            confirm.y0 >= viewport.y0 && confirm.y1 <= viewport.y1,
            "focused confirmation not revealed: {confirm:?} in {viewport:?}"
        );
        assert_eq!(
            find_test_id(&account, "Next").unwrap().get_rect(),
            next,
            "revealing a field moved pinned navigation"
        );
        let offset = account.viewport_offset.get();
        let next_target = nodes
            .values()
            .copied()
            .find(|&widget| {
                // SAFETY: the current production node map, as above.
                unsafe { (*widget).test_id() == Some("Next") }
            })
            .expect("Next layout node");
        assert!(account.reveal_descendant(next_target));
        assert_eq!(
            account.viewport_offset.get(),
            offset,
            "footer focus scrolled content"
        );
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

            // Username + optional full name + password + confirmation, + Back
            // + Next. A password's show/hide control is an inline accessory,
            // not a Tab stop.
            assert_eq!(
                count_focusable(&app),
                6,
                "4 inputs and 2 nav buttons must be focusable"
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
            let footer = find_test_id(&app, "installer.actions").expect("disk actions");
            let next_disabled = footer
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| !b.is_focusable()));
            assert_eq!(next_disabled, Some(true), "Next disabled with no disk");

            let mut app2 = InstallerApp::with_disks(sample());
            app2.step = Step::Disk;
            app2.selected_disk = Some(0);
            let _ = build(&mut app2);
            let footer2 = find_test_id(&app2, "installer.actions").expect("disk actions");
            let next_enabled = footer2
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

    fn prepared_profile_app() -> InstallerApp {
        let mut app = InstallerApp::with_disks(sample_disks());
        let object = serde_json::json!({
            "schema": 1, "hostname": "travel", "username": "owner",
            "full_name": "Owner", "locale": "en_US.UTF-8", "keyboard": "us",
            "timezone": "UTC", "filesystem": "btrfs",
            "prepared_setup": {
                "schema": 1, "sealed_bytes": 4096, "ciphertext_sha256": "a".repeat(64)
            }
        });
        app.apply_media_profile(
            crate::media_profile::validate_object(object.as_object().unwrap()).unwrap(),
        );
        app.tpm2 = false;
        app.selected_disk = Some(0);
        app
    }

    #[test]
    fn prepared_storage_revalidation_reuses_supplied_credentials_but_keeps_review() {
        let mut app = prepared_profile_app();
        app.go_next();
        assert_eq!(
            app.step,
            Step::Account,
            "ciphertext binding is not a credential"
        );
        *app.password.lock().unwrap() = "correct horse battery staple".into();
        *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
        app.go_next();
        assert_eq!(app.step, Step::Media);
        app.go_back();
        app.go_back();
        app.go_back();
        assert_eq!(app.step, Step::Disk);
        let expected = app.install_options().prepared_setup;
        app.go_next();
        assert_eq!(
            app.step,
            Step::Media,
            "storage review must not reask the account"
        );
        assert!(!app.codecs, "media formats still require opt-in");
        app.go_next();
        assert_eq!(app.step, Step::Review);
        app.go_next();
        assert_eq!(app.step, Step::Review, "Next never authorizes installation");
        app.begin_install();
        assert_eq!(app.install_state, InstallState::Idle);
        assert!(app.status.contains("Type /dev/sda exactly"));
        app.erase_confirmation.set("/dev/sda".into());
        app.begin_install();
        assert_eq!(app.install_state, InstallState::Idle);
        assert!(app.status.contains("save your recovery key"));
        assert_eq!(app.install_options().prepared_setup, expected);
        assert_eq!(local_text(&app.username), "owner");
    }

    #[test]
    fn prepared_storage_revalidation_requires_current_device_credentials() {
        for (password, confirmation, separate) in [
            ("", "", ""),
            ("correct horse battery staple", "mismatch", ""),
            ("804927", "804927", ""),
            (
                "correct horse battery staple",
                "correct horse battery staple",
                "correct horse battery staple",
            ),
        ] {
            let mut app = prepared_profile_app();
            // A six-digit PIN worked on the verified target, but must not be
            // reused after boot policy revalidation requires a passphrase.
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            *app.password.lock().unwrap() = password.into();
            *app.password_confirmation.lock().unwrap() = confirmation.into();
            *app.disk_password.lock().unwrap() = separate.into();
            app.target_verified_boot = false;
            app.go_next();
            assert_eq!(app.step, Step::Account);
            assert_eq!(app.install_state, InstallState::Idle);
        }
    }

    #[test]
    fn prepared_storage_revalidation_still_routes_option_repairs() {
        for unsupported_filesystem in [false, true] {
            let mut app = prepared_profile_app();
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            if unsupported_filesystem {
                app.set_filesystem_support(Ok([false, true, true]));
            } else {
                app.profile_completion.as_mut().unwrap().locale = FieldCompletion::NeedsRepair;
                app.locale.set(String::new());
            }
            app.go_next();
            assert_eq!(app.step, Step::Options);
            app.go_next();
            if unsupported_filesystem {
                assert_eq!(
                    app.step,
                    Step::Media,
                    "supported replacement can be reviewed"
                );
            } else {
                assert_eq!(app.step, Step::Options, "locale repair still validates");
            }
            assert_eq!(app.install_state, InstallState::Idle);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn media_profile_skips_configured_screens_and_routes_repairs() {
        {
            let expected = install_profile_model::PreparedSetupExpectation {
                schema: 1,
                sealed_bytes: 4096,
                ciphertext_sha256: "a".repeat(64),
            };
            let mut app = InstallerApp::with_disks(sample_disks());
            let object = serde_json::json!({
                "schema": 1, "purpose": "server", "device_label": "Travel computer",
                "hostname": "travel", "username": "owner", "full_name": "Owner",
                "locale": "en_US.UTF-8", "keyboard": "us", "timezone": "UTC",
                "filesystem": "btrfs", "prepared_setup": expected,
            });
            app.apply_media_profile(
                crate::media_profile::validate_object(object.as_object().unwrap()).unwrap(),
            );
            for selected in [Some(0), Some(1)] {
                app.selected_disk = selected;
                let options = app.install_options();
                assert_eq!(options.prepared_setup.as_ref(), Some(&expected));
                assert_eq!(options.device_label, "Travel computer");
                assert_eq!(options.purpose, "server");
            }
        }
        // Phase `validated_media_profile_skips_already_configured_screens`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                prepared_setup: None,
                windows_preparation: None,
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: Some("ada Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: Vec::new(),
            });
            app.tpm2 = false;
            assert_eq!(app.step, Step::Disk);
            assert_eq!(local_text(&app.hostname), "studio");
            assert_eq!(local_text(&app.username), "ada");
            assert_eq!(app.keymap(), "German (QWERTZ)");
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
            assert_eq!(
                app.step,
                Step::Media,
                "configured Options is not repeated; the formats are still the owner's choice"
            );
            assert!(!app.codecs);
            app.go_back();
            assert_eq!(app.step, Step::Options);
            let _ = build(&mut app);
            assert!(
                has_test_id(app.composed.element().unwrap(), "fido2_choice"),
                "Review Back must keep on-device security choices reachable"
            );
            app.go_back();
            assert_eq!(app.step, Step::Account);
            app.go_back();
            assert_eq!(app.step, Step::Disk);
            app.go_back();
            assert_eq!(app.step, Step::Accounts);
            app.go_back();
            assert_eq!(app.step, Step::Network);
        }
        // Phase `partial_media_profile_routes_to_only_the_repair_screens`.
        {
            let mut app = InstallerApp::with_disks(sample_disks());
            app.apply_media_profile(crate::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                prepared_setup: None,
                windows_preparation: None,
                hostname: None,
                username: Some("ada".into()),
                full_name: Some("ada Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![crate::media_profile::MediaProfileField::Hostname],
            });
            app.tpm2 = false;
            assert_eq!(app.step, Step::Disk, "sound keyboard is retained");
            assert_eq!(local_text(&app.username), "ada");
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
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                prepared_setup: None,
                windows_preparation: None,
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: Some("ada Example".into()),
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
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                prepared_setup: None,
                windows_preparation: None,
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: Some("ada Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: None,
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![crate::media_profile::MediaProfileField::Keyboard],
            });
            assert_eq!(app.step, Step::Keyboard);
            assert_eq!(local_text(&app.hostname), "studio");
            assert_eq!(local_text(&app.username), "ada");
            assert!(app.status.contains("keyboard layout"));
        }
    }
}
