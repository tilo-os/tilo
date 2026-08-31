//! [`InstallerApp`] — the root installer widget + wizard state machine.
//!
//! Structured like the greeter (the verified `ui`-app-under-cage
//! template): a thin root widget owning a single composed child (a
//! [`Card`]) and delegating the five `Widget` passes into it.
//!
//! IMPORTANT (focus): the App discovers focusable widgets by walking the
//! widget tree (`Widget::children`) in lock-step with the taffy tree.
//! Any intermediate taffy node MUST have a matching widget in
//! `children()`, or the recursion derails and inputs never enter
//! `focusable_order` — breaking Tab / arrow navigation. That's why the
//! card is a real [`Card`] widget (not a bare taffy node) and the
//! two-button rows are real `ui::Row`s: the widget tree mirrors the
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
//!   * `installer.password` — `Text` (rendered masked; value stays real)
//!   * `installer.hostname` — `Text` (seeded `qos`)
//!   * `installer.timezone` — `Text` (seeded `UTC`)
//!   * `installer.next` / `back` / `begin` — `bool` rising-edge flags
//!   * `installer.reboot` / `poweroff` — `bool` rising-edge flags (Finish)
//!   * `installer.disk.<i>` — `bool` disk-row selection rising edge
//!   * `installer.fs.<i>`   — `bool` filesystem-segment selection rising edge
//!
//! The install runs on a **worker thread**: on the `begin` rising edge the
//! app spawns [`crate::install::run_install`] and streams each output line
//! back through the winit `EventLoopProxy` as a `ui::UserEvent::External`,
//! folded into the status log by [`InstallerApp::on_external_event`] — so
//! the UI stays live and the log animates. If no proxy is wired (tests /
//! no event loop) it falls back to a synchronous run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
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

use ui::app::UserEvent;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Dropdown, Row, Spinner, Text, TextInput, Widget};

use crate::action_button::ActionButton;
use crate::disk_row::DiskRow;
use crate::disks::{Disk, read_disks};
use crate::install::{InstallOptions, PowerAction};
use crate::keyboard_view::KeyboardView;
use crate::layouts;
use crate::seg_choice::SegChoice;
use crate::status_log::StatusLog;
use crate::step_indicator::StepIndicator;

const USERNAME_PATH: &str = "installer.username";
const PASSWORD_PATH: &str = "installer.password";
const HOSTNAME_PATH: &str = "installer.hostname";
const TIMEZONE_PATH: &str = "installer.timezone";
/// CRDT path holding the selected keyboard layout's *display name*.
const KEYMAP_PATH: &str = "installer.keymap";
/// CRDT path holding the (masked) TPM2 unlock PIN, when encryption is on.
const PIN_PATH: &str = "installer.pin";
const NEXT_PATH: &str = "installer.next";
const BACK_PATH: &str = "installer.back";
const BEGIN_PATH: &str = "installer.begin";
const REBOOT_PATH: &str = "installer.reboot";
const POWEROFF_PATH: &str = "installer.poweroff";
/// Per-disk selection flag path: `installer.disk.<index>`.
fn disk_path(i: usize) -> String {
    format!("installer.disk.{i}")
}
/// Per-filesystem selection flag path: `installer.fs.<index>`.
fn fs_path(i: usize) -> String {
    format!("installer.fs.{i}")
}
/// Per-encryption-choice selection flag path: `installer.encrypt.<index>`
/// (index 0 = off, 1 = on). Mirrors the filesystem segmented selector.
fn encrypt_path(i: usize) -> String {
    format!("installer.encrypt.{i}")
}
/// Per-FIDO2-choice selection flag path: `installer.fido2.<index>`
/// (index 0 = off, 1 = on). Mirrors the encryption segmented selector.
fn fido2_path(i: usize) -> String {
    format!("installer.fido2.{i}")
}
/// Rising-edge flag toggling the "I've saved my recovery key" acknowledgement.
const RECOVERY_ACK_PATH: &str = "installer.recovery_ack";

/// Labels for the encryption segmented selector, in display order. Index 0
/// (off) is the default; index 1 enables multi-factor LUKS.
pub const ENCRYPT_LABELS: [&str; 2] = ["No encryption", "Encrypt disk (LUKS)"];

/// Labels for the FIDO2 segmented selector, in display order. Index 0 (off)
/// is the default; index 1 enrolls a hardware security key as an extra
/// unlock factor. Only shown when encryption is on.
pub const FIDO2_LABELS: [&str; 2] = ["No security key", "Enroll security key"];

/// Default hostname seeded into the CRDT (and the installed system).
const DEFAULT_HOSTNAME: &str = "qos";
/// Default time zone seeded into the CRDT.
const DEFAULT_TIMEZONE: &str = "UTC";
/// Default keyboard-layout display name seeded into the CRDT.
const DEFAULT_KEYMAP_NAME: &str = layouts::DEFAULT_DISPLAY_NAME;

/// Filesystem choices, in display order. Index 0 is the default.
pub const FILESYSTEMS: [&str; 3] = ["btrfs", "ext4", "xfs"];

/// One-line explanation for each filesystem, shown under the chooser.
fn fs_description(i: usize) -> &'static str {
    match i {
        0 => "Btrfs: snapshots, compression and rollback — recommended for qOS.",
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

/// Explanation shown under the encryption chooser, reflecting the choice and
/// what this machine's hardware can actually do.
fn encrypt_description(encrypt: bool, tpm2: bool) -> &'static str {
    match (encrypt, tpm2) {
        (false, _) => "The disk is not encrypted. Anyone with physical access can read your data.",
        (true, true) => {
            "Encrypts the whole disk with LUKS. This computer's TPM2 chip unlocks it at boot, and a recovery key (shown once during install) is always generated as a backup in case the TPM is reset or fails."
        }
        (true, false) => {
            "Encrypts the whole disk with LUKS. This computer has no TPM2 chip, so it cannot unlock itself — you will type a passphrase every time it boots. A recovery key is also generated and shown once, as the backup if you forget the passphrase."
        }
    }
}

/// Control sentinels sent over the `External` channel by the install
/// worker thread to mark terminal outcomes (anything else is a log line).
const CTRL_DONE: &str = "\u{1}done";
const CTRL_FAIL: &str = "\u{1}fail";

/// Card geometry (logical px).
const CARD_WIDTH: f32 = 520.0;
const CARD_PADDING: f32 = 28.0;
const CARD_GAP: f32 = 12.0;
/// Height of the install status log box.
const STATUS_LOG_HEIGHT: f32 = 200.0;

/// The wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Keyboard,
    Disk,
    Account,
    Options,
    Review,
    Install,
}

/// Captions for the step-progress breadcrumb, index-aligned with
/// [`step_index`].
pub const STEP_LABELS: [&str; 6] = [
    "Keyboard", "Disk", "Account", "Options", "Review", "Install",
];

/// Position of `step` in the breadcrumb (0-based). Keyboard is first so the
/// layout is chosen BEFORE the user types anything (username/password).
pub fn step_index(step: Step) -> usize {
    match step {
        Step::Keyboard => 0,
        Step::Disk => 1,
        Step::Account => 2,
        Step::Options => 3,
        Step::Review => 4,
        Step::Install => 5,
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
/// [`InstallerApp::go_next`]).
pub fn next_step(step: Step) -> Option<Step> {
    match step {
        Step::Keyboard => Some(Step::Disk),
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
        Step::Disk => Some(Step::Keyboard),
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

/// Guard for advancing out of `step`. `Ok(())` means it's valid to move
/// forward; `Err(msg)` is a user-facing reason it isn't yet.
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
            if username.trim().is_empty() {
                Err("Enter a username".to_string())
            } else {
                Ok(())
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
        Step::Keyboard | Step::Review | Step::Install => Ok(()),
    }
}

// ===================================================================
// Card — a fixed-width, padded, vertically-stacked rounded container.
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for child in &mut self.children {
            child.sync(state, root);
        }
    }

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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let Ok(child_nodes) = taffy.children(node_id) else {
            return;
        };
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
            if let Some(&cnode) = child_nodes.get(idx)
                && let Some(child) = self.children.get_mut(idx)
            {
                child.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    root,
                    handled,
                    focus_target,
                    cnode,
                    taffy,
                );
            }
            return;
        }
        for (child, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
            child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                root,
                handled,
                focus_target,
                child_node,
                taffy,
            );
            if *handled {
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

    fn needs_commit(&self) -> bool {
        self.children.iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for child in &mut self.children {
            child.on_commit();
        }
    }

    fn name(&self) -> &'static str {
        "InstallerCard"
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
    reason = "each bool is an independent wizard-state flag (encryption/FIDO2 opt-in, TPM presence, shot/bootstrap/dirty/focus one-shots), not a combinatorial state machine"
)]
pub struct InstallerApp {
    disks: Vec<Disk>,
    selected_disk: Option<usize>,
    /// Index into [`FILESYSTEMS`].
    selected_fs: usize,
    /// Full-disk encryption (multi-factor LUKS) opt-in. Default `false`.
    encrypt: bool,
    /// Enroll a FIDO2 security key as an extra unlock factor (only with
    /// encryption). Default `false`.
    fido2: bool,
    /// Cached (masked) TPM2 unlock PIN (mirrors the `installer.pin` CRDT
    /// Text). Empty ⇒ no PIN slot is enrolled. Only used with encryption.
    pin: String,
    /// User has acknowledged they will save the install-time recovery key.
    /// Required before Begin Install when encryption is on. Default `false`.
    recovery_ack: bool,
    /// Whether this machine has a TPM2 device, sampled once at construction.
    /// Encryption works either way; without one the unlock secret stops being
    /// optional, because the alternative is typing the recovery key at boot.
    tpm2: bool,
    step: Step,
    install_state: InstallState,
    /// Streamed install output (also the success/failure summary).
    status_lines: Vec<String>,
    /// Cached account/system fields (mirror their CRDT Text objects) for
    /// the Review display + backend argv.
    username: String,
    hostname: String,
    timezone: String,
    /// Selected keyboard-layout *display name* (mirrors the
    /// `installer.keymap` CRDT Text). Resolved to xkb layout/variant via
    /// [`crate::layouts`] when building the install command.
    keymap: String,
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
    /// Wired by `main` so the worker thread can pump progress back.
    proxy_slot: Option<ProxySlot>,
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
        let mut s = Self::with_disks(read_disks());
        s.timezones = crate::timezones::list_timezones();
        s
    }

    /// Construct with an explicit disk list (tests / headless rendering).
    /// Uses a small deterministic time-zone list so tests don't shell out;
    /// [`Self::new`] swaps in the full system list.
    pub fn with_disks(disks: Vec<Disk>) -> Self {
        Self {
            disks,
            selected_disk: None,
            selected_fs: 0,
            encrypt: false,
            fido2: false,
            pin: String::new(),
            recovery_ack: false,
            tpm2: has_tpm2(),
            step: Step::Keyboard,
            install_state: InstallState::Idle,
            status_lines: Vec::new(),
            username: String::new(),
            hostname: DEFAULT_HOSTNAME.to_string(),
            timezone: DEFAULT_TIMEZONE.to_string(),
            keymap: DEFAULT_KEYMAP_NAME.to_string(),
            timezones: default_timezones(),
            tz_open_for_shot: false,
            status: String::new(),
            bootstrap_done: false,
            dirty_structure: false,
            focus_reset: false,
            rect: Rect::ZERO,
            composed: None,
            install_launched: false,
            proxy_slot: None,
        }
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
        app.hostname = DEFAULT_HOSTNAME.to_string();
        app.timezone = DEFAULT_TIMEZONE.to_string();
        app.keymap = DEFAULT_KEYMAP_NAME.to_string();
        if !matches!(step, Step::Keyboard | Step::Disk) {
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

    /// Seed a non-default keyboard layout (by display name) for a
    /// `--shots --step keyboard` render, so the visual preview clearly
    /// shows remapped keys. The value is seeded into the CRDT on the first
    /// `sync` via [`Self::bootstrap_crdt`].
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

    /// Mutate a `for_shot(Install)` app into the failure screen.
    pub fn shot_failed(&mut self) {
        self.step = Step::Install;
        self.install_state = InstallState::Failed;
        self.status_lines = sample_install_log();
        self.status_lines
            .push("error: bootc install to-disk exited non-zero".to_string());
        self.status_lines
            .push("Installation failed. Review the log above.".to_string());
    }

    /// The selected disk, if any.
    fn selected_disk_ref(&self) -> Option<&Disk> {
        self.selected_disk.and_then(|i| self.disks.get(i))
    }

    /// The selected disk's device path, if any.
    fn selected_dev(&self) -> Option<String> {
        self.selected_disk_ref().map(|d| d.name.clone())
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

    /// May the install start, as far as encryption is concerned? The Begin
    /// button, the Enter key and the start path all ask this, so they cannot
    /// drift apart. Without a TPM the unlock secret is required: leaving it
    /// empty is legal for the backend but means the 48-character recovery key
    /// is what gets typed at every boot, which is not a choice anyone makes on
    /// purpose.
    fn encryption_ready(&self) -> bool {
        if !self.encrypt {
            return true;
        }
        self.recovery_ack && (self.tpm2 || !self.pin.trim().is_empty())
    }

    /// Human-readable summary of the encryption factors for the Review step.
    fn encryption_summary(&self) -> String {
        if !self.encrypt {
            return "Off".to_string();
        }
        // Recovery key is always present; TPM is PIN-protected iff a PIN was
        // entered; FIDO2 is listed only when enrolling.
        let mut factors = vec!["recovery key".to_string()];
        match (self.tpm2, self.pin.trim().is_empty()) {
            (true, true) => factors.push("TPM2".to_string()),
            (true, false) => factors.push("TPM2 + PIN".to_string()),
            // No TPM: the same secret is enrolled as an ordinary passphrase
            // slot, and it is what gets typed at boot.
            (false, false) => factors.push("boot passphrase".to_string()),
            (false, true) => {}
        }
        if self.fido2 {
            factors.push("FIDO2".to_string());
        }
        format!("On (LUKS — {})", factors.join(", "))
    }

    fn install_options(&self) -> InstallOptions {
        let layout = self.layout();
        // PIN / FIDO2 are only meaningful alongside encryption; drop them
        // when it's off so a stale value can't reach the backend.
        let (pin, fido2) = if self.encrypt {
            (self.pin.clone(), self.fido2)
        } else {
            (String::new(), false)
        };
        InstallOptions {
            filesystem: self.filesystem().to_string(),
            hostname: self.hostname.trim().to_string(),
            timezone: self.timezone.trim().to_string(),
            keymap: layout.xkb_layout.to_string(),
            keyvariant: layout.variant_or_empty().to_string(),
            encrypt: self.encrypt,
            pin,
            fido2,
        }
    }

    fn set_status(&mut self, msg: &str) {
        if self.status != msg {
            self.status = msg.to_string();
            self.dirty_structure = true;
        }
    }

    fn bootstrap_crdt(&self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, USERNAME_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, USERNAME_PATH, ObjType::Text);
        }
        if state.get(root, PASSWORD_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, PASSWORD_PATH, ObjType::Text);
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
        // Keyboard layout seeds with the current selection (default or a
        // shot-injected one), so the picker + preview aren't blank.
        if state.get(root, KEYMAP_PATH).ok().flatten().is_none()
            && let Ok(obj) = state.put_object(root, KEYMAP_PATH, ObjType::Text)
        {
            let _ = state.splice_text(&obj, 0, 0, &self.keymap);
        }
        // The TPM2 PIN is a Text object so the masked TextInput can splice
        // into it like every other field; it starts empty (no PIN).
        if state.get(root, PIN_PATH).ok().flatten().is_none() {
            let _ = state.put_object(root, PIN_PATH, ObjType::Text);
        }
        for path in [
            NEXT_PATH,
            BACK_PATH,
            BEGIN_PATH,
            REBOOT_PATH,
            POWEROFF_PATH,
            RECOVERY_ACK_PATH,
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
        for i in 0..FILESYSTEMS.len() {
            let p = fs_path(i);
            if state.get(root, p.as_str()).ok().flatten().is_none() {
                let _ = state.put(root, p.as_str(), false);
            }
        }
        for i in 0..ENCRYPT_LABELS.len() {
            let p = encrypt_path(i);
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
    }

    fn read_text(state: &AutoCommit, root: &ObjId, path: &str) -> Option<String> {
        match state.get(root, path) {
            Ok(Some((Value::Object(ObjType::Text), obj_id))) => state.text(&obj_id).ok(),
            _ => None,
        }
    }

    fn read_flag(state: &AutoCommit, root: &ObjId, path: &str) -> bool {
        matches!(
            state.get(root, path),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), ScalarValue::Boolean(true))
        )
    }

    /// Advance to the next step if the current one validates.
    fn go_next(&mut self) {
        match validate_step(
            self.step,
            self.selected_disk,
            &self.username,
            &self.hostname,
        ) {
            Ok(()) => {
                if let Some(n) = next_step(self.step) {
                    // Never auto-advance into Install via Next; the Begin
                    // button (begin flag) is the only entry into install.
                    if n != Step::Install {
                        self.step = n;
                        self.status.clear();
                        self.dirty_structure = true;
                        self.focus_reset = true;
                    }
                }
            }
            Err(msg) => self.set_status(&msg),
        }
    }

    /// Step backward. From a *failed* install the Back button returns to
    /// Review so the user can retry; otherwise it walks the wizard back.
    fn go_back(&mut self) {
        if self.step == Step::Install {
            if self.install_state == InstallState::Failed {
                self.step = Step::Review;
                self.install_state = InstallState::Idle;
                self.install_launched = false;
                self.status_lines.clear();
                self.status.clear();
                self.dirty_structure = true;
                self.focus_reset = true;
            }
            return;
        }
        if let Some(p) = prev_step(self.step) {
            self.step = p;
            self.status.clear();
            self.dirty_structure = true;
            self.focus_reset = true;
        }
    }

    /// Enter the Install step and kick off the backend. Prefers a worker
    /// thread (live streaming via the event-loop proxy); falls back to a
    /// blocking run when no proxy is wired (tests / no event loop).
    fn begin_install(&mut self, state: &AutoCommit, root: &ObjId) {
        if self.install_launched {
            return;
        }
        let Some(disk) = self.selected_dev() else {
            self.set_status("Select a disk to continue");
            return;
        };
        // Encryption requires the recovery-key acknowledgement (defense in
        // depth — the Begin button and Enter both gate on this too).
        if self.encrypt && !self.recovery_ack {
            self.set_status("Confirm you'll save your recovery key first");
            return;
        }
        if self.encrypt && !self.tpm2 && self.pin.trim().is_empty() {
            self.set_status("No TPM2 chip — set a boot passphrase, or the recovery key becomes it");
            return;
        }
        let user = Self::read_text(state, root, USERNAME_PATH).unwrap_or_default();
        let password = Self::read_text(state, root, PASSWORD_PATH).unwrap_or_default();
        let opts = self.install_options();

        self.step = Step::Install;
        self.install_state = InstallState::Running;
        self.install_launched = true;
        self.status_lines = vec![format!(
            "Starting install to {disk} for user {user} ({}).",
            opts.filesystem
        )];
        self.dirty_structure = true;
        self.focus_reset = true;

        let proxy = self
            .proxy_slot
            .as_ref()
            .and_then(|s| s.lock().ok().and_then(|g| g.clone()));

        if let Some(proxy) = proxy {
            // Worker thread: stream each line back as an External event.
            std::thread::spawn(move || {
                let result = crate::install::run_install(&disk, &user, &password, &opts, |line| {
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
            // Synchronous fallback (blocks the caller).
            let mut lines = std::mem::take(&mut self.status_lines);
            let result = crate::install::run_install(&disk, &user, &password, &opts, |line| {
                lines.push(line);
            });
            self.status_lines = lines;
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
        self.status_lines
            .push("Installed — remove the medium and reboot.".to_string());
        self.dirty_structure = true;
        self.focus_reset = true;
    }

    fn mark_install_failed(&mut self, extra: Option<String>) {
        if let Some(line) = extra {
            self.status_lines.push(line);
        }
        self.install_state = InstallState::Failed;
        self.status_lines
            .push("Installation failed. Review the log above.".to_string());
        self.dirty_structure = true;
        self.focus_reset = true;
    }

    /// Run a terminal power action (Reboot / Power off). On success the
    /// machine goes down before this returns; a failure surfaces a status.
    fn run_power(&mut self, action: PowerAction) {
        match crate::install::run_power(action) {
            Ok(true) => {}
            _ => self
                .set_status("Could not run power action — remove the medium and reboot manually."),
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
            Step::Disk => self.compose_disk_step(theme, &mut children),
            Step::Account => self.compose_account_step(theme, &mut children),
            Step::Keyboard => self.compose_keyboard_step(theme, &mut children),
            Step::Options => self.compose_options_step(theme, &mut children),
            Step::Review => self.compose_review_step(theme, &mut children),
            Step::Install => self.compose_install_step(theme, &mut children),
        }

        // A transient validation/error line under every step's controls.
        if !self.status.is_empty() {
            children.push(Box::new(
                Text::new(&self.status)
                    .with_size(theme.tokens.typography.scale.sm)
                    .with_color(theme.tokens.colors.semantic.danger),
            ));
        }

        Box::new(Card::new(children))
    }

    fn compose_disk_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let danger = theme.tokens.colors.semantic.danger;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Install qOS").with_size(xl)));
        children.push(Box::new(
            Text::new("Choose where to install. The selected disk will be completely erased.")
                .with_size(sm)
                .with_color(dim),
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
        children.push(nav_row(
            Step::Disk,
            NEXT_PATH,
            "Next",
            self.selected_disk.is_none(),
        ));
    }

    fn compose_account_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Create your account").with_size(xl)));
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
        children.push(Box::new(Text::new("Password").with_size(base)));
        children.push(Box::new(
            TextInput::new(Vec::new())
                .with_id("installer.password")
                .with_on_change_path(PASSWORD_PATH)
                .with_secret(true),
        ));
        children.push(Box::new(
            Text::new("Choose a strong password; you'll use it to log in.")
                .with_size(sm)
                .with_color(dim),
        ));
        children.push(nav_row(Step::Account, NEXT_PATH, "Next", false));
    }

    fn compose_keyboard_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
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
        children.push(nav_row(Step::Keyboard, NEXT_PATH, "Next", false));
    }

    fn compose_options_step(&self, theme: &Theme, children: &mut Vec<Box<dyn Widget>>) {
        let dim = theme.text_dim;
        let xl = theme.tokens.typography.scale.xl;
        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;

        children.push(Box::new(Text::new("Installation options").with_size(xl)));

        children.push(Box::new(Text::new("Filesystem").with_size(base)));
        children.push(Box::new(SegChoice::new(
            FILESYSTEMS
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            (0..FILESYSTEMS.len()).map(fs_path).collect(),
            self.selected_fs,
        )));
        children.push(Box::new(
            Text::new(fs_description(self.selected_fs))
                .with_size(sm)
                .with_color(dim),
        ));

        children.push(Box::new(Text::new("Disk encryption").with_size(base)));
        children.push(Box::new(
            SegChoice::new(
                ENCRYPT_LABELS
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                (0..ENCRYPT_LABELS.len()).map(encrypt_path).collect(),
                usize::from(self.encrypt),
            )
            .with_test_id("encryption_choice"),
        ));
        children.push(Box::new(
            Text::new(encrypt_description(self.encrypt, self.tpm2))
                .with_size(sm)
                .with_color(dim),
        ));

        // The extra unlock factors only make sense with encryption on,
        // so they appear only then.
        if self.encrypt {
            let pin_label = if self.tpm2 {
                "Unlock PIN (optional)"
            } else {
                "Boot passphrase (required — no TPM2 chip)"
            };
            children.push(Box::new(Text::new(pin_label).with_size(base)));
            children.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("installer.pin")
                    .with_on_change_path(PIN_PATH)
                    .with_secret(true),
            ));
            children.push(Box::new(
                Text::new("Add a PIN you'll type at boot alongside the TPM. Leave blank to unlock with the TPM alone.")
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
        }

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

        children.push(nav_row(Step::Options, NEXT_PATH, "Next", false));
    }

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
        children.push(summary_row(theme, "Filesystem", self.filesystem()));
        children.push(summary_row(theme, "Encryption", &self.encryption_summary()));
        children.push(summary_row(theme, "Hostname", self.hostname.trim()));
        children.push(summary_row(theme, "Time zone", self.timezone.trim()));
        children.push(summary_row(theme, "Keyboard layout", self.keymap.trim()));
        children.push(summary_row(theme, "Username", self.username.trim()));

        children.push(Box::new(
            Text::new(&format!("This will ERASE {dev} and all data on it."))
                .with_size(base)
                .with_color(danger),
        ));

        // Encryption generates a recovery key shown ONCE during the
        // install. Make the user explicitly acknowledge they'll save
        // it before they can proceed — otherwise a lost TPM = lost
        // data with no warning.
        if self.encrypt {
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
                ActionButton::new(ack_label, RECOVERY_ACK_PATH).with_test_id("recovery_ack"),
            ));
        }

        // Begin is gated on the recovery-key acknowledgement when
        // encrypting (otherwise always enabled).
        let begin_disabled = !self.encryption_ready();
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

        match self.install_state {
            InstallState::Done => {
                children.push(Box::new(
                    Text::new("qOS is installed")
                        .with_size(xl)
                        .with_color(theme.status_success),
                ));
                children.push(Box::new(
                    Text::new("Installation complete. Remove the installer medium, then reboot.")
                        .with_size(sm)
                        .with_color(dim),
                ));
                children.push(Box::new(StatusLog::new(
                    self.status_lines.clone(),
                    STATUS_LOG_HEIGHT,
                )));
                children.push(finish_row());
            }
            InstallState::Failed => {
                children.push(Box::new(
                    Text::new("Installation failed")
                        .with_size(xl)
                        .with_color(danger),
                ));
                children.push(Box::new(
                    Text::new("Something went wrong. Review the log, then go back to try again.")
                        .with_size(sm)
                        .with_color(dim),
                ));
                children.push(Box::new(StatusLog::new(
                    self.status_lines.clone(),
                    STATUS_LOG_HEIGHT,
                )));
                children.push(Box::new(ActionButton::new("Back", BACK_PATH)));
            }
            _ => {
                children.push(Box::new(Text::new("Installing qOS…").with_size(xl)));
                children.push(Box::new(
                    Row::new().push(Box::new(Spinner::new())).push(Box::new(
                        Text::new("Working — this can take a few minutes. Don't power off.")
                            .with_size(sm)
                            .with_color(dim),
                    )),
                ));
                children.push(Box::new(StatusLog::new(
                    self.status_lines.clone(),
                    STATUS_LOG_HEIGHT,
                )));
            }
        }
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

/// The Finish-screen action row: Reboot (primary) + Power off (secondary).
fn finish_row() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(Box::new(
                ActionButton::new("Reboot now", REBOOT_PATH).primary(),
            ))
            .push(Box::new(ActionButton::new("Power off", POWEROFF_PATH))),
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
        },
        Disk {
            name: "/dev/nvme0n1".to_string(),
            size: "1T".to_string(),
            model: "WD Black SN770".to_string(),
        },
    ]
}

/// Representative streamed backend output for `--shots`. Includes long
/// lines that wrap so the status log's wrap-aware tailing is exercised.
fn sample_install_log() -> Vec<String> {
    vec![
        "Starting install to /dev/sda for user quinten (btrfs).".to_string(),
        "==> Copying the running qOS image into local container storage".to_string(),
        "Copying local image docker://localhost/qos-installer:latest to containers-storage:localhost/bootc".to_string(),
        "==> Installing qOS onto /dev/sda (this ERASES the disk)".to_string(),
        "  base system installed.".to_string(),
        "==> Preparing the 'quinten' account on the installed system".to_string(),
        "==> Setting hostname 'qos' and time zone 'UTC'".to_string(),
    ]
}

impl InstallerApp {
    /// Cache the editable text fields for display / backend argv.
    fn sync_cached_fields(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(u) = Self::read_text(state, root, USERNAME_PATH) {
            self.username = u;
        }
        if let Some(h) = Self::read_text(state, root, HOSTNAME_PATH) {
            self.hostname = h;
        }
        if let Some(t) = Self::read_text(state, root, TIMEZONE_PATH) {
            self.timezone = t;
        }
        if let Some(k) = Self::read_text(state, root, KEYMAP_PATH) {
            self.keymap = k;
        }
        // Cache the (masked) PIN text for the backend argv / stdin. Reading
        // it here (rather than only after the encryption-toggle loop below)
        // is safe: turning encryption off always clears `self.pin` directly
        // (see `sync_selection_rising_edges`), so that write wins regardless
        // of read order.
        if let Some(pin) = Self::read_text(state, root, PIN_PATH) {
            self.pin = pin;
        }
    }

    /// Disk / filesystem / encryption / FIDO2 segmented-selector rising
    /// edges — each selector raises a per-option boolean flag on pick,
    /// which this drains and resolves into the corresponding index/state.
    fn sync_selection_rising_edges(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Disk-selection rising edges (only meaningful on the Disk step).
        for i in 0..self.disks.len() {
            let p = disk_path(i);
            if Self::read_flag(state, root, p.as_str()) {
                let _ = state.put(root, p.as_str(), false);
                self.selected_disk = Some(i);
                self.status.clear();
                self.dirty_structure = true;
            }
        }

        // Filesystem-selection rising edges (Options step).
        for i in 0..FILESYSTEMS.len() {
            let p = fs_path(i);
            if Self::read_flag(state, root, p.as_str()) {
                let _ = state.put(root, p.as_str(), false);
                self.selected_fs = i;
                self.dirty_structure = true;
            }
        }

        // Encryption-choice rising edges (Options step): index 0 = off,
        // 1 = on. Same segmented-selector pattern as the filesystem choice.
        for i in 0..ENCRYPT_LABELS.len() {
            let p = encrypt_path(i);
            if Self::read_flag(state, root, p.as_str()) {
                let _ = state.put(root, p.as_str(), false);
                let now_on = i == 1;
                if now_on != self.encrypt {
                    self.encrypt = now_on;
                    // Turning encryption off clears the dependent factors so
                    // a stale PIN / FIDO2 / ack can't survive a toggle.
                    if !now_on {
                        self.fido2 = false;
                        self.recovery_ack = false;
                        if let Some((Value::Object(ObjType::Text), pobj)) =
                            state.get(root, PIN_PATH).ok().flatten()
                            && let Ok(cur) = state.text(&pobj)
                        {
                            let _ = state.splice_text(
                                &pobj,
                                0,
                                ui::num::isize_of_usize(cur.chars().count()),
                                "",
                            );
                        }
                        self.pin.clear();
                    }
                    self.dirty_structure = true;
                }
            }
        }

        // FIDO2-choice rising edges (Options step, only when encrypting).
        for i in 0..FIDO2_LABELS.len() {
            let p = fido2_path(i);
            if Self::read_flag(state, root, p.as_str()) {
                let _ = state.put(root, p.as_str(), false);
                self.fido2 = i == 1;
                self.dirty_structure = true;
            }
        }
    }

    /// Recovery-key acknowledgement + wizard-navigation (Back/Next/Begin)
    /// and Finish-screen (Reboot/Power-off) rising edges.
    fn sync_navigation_rising_edges(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Recovery-key acknowledgement toggle (Review step, when encrypting).
        if Self::read_flag(state, root, RECOVERY_ACK_PATH) {
            let _ = state.put(root, RECOVERY_ACK_PATH, false);
            self.recovery_ack = !self.recovery_ack;
            self.status.clear();
            self.dirty_structure = true;
        }

        // Back rising edge.
        if Self::read_flag(state, root, BACK_PATH) {
            let _ = state.put(root, BACK_PATH, false);
            self.go_back();
        }

        // Next rising edge.
        if Self::read_flag(state, root, NEXT_PATH) {
            let _ = state.put(root, NEXT_PATH, false);
            self.go_next();
        }

        // Begin rising edge — only valid on the Review step.
        if Self::read_flag(state, root, BEGIN_PATH) {
            let _ = state.put(root, BEGIN_PATH, false);
            if self.step == Step::Review {
                self.begin_install(state, root);
            }
        }

        // Reboot / Power-off rising edges — only on the Finish screen.
        if Self::read_flag(state, root, REBOOT_PATH) {
            let _ = state.put(root, REBOOT_PATH, false);
            if self.step == Step::Install && self.install_state == InstallState::Done {
                self.run_power(PowerAction::Reboot);
            }
        }
        if Self::read_flag(state, root, POWEROFF_PATH) {
            let _ = state.put(root, POWEROFF_PATH, false);
            if self.step == Step::Install && self.install_state == InstallState::Done {
                self.run_power(PowerAction::PowerOff);
            }
        }
    }
}

impl Widget for InstallerApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, root);
            self.bootstrap_done = true;
        }

        self.sync_cached_fields(state, root);
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Forward to the composed card FIRST so a focused field can
        // consume the event before the wizard interprets it. This matters
        // for the Options-step time-zone `Dropdown`: when it's open, Enter
        // selects an option (and the Dropdown marks the event handled) —
        // without forwarding first we'd also raise `NEXT` and skip the
        // step out from under the user.
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&card_node) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                root,
                handled,
                focus_target,
                card_node,
                taffy,
            );
        }

        // Enter advances only when no inner field consumed it: Begin on
        // Review, Next on Disk/Account/Options. We mark the event handled
        // so the App runs its post-event `sync` (which detects the rising
        // edge).
        if !*handled
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
                    if self.encryption_ready() {
                        let _ = state.put(root, BEGIN_PATH, true);
                    }
                    *handled = true;
                }
                Step::Disk | Step::Account | Step::Keyboard | Step::Options => {
                    let _ = state.put(root, NEXT_PATH, true);
                    *handled = true;
                }
                Step::Install => {}
            }
        }
    }

    fn on_external_event(&mut self, payload: &str, _state: &mut AutoCommit, _root: &ObjId) {
        if payload == CTRL_DONE {
            self.mark_install_done();
        } else if payload == CTRL_FAIL {
            self.mark_install_failed(None);
        } else {
            self.status_lines.push(payload.to_string());
            self.dirty_structure = true;
        }
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

    fn needs_commit(&self) -> bool {
        self.composed.as_ref().is_some_and(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        if let Some(c) = &mut self.composed {
            c.on_commit();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Disk> {
        sample_disks()
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

    fn build(app: &mut InstallerApp) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = app.build_layout(&mut taffy, &theme, &mut node_map);
        (taffy, root)
    }

    // --- pure state-machine ---------------------------------------

    #[test]
    fn next_step_walks_forward_and_stops_at_install() {
        assert_eq!(next_step(Step::Keyboard), Some(Step::Disk));
        assert_eq!(next_step(Step::Disk), Some(Step::Account));
        assert_eq!(next_step(Step::Account), Some(Step::Options));
        assert_eq!(next_step(Step::Options), Some(Step::Review));
        assert_eq!(next_step(Step::Review), Some(Step::Install));
        assert_eq!(next_step(Step::Install), None);
    }

    #[test]
    fn prev_step_walks_back_and_stops_at_keyboard() {
        assert_eq!(prev_step(Step::Keyboard), None);
        assert_eq!(prev_step(Step::Disk), Some(Step::Keyboard));
        assert_eq!(prev_step(Step::Account), Some(Step::Disk));
        assert_eq!(prev_step(Step::Options), Some(Step::Account));
        assert_eq!(prev_step(Step::Review), Some(Step::Options));
        assert_eq!(prev_step(Step::Install), None);
    }

    #[test]
    fn step_index_matches_labels() {
        assert_eq!(STEP_LABELS.len(), 6);
        assert_eq!(step_index(Step::Keyboard), 0);
        assert_eq!(step_index(Step::Disk), 1);
        assert_eq!(step_index(Step::Account), 2);
        assert_eq!(step_index(Step::Options), 3);
        assert_eq!(step_index(Step::Review), 4);
        assert_eq!(step_index(Step::Install), 5);
    }

    #[test]
    fn keyboard_step_never_blocks_forward_progress() {
        assert!(validate_step(Step::Keyboard, Some(0), "quinten", "qos").is_ok());
    }

    #[test]
    fn validate_disk_requires_selection() {
        assert!(validate_step(Step::Disk, None, "", "qos").is_err());
        assert!(validate_step(Step::Disk, Some(0), "", "qos").is_ok());
    }

    #[test]
    fn validate_account_requires_username() {
        assert!(validate_step(Step::Account, Some(0), "", "qos").is_err());
        assert!(validate_step(Step::Account, Some(0), "   ", "qos").is_err());
        assert!(validate_step(Step::Account, Some(0), "quinten", "qos").is_ok());
    }

    #[test]
    fn validate_options_requires_valid_hostname() {
        assert!(validate_step(Step::Options, Some(0), "q", "qos").is_ok());
        assert!(validate_step(Step::Options, Some(0), "q", "").is_err());
        assert!(validate_step(Step::Options, Some(0), "q", "-bad").is_err());
        assert!(validate_step(Step::Options, Some(0), "q", "bad host").is_err());
    }

    #[test]
    fn hostname_validator() {
        assert!(valid_hostname("qos"));
        assert!(valid_hostname("lab-1"));
        assert!(!valid_hostname(""));
        assert!(!valid_hostname("-x"));
        assert!(!valid_hostname("x-"));
        assert!(!valid_hostname("has space"));
        assert!(!valid_hostname("under_score"));
    }

    // --- app behaviour --------------------------------------------

    #[test]
    fn next_on_disk_without_selection_sets_status_and_stays() {
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
        assert!(!InstallerApp::read_flag(&doc, &root, NEXT_PATH));
    }

    #[test]
    fn selecting_disk_then_next_advances_to_account() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Disk;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        // Pick the second disk via its flag.
        let _ = doc.put(&root, disk_path(1).as_str(), true);
        app.sync(&mut doc, &root);
        assert_eq!(app.selected_disk, Some(1));
        assert!(!InstallerApp::read_flag(&doc, &root, disk_path(1).as_str()));

        // Now Next advances and requests a focus reset for the new step.
        let _ = doc.put(&root, NEXT_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.step, Step::Account);
        assert!(app.take_focus_reset(), "step change requests focus reset");
    }

    #[test]
    fn account_step_requires_username_before_advancing() {
        let mut app = InstallerApp::with_disks(sample());
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

        // Provide a username through the CRDT Text object, then advance.
        let (_, uobj) = doc.get(&root, USERNAME_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&uobj, 0, 0, "quinten");
        let _ = doc.put(&root, NEXT_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.step, Step::Options);
        assert_eq!(app.username, "quinten");
    }

    #[test]
    fn keyboard_selection_threads_layout_into_install_options() {
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

    #[test]
    fn filesystem_flag_updates_selection() {
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
        assert!(!InstallerApp::read_flag(&doc, &root, fs_path(2).as_str()));
    }

    #[test]
    fn encrypt_flag_threads_into_install_options() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Options;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        // Default: encryption off; install options omit it.
        assert!(!app.encrypt);
        assert!(!app.install_options().encrypt);

        // Raise the "on" segment (index 1) → encryption enabled.
        let _ = doc.put(&root, encrypt_path(1).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(app.encrypt, "selecting the LUKS segment enables encryption");
        assert!(app.install_options().encrypt);
        assert!(!InstallerApp::read_flag(
            &doc,
            &root,
            encrypt_path(1).as_str()
        ));

        // Raise the "off" segment (index 0) → back to unencrypted.
        let _ = doc.put(&root, encrypt_path(0).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(
            !app.encrypt,
            "selecting the off segment disables encryption"
        );
        assert!(!app.install_options().encrypt);
    }

    #[test]
    fn fido2_flag_only_threads_into_options_when_encrypting() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Options;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        // Raise FIDO2 "on" while encryption is OFF: the field tracks the
        // toggle, but install_options drops it (only meaningful with LUKS).
        let _ = doc.put(&root, fido2_path(1).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(app.fido2, "the toggle state is tracked");
        assert!(
            !app.install_options().fido2,
            "FIDO2 must not reach the backend without encryption"
        );

        // Turn encryption on → FIDO2 now flows through.
        let _ = doc.put(&root, encrypt_path(1).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(app.install_options().encrypt);
        assert!(
            app.install_options().fido2,
            "FIDO2 threads through with LUKS"
        );
    }

    #[test]
    fn pin_text_threads_into_options_only_when_encrypting() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Options;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        // Type a PIN into the CRDT Text while encryption is OFF.
        let (_, pobj) = doc.get(&root, PIN_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&pobj, 0, 0, "2468");
        app.sync(&mut doc, &root);
        assert_eq!(app.pin, "2468");
        assert!(
            app.install_options().pin.is_empty(),
            "PIN must not reach the backend without encryption"
        );

        // Enable encryption → the PIN now flows through.
        let _ = doc.put(&root, encrypt_path(1).as_str(), true);
        app.sync(&mut doc, &root);
        assert_eq!(app.install_options().pin, "2468");
    }

    #[test]
    fn turning_encryption_off_clears_pin_fido2_and_ack() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Options;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        // Encrypt on, with a PIN, FIDO2, and acknowledgement.
        let _ = doc.put(&root, encrypt_path(1).as_str(), true);
        let _ = doc.put(&root, fido2_path(1).as_str(), true);
        let (_, pobj) = doc.get(&root, PIN_PATH).unwrap().unwrap();
        let _ = doc.splice_text(&pobj, 0, 0, "1357");
        let _ = doc.put(&root, RECOVERY_ACK_PATH, true);
        app.sync(&mut doc, &root);
        assert!(app.encrypt && app.fido2 && app.recovery_ack && app.pin == "1357");

        // Encrypt off → every dependent factor is reset.
        let _ = doc.put(&root, encrypt_path(0).as_str(), true);
        app.sync(&mut doc, &root);
        assert!(!app.encrypt);
        assert!(!app.fido2, "FIDO2 cleared when encryption turned off");
        assert!(!app.recovery_ack, "ack cleared when encryption turned off");
        assert!(app.pin.is_empty(), "PIN cleared when encryption turned off");
        let opts = app.install_options();
        assert!(!opts.encrypt && !opts.fido2 && opts.pin.is_empty());
    }

    #[test]
    fn recovery_ack_toggles_and_gates_begin() {
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.step = Step::Review;
        app.encrypt = true;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(!app.recovery_ack);

        // Begin while encrypting + un-acknowledged: install must NOT start.
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
        assert!(!InstallerApp::read_flag(&doc, &root, RECOVERY_ACK_PATH));

        // Toggle off again (it's a checkbox-style flip).
        let _ = doc.put(&root, RECOVERY_ACK_PATH, true);
        app.sync(&mut doc, &root);
        assert!(!app.recovery_ack, "ack flips back off");
    }

    #[test]
    fn encryption_summary_reflects_factors() {
        let mut app = InstallerApp::with_disks(sample());
        // Pin the hardware: sampled from the host, this would assert something
        // different on a machine with no TPM.
        app.tpm2 = true;
        assert_eq!(app.encryption_summary(), "Off");

        app.encrypt = true;
        assert_eq!(app.encryption_summary(), "On (LUKS — recovery key, TPM2)");

        app.pin = "1234".to_string();
        assert_eq!(
            app.encryption_summary(),
            "On (LUKS — recovery key, TPM2 + PIN)"
        );

        app.fido2 = true;
        assert_eq!(
            app.encryption_summary(),
            "On (LUKS — recovery key, TPM2 + PIN, FIDO2)"
        );
    }

    #[test]
    fn without_a_tpm_the_unlock_secret_is_required() {
        // A machine with no TPM cannot unlock itself. Encryption still works —
        // that is the point of building the volume around a recovery key — but
        // with no passphrase enrolled, the recovery key is what gets typed at
        // every boot. Requiring the secret is what keeps the degradation
        // usable instead of merely correct.
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.encrypt = true;
        app.tpm2 = false;
        app.recovery_ack = true;
        assert!(
            !app.encryption_ready(),
            "no TPM and no passphrase must not be installable"
        );

        app.pin = "hunter2".to_string();
        assert!(app.encryption_ready(), "a passphrase unblocks it");

        // Whitespace is not a passphrase.
        app.pin = "   ".to_string();
        assert!(
            !app.encryption_ready(),
            "blank passphrase is still no passphrase"
        );
    }

    #[test]
    fn with_a_tpm_the_unlock_secret_stays_optional() {
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.encrypt = true;
        app.tpm2 = true;
        app.recovery_ack = true;
        assert!(
            app.encryption_ready(),
            "the TPM unlocks it, so the PIN is a choice"
        );
    }

    #[test]
    fn encryption_ready_is_vacuous_without_encryption() {
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.tpm2 = false;
        assert!(app.encryption_ready(), "no encryption, nothing to gate on");
    }

    #[test]
    fn summary_and_description_name_the_passphrase_when_there_is_no_tpm() {
        let mut app = InstallerApp::with_disks(sample());
        app.encrypt = true;
        app.tpm2 = false;
        app.pin = "hunter2".to_string();
        assert_eq!(
            app.encryption_summary(),
            "On (LUKS — recovery key, boot passphrase)",
            "the review step must not claim a TPM factor this machine cannot provide"
        );

        app.pin = String::new();
        assert_eq!(app.encryption_summary(), "On (LUKS — recovery key)");

        assert!(
            encrypt_description(true, false).contains("no TPM2 chip"),
            "the chooser has to say the machine cannot unlock itself"
        );
        assert!(
            !encrypt_description(true, true).contains("Requires a TPM2 device"),
            "encryption no longer requires a TPM, and the copy must not claim it does"
        );
    }

    #[test]
    fn review_begin_disabled_until_ack_when_encrypting() {
        // Encrypting + un-acknowledged: the Begin button (nav row's primary,
        // its last child) must be disabled (non-focusable).
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.step = Step::Review;
        app.encrypt = true;
        app.tpm2 = true;
        let _ = build(&mut app);
        let card = app.children()[0];
        let begin_disabled = card
            .children()
            .last()
            .and_then(|nav| nav.children().last().map(|b| !b.is_focusable()));
        assert_eq!(begin_disabled, Some(true), "Begin disabled until ack");

        // Acknowledge → Begin enables.
        let mut app2 = InstallerApp::with_disks(sample());
        app2.selected_disk = Some(0);
        app2.step = Step::Review;
        app2.encrypt = true;
        app2.tpm2 = true;
        app2.recovery_ack = true;
        let _ = build(&mut app2);
        let card2 = app2.children()[0];
        let begin_enabled = card2
            .children()
            .last()
            .and_then(|nav| nav.children().last().map(|b| b.is_focusable()));
        assert_eq!(begin_enabled, Some(true), "Begin enabled once acknowledged");
    }

    #[test]
    fn unencrypted_review_begin_always_enabled() {
        // No encryption ⇒ no acknowledgement needed; Begin is enabled.
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.step = Step::Review;
        app.encrypt = false;
        let _ = build(&mut app);
        let card = app.children()[0];
        let begin_enabled = card
            .children()
            .last()
            .and_then(|nav| nav.children().last().map(|b| b.is_focusable()));
        assert_eq!(begin_enabled, Some(true));
    }

    #[test]
    fn options_step_shows_pin_and_fido2_only_when_encrypting() {
        // Encryption off: no PIN input, no FIDO2 SegChoice.
        let mut off = InstallerApp::with_disks(sample());
        off.step = Step::Options;
        off.encrypt = false;
        let _ = build(&mut off);
        let card = off.children()[0];
        let has_fido2 = card
            .children()
            .iter()
            .any(|c| c.test_id() == Some("fido2_choice"));
        assert!(!has_fido2, "no FIDO2 control when encryption is off");

        // Encryption on: the FIDO2 SegChoice appears.
        let mut on = InstallerApp::with_disks(sample());
        on.step = Step::Options;
        on.encrypt = true;
        let _ = build(&mut on);
        let card = on.children()[0];
        let has_fido2 = card
            .children()
            .iter()
            .any(|c| c.test_id() == Some("fido2_choice"));
        assert!(has_fido2, "FIDO2 control shows when encrypting");
    }

    #[test]
    fn options_step_uses_seeded_hostname_and_timezone() {
        let mut app = InstallerApp::with_disks(sample());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert_eq!(app.hostname, "qos");
        assert_eq!(app.timezone, "UTC");
        let opts = app.install_options();
        assert_eq!(opts.hostname, "qos");
        assert_eq!(opts.timezone, "UTC");
        assert_eq!(opts.filesystem, "btrfs");
    }

    #[test]
    fn back_walks_the_wizard_in_reverse() {
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
        assert_eq!(app.step, Step::Keyboard);

        // Back from the first step (Keyboard) is a no-op.
        let _ = doc.put(&root, BACK_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.step, Step::Keyboard);
    }

    #[test]
    fn begin_only_runs_install_from_review() {
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
        assert!(!InstallerApp::read_flag(&doc, &root, BEGIN_PATH));
    }

    #[test]
    fn failed_install_back_returns_to_review() {
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.step = Step::Install;
        app.install_state = InstallState::Failed;
        app.install_launched = true;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        let _ = doc.put(&root, BACK_PATH, true);
        app.sync(&mut doc, &root);
        assert_eq!(app.step, Step::Review);
        assert_eq!(app.install_state, InstallState::Idle);
        assert!(!app.install_launched, "retry must re-arm the launch guard");
    }

    #[test]
    fn external_lines_stream_into_log_and_sentinels_set_state() {
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
            "done sets focus reset for finish row"
        );
    }

    #[test]
    fn external_fail_sentinel_marks_failed() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Install;
        app.install_state = InstallState::Running;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.on_external_event(CTRL_FAIL, &mut doc, &root);
        assert_eq!(app.install_state, InstallState::Failed);
    }

    #[test]
    fn confirm_shows_selected_device() {
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(1);
        assert_eq!(app.selected_dev().as_deref(), Some("/dev/nvme0n1"));
    }

    // --- structural invariants (widget tree mirrors taffy) --------

    #[test]
    fn every_step_widget_tree_mirrors_taffy() {
        for step in [
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
    fn account_step_inputs_focusable() {
        let mut app = InstallerApp::with_disks(sample());
        app.selected_disk = Some(0);
        app.step = Step::Account;
        let (_taffy, _root) = build(&mut app);

        // username + password + Back + Next = 4 focusable widgets.
        assert_eq!(
            count_focusable(&app),
            4,
            "2 inputs + 2 nav buttons must be focusable"
        );
    }

    #[test]
    fn keyboard_step_has_dropdown_and_nav_and_a_preview() {
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
        let card = app.children()[0];
        assert!(
            card.children().iter().any(|c| c.name() == "KeyboardView"),
            "the Keyboard step renders the visual keyboard preview"
        );
    }

    #[test]
    fn disk_step_first_button_disabled_until_selection() {
        // Build without a selection: the nav row's primary ("Next", its last
        // child after "Back") must be non-focusable (disabled). After
        // selecting a disk it enables.
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Disk;
        let _ = build(&mut app);
        let card = app.children()[0];
        let next_disabled = card
            .children()
            .last()
            .and_then(|nav| nav.children().last().map(|b| !b.is_focusable()));
        assert_eq!(next_disabled, Some(true), "Next disabled with no disk");

        let mut app2 = InstallerApp::with_disks(sample());
        app2.step = Step::Disk;
        app2.selected_disk = Some(0);
        let _ = build(&mut app2);
        let card2 = app2.children()[0];
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

    #[test]
    fn disk_step_rows_are_focusable() {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Disk;
        let _ = build(&mut app);
        let card = app.children()[0];
        let fields = card.children();
        let disk_rows = fields.iter().filter(|c| c.name() == "DiskRow").count();
        assert_eq!(disk_rows, 2, "both sample disks render as rows");
        assert!(
            fields
                .iter()
                .any(|c| c.name() == "DiskRow" && c.is_focusable())
        );
    }
}
